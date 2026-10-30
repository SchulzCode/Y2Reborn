/* SPDX-License-Identifier: MIT
 *
 * The only native media membrane used by Reborn. FFmpeg owns demuxing,
 * decoding, the floating point filter graph, artwork conversion and the final
 * libswresample conversion. Rust sees an opaque, single-owner context and a
 * byte-oriented stereo PCM stream.
 */
#define _POSIX_C_SOURCE 200809L
#define _FILE_OFFSET_BITS 64
#include <libavcodec/avcodec.h>
#include <libavcodec/packet.h>
#include <libavcodec/version.h>
#include <libavfilter/avfilter.h>
#include <libavfilter/buffersink.h>
#include <libavfilter/buffersrc.h>
#include <libavfilter/version.h>
#include <libavformat/avformat.h>
#include <libavformat/version.h>
#include <libavutil/avutil.h>
#include <libavutil/channel_layout.h>
#include <libavutil/dict.h>
#include <libavutil/error.h>
#include <libavutil/frame.h>
#include <libavutil/intreadwrite.h>
#include <libavutil/mem.h>
#include <libavutil/opt.h>
#include <libavutil/samplefmt.h>
#include <libavutil/replaygain.h>
#include <libswresample/swresample.h>
#include <libswresample/version.h>
#include <libswscale/swscale.h>
#include <libswscale/version.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <sys/stat.h>
#include <unistd.h>
#include <ctype.h>
#include <fcntl.h>
#include <math.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

typedef struct {
  atomic_int cancelled;
  atomic_int_fast64_t deadline;
} RbCancel;

static int64_t mono(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec * 1000LL + t.tv_nsec / 1000000;
}
RbCancel *rb_cancel_new(void) {
  RbCancel *c = calloc(1, sizeof(*c));
  if (c) {
    atomic_init(&c->cancelled, 0);
    atomic_init(&c->deadline, mono() + 10000);
  }
  return c;
}
void rb_cancel_free(RbCancel *c) { free(c); }
void rb_cancel_set(RbCancel *c) { atomic_store(&c->cancelled, 1); }
static int interrupted(void *p) {
  RbCancel *c = p;
  return !c || atomic_load(&c->cancelled) || mono() > atomic_load(&c->deadline);
}
static void deadline(RbCancel *c) {
  if (c)
    atomic_store(&c->deadline, mono() + 10000);
}

static void (*logger)(int, const char *);
static int (*log_enabled)(int);
static void bridge(void *ctx, int level, const char *fmt, va_list args) {
  if (!logger || !log_enabled || !log_enabled(level))
    return;
  char line[1024];
  int prefix = 1;
  av_log_format_line2(ctx, level, fmt, args, line, sizeof(line), &prefix);
  logger(level, line);
}
void rb_media_logging(void (*callback)(int, const char *),
                      int (*enabled)(int)) {
  log_enabled = enabled;
  av_log_set_level(AV_LOG_TRACE);
  logger = callback;
  av_log_set_callback(bridge);

}
/* Called exactly once by Rust before exposing the loaded API to workers. */
/* One allocation's ceiling. The MP4 demuxer keeps a whole file's frame
 * index in one block (24 bytes a frame): 128 MiB holds a ≈36-hour AAC-LC
 * audiobook at 44.1 kHz (32 MiB stopped at ≈9 h: no audio, no seeking). */
void rb_media_limits(void) { av_max_alloc(128 * 1024 * 1024); }
const char *rb_media_version(void) { return av_version_info(); }
void rb_media_error(int code, char *out, int size) {
  av_strerror(code, out, size);
}

typedef struct {
  char title[512], artist[512], album[512], album_artist[512], codec[64];
  char source_sample_fmt[32], decoder_sample_fmt[32], internal_sample_fmt[32];
  char final_sample_fmt[32], filters[1024], resample_reason[256];
  uint64_t duration_ms, bitrate, skip_start, skip_end, corrupt_packets;
  uint32_t rate, channels, source_bits, output_rate, output_channels;
  uint32_t track, disc, artwork, output_format, resampling, format_conversion;
  uint32_t replaygain_mode, eq_active;
  double track_gain_db, album_gain_db, track_peak, album_peak;
  double applied_gain_db, headroom_db;
  char date[128], genre[512];
  uint64_t open_us, probe_us, io_bytes;
  uint32_t info_runs;
} RbMetadata;

#define RB_EQ_BANDS 10
typedef struct {
  int replay_gain_mode;
  uint32_t volume;
  uint32_t eq_count;
  struct {
    double frequency_hz;
    double gain_db;
    double q;
    uint32_t kind; /* 0 peaking, 1 low shelf, 2 high shelf */
  } eq[RB_EQ_BANDS];
  uint32_t crossfade_ms;
  double preamp_db; /* <= 0, before the equalizer */
  double speed;     /* playback speed (an audiobook's), 0.5..2; 1 is none */
} RbDspConfig;
/* Pinned with RawDspConfig in src/native.rs (same numbers on ARM and x86-64). */
_Static_assert(sizeof(RbDspConfig) == 360 && offsetof(RbDspConfig, eq) == 16 &&
                   sizeof(((RbDspConfig *)0)->eq[0]) == 32 &&
                   offsetof(RbDspConfig, eq[0].kind) == 40 &&
                   offsetof(RbDspConfig, crossfade_ms) == 336 &&
                   offsetof(RbDspConfig, preamp_db) == 344 &&
                   offsetof(RbDspConfig, speed) == 352,
               "RbDspConfig layout differs from RawDspConfig");

typedef struct {
  AVFormatContext *f;
  AVIOContext *io;
  FILE *file;
  /* Internet radio: a pipe from the network fetcher, never a socket. */
  int stream_fd;
  int64_t file_size;
  uint64_t io_read;
  int io_limited;
  AVCodecContext *c;
  AVPacket *p;
  AVFrame *frame;
  AVFilterGraph *graph;
  AVFilterContext *src;
  AVFilterContext *sink;
  SwrContext *canonical_swr;
  SwrContext *output_swr;
  RbCancel *cancel;
  RbDspConfig dsp;
  int stream, output_rate, output_bytes, output_format;
  enum AVSampleFormat output_fmt;
  int decoder_eof, source_eof, source_null_sent, filter_eof, output_drained;
  int first_frame;
  int seek_pending;
  uint64_t skip_start, skip_end, packets, frames, corrupt_packets;
  int64_t position, seek_target, seek_trim_remaining;
  uint8_t *pending;
  size_t pending_frames, pending_offset, pending_capacity;
  double applied_gain_db, headroom_db;
  char filter_description[1024];
  uint64_t open_us, probe_us;
  uint32_t info_runs;
} RbMedia;

static const char *tag(RbMedia *m, const char *key) {
  AVDictionaryEntry *e = av_dict_get(m->f->metadata, key, NULL, 0);
  if (!e || !*e->value)
    e = av_dict_get(m->f->streams[m->stream]->metadata, key, NULL, 0);
  return e ? e->value : "";
}
static int parse_tag_number(const char *s, double minimum, double maximum,
                            double *value, int allow_db) {
  if (!s || !*s || !value)
    return 0;
  errno = 0;
  char *end = NULL;
  double parsed = strtod(s, &end);
  if (end == s || errno == ERANGE || !isfinite(parsed) || parsed < minimum ||
      parsed > maximum)
    return 0;
  while (*end && isspace((unsigned char)*end))
    end++;
  /* ReplayGain gain tags conventionally carry a human-readable "dB"
   * suffix. Accept that suffix, while still rejecting any other trailing
   * text. */
  if (allow_db && strncasecmp(end, "db", 2) == 0) {
    end += 2;
    while (*end && isspace((unsigned char)*end))
      end++;
  }
  if (*end)
    return 0;
  *value = parsed;
  return 1;
}
static const AVReplayGain *replaygain(RbMedia *m) {
  AVCodecParameters *p = m->f->streams[m->stream]->codecpar;
  const AVPacketSideData *sd = av_packet_side_data_get(
      p->coded_side_data, p->nb_coded_side_data, AV_PKT_DATA_REPLAYGAIN);
  return sd && sd->size >= sizeof(AVReplayGain) ? (const AVReplayGain *)sd->data : NULL;
}
static int gain_tag_value(RbMedia *m, const char *upper, const char *lower,
                          double *value) {
  if (parse_tag_number(tag(m, upper), -60.0, 60.0, value, 1))
    return 1;
  if (*tag(m, lower))
    return parse_tag_number(tag(m, lower), -60.0, 60.0, value, 1);
  const AVReplayGain *rg = replaygain(m);
  if (!rg) return 0;
  int32_t gain = strstr(lower, "album") ? rg->album_gain : rg->track_gain;
  if (gain == INT32_MIN) return 0;
  *value = gain / 100000.0;
  return *value >= -60.0 && *value <= 60.0;
}
static int peak_tag_value(RbMedia *m, const char *upper, const char *lower,
                          double *value) {
  if (parse_tag_number(tag(m, upper), 0.0, 16.0, value, 0))
    return 1;
  if (*tag(m, lower))
    return parse_tag_number(tag(m, lower), 0.0, 16.0, value, 0);
  const AVReplayGain *rg = replaygain(m);
  if (!rg) return 0;
  uint32_t peak = strstr(lower, "album") ? rg->album_peak : rg->track_peak;
  *value = peak / 100000.0;
  return peak > 0 && *value <= 16.0;
}
static double gain_tag(RbMedia *m, const char *upper, const char *lower) {
  double value = 0.0;
  (void)gain_tag_value(m, upper, lower, &value);
  return value;
}
static double peak_tag(RbMedia *m, const char *upper, const char *lower) {
  double value = 0.0;
  (void)peak_tag_value(m, upper, lower, &value);
  return value;
}
static void text(char *dst, size_t n, const char *src) {
  if (!n) return;
  size_t length = strnlen(src ? src : "", n - 1);
  /* Do not split a UTF-8 code point at the bounded ABI edge. Invalid input
   * elsewhere is repaired by Rust's lossy conversion, as before. */
  if (src && src[length])
    while (length && ((unsigned char)src[length] & 0xc0) == 0x80) length--;
  if (length) memcpy(dst, src, length);
  dst[length] = 0;
}
static enum AVSampleFormat format_from_int(int format) {
  switch (format) {
  case 1:
    return AV_SAMPLE_FMT_S16;
  case 2:
  case 3: /* S24_LE is packed into the low three bytes of an S32 container. */
    return AV_SAMPLE_FMT_S32;
  default:
    return AV_SAMPLE_FMT_NONE;
  }
}

static uint32_t read_s24le(const uint8_t *p) {
  return (uint32_t)p[0] | ((uint32_t)p[1] << 8) | ((uint32_t)p[2] << 16);
}

static void normalize_s24le(const uint8_t *input, uint8_t *output,
                            size_t samples) {
  for (size_t i = 0; i < samples; i++) {
    memcpy(output + i * 4, input + i * 4, 3);
    output[i * 4 + 3] = 0;
  }
}

