/* SPDX-License-Identifier: MIT
 *
 * The only native media membrane used by Reborn. FFmpeg owns demuxing,
 * decoding, the floating point filter graph, artwork conversion and the final
 * libswresample conversion. Rust sees an opaque, single-owner context and a
 * byte-oriented stereo PCM stream.
 */
#define _POSIX_C_SOURCE 200809L
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
#include <libswresample/swresample.h>
#include <libswresample/version.h>
#include <libswscale/swscale.h>
#include <libswscale/version.h>
#include <errno.h>
#include <ctype.h>
#include <math.h>
#include <stdarg.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <time.h>

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
  av_max_alloc(32 * 1024 * 1024);
}
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
} RbMetadata;

typedef struct {
  int replay_gain_mode;
  uint32_t volume;
  uint32_t eq_count;
  struct {
    double frequency_hz;
    double gain_db;
    double q;
  } eq[8];
  uint32_t crossfade_ms;
} RbDspConfig;

typedef struct {
  AVFormatContext *f;
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
  int stream, output_rate, output_bytes;
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
} RbMedia;

static const char *tag(RbMedia *m, const char *key) {
  AVDictionaryEntry *e = av_dict_get(m->f->metadata, key, NULL, 0);
  if (!e)
    e = av_dict_get(m->f->streams[m->stream]->metadata, key, NULL, 0);
  return e ? e->value : "";
}
static int parse_tag_number(const char *s, double minimum, double maximum,
                            double *value) {
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
  if (strncasecmp(end, "db", 2) == 0) {
    end += 2;
    while (*end && isspace((unsigned char)*end))
      end++;
  }
  if (*end)
    return 0;
  *value = parsed;
  return 1;
}
static int gain_tag_value(RbMedia *m, const char *upper, const char *lower,
                          double *value) {
  if (parse_tag_number(tag(m, upper), -60.0, 60.0, value))
    return 1;
  return parse_tag_number(tag(m, lower), -60.0, 60.0, value);
}
static int peak_tag_value(RbMedia *m, const char *upper, const char *lower,
                          double *value) {
  if (parse_tag_number(tag(m, upper), 0.0, 16.0, value))
    return 1;
  return parse_tag_number(tag(m, lower), 0.0, 16.0, value);
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
  snprintf(dst, n, "%s", src ? src : "");
}
static enum AVSampleFormat format_from_int(int format) {
  return format == 2 ? AV_SAMPLE_FMT_S32 : AV_SAMPLE_FMT_S16;
}

/* Final sink conversion is kept here with the media membrane.  Playback may
 * use S32 as the transition working format even when the currently qualified
 * ALSA sink is S16; Rust never performs a sample-format conversion itself. */
