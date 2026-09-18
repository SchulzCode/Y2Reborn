/* SPDX-License-Identifier: MIT
 * ABI membrane only. All contexts have one owner; pointers never escape to app
 * logic. Production builds compile against pinned Buildroot headers, without
 * bindgen. */
#define _POSIX_C_SOURCE 200809L
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/avutil.h>
#include <libavutil/channel_layout.h>
#include <libavutil/opt.h>
#include <libswresample/swresample.h>
#include <libswscale/swscale.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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
  return atomic_load(&c->cancelled) || mono() > atomic_load(&c->deadline);
}
static void deadline(RbCancel *c) {
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
  uint64_t duration_ms, bitrate;
  uint32_t rate, channels, track, disc, artwork;
} RbMetadata;
typedef struct {
  AVFormatContext *f;
  AVCodecContext *c;
  AVPacket *p;
  AVFrame *frame;
  SwrContext *swr;
  RbCancel *cancel;
  int stream, rate, eof, drained, pending, offset;
  int16_t pcm[65536 * 2];
  uint64_t packets, frames;
  int64_t position, seek_target;
} RbMedia;
void rb_media_close(RbMedia *m) {
  if (!m)
    return;
  swr_free(&m->swr);
  av_frame_free(&m->frame);
  av_packet_free(&m->p);
  avcodec_free_context(&m->c);
  avformat_close_input(&m->f);
  free(m);
}
static const char *tag(RbMedia *m, const char *key) {
  AVDictionaryEntry *e = av_dict_get(m->f->metadata, key, NULL, 0);
  if (!e)
    e = av_dict_get(m->f->streams[m->stream]->metadata, key, NULL, 0);
  return e ? e->value : "";
}
int rb_media_open(const char *path, int rate, RbCancel *cancel, RbMedia **out,
                  RbMetadata *meta) {
  int ret = AVERROR(ENOMEM);
  *out = NULL;
  RbMedia *m = calloc(1, sizeof(*m));
  if (!m)
    return ret;
  m->cancel = cancel;
  m->rate = rate;
  deadline(cancel);
  m->f = avformat_alloc_context();
  if (!m->f)
    goto fail;
  m->f->interrupt_callback = (AVIOInterruptCB){interrupted, cancel};
  m->f->probesize = 2 * 1024 * 1024;
  m->f->max_analyze_duration = 3 * AV_TIME_BASE;
  AVDictionary *options = NULL;
  av_dict_set(&options, "protocol_whitelist", "file", 0);
  av_dict_set(&options, "format_whitelist", "flac,mp3,mov,ogg,wav,aac", 0);
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
  if ((ret = avcodec_parameters_to_context(
           m->c, m->f->streams[m->stream]->codecpar)) < 0)
    goto fail;
  m->c->thread_count = 1;
  if ((ret = avcodec_open2(m->c, codec, NULL)) < 0)
    goto fail;
  if (m->c->sample_rate < 8000 || m->c->sample_rate > 384000 ||
      m->c->ch_layout.nb_channels < 1 || m->c->ch_layout.nb_channels > 8) {
    ret = AVERROR(EINVAL);
    goto fail;
  }
  AVChannelLayout stereo = AV_CHANNEL_LAYOUT_STEREO;
  ret = swr_alloc_set_opts2(&m->swr, &stereo, AV_SAMPLE_FMT_S16, rate,
                            &m->c->ch_layout, m->c->sample_fmt,
                            m->c->sample_rate, 0, NULL);
  if (ret < 0)
    goto fail;
  if ((ret = swr_init(m->swr)) < 0)
    goto fail;
  m->p = av_packet_alloc();
  m->frame = av_frame_alloc();
  if (!m->p || !m->frame) {
    ret = AVERROR(ENOMEM);
    goto fail;
  }
  memset(meta, 0, sizeof(*meta));
  snprintf(meta->title, sizeof(meta->title), "%s", tag(m, "title"));
  snprintf(meta->artist, sizeof(meta->artist), "%s", tag(m, "artist"));
  snprintf(meta->album, sizeof(meta->album), "%s", tag(m, "album"));
  snprintf(meta->album_artist, sizeof(meta->album_artist), "%s",
           tag(m, "album_artist"));
  snprintf(meta->codec, sizeof(meta->codec), "%s", codec->name);
  meta->track = strtoul(tag(m, "track"), NULL, 10);
  meta->disc = strtoul(tag(m, "disc"), NULL, 10);
  meta->rate = m->c->sample_rate;
  meta->channels = m->c->ch_layout.nb_channels;
  meta->bitrate = m->f->bit_rate > 0 ? (uint64_t)m->f->bit_rate : 0;
  meta->duration_ms = m->f->duration > 0 ? (uint64_t)m->f->duration / 1000 : 0;
  for (unsigned i = 0; i < m->f->nb_streams; i++)
    if (m->f->streams[i]->disposition & AV_DISPOSITION_ATTACHED_PIC)
      meta->artwork = 1;
  *out = m;
  return 0;
fail:
  rb_media_close(m);
  return ret;
}
int rb_media_read(RbMedia *m, int16_t *out, int capacity, uint64_t *packets,
                  uint64_t *frames, int64_t *position) {
  deadline(m->cancel);
  int ret;
  uint8_t *dst = (uint8_t *)m->pcm;
  while (m->offset >= m->pending) {
    if (interrupted(m->cancel))
      return AVERROR_EXIT;
    m->pending = m->offset = 0;
    ret = avcodec_receive_frame(m->c, m->frame);
    if (ret == 0) {
      m->frames++;
      if (m->frame->nb_samples > 262144)
        return AVERROR(EINVAL);
      ret = swr_convert(m->swr, &dst, 65536,
                        (const uint8_t **)m->frame->extended_data,
                        m->frame->nb_samples);
      int64_t pts = m->frame->best_effort_timestamp;
      if (pts != AV_NOPTS_VALUE)
        m->position = av_rescale_q(pts, m->f->streams[m->stream]->time_base,
                                   (AVRational){1, m->rate});
      av_frame_unref(m->frame);
      if (ret < 0)
        return ret;
      m->pending = ret;
      if (m->seek_target > m->position)
        m->offset = (int)((m->seek_target - m->position) < ret
                              ? (m->seek_target - m->position)
                              : ret);
      if (m->offset < m->pending)
        break;
      continue;
    }
    if (ret == AVERROR_EOF) {
      ret = swr_convert(m->swr, &dst, 65536, NULL, 0);
      if (ret < 0)
        return ret;
      if (!ret)
        return 0;
      m->pending = ret;
      break;
    }
    if (ret != AVERROR(EAGAIN))
      return ret;
    if (m->eof) {
      if (m->drained)
        return AVERROR_INVALIDDATA;
      ret = avcodec_send_packet(m->c, NULL);
      if (ret < 0 && ret != AVERROR_EOF)
        return ret;
      m->drained = 1;
      continue;
    }
    do {
      ret = av_read_frame(m->f, m->p);
      if (ret < 0)
        break;
      if (m->p->stream_index != m->stream)
        av_packet_unref(m->p);
      else
        break;
    } while (!interrupted(m->cancel));
    if (ret == AVERROR_EOF) {
      m->eof = 1;
      continue;
    }
    if (ret < 0)
      return ret;
    if (interrupted(m->cancel)) {
      av_packet_unref(m->p);
      return AVERROR_EXIT;
    }
    m->packets++;
    ret = avcodec_send_packet(m->c, m->p);
    av_packet_unref(m->p);
    if (ret < 0)
      return ret;
  }
  int n = m->pending - m->offset;
  if (n > capacity)
    n = capacity;
  memcpy(out, m->pcm + m->offset * 2, n * 2 * sizeof(int16_t));
  *position = av_rescale(m->position + m->offset, 1000, m->rate);
  m->offset += n;
  *packets = m->packets;
  *frames = m->frames;
  return n;
}
int rb_media_seek(RbMedia *m, int64_t ms) {
  deadline(m->cancel);
  int r = avformat_seek_file(m->f, -1, INT64_MIN, ms * 1000, ms * 1000, 0);
  if (r < 0)
    return r;
  avcodec_flush_buffers(m->c);
  swr_close(m->swr);
  r = swr_init(m->swr);
  m->eof = m->drained = m->pending = m->offset = 0;
  m->position = m->seek_target = av_rescale(ms, m->rate, 1000);
  return r;
}
int rb_media_art(RbMedia *m, uint8_t *out, int side) {
  if (side < 1 || side > 256)
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