static void unpack_s24le_to_s32(const uint8_t *input, uint8_t *output,
                                size_t samples) {
  for (size_t i = 0; i < samples; i++) {
    uint32_t value = read_s24le(input + i * 4);
    if (value & 0x00800000u)
      value |= 0xff000000u;
    uint32_t q31 = value << 8;
    memcpy(output + i * 4, &q31, sizeof(q31));
  }
}

/* Final integer narrowing: nearest, ties to even, then saturate. Arithmetic
 * right shifts bias negative samples. Exact destination-grid values survive.
 * The only callers use divisors 256 (S24) and 65536 (S16).
 */
static int32_t quantize_q31(int32_t q31, int32_t divisor) {
    int32_t rounded = q31 / divisor;
    int32_t remainder = q31 % divisor;
    if (remainder > divisor / 2 ||
        (remainder == divisor / 2 && (rounded % 2)))
      rounded++;
    if (remainder < -divisor / 2 ||
        (remainder == -divisor / 2 && (rounded % 2)))
      rounded--;
    int32_t maximum = INT32_MAX / divisor;
    return rounded > maximum ? maximum : rounded;
}

static void pack_s32_to_s24le(const uint8_t *input, uint8_t *output,
                              size_t samples) {
  for (size_t i = 0; i < samples; i++) {
    int32_t q31;
    memcpy(&q31, input + i * 4, sizeof(q31));
    uint32_t value = (uint32_t)quantize_q31(q31, 256);
    output[i * 4] = (uint8_t)value;
    output[i * 4 + 1] = (uint8_t)(value >> 8);
    output[i * 4 + 2] = (uint8_t)(value >> 16);
    output[i * 4 + 3] = 0; /* ALSA's S24_LE padding byte. */
  }
}

/* Final sink conversion is kept here with the media membrane.  Playback may
 * use S32 as the transition working format even when the currently qualified
 * ALSA sink is S16; Rust never performs a sample-format conversion itself. */
int rb_media_convert(const uint8_t *input, int frames, int rate, int input_format,
                     int output_format, uint8_t *output, int capacity_frames) {
  if (!input || !output || frames <= 0 || capacity_frames < frames ||
      rate < 8000 || rate > 384000 ||
      (input_format != 1 && input_format != 2 && input_format != 3) ||
      (output_format != 1 && output_format != 2 && output_format != 3))
    return AVERROR(EINVAL);
  if ((size_t)frames > SIZE_MAX / (2 * sizeof(int32_t)))
    return AVERROR(EINVAL);
  size_t samples = (size_t)frames * 2;
  if (input_format == 2 && output_format == 1) {
    for (size_t i = 0; i < samples; i++) {
      int32_t q31;
      memcpy(&q31, input + i * 4, sizeof(q31));
      uint32_t value = (uint32_t)quantize_q31(q31, 65536);
      output[i * 2] = (uint8_t)value;
      output[i * 2 + 1] = (uint8_t)(value >> 8);
    }
    return frames;
  }
  enum AVSampleFormat in_fmt = format_from_int(input_format);
  enum AVSampleFormat out_fmt = format_from_int(output_format);
  if (input_format == output_format) {
    if (input_format == 3)
      normalize_s24le(input, output, samples);
    else
      memcpy(output, input, samples * (size_t)av_get_bytes_per_sample(in_fmt));
    return frames;
  }
  uint8_t *normalized_input = NULL;
  uint8_t *converted_output = NULL;
  const uint8_t *swr_input = input;
  uint8_t *swr_destination = output;
  if (input_format == 3) {
    normalized_input = av_malloc_array(samples, sizeof(int32_t));
    if (!normalized_input)
      return AVERROR(ENOMEM);
    unpack_s24le_to_s32(input, normalized_input, samples);
    swr_input = normalized_input;
  }
  if (output_format == 3) {
    converted_output = av_malloc_array(samples, sizeof(int32_t));
    if (!converted_output) {
      av_free(normalized_input);
      return AVERROR(ENOMEM);
    }
    swr_destination = converted_output;
  }
  AVChannelLayout stereo = AV_CHANNEL_LAYOUT_STEREO;
  SwrContext *swr = NULL;
  int r = swr_alloc_set_opts2(&swr, &stereo, out_fmt, rate, &stereo, in_fmt,
                              rate, 0, NULL);
  if (r < 0 || (r = swr_init(swr)) < 0) {
    swr_free(&swr);
    av_free(normalized_input);
    av_free(converted_output);
    return r < 0 ? r : AVERROR(EINVAL);
  }
  uint8_t *out[] = {swr_destination};
  r = swr_convert(swr, out, capacity_frames, &swr_input, frames);
  swr_free(&swr);
  if (r >= 0 && output_format == 3)
    pack_s32_to_s24le(converted_output, output, (size_t)r * 2);
  av_free(normalized_input);
  av_free(converted_output);
  return r < 0 ? r : r;
}

static int crossfade_frame(const uint8_t *input, int frames, int rate,
                           int input_format, AVFrame **out,
                           SwrContext **to_float) {
  AVChannelLayout stereo = AV_CHANNEL_LAYOUT_STEREO;
  AVFrame *frame = av_frame_alloc();
  if (!frame)
    return AVERROR(ENOMEM);
  frame->format = AV_SAMPLE_FMT_FLTP;
  frame->sample_rate = rate;
  frame->ch_layout = stereo;
  frame->nb_samples = frames;
  int r = av_frame_get_buffer(frame, 0);
  int new_swr = 0;
  if (r >= 0 && !*to_float) {
    r = swr_alloc_set_opts2(to_float, &stereo, AV_SAMPLE_FMT_FLTP, rate,
                            &stereo, format_from_int(input_format), rate, 0,
                            NULL);
    new_swr = r >= 0;
  }
  if (r >= 0 && new_swr)
    r = swr_init(*to_float);
  uint8_t *normalized_input = NULL;
  const uint8_t *swr_input = input;
  if (r >= 0 && input_format == 3) {
    if ((size_t)frames > SIZE_MAX / (2 * sizeof(int32_t))) {
      r = AVERROR(EINVAL);
    } else {
      normalized_input = av_malloc_array((size_t)frames * 2, sizeof(int32_t));
      if (!normalized_input)
        r = AVERROR(ENOMEM);
      else {
        unpack_s24le_to_s32(input, normalized_input, (size_t)frames * 2);
        swr_input = normalized_input;
      }
    }
  }
  if (r >= 0) {
    r = swr_convert(*to_float, frame->extended_data, frames,
                    &swr_input, frames);
    if (r >= 0 && r != frames)
      r = AVERROR(EINVAL);
  }
  av_free(normalized_input);
  if (r < 0) {
    av_frame_free(&frame);
    return r;
  }
  *out = frame;
  return 0;
}

/* Process one bounded transition window through libavfilter's acrossfade.
 * The input and output are already at the qualified sink rate, but the
 * filter itself always receives the canonical FLTP representation. */
int rb_media_crossfade(const uint8_t *a, const uint8_t *b, int frames, int rate,
                       int format, uint8_t *output, int capacity_frames,
                       int *written_frames) {
  if (!a || !b || !output || !written_frames || frames <= 0 ||
      capacity_frames < frames || rate < 8000 || rate > 384000 ||
      (format != 1 && format != 2 && format != 3))
    return AVERROR(EINVAL);
  *written_frames = 0;
  AVFilterGraph *graph = NULL;
  AVFilterContext *in_a = NULL, *in_b = NULL, *xfade = NULL, *sink = NULL;
  SwrContext *to_float = NULL, *from_float = NULL;
  AVFrame *frame_a = NULL, *frame_b = NULL, *filtered = NULL;
  AVChannelLayout stereo = AV_CHANNEL_LAYOUT_STEREO;
  int r = AVERROR(ENOMEM);
  graph = avfilter_graph_alloc();
  if (!graph)
    goto done;
  /* Acrossfade and its buffer endpoints have no slice-parallel work. */
  graph->nb_threads = 1;
  char args[256];
  snprintf(args, sizeof(args),
           "time_base=1/%d:sample_rate=%d:sample_fmt=fltp:channel_layout=stereo",
           rate, rate);
  const AVFilter *abuffer = avfilter_get_by_name("abuffer");
  const AVFilter *across = avfilter_get_by_name("acrossfade");
  const AVFilter *abuffersink = avfilter_get_by_name("abuffersink");
  if (!abuffer || !across || !abuffersink) {
    r = AVERROR_FILTER_NOT_FOUND;
    goto done;
  }
  if ((r = avfilter_graph_create_filter(&in_a, abuffer, "in_a", args, NULL,
                                        graph)) < 0)
    goto done;
  if ((r = avfilter_graph_create_filter(&in_b, abuffer, "in_b", args, NULL,
                                        graph)) < 0)
    goto done;
  snprintf(args, sizeof(args), "nb_samples=%d:overlap=1", frames);
  if ((r = avfilter_graph_create_filter(&xfade, across, "acrossfade", args,
                                        NULL, graph)) < 0)
    goto done;
  if ((r = avfilter_graph_create_filter(&sink, abuffersink, "out",
                                        "sample_formats=fltp:channel_layouts=stereo",
                                        NULL, graph)) < 0)
    goto done;
  if ((r = avfilter_link(in_a, 0, xfade, 0)) < 0 ||
      (r = avfilter_link(in_b, 0, xfade, 1)) < 0 ||
      (r = avfilter_link(xfade, 0, sink, 0)) < 0 ||
      (r = avfilter_graph_config(graph, NULL)) < 0)
    goto done;
  if ((r = crossfade_frame(a, frames, rate, format, &frame_a, &to_float)) < 0)
    goto done;
  if ((r = crossfade_frame(b, frames, rate, format, &frame_b, &to_float)) < 0)
    goto done;
  if ((r = av_buffersrc_add_frame(in_a, frame_a)) < 0)
    goto done;
  av_frame_free(&frame_a);
  if ((r = av_buffersrc_add_frame(in_a, NULL)) < 0)
    goto done;
  if ((r = av_buffersrc_add_frame(in_b, frame_b)) < 0)
    goto done;
  av_frame_free(&frame_b);
  if ((r = av_buffersrc_add_frame(in_b, NULL)) < 0)
    goto done;
  if ((r = swr_alloc_set_opts2(&from_float, &stereo, format_from_int(format),
                               rate, &stereo, AV_SAMPLE_FMT_FLTP, rate, 0,
                               NULL)) < 0 ||
      (r = swr_init(from_float)) < 0)
    goto done;
  for (;;) {
    filtered = av_frame_alloc();
    if (!filtered) {
      r = AVERROR(ENOMEM);
      goto done;
    }
    r = av_buffersink_get_frame(sink, filtered);
    if (r == AVERROR(EAGAIN) || r == AVERROR_EOF) {
      r = 0;
      av_frame_free(&filtered);
      break;
    }
    if (r < 0)
      goto done;
    int room = capacity_frames - *written_frames;
    if (room <= 0) {
      r = AVERROR(ENOSPC);
      goto done;
    }
    int sample_bytes = av_get_bytes_per_sample(format_from_int(format));
    if (sample_bytes <= 0) {
      av_frame_free(&filtered);
      filtered = NULL;
      r = AVERROR(EINVAL);
      goto done;
    }
    size_t room_samples = (size_t)room * 2;
    uint8_t *temporary_output = NULL;
    uint8_t *destination = output + (size_t)*written_frames * 2 * sample_bytes;
    if (format == 3) {
      if (room_samples > SIZE_MAX / sizeof(int32_t)) {
        av_frame_free(&filtered);
        filtered = NULL;
        r = AVERROR(EINVAL);
        goto done;
      }
      temporary_output = av_malloc_array(room_samples, sizeof(int32_t));
      if (!temporary_output) {
        av_frame_free(&filtered);
        filtered = NULL;
        r = AVERROR(ENOMEM);
        goto done;
      }
      destination = temporary_output;
    }
    uint8_t *out[] = {destination};
    int n = swr_convert(from_float, out, room,
                        (const uint8_t **)filtered->extended_data,
                        filtered->nb_samples);
    av_frame_free(&filtered);
    filtered = NULL;
    if (n < 0) {
      av_free(temporary_output);
      r = n;
      goto done;
    }
    if (format == 3)
      pack_s32_to_s24le(temporary_output, output + (size_t)*written_frames * 8,
                        (size_t)n * 2);
    av_free(temporary_output);
    *written_frames += n;
  }
done:
  av_frame_free(&frame_a);
  av_frame_free(&frame_b);
  av_frame_free(&filtered);
  swr_free(&to_float);
  swr_free(&from_float);
  avfilter_graph_free(&graph);
  return r;
}