int rb_media_convert(const uint8_t *input, int frames, int rate, int input_format,
                     int output_format, uint8_t *output, int capacity_frames) {
  if (!input || !output || frames <= 0 || capacity_frames < frames ||
      rate < 8000 || rate > 384000 ||
      (input_format != 1 && input_format != 2) ||
      (output_format != 1 && output_format != 2))
    return AVERROR(EINVAL);
  enum AVSampleFormat in_fmt = format_from_int(input_format);
  enum AVSampleFormat out_fmt = format_from_int(output_format);
  if (in_fmt == out_fmt) {
    size_t bytes = (size_t)frames * 2 * av_get_bytes_per_sample(in_fmt);
    memcpy(output, input, bytes);
    return frames;
  }
  AVChannelLayout stereo = AV_CHANNEL_LAYOUT_STEREO;
  SwrContext *swr = NULL;
  int r = swr_alloc_set_opts2(&swr, &stereo, out_fmt, rate, &stereo, in_fmt,
                              rate, 0, NULL);
  if (r < 0 || (r = swr_init(swr)) < 0) {
    swr_free(&swr);
    return r < 0 ? r : AVERROR(EINVAL);
  }
  uint8_t *in[] = {(uint8_t *)input};
  uint8_t *out[] = {output};
  r = swr_convert(swr, out, capacity_frames, (const uint8_t **)in, frames);
  swr_free(&swr);
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
  if (r >= 0) {
    uint8_t *in[] = {(uint8_t *)input};
    r = swr_convert(*to_float, frame->extended_data, frames,
                    (const uint8_t **)in, frames);
    if (r >= 0 && r != frames)
      r = AVERROR(EINVAL);
  }
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
      (format != 1 && format != 2))
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
    uint8_t *out[] = {output + (size_t)*written_frames *
                               2 * av_get_bytes_per_sample(format)};
    int n = swr_convert(from_float, out, room,
                        (const uint8_t **)filtered->extended_data,
                        filtered->nb_samples);
    av_frame_free(&filtered);
    filtered = NULL;
    if (n < 0) {
      r = n;
      goto done;
    }
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
  memcpy(m->pending + m->pending_frames * m->output_bytes, data, bytes);
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
  double applied = rg + user + headroom;
  m->applied_gain_db = applied;
  m->headroom_db = headroom;
  char chain[1024];
  int used = snprintf(chain, sizeof(chain),
                      "aformat=sample_fmts=fltp:channel_layouts=stereo");
  if (used < 0 || (size_t)used >= sizeof(chain))
    return AVERROR(EINVAL);
  if (m->dsp.volume != 100 || rg != 0.0 || headroom != 0.0) {
    double linear = pow(10.0, applied / 20.0);
    used += snprintf(chain + used, sizeof(chain) - (size_t)used,
                     ",volume=volume=%.12f", linear);
  }
  for (uint32_t i = 0; i < m->dsp.eq_count && i < 8; i++) {
    if (m->dsp.eq[i].gain_db == 0.0)
      continue;
    double q = m->dsp.eq[i].q > 0.01 ? m->dsp.eq[i].q : 1.0;
    used += snprintf(chain + used, sizeof(chain) - (size_t)used,
                     ",equalizer=f=%.9g:t=q:w=%.9g:g=%.9g",
                     m->dsp.eq[i].frequency_hz, q, m->dsp.eq[i].gain_db);
  }
  /* The limiter is also an FFmpeg graph stage so EQ peaks cannot wrap during
   * the final integer conversion. */
  used += snprintf(chain + used, sizeof(chain) - (size_t)used,
                   ",alimiter=limit=0.98:latency=1:level=0");
  if (used <= 0 || (size_t)used >= sizeof(chain))
    return AVERROR(EINVAL);
  snprintf(m->filter_description, sizeof(m->filter_description),
           "ReplayGain=%s,volume=%u,EQ=%s,alimiter,acrossfade=%ums",
           rg_name, m->dsp.volume, m->dsp.eq_count ? "configured" : "off",
           m->dsp.crossfade_ms);
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
  free(m->pending);
  free(m);
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
  m->output_fmt = format_from_int(output_format);
  m->output_bytes = av_get_bytes_per_sample(m->output_fmt) * 2;
  if (rate < 8000 || rate > 384000 || (output_format != 1 && output_format != 2)) {
    ret = AVERROR(EINVAL);
    goto fail;
  }
  deadline(cancel);
  m->f = avformat_alloc_context();
  if (!m->f)
    goto fail;
  m->f->interrupt_callback = (AVIOInterruptCB){interrupted, cancel};
  m->f->probesize = 2 * 1024 * 1024;
  m->f->max_analyze_duration = 3 * AV_TIME_BASE;
  AVDictionary *options = NULL;
  av_dict_set(&options, "protocol_whitelist", "file", 0);
  av_dict_set(&options, "format_whitelist", "flac,mp3,mov,ogg,wav,aac,aiff,ape,wv", 0);
  ret = avformat_open_input(&m->f, path, NULL, &options);
  av_dict_free(&options);
  if (ret < 0)
    goto fail;
  if ((ret = avformat_find_stream_info(m->f, NULL)) < 0)
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
  av_opt_set_int(m->c, "skip_manual", 1, AV_OPT_SEARCH_CHILDREN);
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
  memset(meta, 0, sizeof(*meta));
  text(meta->title, sizeof(meta->title), tag(m, "title"));
  text(meta->artist, sizeof(meta->artist), tag(m, "artist"));
  text(meta->album, sizeof(meta->album), tag(m, "album"));
  text(meta->album_artist, sizeof(meta->album_artist), tag(m, "album_artist"));
  text(meta->codec, sizeof(meta->codec), codec->name);
  text(meta->source_sample_fmt, sizeof(meta->source_sample_fmt),
       av_get_sample_fmt_name(m->c->sample_fmt));
  text(meta->decoder_sample_fmt, sizeof(meta->decoder_sample_fmt),
       av_get_sample_fmt_name(m->c->sample_fmt));
  text(meta->internal_sample_fmt, sizeof(meta->internal_sample_fmt), "fltp");
  text(meta->final_sample_fmt, sizeof(meta->final_sample_fmt),
       av_get_sample_fmt_name(m->output_fmt));
  text(meta->filters, sizeof(meta->filters), m->filter_description);
  meta->track = strtoul(tag(m, "track"), NULL, 10);
  meta->disc = strtoul(tag(m, "disc"), NULL, 10);
  meta->rate = m->c->sample_rate;
  meta->channels = m->c->ch_layout.nb_channels;
  meta->source_bits =
      m->f->streams[m->stream]->codecpar->bits_per_raw_sample > 0
          ? (uint32_t)m->f->streams[m->stream]->codecpar->bits_per_raw_sample
          : (uint32_t)av_get_bits_per_sample(codec->id);
  /* Compressed formats have no source PCM bit depth. Keep that distinction in
   * diagnostics instead of reporting the decoder's working width as source
   * precision. */
  meta->bitrate = m->f->bit_rate > 0 ? (uint64_t)m->f->bit_rate : 0;
  meta->duration_ms = m->f->duration > 0 ? (uint64_t)m->f->duration / 1000 : 0;
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
  meta->track_gain_db = gain_tag(m, "REPLAYGAIN_TRACK_GAIN", "replaygain_track_gain");
  meta->album_gain_db = gain_tag(m, "REPLAYGAIN_ALBUM_GAIN", "replaygain_album_gain");
  meta->track_peak = peak_tag(m, "REPLAYGAIN_TRACK_PEAK", "replaygain_track_peak");
  meta->album_peak = peak_tag(m, "REPLAYGAIN_ALBUM_PEAK", "replaygain_album_peak");
  meta->applied_gain_db = m->applied_gain_db;
  meta->headroom_db = m->headroom_db;
  for (unsigned i = 0; i < m->f->nb_streams; i++)
    if (m->f->streams[i]->disposition & AV_DISPOSITION_ATTACHED_PIC)
      meta->artwork = 1;
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