static int append_pending(RbMedia *m, const uint8_t *data, size_t frames) {
  if (frames > SIZE_MAX / (size_t)m->output_bytes)
    return AVERROR(EINVAL);
  size_t bytes = frames * (size_t)m->output_bytes;
  size_t available = m->pending_frames - m->pending_offset;
  if (m->pending_offset && available)
    memmove(m->pending, m->pending + m->pending_offset * m->output_bytes,
            available * m->output_bytes);
  m->pending_frames = available;
  m->pending_offset = 0;
  if (frames > (SIZE_MAX / (size_t)m->output_bytes) - m->pending_frames)
    return AVERROR(EINVAL);
  size_t need = (m->pending_frames + frames) * (size_t)m->output_bytes;
  if (need > 32 * 1024 * 1024) return AVERROR(EFBIG);
  if (need > m->pending_capacity) {
    size_t cap = m->pending_capacity ? m->pending_capacity : 65536;
    while (cap < need) {
      if (cap > SIZE_MAX / 2)
        return AVERROR(ENOMEM);
      cap *= 2;
    }
    uint8_t *next = realloc(m->pending, cap);
    if (!next)
      return AVERROR(ENOMEM);
    m->pending = next;
    m->pending_capacity = cap;
  }
  uint8_t *destination = m->pending + m->pending_frames * m->output_bytes;
  if (m->output_format == 3)
    pack_s32_to_s24le(data, destination, frames * 2);
  else
    memcpy(destination, data, bytes);
  m->pending_frames += frames;
  return 0;
}

static int queue_filter_frame(RbMedia *m, AVFrame *filtered) {
  int max = swr_get_out_samples(m->output_swr, filtered->nb_samples);
  if (max < filtered->nb_samples)
    max = filtered->nb_samples + 1024;
  uint8_t **data = NULL;
  int r = av_samples_alloc_array_and_samples(&data, NULL, 2, max,
                                             m->output_fmt, 0);
  if (r < 0)
    return r;
  r = swr_convert(m->output_swr, data, max,
                  (const uint8_t **)filtered->extended_data,
                  filtered->nb_samples);
  if (r >= 0)
    r = append_pending(m, data[0], (size_t)r);
  av_freep(&data[0]);
  av_freep(&data);
  return r;
}
static int pull_filter(RbMedia *m) {
  AVFrame *filtered = av_frame_alloc();
  if (!filtered)
    return AVERROR(ENOMEM);
  int r = av_buffersink_get_frame(m->sink, filtered);
  if (r == 0)
    r = queue_filter_frame(m, filtered);
  else if (r == AVERROR_EOF)
    m->filter_eof = 1;
  av_frame_free(&filtered);
  return r;
}

static int make_filter_graph(RbMedia *m) {
  int r;
  m->graph = avfilter_graph_alloc();
  if (!m->graph)
    return AVERROR(ENOMEM);
  /* One thread: the biquads' slice threads (one job per channel) cost more in
   * handoffs than they save. FFmpeg 9.0.2, HD 650 profile, 5 min on the host:
   * 43 % less CPU with 1152-sample frames, 20 % with 4096; output
   * bit-identical. The Y2 measured 4.4 % -> 11.7 % of one core with two. */
  m->graph->nb_threads = 1;
  int active_eq = 0;
  for (uint32_t i = 0; i < m->dsp.eq_count && i < RB_EQ_BANDS; i++)
    active_eq |= m->dsp.eq[i].gain_db != 0.0;
  char args[256];
  snprintf(args, sizeof(args),
           "time_base=1/%d:sample_rate=%d:sample_fmt=fltp:channel_layout=stereo",
           m->c->sample_rate, m->c->sample_rate);
  const AVFilter *abuffer = avfilter_get_by_name("abuffer");
  const AVFilter *abuffersink = avfilter_get_by_name("abuffersink");
  if (!abuffer || !abuffersink)
    return AVERROR_FILTER_NOT_FOUND;
  r = avfilter_graph_create_filter(&m->src, abuffer, "in", args, NULL,
                                   m->graph);
  if (r < 0)
    return r;
  r = avfilter_graph_create_filter(&m->sink, abuffersink, "out",
                                   "sample_formats=fltp:channel_layouts=stereo",
                                   NULL,
                                   m->graph);
  if (r < 0)
    return r;

  double rg = 0.0, peak = 0.0;
  const char *rg_name = "off";
  if (m->dsp.replay_gain_mode == 1) {
    rg = gain_tag(m, "REPLAYGAIN_TRACK_GAIN", "replaygain_track_gain");
    peak = peak_tag(m, "REPLAYGAIN_TRACK_PEAK", "replaygain_track_peak");
    rg_name = "track";
  } else if (m->dsp.replay_gain_mode == 2) {
    int have_album_gain =
        gain_tag_value(m, "REPLAYGAIN_ALBUM_GAIN", "replaygain_album_gain", &rg);
    int have_album_peak =
        peak_tag_value(m, "REPLAYGAIN_ALBUM_PEAK", "replaygain_album_peak", &peak);
    /* Album tags are preferred independently. If either is absent or invalid,
     * use the corresponding track tag rather than treating a malformed album
     * value as a gain command. This keeps zero dB a valid explicit value. */
    if (!have_album_gain)
      (void)gain_tag_value(m, "REPLAYGAIN_TRACK_GAIN", "replaygain_track_gain", &rg);
    if (!have_album_peak)
      (void)peak_tag_value(m, "REPLAYGAIN_TRACK_PEAK", "replaygain_track_peak", &peak);
    rg_name = "album";
  }
  double user = m->dsp.volume > 0
                    ? 20.0 * log10(pow((double)(m->dsp.volume > 100 ? 100 : m->dsp.volume) /
                                              100.0,
                                          2.0))
                    : -120.0;
  double headroom = 0.0;
  if (peak > 0.0 && rg + 20.0 * log10(peak) > -0.1)
    headroom = -0.1 - rg - 20.0 * log10(peak);
  /* A headphone profile's preamp: an attenuation the profile's own filters
   * give back, so it is part of the gain stage before them. */
  double preamp = active_eq && m->dsp.preamp_db < 0.0 ? m->dsp.preamp_db : 0.0;
  double applied = rg + user + headroom + preamp;
  m->applied_gain_db = applied;
  m->headroom_db = headroom;
  char chain[2048];
  int used = snprintf(chain, sizeof(chain),
                      "aformat=sample_fmts=fltp:channel_layouts=stereo");
  if (used < 0 || (size_t)used >= sizeof(chain))
    return AVERROR(EINVAL);
  /* Speed without a pitch change (WSOLA). Positions stay in the source's
   * time: the decoder's timestamps come before the filter. */
  if (m->dsp.speed >= 0.5 && m->dsp.speed <= 2.0 && fabs(m->dsp.speed - 1.0) > 0.001)
    used += snprintf(chain + used, sizeof(chain) - (size_t)used,
                     ",atempo=tempo=%.4f", m->dsp.speed);
  if (m->dsp.volume != 100 || rg != 0.0 || headroom != 0.0 || preamp != 0.0) {
    double linear = pow(10.0, applied / 20.0);
    used += snprintf(chain + used, sizeof(chain) - (size_t)used,
                     ",volume=volume=%.12f", linear);
  }
  static const char *const eq_filter[] = {"equalizer", "lowshelf", "highshelf"};
  for (uint32_t i = 0; i < m->dsp.eq_count && i < RB_EQ_BANDS; i++) {
    if (m->dsp.eq[i].gain_db == 0.0)
      continue;
    if (m->dsp.eq[i].kind > 2 || (size_t)used >= sizeof(chain))
      return AVERROR(EINVAL);
    double q = m->dsp.eq[i].q > 0.01 ? m->dsp.eq[i].q : 1.0;
    used += snprintf(chain + used, sizeof(chain) - (size_t)used,
                     ",%s=f=%.9g:t=q:w=%.9g:g=%.9g", eq_filter[m->dsp.eq[i].kind],
                     m->dsp.eq[i].frequency_hz, q, m->dsp.eq[i].gain_db);
  }
  if ((size_t)used >= sizeof(chain))
    return AVERROR(EINVAL);
  /* The limiter is also an FFmpeg graph stage so EQ peaks cannot wrap during
   * the final integer conversion. It is only needed when this graph can raise
   * a peak: an active EQ band, or a net positive gain (ReplayGain without a
   * peak tag; with one, the headroom above already caps it at -0.1 dBFS).
   * Otherwise the graph only attenuates, the output conversion saturates, and
   * the limiter would cost about half the pipeline's CPU (measured on the Y2:
   * 320k MP3 2.5% -> 1.5% of one core, 24/96 FLAC 4.6% -> 2.4%) and audibly
   * touch every peak above -0.18 dBFS of a loud master. */
  int limiter = active_eq || applied > 0.0;
  if (limiter)
    used += snprintf(chain + used, sizeof(chain) - (size_t)used,
                     ",alimiter=limit=0.98:latency=1:level=0");
  if (used <= 0 || (size_t)used >= sizeof(chain))
    return AVERROR(EINVAL);
  char speed[32] = "";
  if (strstr(chain, ",atempo="))
    snprintf(speed, sizeof(speed), ",atempo=%.2f", m->dsp.speed);
  snprintf(m->filter_description, sizeof(m->filter_description),
           "ReplayGain=%s,volume=%u,EQ=%s,preamp=%.1fdB,%s,acrossfade=%ums%s",
           rg_name, m->dsp.volume, m->dsp.eq_count ? "configured" : "off", preamp,
           limiter ? "alimiter" : "no-limiter", m->dsp.crossfade_ms, speed);
  AVFilterInOut *inputs = NULL, *outputs = NULL;
  r = avfilter_graph_parse2(m->graph, chain, &inputs, &outputs);
  if (r < 0)
    goto graph_done;
  if (!inputs || !outputs) {
    r = AVERROR(EINVAL);
    goto graph_done;
  }
  r = avfilter_link(m->src, 0, inputs->filter_ctx, inputs->pad_idx);
  if (r >= 0)
    r = avfilter_link(outputs->filter_ctx, outputs->pad_idx, m->sink, 0);
graph_done:
  avfilter_inout_free(&inputs);
  avfilter_inout_free(&outputs);
  if (r < 0)
    return r;
  return avfilter_graph_config(m->graph, NULL);
}

static int source_to_canonical(RbMedia *m, AVFrame *frame) {
  uint32_t skip_start = 0, skip_end = 0;
  AVFrameSideData *side = av_frame_get_side_data(frame, AV_FRAME_DATA_SKIP_SAMPLES);
  if (side && side->size >= 10) {
    skip_start = AV_RL32(side->data);
    skip_end = AV_RL32(side->data + 4);
  }
  if (m->first_frame) {
    m->first_frame = 0;
    m->skip_start = skip_start;
  }
  int start = m->skip_start > (uint64_t)frame->nb_samples
                  ? frame->nb_samples
                  : (int)m->skip_start;
  m->skip_start -= (uint64_t)start;
  if (m->seek_trim_remaining > 0 && start < frame->nb_samples) {
    int64_t remaining = frame->nb_samples - start;
    int64_t trim = m->seek_trim_remaining < remaining
                       ? m->seek_trim_remaining
                       : remaining;
    start += (int)trim;
    m->seek_trim_remaining -= trim;
  }
  int available = frame->nb_samples - start;
  if (skip_end > (uint32_t)available)
    skip_end = (uint32_t)available;
  available -= (int)skip_end;
  m->skip_end += skip_end;
  if (available <= 0)
    return 0;
  int channels = m->c->ch_layout.nb_channels;
  uint8_t **trimmed = NULL;
  int r = av_samples_alloc_array_and_samples(&trimmed, NULL, channels, available,
                                             m->c->sample_fmt, 0);
  if (r < 0)
    return r;
  r = av_samples_copy(trimmed, (uint8_t **)frame->extended_data, 0, start,
                      available, channels, m->c->sample_fmt);
  if (r < 0) {
    av_freep(&trimmed[0]);
    av_freep(&trimmed);
    return r;
  }
  int max = (int)av_rescale_rnd(swr_get_delay(m->canonical_swr, m->c->sample_rate) +
                                    available,
                                m->c->sample_rate, m->c->sample_rate,
                                AV_ROUND_UP) + 1024;
  AVFrame *canonical = av_frame_alloc();
  if (!canonical) {
    av_freep(&trimmed[0]);
    av_freep(&trimmed);
    return AVERROR(ENOMEM);
  }
  canonical->format = AV_SAMPLE_FMT_FLTP;
  canonical->sample_rate = m->c->sample_rate;
  canonical->ch_layout = (AVChannelLayout)AV_CHANNEL_LAYOUT_STEREO;
  canonical->nb_samples = max;
  r = av_frame_get_buffer(canonical, 0);
  if (r >= 0)
    r = swr_convert(m->canonical_swr, canonical->extended_data, max,
                    (const uint8_t **)trimmed, available);
  av_freep(&trimmed[0]);
  av_freep(&trimmed);
  if (r < 0) {
    av_frame_free(&canonical);
    return r;
  }
  canonical->nb_samples = r;
  if (r > 0)
    r = av_buffersrc_add_frame(m->src, canonical);
  /* av_buffersrc_add_frame() moves the frame references into the graph but
   * does not free the caller-owned AVFrame shell. Always release our shell on
   * both the success and error paths. */
  av_frame_free(&canonical);
  return r;
}

static int decode_more(RbMedia *m) {
  int r = avcodec_receive_frame(m->c, m->frame);
  if (r == 0) {
    m->frames++;
    int64_t pts = m->frame->best_effort_timestamp;
    if (pts != AV_NOPTS_VALUE) {
      int64_t decoded = av_rescale_q(pts, m->f->streams[m->stream]->time_base,
                                     (AVRational){1, m->c->sample_rate});
      if (m->seek_pending) {
        m->seek_trim_remaining = decoded < m->seek_target
                                     ? m->seek_target - decoded
                                     : 0;
        m->seek_pending = 0;
      }
      if (decoded >= m->seek_target)
        m->position = decoded;
    }
    int result = source_to_canonical(m, m->frame);
    av_frame_unref(m->frame);
    return result;
  }
  if (r == AVERROR_EOF) {
    m->decoder_eof = 1;
    if (!m->source_null_sent) {
      m->source_null_sent = 1;
      m->source_eof = 1;
      return av_buffersrc_add_frame(m->src, NULL);
    }
    return AVERROR_EOF;
  }
  if (r != AVERROR(EAGAIN)) {
    if (r == AVERROR_INVALIDDATA) {
      m->corrupt_packets++;
      return 0;
    }
    return r;
  }
  if (m->decoder_eof)
    return AVERROR_EOF;
  for (;;) {
    if (interrupted(m->cancel))
      return AVERROR_EXIT;
    r = av_read_frame(m->f, m->p);
    if (r == AVERROR_EOF) {
      r = avcodec_send_packet(m->c, NULL);
      if (r < 0 && r != AVERROR_EOF)
        return r;
      return decode_more(m);
    }
    if (r < 0)
      return r;
    if (m->p->stream_index != m->stream) {
      av_packet_unref(m->p);
      continue;
    }
    m->packets++;
    r = avcodec_send_packet(m->c, m->p);
    av_packet_unref(m->p);
    if (r == AVERROR_INVALIDDATA) {
      m->corrupt_packets++;
      continue;
    }
    if (r < 0 && r != AVERROR(EAGAIN))
      return r;
    return decode_more(m);
  }
}

static int fill(RbMedia *m) {
  while (m->pending_frames == m->pending_offset && !m->output_drained) {
    int r = pull_filter(m);
    if (r == 0) {
      if (m->pending_frames > m->pending_offset)
        return 1;
      continue;
    }
    if (r != AVERROR(EAGAIN) && r != AVERROR_EOF)
      return r;
    if (!m->filter_eof) {
      if (m->source_null_sent)
        continue;
      r = decode_more(m);
      if (r == AVERROR_EOF)
        continue;
      if (r < 0)
        return r;
      continue;
    }
    int max = swr_get_out_samples(m->output_swr, 0);
    if (max <= 0) {
      m->output_drained = 1;
      break;
    }
    uint8_t **data = NULL;
    r = av_samples_alloc_array_and_samples(&data, NULL, 2, max,
                                           m->output_fmt, 0);
    if (r < 0)
      return r;
    r = swr_convert(m->output_swr, data, max, NULL, 0);
    if (r > 0)
      r = append_pending(m, data[0], (size_t)r);
    av_freep(&data[0]);
    av_freep(&data);
    if (r < 0)
      return r;
    if (r == 0)
      m->output_drained = 1;
  }
  return m->pending_frames > m->pending_offset ? 1 : 0;
}

static void close_graph(RbMedia *m) {
  avfilter_graph_free(&m->graph);
  m->src = NULL;
  m->sink = NULL;
}
void rb_media_close(RbMedia *m) {
  if (!m)
    return;
  close_graph(m);
  swr_free(&m->output_swr);
  swr_free(&m->canonical_swr);
  av_frame_free(&m->frame);
  av_packet_free(&m->p);
  avcodec_free_context(&m->c);
  avformat_close_input(&m->f);
  if (m->io) {
    av_freep(&m->io->buffer);
    avio_context_free(&m->io);
  }
  if (m->file) fclose(m->file);
  if (m->stream_fd > 0) close(m->stream_fd);
  free(m->pending);
  free(m);
}

static uint64_t micros(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (uint64_t)t.tv_sec * 1000000 + (uint64_t)t.tv_nsec / 1000;
}
static int local_read(void *opaque, uint8_t *buffer, int size) {
  RbMedia *m = opaque;
  if (interrupted(m->cancel)) return AVERROR_EXIT;
  /* A per-open byte budget bounds aggregate header/tag work as well as
   * individual av_max_alloc allocations. Normal playback releases this
   * analysis budget after initialization, retaining cancellation/deadlines. */
  if (m->io_limited) {
    uint64_t remaining = 32 * 1024 * 1024ULL - m->io_read;
    if (!remaining) return AVERROR(EFBIG);
    if ((uint64_t)size > remaining) size = (int)remaining;
  }
  size_t n = fread(buffer, 1, (size_t)size, m->file);
  m->io_read += n;
  if (n) return (int)n;
  return ferror(m->file) ? AVERROR(EIO) : AVERROR_EOF;
}
static int64_t local_seek(void *opaque, int64_t offset, int whence) {
  RbMedia *m = opaque;
  if (interrupted(m->cancel)) return AVERROR_EXIT;
  if (whence & AVSEEK_SIZE) return m->file_size;
  whence &= ~AVSEEK_FORCE;
  if (fseeko(m->file, offset, whence) != 0) return AVERROR(errno);
  return ftello(m->file);
}
/* Internet radio. The network fetcher (curl, outside FFmpeg) writes plain
 * audio bytes into a pipe; FFmpeg stays built without network protocols.
 * Reads wait in short polls so cancellation is prompt, keep the per-call
 * deadline alive while the fetcher reconnects, and give up after a stall. */
#define RB_STREAM_STALL_MS 45000
static int stream_read(void *opaque, uint8_t *buffer, int size) {
  RbMedia *m = opaque;
  uint64_t waited = 0;
  for (;;) {
    if (!m->cancel || atomic_load(&m->cancel->cancelled)) return AVERROR_EXIT;
    struct pollfd p = {m->stream_fd, POLLIN, 0};
    int r = poll(&p, 1, 200);
    if (r < 0 && errno != EINTR) return AVERROR(errno);
    if (r > 0) {
      ssize_t n = read(m->stream_fd, buffer, (size_t)size);
      if (n > 0) {
        deadline(m->cancel);
        m->io_read += (uint64_t)n;
        return (int)n;
      }
      if (n == 0) return AVERROR_EOF;
      if (errno != EAGAIN && errno != EINTR) return AVERROR(errno);
    }
    waited += 200;
    deadline(m->cancel);
    if (waited >= RB_STREAM_STALL_MS) return AVERROR(ETIMEDOUT);
  }
}
static int64_t stream_seek(void *opaque, int64_t offset, int whence) {
  (void)opaque; (void)offset;
  if (whence & AVSEEK_SIZE) return -1;
  return AVERROR(ESPIPE);
}
static int stream_fd_from_path(const char *path) {
  static const char prefix[] = "rbpipe:";
  if (strncmp(path, prefix, sizeof(prefix) - 1)) return -1;
  const char *digits = path + sizeof(prefix) - 1;
  if (!*digits) return -1;
  long value = 0;
  for (const char *c = digits; *c; c++) {
    if (!isdigit((unsigned char)*c) || value > 65535) return -1;
    value = value * 10 + (*c - '0');
  }
  return value > 2 && value <= 65535 ? (int)value : -1;
}
/* Video is refused, except a cover: an attached picture, or in MP4/M4B
 * beside audio a picture track (audiobook tools store the cover as a
 * one-frame video stream). Such a track is never read or decoded. */