int rb_media_art(RbMedia *m, uint8_t *out, int side) {
  if (!m || !out || side < 1 || side > 256)
    return AVERROR(EINVAL);
  for (unsigned i = 0; i < m->f->nb_streams; i++) {
    AVStream *s = m->f->streams[i];
    if (!(s->disposition & AV_DISPOSITION_ATTACHED_PIC))
      continue;
    const AVCodec *c = avcodec_find_decoder(s->codecpar->codec_id);
    if (!c)
      return AVERROR_DECODER_NOT_FOUND;
    AVCodecContext *ctx = avcodec_alloc_context3(c);
    AVFrame *f = av_frame_alloc();
    if (!ctx || !f) {
      avcodec_free_context(&ctx);
      av_frame_free(&f);
      return AVERROR(ENOMEM);
    }
    ctx->max_pixels = 16 * 1024 * 1024;
    ctx->thread_count = 1;
    int r = avcodec_parameters_to_context(ctx, s->codecpar);
    if (r >= 0)
      r = avcodec_open2(ctx, c, NULL);
    if (r >= 0)
      r = avcodec_send_packet(ctx, &s->attached_pic);
    if (r >= 0)
      r = avcodec_receive_frame(ctx, f);
    if (r >= 0) {
      struct SwsContext *scale =
          sws_getContext(f->width, f->height, f->format, side, side,
                         AV_PIX_FMT_RGBA, SWS_BILINEAR, NULL, NULL, NULL);
      if (!scale)
        r = AVERROR(ENOMEM);
      else {
        uint8_t *dest[] = {out};
        int stride[] = {side * 4};
        r = sws_scale(scale, (const uint8_t *const *)f->data, f->linesize, 0,
                      f->height, dest, stride);
        sws_freeContext(scale);
      }
    }
    avcodec_free_context(&ctx);
    av_frame_free(&f);
    return r < 0 ? r : 0;
  }
  return AVERROR(ENOENT);
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

/* External cover images use the same FFmpeg image decoders and libswscale as
 * attached artwork. They are read as bounded local files without enabling an
 * image demuxer or any network protocol in the production build. */
int rb_media_art_file(const char *path, uint8_t *out, int side) {
  if (!path || !out || side < 1 || side > 256)
    return AVERROR(EINVAL);
  enum AVCodecID id = image_codec_for_path(path);
  const AVCodec *codec = id == AV_CODEC_ID_NONE ? NULL : avcodec_find_decoder(id);
  if (!codec)
    return AVERROR_DECODER_NOT_FOUND;
  FILE *file = fopen(path, "rb");
  if (!file)
    return AVERROR(errno);
  if (fseek(file, 0, SEEK_END) != 0) {
    fclose(file);
    return AVERROR(EIO);
  }
  long length = ftell(file);
  if (length <= 0 || length > 16 * 1024 * 1024 || fseek(file, 0, SEEK_SET) != 0) {
    fclose(file);
    return AVERROR(EINVAL);
  }
  AVPacket *packet = av_packet_alloc();
  AVCodecContext *ctx = avcodec_alloc_context3(codec);
  AVFrame *frame = av_frame_alloc();
  int r = packet && ctx && frame ? av_new_packet(packet, length) : AVERROR(ENOMEM);
  if (r >= 0 && fread(packet->data, 1, (size_t)length, file) != (size_t)length)
    r = AVERROR(EIO);
  fclose(file);
  if (r >= 0)
    r = avcodec_open2(ctx, codec, NULL);
  if (r >= 0)
    r = avcodec_send_packet(ctx, packet);
  if (r >= 0)
    r = avcodec_receive_frame(ctx, frame);
  if (r >= 0) {
    struct SwsContext *scale =
        sws_getContext(frame->width, frame->height, frame->format, side, side,
                       AV_PIX_FMT_RGBA, SWS_BILINEAR, NULL, NULL, NULL);
    if (!scale)
      r = AVERROR(ENOMEM);
    else {
      uint8_t *dest[] = {out};
      int stride[] = {side * 4};
      r = sws_scale(scale, (const uint8_t *const *)frame->data,
                    frame->linesize, 0, frame->height, dest, stride);
      sws_freeContext(scale);
    }
  }
  av_packet_free(&packet);
  avcodec_free_context(&ctx);
  av_frame_free(&frame);
  return r < 0 ? r : 0;
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