static int admit_streams(RbMedia *m) {
  unsigned audio = 0;
  for (unsigned i = 0; i < m->f->nb_streams; i++)
    audio += m->f->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO;
  int mov = !strncmp(m->f->iformat->name, "mov,", 4);
  for (unsigned i = 0; i < m->f->nb_streams; i++) {
    AVStream *s = m->f->streams[i];
    if (s->codecpar->codec_type != AVMEDIA_TYPE_VIDEO ||
        (s->disposition & AV_DISPOSITION_ATTACHED_PIC))
      continue;
    if (!mov || !audio) return AVERROR_INVALIDDATA;
    s->discard = AVDISCARD_ALL;
  }
  return 0;
}
static int open_stream(RbMedia *m, int fd) {
  struct stat info;
  if (fstat(fd, &info) < 0 || !S_ISFIFO(info.st_mode)) return AVERROR(EINVAL);
  m->stream_fd = fcntl(fd, F_DUPFD_CLOEXEC, 3);
  if (m->stream_fd < 0) return AVERROR(errno);
  uint8_t *buffer = av_malloc(32768);
  if (!buffer) return AVERROR(ENOMEM);
  m->io = avio_alloc_context(buffer, 32768, 0, m, stream_read, NULL, stream_seek);
  if (!m->io) { av_free(buffer); return AVERROR(ENOMEM); }
  m->io->seekable = 0;
  m->f->pb = m->io;
  m->f->flags |= AVFMT_FLAG_CUSTOM_IO;
  m->file_size = -1;
  /* Start quickly: one second of analysis is plenty for a radio stream. */
  m->f->probesize = 256 * 1024;
  m->f->max_analyze_duration = AV_TIME_BASE;
  m->f->max_streams = 4;
  AVDictionary *options = NULL;
  av_dict_set(&options, "protocol_whitelist", "file", 0);
  /* wav: Spotify's PCM from spotifyd, framed by reborn-platform::spotify. */
  av_dict_set(&options, "format_whitelist", "mp3,aac,ogg,flac,wav", 0);
  uint64_t start = micros();
  int ret = avformat_open_input(&m->f, "", NULL, &options);
  m->open_us = micros() - start;
  av_dict_free(&options);
  if (ret < 0) return ret;
  return admit_streams(m);
}
static int open_input(RbMedia *m, const char *path, RbCancel *cancel) {
  deadline(cancel);
  m->cancel = cancel;
  m->f = avformat_alloc_context();
  if (!m->f) return AVERROR(ENOMEM);
  m->f->interrupt_callback = (AVIOInterruptCB){interrupted, cancel};
  int stream = stream_fd_from_path(path);
  if (stream >= 0) return open_stream(m, stream);
  int fd = open(path, O_RDONLY | O_CLOEXEC | O_NONBLOCK);
  if (fd < 0) return AVERROR(errno);
  struct stat info;
  if (fstat(fd, &info) < 0 || !S_ISREG(info.st_mode) || info.st_size <= 0) {
    close(fd); return AVERROR_INVALIDDATA;
  }
  m->file_size = info.st_size;
  m->file = fdopen(fd, "rb");
  if (!m->file) { int error = errno; close(fd); return AVERROR(error); }
  uint8_t *buffer = av_malloc(32768);
  if (!buffer) return AVERROR(ENOMEM);
  m->io = avio_alloc_context(buffer, 32768, 0, m, local_read, NULL, local_seek);
  if (!m->io) { av_free(buffer); return AVERROR(ENOMEM); }
  m->io_limited = 1;
  m->f->pb = m->io;
  m->f->flags |= AVFMT_FLAG_CUSTOM_IO;
  /* Retain detection/probe budgets: a header shortcut must not weaken probing. */
  m->f->probesize = 2 * 1024 * 1024;
  m->f->max_analyze_duration = 3 * AV_TIME_BASE;
  m->f->max_streams = 16;
  AVDictionary *options = NULL;
  av_dict_set(&options, "protocol_whitelist", "file", 0);
  av_dict_set(&options, "format_whitelist", "flac,mp3,mov,ogg,wav,aac,aiff,ape,wv", 0);
  uint64_t start = micros();
  int ret = avformat_open_input(&m->f, path, NULL, &options);
  m->open_us = micros() - start;
  av_dict_free(&options);
  if (ret < 0) return ret;
  if (m->io->error == AVERROR(EFBIG) || m->io->error == AVERROR_EXIT) return m->io->error;
  return admit_streams(m);
}
static int stream_info(RbMedia *m) {
  /* Attached images do not supply audio properties. Discard them during
   * stream analysis so FFmpeg cannot decode a giant cover just to read tags. */
  for (unsigned i = 0; i < m->f->nb_streams; i++)
    if (m->f->streams[i]->disposition & AV_DISPOSITION_ATTACHED_PIC)
      m->f->streams[i]->discard = AVDISCARD_ALL;
  uint64_t start = micros();
  int ret = avformat_find_stream_info(m->f, NULL);
  m->probe_us += micros() - start;
  m->info_runs++;
  if (m->io->error == AVERROR(EFBIG) || m->io->error == AVERROR_EXIT) return m->io->error;
  return ret;
}
static uint32_t source_bits(AVCodecParameters *p) {
  int bits = av_get_bits_per_sample(p->codec_id);
  /* Lossy decoded PCM width is not an encoded precision. */
  if (bits <= 0 && (p->codec_id == AV_CODEC_ID_FLAC ||
      p->codec_id == AV_CODEC_ID_ALAC || p->codec_id == AV_CODEC_ID_APE ||
      p->codec_id == AV_CODEC_ID_WAVPACK))
    bits = p->bits_per_raw_sample > 0 ? p->bits_per_raw_sample : p->bits_per_coded_sample;
  return bits > 0 && bits <= 64 ? (uint32_t)bits : 0;
}
static uint32_t ordinal(const char *s) {
  while (isspace((unsigned char)*s)) s++;
  if (!isdigit((unsigned char)*s)) return 0;
  errno = 0;
  char *end;
  unsigned long value = strtoul(s, &end, 10);
  if (errno || value > UINT32_MAX) return 0;
  while (isspace((unsigned char)*end)) end++;
  if (*end == '/') {
    end++;
    while (isspace((unsigned char)*end)) end++;
    if (!isdigit((unsigned char)*end)) return 0;
    errno = 0;
    (void)strtoul(end, &end, 10);
    if (errno) return 0;
    while (isspace((unsigned char)*end)) end++;
  }
  return *end ? 0 : (uint32_t)value;
}
static const char *alias(RbMedia *m, const char *key, const char *other) {
  const char *value = tag(m, key);
  return *value ? value : tag(m, other);
}
static int image_codec(enum AVCodecID id) {
  return id == AV_CODEC_ID_MJPEG || id == AV_CODEC_ID_PNG || id == AV_CODEC_ID_WEBP;
}
static void extract_metadata(RbMedia *m, RbMetadata *meta) {
  AVStream *st = m->f->streams[m->stream];
  AVCodecParameters *p = st->codecpar;
  memset(meta, 0, sizeof(*meta));
  text(meta->title, sizeof(meta->title), tag(m, "title"));
  text(meta->artist, sizeof(meta->artist), !strcmp(m->f->iformat->name, "aiff")
      ? alias(m, "artist", "author") : tag(m, "artist"));
  text(meta->album, sizeof(meta->album), tag(m, "album"));
  /* foobar2000 and others write Vorbis/APE "ALBUM ARTIST" with a space. */
  const char *album_artist = alias(m, "album_artist", "albumartist");
  text(meta->album_artist, sizeof(meta->album_artist),
       album_artist && *album_artist ? album_artist : tag(m, "album artist"));
  text(meta->date, sizeof(meta->date), alias(m, "date", "year"));
  text(meta->genre, sizeof(meta->genre), tag(m, "genre"));
  text(meta->codec, sizeof(meta->codec), avcodec_get_name(p->codec_id));
  meta->track = ordinal(alias(m, "track", "tracknumber"));
  meta->disc = ordinal(alias(m, "disc", "discnumber"));
  meta->rate = p->sample_rate;
  meta->channels = p->ch_layout.nb_channels;
  meta->source_bits = source_bits(p);
  if (av_get_bits_per_sample(p->codec_id) > 0)
    text(meta->source_sample_fmt, sizeof(meta->source_sample_fmt), avcodec_get_name(p->codec_id));
  else if (meta->source_bits)
    snprintf(meta->source_sample_fmt, sizeof(meta->source_sample_fmt),
        p->codec_id == AV_CODEC_ID_WAVPACK ? "lossless-%u-bit" : "s%u", meta->source_bits);
  else
    text(meta->source_sample_fmt, sizeof(meta->source_sample_fmt), "unspecified");
  int64_t stream_duration = st->duration;
  /* Ogg Opus's final granule includes pre-skip, unlike MP3's already-trimmed
   * Xing duration. Subtract only the explicit Opus/Ogg codec delay. */
  if (p->codec_id == AV_CODEC_ID_OPUS && !strcmp(m->f->iformat->name, "ogg") &&
      p->initial_padding > 0 && p->sample_rate > 0 && st->time_base.num > 0 && st->time_base.den > 0) {
    int64_t delay = av_rescale_q(p->initial_padding, (AVRational){1, p->sample_rate}, st->time_base);
    if (stream_duration > delay) stream_duration -= delay;
  }
  int64_t duration = stream_duration > 0 && st->time_base.den > 0
      ? av_rescale_q(stream_duration, st->time_base, (AVRational){1, 1000}) : 0;
  meta->duration_ms = duration > 0 ? (uint64_t)duration :
      (m->f->duration > 0 ? (uint64_t)m->f->duration / 1000 : 0);
  meta->bitrate = p->bit_rate > 0 ? (uint64_t)p->bit_rate :
      (m->f->bit_rate > 0 ? (uint64_t)m->f->bit_rate : 0);
  meta->skip_start = p->initial_padding > 0 ? (uint64_t)p->initial_padding : 0;
  meta->skip_end = p->trailing_padding > 0 ? (uint64_t)p->trailing_padding : 0;
  meta->track_gain_db = gain_tag(m, "REPLAYGAIN_TRACK_GAIN", "replaygain_track_gain");
  meta->album_gain_db = gain_tag(m, "REPLAYGAIN_ALBUM_GAIN", "replaygain_album_gain");
  meta->track_peak = peak_tag(m, "REPLAYGAIN_TRACK_PEAK", "replaygain_track_peak");
  meta->album_peak = peak_tag(m, "REPLAYGAIN_ALBUM_PEAK", "replaygain_album_peak");
  for (unsigned i = 0; i < m->f->nb_streams; i++) {
    AVStream *s = m->f->streams[i];
    if ((s->disposition & AV_DISPOSITION_ATTACHED_PIC) && image_codec(s->codecpar->codec_id) &&
        s->attached_pic.size > 0 && s->attached_pic.size <= 16 * 1024 * 1024)
      meta->artwork = 1;
  }
  meta->open_us = m->open_us;
  meta->probe_us = m->probe_us;
  meta->info_runs = m->info_runs;
  meta->io_bytes = m->f->pb && m->f->pb->bytes_read > 0 ? (uint64_t)m->f->pb->bytes_read : 0;
}
/* A demux-only owner for scans and collection covers. It never creates a DSP
 * graph, resampler or PCM buffers. Decoder initialization may parse codec
 * extradata (FLAC/ALAC), but no audio frames are decoded on the header path. */
int rb_media_inspect(const char *path, RbCancel *cancel, RbMedia **out, RbMetadata *meta) {
  *out = NULL;
  RbMedia *m = calloc(1, sizeof(*m));
  if (!m) return AVERROR(ENOMEM);
  int ret = open_input(m, path, cancel);
  if (ret < 0) goto fail;
  m->stream = -1;
  for (unsigned i = 0; i < m->f->nb_streams; i++)
    if (m->f->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO) { m->stream = (int)i; break; }
  if (m->stream < 0) { ret = AVERROR_STREAM_NOT_FOUND; goto fail; }
  AVStream *st = m->f->streams[m->stream];
  AVCodecParameters *p = st->codecpar;
  /* FFmpeg's FLAC demuxer leaves channel/precision extraction to its codec.
   * Use that codec's extradata initialization, never a custom tag/header parser. */
  if (p->codec_id == AV_CODEC_ID_FLAC && p->extradata_size > 0) {
    const AVCodec *codec = avcodec_find_decoder(p->codec_id);
    AVCodecContext *ctx = avcodec_alloc_context3(codec);
    if (!ctx) { ret = AVERROR(ENOMEM); goto fail; }
    ctx->thread_count = 1;
    ret = avcodec_parameters_to_context(ctx, p);
    if (ret >= 0) ret = avcodec_open2(ctx, codec, NULL);
    if (ret >= 0) {
      p->sample_rate = ctx->sample_rate;
      p->bits_per_raw_sample = ctx->bits_per_raw_sample;
      ret = av_channel_layout_copy(&p->ch_layout, &ctx->ch_layout);
    }
    avcodec_free_context(&ctx);
    if (ret < 0) goto fail;
  }
  unsigned audio = 0;
  for (unsigned i = 0; i < m->f->nb_streams; i++)
    audio += m->f->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO;
  const char *name = m->f->iformat->name;
  int header_format = !strcmp(name, "flac") || !strcmp(name, "wav") || !strcmp(name, "aiff") ||
      !strcmp(name, "ape") || !strcmp(name, "wv") || !strcmp(name, "ogg") || !strncmp(name, "mov,", 4);
  if (!header_format || audio != 1 || p->sample_rate <= 0 || p->ch_layout.nb_channels <= 0 || st->duration <= 0) {
    if ((ret = stream_info(m)) < 0) goto fail;
    ret = av_find_best_stream(m->f, AVMEDIA_TYPE_AUDIO, -1, -1, NULL, 0);
    if (ret < 0) goto fail;
    m->stream = ret;
    p = m->f->streams[m->stream]->codecpar;
  }
  if (p->sample_rate < 8000 || p->sample_rate > 384000 ||
      p->ch_layout.nb_channels < 1 || p->ch_layout.nb_channels > 8 ||
      !avcodec_find_decoder(p->codec_id)) { ret = AVERROR_INVALIDDATA; goto fail; }
  extract_metadata(m, meta);
  *out = m;
  return 0;
fail:
  rb_media_close(m);
  return ret;
}

int rb_media_open(const char *path, int rate, int output_format,
                  const RbDspConfig *dsp, RbCancel *cancel, RbMedia **out,
                  RbMetadata *meta) {
  int ret = AVERROR(ENOMEM);
  *out = NULL;
  RbMedia *m = calloc(1, sizeof(*m));
  if (!m)
    return ret;
  m->cancel = cancel;
  if (dsp)
    m->dsp = *dsp;
  if (m->dsp.volume > 100)
    m->dsp.volume = 100;
  m->output_rate = rate;
  if (rate < 8000 || rate > 384000 ||
      (output_format != 1 && output_format != 2 && output_format != 3)) {
    ret = AVERROR(EINVAL);
    goto fail;
  }
  m->output_format = output_format;
  m->output_fmt = format_from_int(output_format);
  m->output_bytes = av_get_bytes_per_sample(m->output_fmt) * 2;
  ret = open_input(m, path, cancel);
  if (ret < 0 || (ret = stream_info(m)) < 0)
    goto fail;
  const AVCodec *codec = NULL;
  ret = av_find_best_stream(m->f, AVMEDIA_TYPE_AUDIO, -1, -1, &codec, 0);
  if (ret < 0)
    goto fail;
  m->stream = ret;
  m->c = avcodec_alloc_context3(codec);
  if (!m->c) {
    ret = AVERROR(ENOMEM);
    goto fail;
  }
  if ((ret = avcodec_parameters_to_context(m->c,
                                           m->f->streams[m->stream]->codecpar)) < 0)
    goto fail;
  m->c->thread_count = 1;
  /* Preserve codec/container delay and padding as skip-sample side data. */
  m->c->flags2 |= AV_CODEC_FLAG2_SKIP_MANUAL;
  if ((ret = avcodec_open2(m->c, codec, NULL)) < 0)
    goto fail;
  if (m->c->sample_rate < 8000 || m->c->sample_rate > 384000 ||
      m->c->ch_layout.nb_channels < 1 || m->c->ch_layout.nb_channels > 8) {
    ret = AVERROR(EINVAL);
    goto fail;
  }
  AVChannelLayout stereo = AV_CHANNEL_LAYOUT_STEREO;
  ret = swr_alloc_set_opts2(&m->canonical_swr, &stereo, AV_SAMPLE_FMT_FLTP,
                            m->c->sample_rate, &m->c->ch_layout,
                            m->c->sample_fmt, m->c->sample_rate, 0, NULL);
  if (ret < 0 || (ret = swr_init(m->canonical_swr)) < 0)
    goto fail;
  ret = swr_alloc_set_opts2(&m->output_swr, &stereo, m->output_fmt,
                            m->output_rate, &stereo, AV_SAMPLE_FMT_FLTP,
                            m->c->sample_rate, 0, NULL);
  if (ret < 0 || (ret = swr_init(m->output_swr)) < 0)
    goto fail;
  if ((ret = make_filter_graph(m)) < 0)
    goto fail;
  m->p = av_packet_alloc();
  m->frame = av_frame_alloc();
  if (!m->p || !m->frame) {
    ret = AVERROR(ENOMEM);
    goto fail;
  }
  m->first_frame = 1;
  m->seek_pending = 0;
  m->seek_trim_remaining = 0;
  extract_metadata(m, meta);
  text(meta->decoder_sample_fmt, sizeof(meta->decoder_sample_fmt),
       av_get_sample_fmt_name(m->c->sample_fmt));
  text(meta->internal_sample_fmt, sizeof(meta->internal_sample_fmt), "fltp");
  text(meta->final_sample_fmt, sizeof(meta->final_sample_fmt),
       output_format == 3 ? "s24le-in-32" : av_get_sample_fmt_name(m->output_fmt));
  text(meta->filters, sizeof(meta->filters), m->filter_description);
  meta->output_rate = rate;
  meta->output_channels = 2;
  meta->output_format = (uint32_t)output_format;
  meta->resampling = (m->c->sample_rate != rate || m->c->ch_layout.nb_channels != 2);
  /* The canonical graph is FLTP, so the final sink conversion is always
   * intentional even when the source rate/channels already match ALSA. */
  meta->format_conversion = 1;
  text(meta->resample_reason, sizeof(meta->resample_reason),
       meta->resampling
           ? "source rate/channel differs; final FLTP to packed sink conversion"
           : "no rate/channel resampling; final FLTP to packed sink conversion");
  meta->replaygain_mode = (uint32_t)m->dsp.replay_gain_mode;
  meta->eq_active = m->dsp.eq_count != 0;
  meta->applied_gain_db = m->applied_gain_db;
  meta->headroom_db = m->headroom_db;
  m->io_limited = 0;
  *out = m;
  return 0;
fail:
  rb_media_close(m);
  return ret;
}

int rb_media_read(RbMedia *m, uint8_t *out, int capacity_frames,
                  uint64_t *packets, uint64_t *frames, int64_t *position) {
  if (!m || !out || capacity_frames <= 0)
    return AVERROR(EINVAL);
  deadline(m->cancel);
  int r = fill(m);
  if (r < 0)
    return r;
  if (m->pending_frames == m->pending_offset)
    return 0;
  size_t n = m->pending_frames - m->pending_offset;
  if (n > (size_t)capacity_frames)
    n = (size_t)capacity_frames;
  memcpy(out, m->pending + m->pending_offset * m->output_bytes,
         n * (size_t)m->output_bytes);
  *position = av_rescale(m->position, 1000, m->c->sample_rate);
  m->pending_offset += n;
  if (m->pending_offset == m->pending_frames)
    m->pending_offset = m->pending_frames = 0;
  *packets = m->packets;
  *frames = n;
  return (int)n;
}

int rb_media_seek(RbMedia *m, int64_t ms) {
  if (!m || ms < 0)
    return AVERROR(EINVAL);
  deadline(m->cancel);
  int r = avformat_seek_file(m->f, -1, INT64_MIN, ms * 1000, ms * 1000, 0);
  if (r < 0)
    return r;
  avcodec_flush_buffers(m->c);
  swr_close(m->canonical_swr);
  swr_close(m->output_swr);
  if ((r = swr_init(m->canonical_swr)) < 0 || (r = swr_init(m->output_swr)) < 0)
    return r;
  close_graph(m);
  if ((r = make_filter_graph(m)) < 0)
    return r;
  m->decoder_eof = m->source_eof = m->source_null_sent = m->filter_eof = 0;
  m->output_drained = 0;
  m->first_frame = 1;
  m->seek_pending = 1;
  m->seek_trim_remaining = 0;
  m->skip_start = m->skip_end = 0;
  m->pending_frames = m->pending_offset = 0;
  m->position = av_rescale(ms, m->c->sample_rate, 1000);
  m->seek_target = m->position;
  return 0;
}

/* Ask FFmpeg to read JPEG parameters without allocating a decoded picture.
 * Use its supported reduced IDCT only while both edges still cover the output.
 * Small covers and decoders without lowres retain their original resolution. */
static int artwork_lowres(const AVCodec *codec, const AVPacket *packet, int side) {
  if (codec->id != AV_CODEC_ID_MJPEG || !codec->max_lowres)
    return 0;
  AVCodecContext *probe = avcodec_alloc_context3(codec);
  AVFrame *frame = av_frame_alloc();
  int lowres = 0;
  if (!probe || !frame)
    goto done;
  probe->max_pixels = 16 * 1024 * 1024;
  probe->thread_count = 1;
  probe->skip_frame = AVDISCARD_ALL;
  if (avcodec_open2(probe, codec, NULL) < 0 ||
      avcodec_send_packet(probe, packet) < 0)
    goto done;
  /* A header-only decode normally returns EAGAIN, with dimensions filled. */
  int r = avcodec_receive_frame(probe, frame);
  if (r < 0 && r != AVERROR(EAGAIN) && r != AVERROR_EOF)
    goto done;
  if (probe->profile != AV_PROFILE_MJPEG_HUFFMAN_BASELINE_DCT &&
      probe->profile != AV_PROFILE_MJPEG_HUFFMAN_EXTENDED_SEQUENTIAL_DCT &&
      probe->profile != AV_PROFILE_MJPEG_HUFFMAN_PROGRESSIVE_DCT)
    goto done;
  while (lowres < codec->max_lowres &&
         (probe->width >> (lowres + 1)) >= side &&
         (probe->height >> (lowres + 1)) >= side)
    lowres++;
done:
  av_frame_free(&frame);
  avcodec_free_context(&probe);
  return lowres;
}

/* A scaler for artwork: Lanczos when shrinking (sharp, no aliasing), bicubic
 * when a small picture has to grow (Lanczos rings there), both with exact
 * rounding and full-resolution chroma so JPEG colour edges stay clean. */
static struct SwsContext *art_scaler(const AVFrame *frame, int w, int h) {
  int shrink = frame->width >= w && frame->height >= h;
  return sws_getContext(frame->width, frame->height, frame->format, w, h,
      AV_PIX_FMT_RGBA, (shrink ? SWS_LANCZOS : SWS_BICUBIC) | SWS_ACCURATE_RND |
      SWS_FULL_CHR_H_INT | SWS_FULL_CHR_H_INP, NULL, NULL, NULL);
}

static int decode_art(enum AVCodecID id, AVCodecParameters *params,
                       AVPacket *packet, uint8_t *out, int side) {
  if (!image_codec(id) || !packet || packet->size <= 0 || packet->size > 16 * 1024 * 1024)
    return AVERROR_INVALIDDATA;
  const AVCodec *codec = avcodec_find_decoder(id);
  if (!codec) return AVERROR_DECODER_NOT_FOUND;
  AVCodecContext *ctx = avcodec_alloc_context3(codec);
  AVFrame *frame = av_frame_alloc();
  if (!ctx || !frame) {
    avcodec_free_context(&ctx); av_frame_free(&frame); return AVERROR(ENOMEM);
  }
  int r = params ? avcodec_parameters_to_context(ctx, params) : 0;
  /* PNG/WebP have no reduced-resolution decoder. Bound their full decode to
   * 4 MP (~16 MiB RGBA); JPEG additionally decodes at 1/8 native resolution. */
  ctx->max_pixels = id == AV_CODEC_ID_MJPEG ? 16 * 1024 * 1024 : 4 * 1024 * 1024;
  ctx->thread_count = 1;
  if (id == AV_CODEC_ID_MJPEG) ctx->lowres = artwork_lowres(codec, packet, side);
  if (r >= 0) r = avcodec_open2(ctx, codec, NULL);
  if (r >= 0) r = avcodec_send_packet(ctx, packet);
  if (r >= 0) r = avcodec_receive_frame(ctx, frame);
  if (r >= 0 && (frame->width < 1 || frame->height < 1)) r = AVERROR_INVALIDDATA;
  if (r >= 0 && frame->width != frame->height) {
    /* A cover fills its square: the centre of a wide or tall picture, never
     * stretched. Even offsets keep 4:2:0 chroma aligned with its luma. */
    int edge = frame->width < frame->height ? frame->width : frame->height;
    frame->crop_left = (size_t)((frame->width - edge) / 2 & ~1);
    frame->crop_top = (size_t)((frame->height - edge) / 2 & ~1);
    frame->crop_right = (size_t)(frame->width - edge) - frame->crop_left;
    frame->crop_bottom = (size_t)(frame->height - edge) - frame->crop_top;
    r = av_frame_apply_cropping(frame, AV_FRAME_CROP_UNALIGNED);
  }
  if (r >= 0) {
    struct SwsContext *scale = art_scaler(frame, side, side);
    if (!scale) r = AVERROR(ENOMEM);
    else {
      uint8_t *dest[] = {out}; int stride[] = {side * 4};
      r = sws_scale(scale, (const uint8_t *const *)frame->data, frame->linesize,
          0, frame->height, dest, stride);
      sws_freeContext(scale);
    }
  }
  avcodec_free_context(&ctx); av_frame_free(&frame);
  return r < 0 ? r : 0;
}

/* A chapter of the file, in milliseconds, with its title (UTF-8). */
typedef struct {
  int64_t start_ms, end_ms;
  char title[256];
} RbChapter;
_Static_assert(sizeof(RbChapter) == 272, "RbChapter ABI");

/* The file's chapters as FFmpeg's demuxers read them (MP4/M4B chapter track
 * or Nero chpl, MP3 ID3 CHAP, Ogg CHAPTERxx), in order, at most `max`.
 * Returns how many were written; an unordered or empty one is skipped. */
int rb_media_chapters(RbMedia *m, RbChapter *out, int max) {
  if (!m || !m->f || !out || max < 0) return AVERROR(EINVAL);
  int n = 0;
  int64_t last = -1;
  for (unsigned i = 0; i < m->f->nb_chapters && n < max; i++) {
    const AVChapter *c = m->f->chapters[i];
    int64_t start = av_rescale_q(c->start, c->time_base, (AVRational){1, 1000});
    int64_t end = av_rescale_q(c->end, c->time_base, (AVRational){1, 1000});
    if (start < 0 || end <= start || start < last) continue;
    last = start;
    const AVDictionaryEntry *title = av_dict_get(c->metadata, "title", NULL, 0);
    out[n].start_ms = start;
    out[n].end_ms = end;
    text(out[n].title, sizeof(out[n].title), title ? title->value : "");
    n++;
  }
  return n;
}

int rb_media_art(RbMedia *m, uint8_t *out, int side) {
  if (!m || !out || side < 1 || side > 256) return AVERROR(EINVAL);
  int error = AVERROR(ENOENT);
  for (unsigned i = 0; i < m->f->nb_streams; i++) {
    AVStream *s = m->f->streams[i];
    if (!(s->disposition & AV_DISPOSITION_ATTACHED_PIC)) continue;
    int r = decode_art(s->codecpar->codec_id, s->codecpar, &s->attached_pic, out, side);
    if (r >= 0) return r;
    error = r; /* A corrupt first cover must not hide a later valid cover. */

  }
  return error;
}

static enum AVCodecID image_codec_for_path(const char *path) {
  const char *dot = strrchr(path, '.');
  if (!dot)
    return AV_CODEC_ID_NONE;
  if (!strcasecmp(dot, ".jpg") || !strcasecmp(dot, ".jpeg"))
    return AV_CODEC_ID_MJPEG;
  if (!strcasecmp(dot, ".png"))
    return AV_CODEC_ID_PNG;
  if (!strcasecmp(dot, ".webp"))
    return AV_CODEC_ID_WEBP;
  return AV_CODEC_ID_NONE;
}

/* Descriptor-pinned sidecars have no filename extension. Keep this fallback
 * bounded to the same three image codecs supported by named local files. */
static enum AVCodecID image_codec_for_header(const uint8_t *b, size_t n) {
  if (n >= 3 && b[0] == 0xff && b[1] == 0xd8 && b[2] == 0xff)
    return AV_CODEC_ID_MJPEG;
  if (n >= 8 && !memcmp(b, "\211PNG\r\n\032\n", 8))
    return AV_CODEC_ID_PNG;
  if (n >= 12 && !memcmp(b, "RIFF", 4) && !memcmp(b + 8, "WEBP", 4))
    return AV_CODEC_ID_WEBP;
  return AV_CODEC_ID_NONE;
}

/* External cover images use the same FFmpeg image decoders and libswscale as
 * attached artwork. They are read as bounded local files without enabling an
 * image demuxer or any network protocol in the production build. */
/* Read a bounded local image file into a packet and pick its decoder by
 * extension, else by signature. */
static int read_image_file(const char *path, AVPacket **out, enum AVCodecID *codec_id) {
  enum AVCodecID id = image_codec_for_path(path);
  int fd = open(path, O_RDONLY | O_NONBLOCK | O_CLOEXEC);
  if (fd < 0)
    return AVERROR(errno);
  struct stat st;
  if (fstat(fd, &st) < 0) {
    int error = AVERROR(errno);
    close(fd);
    return error;
  }
  if (!S_ISREG(st.st_mode) || st.st_size <= 0 || st.st_size > 16 * 1024 * 1024) {
    close(fd);
    return AVERROR(EINVAL);
  }
  long length = (long)st.st_size;
  FILE *file = fdopen(fd, "rb");
  if (!file) {
    int error = AVERROR(errno);
    close(fd);
    return error;
  }
  if (id == AV_CODEC_ID_NONE) {
    uint8_t header[12];
    size_t n = fread(header, 1, sizeof(header), file);
    if (n != (size_t)(length < 12 ? length : 12)) {
      fclose(file);
      return AVERROR(EIO);
    }
    id = image_codec_for_header(header, n);
    if (fseek(file, 0, SEEK_SET) != 0) {
      fclose(file);
      return AVERROR(EIO);
    }
  }
  if (id == AV_CODEC_ID_NONE || !avcodec_find_decoder(id)) {
    fclose(file);
    return AVERROR_DECODER_NOT_FOUND;
  }
  AVPacket *packet = av_packet_alloc();
  int r = packet ? av_new_packet(packet, (int)length) : AVERROR(ENOMEM);
  if (r >= 0 && fread(packet->data, 1, (size_t)length, file) != (size_t)length)
    r = AVERROR(EIO);
  fclose(file);
  if (r < 0) {
    av_packet_free(&packet);
    return r;
  }
  *out = packet;
  *codec_id = id;
  return 0;
}

/* External cover images use the same FFmpeg image decoders and libswscale as
 * attached artwork. They are read as bounded local files without enabling an
 * image demuxer or any network protocol in the production build. */
int rb_media_art_file(const char *path, uint8_t *out, int side) {
  if (!path || !out || side < 1 || side > 256)
    return AVERROR(EINVAL);
  AVPacket *packet = NULL;
  enum AVCodecID id;
  int r = read_image_file(path, &packet, &id);
  if (r >= 0) r = decode_art(id, NULL, packet, out, side);
  av_packet_free(&packet);
  return r;
}

/* Station logos: the whole picture, aspect kept, centred in side x side
 * straight-alpha RGBA with transparent margins (a cover is cropped to fill
 * its square; a logo must never be cut or stretched). Same decoders and
 * bounds as covers. */
int rb_media_logo_file(const char *path, uint8_t *out, int side) {
  if (!path || !out || side < 1 || side > 256)
    return AVERROR(EINVAL);
  AVPacket *packet = NULL;
  enum AVCodecID id;
  int r = read_image_file(path, &packet, &id);
  if (r < 0)
    return r;
  const AVCodec *codec = avcodec_find_decoder(id);
  AVCodecContext *ctx = codec ? avcodec_alloc_context3(codec) : NULL;
  AVFrame *frame = av_frame_alloc();
  if (!ctx || !frame) r = AVERROR(ENOMEM);
  if (r >= 0) {
    ctx->max_pixels = id == AV_CODEC_ID_MJPEG ? 16 * 1024 * 1024 : 4 * 1024 * 1024;
    ctx->thread_count = 1;
    if (id == AV_CODEC_ID_MJPEG) ctx->lowres = artwork_lowres(codec, packet, side);
    r = avcodec_open2(ctx, codec, NULL);
  }
  if (r >= 0) r = avcodec_send_packet(ctx, packet);
  if (r >= 0) r = avcodec_receive_frame(ctx, frame);
  if (r >= 0 && (frame->width < 1 || frame->height < 1)) r = AVERROR_INVALIDDATA;
  if (r >= 0) {
    int w = side, h = side;
    if (frame->width >= frame->height)
      h = (int)((int64_t)side * frame->height / frame->width);
    else
      w = (int)((int64_t)side * frame->width / frame->height);
    if (w < 1) w = 1;
    if (h < 1) h = 1;
    memset(out, 0, (size_t)side * side * 4);
    struct SwsContext *scale = art_scaler(frame, w, h);
    if (!scale) r = AVERROR(ENOMEM);
    else {
      uint8_t *dest[] = {out + ((size_t)((side - h) / 2) * side + (side - w) / 2) * 4};
      int stride[] = {side * 4};
      r = sws_scale(scale, (const uint8_t *const *)frame->data, frame->linesize,
          0, frame->height, dest, stride);
      sws_freeContext(scale);
    }
  }
  avcodec_free_context(&ctx);
  av_frame_free(&frame);
  av_packet_free(&packet);
  return r < 0 ? r : 0;
}

/* Skin pictures: decoded at their own size into straight-alpha RGBA. An image
 * larger than max_w x max_h is refused, never scaled, so a skin looks the same
 * everywhere. `out` holds max_w * max_h * 4 bytes; *w and *h receive the size. */
int rb_media_image_file(const char *path, uint8_t *out, int max_w, int max_h,
                        int *w, int *h) {
  if (!path || !out || !w || !h || max_w < 1 || max_h < 1 || max_w > 2048 || max_h > 2048)
    return AVERROR(EINVAL);
  AVPacket *packet = NULL;
  enum AVCodecID id;
  int r = read_image_file(path, &packet, &id);
  if (r < 0)
    return r;
  const AVCodec *codec = avcodec_find_decoder(id);
  AVCodecContext *ctx = avcodec_alloc_context3(codec);
  AVFrame *frame = av_frame_alloc();
  if (!ctx || !frame) r = AVERROR(ENOMEM);
  if (r >= 0) {
    ctx->max_pixels = (int64_t)max_w * max_h;
    ctx->thread_count = 1;
    r = avcodec_open2(ctx, codec, NULL);
  }
  if (r >= 0) r = avcodec_send_packet(ctx, packet);
  if (r >= 0) r = avcodec_receive_frame(ctx, frame);
  if (r >= 0 && (frame->width < 1 || frame->height < 1 ||
                 frame->width > max_w || frame->height > max_h))
    r = AVERROR(ERANGE);
  if (r >= 0) {
    struct SwsContext *scale = sws_getContext(frame->width, frame->height, frame->format,
        frame->width, frame->height, AV_PIX_FMT_RGBA, SWS_POINT, NULL, NULL, NULL);
    if (!scale) r = AVERROR(ENOMEM);
    else {
      uint8_t *dest[] = {out}; int stride[] = {frame->width * 4};
      r = sws_scale(scale, (const uint8_t *const *)frame->data, frame->linesize,
          0, frame->height, dest, stride);
      sws_freeContext(scale);
    }
    if (r >= 0) {
      *w = frame->width;
      *h = frame->height;
      r = 0;
    }
  }
  avcodec_free_context(&ctx);
  av_frame_free(&frame);
  av_packet_free(&packet);
  return r;
}

static int json_append(char *out, int size, int *used, const char *fmt, ...) {
  if (*used >= size)
    return AVERROR(ENOSPC);
  va_list args;
  va_start(args, fmt);
  int n = vsnprintf(out + *used, (size_t)(size - *used), fmt, args);
  va_end(args);
  if (n < 0 || n >= size - *used)
    return AVERROR(ENOSPC);
  *used += n;
  return 0;
}
static int json_string(char *out, int size, int *used, const char *s) {
  int r = json_append(out, size, used, "\"");
  for (const unsigned char *p = (const unsigned char *)(s ? s : "");
       r >= 0 && *p; p++) {
    if (*p == '"' || *p == '\\')
      r = json_append(out, size, used, "\\%c", *p);
    else if (*p < 0x20)
      r = json_append(out, size, used, "\\u%04x", *p);
    else
      r = json_append(out, size, used, "%c", *p);
  }
  if (r >= 0)
    r = json_append(out, size, used, "\"");
  return r;
}
static int list_codecs(char *out, int size, int *used, int audio,
                       int artwork, int encoders) {
  void *opaque = NULL;
  const AVCodec *c;
  int first = 1;
  while ((c = av_codec_iterate(&opaque))) {
    if ((encoders ? !av_codec_is_encoder(c) : av_codec_is_encoder(c)) ||
        (audio && c->type != AVMEDIA_TYPE_AUDIO) ||
        (artwork && strcmp(c->name, "mjpeg") && strcmp(c->name, "png") &&
         strcmp(c->name, "webp") && strcmp(c->name, "webp_anim")))
      continue;
    if (!first && json_append(out, size, used, ",") < 0)
      return AVERROR(ENOSPC);
    if (json_string(out, size, used, c->name) < 0)
      return AVERROR(ENOSPC);
    first = 0;
  }
  return 0;
}
static int list_demuxers(char *out, int size, int *used, int muxers) {
  void *opaque = NULL;
  const AVInputFormat *in;
  const AVOutputFormat *outf;
  int first = 1;
  if (!muxers) {
    while ((in = av_demuxer_iterate(&opaque))) {
      const char *name = in->name;
      /* The MOV demuxer advertises its container aliases as one runtime
       * name. Report the configured component name so runtime and generated
       * Buildroot manifests compare the same thing. */
      if (!strncmp(name, "mov,", 4))
        name = "mov";
      if (!first && json_append(out, size, used, ",") < 0)
        return AVERROR(ENOSPC);
      if (json_string(out, size, used, name) < 0)
        return AVERROR(ENOSPC);
      first = 0;
    }
  } else {
    opaque = NULL;
    while ((outf = av_muxer_iterate(&opaque))) {
      if (!first && json_append(out, size, used, ",") < 0)
        return AVERROR(ENOSPC);
      if (json_string(out, size, used, outf->name) < 0)
        return AVERROR(ENOSPC);
      first = 0;
    }
  }
  return 0;
}
static int list_parsers(char *out, int size, int *used) {
  void *opaque = NULL;
  const AVCodecParser *p;
  int first = 1;
  while ((p = av_parser_iterate(&opaque))) {
    const char *name = avcodec_get_name(p->codec_ids[0]);
    /* FFmpeg exposes the MPEG audio parser as several codec IDs. Report the
     * configure component name so the runtime manifest is comparable with the
     * generated Buildroot component manifest. */
    if (p->codec_ids[0] == AV_CODEC_ID_MP1 ||
        p->codec_ids[0] == AV_CODEC_ID_MP2 ||
        p->codec_ids[0] == AV_CODEC_ID_MP3)
      name = "mpegaudio";
    if (!first && json_append(out, size, used, ",") < 0)
      return AVERROR(ENOSPC);
    if (json_string(out, size, used, name) < 0)
      return AVERROR(ENOSPC);
    first = 0;
  }
  return 0;
}
static int list_filters(char *out, int size, int *used) {
  void *opaque = NULL;
  const AVFilter *f;
  int first = 1;
  while ((f = av_filter_iterate(&opaque))) {
    if (!first && json_append(out, size, used, ",") < 0)
      return AVERROR(ENOSPC);
    if (json_string(out, size, used, f->name) < 0)
      return AVERROR(ENOSPC);
    first = 0;
  }
  return 0;
}
static int list_protocols(char *out, int size, int *used) {
  void *opaque = NULL;
  const char *p;
  int first = 1;
  while ((p = avio_enum_protocols(&opaque, 0))) {
    if (!first && json_append(out, size, used, ",") < 0)
      return AVERROR(ENOSPC);
    if (json_string(out, size, used, p) < 0)
      return AVERROR(ENOSPC);
    first = 0;
  }
  return 0;
}
int rb_media_components(char *out, int size) {
  if (!out || size < 1024)
    return AVERROR(EINVAL);
  int used = 0, r;
  r = json_append(out, size, &used,
                  "{\"version\":\"%s\",\"libraries\":{\"libavutil\":\"%d.%d.%d\",\"libavcodec\":\"%d.%d.%d\",\"libavformat\":\"%d.%d.%d\",\"libavfilter\":\"%d.%d.%d\",\"libswresample\":\"%d.%d.%d\",\"libswscale\":\"%d.%d.%d\"},\"configuration\":",
                  av_version_info(), LIBAVUTIL_VERSION_MAJOR,
                  LIBAVUTIL_VERSION_MINOR, LIBAVUTIL_VERSION_MICRO,
                  LIBAVCODEC_VERSION_MAJOR, LIBAVCODEC_VERSION_MINOR,
                  LIBAVCODEC_VERSION_MICRO, LIBAVFORMAT_VERSION_MAJOR,
                  LIBAVFORMAT_VERSION_MINOR, LIBAVFORMAT_VERSION_MICRO,
                  LIBAVFILTER_VERSION_MAJOR, LIBAVFILTER_VERSION_MINOR,
                  LIBAVFILTER_VERSION_MICRO, LIBSWRESAMPLE_VERSION_MAJOR,
                  LIBSWRESAMPLE_VERSION_MINOR, LIBSWRESAMPLE_VERSION_MICRO,
                  LIBSWSCALE_VERSION_MAJOR, LIBSWSCALE_VERSION_MINOR,
                  LIBSWSCALE_VERSION_MICRO);
  if (r < 0 || json_string(out, size, &used, avcodec_configuration()) < 0)
    return AVERROR(ENOSPC);
  const char *names[] = {"demuxers", "muxers", "parsers", "filters",
                         "protocols", "audio_decoders", "artwork_decoders",
                         "encoders"};
  for (unsigned i = 0; i < sizeof(names) / sizeof(names[0]); i++) {
    if (json_append(out, size, &used, ",\"%s\":[", names[i]) < 0)
      return AVERROR(ENOSPC);
    if (!strcmp(names[i], "demuxers"))
      r = list_demuxers(out, size, &used, 0);
    else if (!strcmp(names[i], "muxers"))
      r = list_demuxers(out, size, &used, 1);
    else if (!strcmp(names[i], "parsers"))
      r = list_parsers(out, size, &used);
    else if (!strcmp(names[i], "filters"))
      r = list_filters(out, size, &used);
    else if (!strcmp(names[i], "protocols"))
      r = list_protocols(out, size, &used);
    else if (!strcmp(names[i], "audio_decoders"))
      r = list_codecs(out, size, &used, 1, 0, 0);
    else if (!strcmp(names[i], "artwork_decoders"))
      r = list_codecs(out, size, &used, 0, 1, 0);
    else
      r = list_codecs(out, size, &used, 0, 0, 1);
    if (r < 0 || json_append(out, size, &used, "]") < 0)
      return AVERROR(ENOSPC);
  }
  if (json_append(out, size, &used, "}") < 0)
    return AVERROR(ENOSPC);
  return used;
}
