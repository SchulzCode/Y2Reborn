/* SPDX-License-Identifier: MIT
 *
 * Video for Reborn: a second, picture-only reader of a file whose sound the audio
 * membrane (media.c) plays. FFmpeg demuxes and decodes (H.264 and MPEG-4 on the Y2's
 * hardware decoder when it takes the stream, else in software); this file turns each
 * picture into a small RGBA image for the 480x360 screen and collects subtitle text.
 *
 * A hardware picture arrives in the decoder's block layout (libavcodec/y2vdec.h:
 * luma in 16x32 blocks, CbCr in 16x16 blocks, format AV_PIX_FMT_DRM_PRIME); software
 * pictures are planar 4:2:0. Both are averaged down in one pass; anything else goes
 * through libswscale.
 */
#define _POSIX_C_SOURCE 200809L
#define _FILE_OFFSET_BITS 64
/* The picture loops run for every pixel of every picture: built for speed, whatever the size
 * setting of the rest of the membrane. */
#pragma GCC optimize("O2")
#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/imgutils.h>
#include <libavutil/pixdesc.h>
#include <libswscale/swscale.h>
#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define RB_CUES 4096
#define RB_CUE_TEXT 384

typedef struct {
  int64_t start_ms, end_ms;
  char text[RB_CUE_TEXT];
} RbCue;

typedef struct {
  uint32_t width, height;         /* the picture as stored */
  uint32_t out_width, out_height; /* the RGBA image rb_video_convert writes */
  uint32_t hardware;              /* 1: the hardware decoder took the stream */
  uint32_t fps_milli;
  uint32_t subtitles;             /* 1: the file (or a file beside it) has subtitle text */
  uint32_t reserved;
  uint64_t duration_ms;
  char codec[32];
} RbVideoInfo;

typedef struct RbVideo {
  AVFormatContext *f, *sub_file;
  AVCodecContext *c, *sub;
  AVPacket *p;
  AVFrame *frame;
  struct SwsContext *sws;
  int stream, sub_stream, hardware, draining, held;
  int out_w, out_h, step;
  int64_t start_ms;
  /* For each output column: the two neighbouring columns of the averaged line it is the mean of. */
  uint16_t *tap_y, *tap_c;
  uint8_t *wide;                   /* one averaged line at the source's (or half its) width */
  int half;                        /* columns are added in pairs: the averaged line is half as wide */
  uint16_t *acc_y, *acc_u, *acc_v; /* the source lines of one output row, added up column by column */
  uint8_t *line_y;                 /* one output row of averaged luma */
  uint8_t *chroma;                 /* averaged CbCr at half the output size */
  RbCue *cues;
  int cue_count, cue_next;
} RbVideo;

/* The picture codecs Reborn plays (y2_production_defconfig); a cover's are not among them. */
int rb_video_codec(enum AVCodecID id) {
  return id == AV_CODEC_ID_H264 || id == AV_CODEC_ID_MPEG4 || id == AV_CODEC_ID_H263 ||
         id == AV_CODEC_ID_MSMPEG4V3 || id == AV_CODEC_ID_VP8 || id == AV_CODEC_ID_VP9;
}

static enum AVPixelFormat pick_format(AVCodecContext *c, const enum AVPixelFormat *formats) {
  RbVideo *v = c->opaque;
  const enum AVPixelFormat *f;
  if (v->hardware)
    for (f = formats; *f != AV_PIX_FMT_NONE; f++)
      if (*f == AV_PIX_FMT_DRM_PRIME) return *f;
  for (f = formats; *f != AV_PIX_FMT_NONE; f++)
    if (!(av_pix_fmt_desc_get(*f)->flags & AV_PIX_FMT_FLAG_HWACCEL)) return *f;
  return AV_PIX_FMT_NONE;
}

static int text_subtitle(enum AVCodecID id) {
  return id == AV_CODEC_ID_SUBRIP || id == AV_CODEC_ID_ASS || id == AV_CODEC_ID_SSA ||
         id == AV_CODEC_ID_MOV_TEXT || id == AV_CODEC_ID_WEBVTT || id == AV_CODEC_ID_TEXT;
}

/* An ASS event ("ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text") or plain
 * text as lines: override blocks dropped, \N and \n as line breaks, \h as a space. */
static void plain_text(const char *in, int ass, char *out, size_t size) {
  size_t n = 0;
  if (ass)
    for (int commas = 0; *in && commas < 8; in++) commas += *in == ',';
  for (; *in && n + 1 < size; in++) {
    if (*in == '{') {
      const char *close = strchr(in, '}');
      if (close) { in = close; continue; }
    }
    if (*in == '\\' && (in[1] == 'N' || in[1] == 'n')) { out[n++] = '\n'; in++; continue; }
    if (*in == '\\' && in[1] == 'h') { out[n++] = ' '; in++; continue; }
    if (*in == '\r') continue;
    if (*in == '<') { /* <i>, </font …> of SubRip */
      const char *close = strchr(in, '>');
      if (close && close - in < 40) { in = close; continue; }
    }
    out[n++] = *in;
  }
  while (n && (out[n - 1] == '\n' || out[n - 1] == ' ')) n--;
  out[n] = 0;
}

static void add_cue(RbVideo *v, AVCodecContext *decoder, AVPacket *p, AVRational tb) {
  AVSubtitle s;
  int got = 0;
  if (v->cue_count == RB_CUES || avcodec_decode_subtitle2(decoder, &s, &got, p) < 0 || !got) return;
  int64_t at = p->pts == AV_NOPTS_VALUE ? 0 : av_rescale_q(p->pts, tb, (AVRational){1, 1000});
  int64_t length = s.end_display_time > s.start_display_time
      ? (int64_t)s.end_display_time - s.start_display_time
      : av_rescale_q(p->duration, tb, (AVRational){1, 1000});
  RbCue *cue = &v->cues[v->cue_count];
  cue->start_ms = at + s.start_display_time - v->start_ms;
  cue->end_ms = cue->start_ms + (length > 0 ? length : 4000);
  cue->text[0] = 0;
  for (unsigned i = 0; i < s.num_rects; i++) {
    const AVSubtitleRect *r = s.rects[i];
    const char *text = r->type == SUBTITLE_ASS ? r->ass : r->type == SUBTITLE_TEXT ? r->text : NULL;
    size_t used = strlen(cue->text);
    if (!text || used + 2 >= sizeof(cue->text)) continue;
    if (used) cue->text[used++] = '\n';
    plain_text(text, r->type == SUBTITLE_ASS, cue->text + used, sizeof(cue->text) - used);
  }
  avsubtitle_free(&s);
  if (cue->text[0]) v->cue_count++;
}

/* Read the subtitle file `name` (SubRip, ASS, WebVTT) whole, for a film without a text track of
 * its own. Returns the number of lines it gave. */
int rb_video_subtitles(RbVideo *v, const char *name) {
  AVFormatContext *f = NULL;
  AVCodecContext *c = NULL;
  int before = v->cue_count;
  if (v->sub_stream >= 0 || avformat_open_input(&f, name, NULL, NULL) < 0) return 0;
  if (avformat_find_stream_info(f, NULL) >= 0 && f->nb_streams &&
      text_subtitle(f->streams[0]->codecpar->codec_id)) {
    const AVCodec *codec = avcodec_find_decoder(f->streams[0]->codecpar->codec_id);
    c = codec ? avcodec_alloc_context3(codec) : NULL;
    if (c && avcodec_parameters_to_context(c, f->streams[0]->codecpar) >= 0 &&
        avcodec_open2(c, codec, NULL) >= 0) {
      int64_t offset = v->start_ms;
      v->start_ms = 0; /* a file's own times count from the start of the film */
      while (av_read_frame(f, v->p) >= 0) {
        add_cue(v, c, v->p, f->streams[0]->time_base);
        av_packet_unref(v->p);
      }
      v->start_ms = offset;
    }
  }
  avcodec_free_context(&c);
  avformat_close_input(&f);
  return v->cue_count - before;
}

/* A subtitle file beside the video ("film.srt", "film.ass", "film.vtt" for "film.mkv"). */
static void load_subtitle_file(RbVideo *v, const char *path) {
  static const char *const endings[] = {".srt", ".ass", ".ssa", ".vtt"};
  const char *dot = strrchr(path, '.'), *slash = strrchr(path, '/');
  size_t stem = dot && (!slash || dot > slash) ? (size_t)(dot - path) : strlen(path);
  char name[4096];
  if (stem + 8 > sizeof(name)) return;
  for (unsigned i = 0; i < sizeof(endings) / sizeof(*endings) && !v->cue_count; i++) {
    memcpy(name, path, stem);
    strcpy(name + stem, endings[i]);
    rb_video_subtitles(v, name);
  }
}

static void free_scaler(RbVideo *v) {
  free(v->tap_y); free(v->tap_c); free(v->wide);
  free(v->chroma);
  v->tap_y = v->tap_c = NULL;
  v->wide = NULL;
  free(v->acc_y); free(v->acc_u); free(v->acc_v); free(v->line_y);
  v->acc_y = v->acc_u = v->acc_v = NULL;
  v->line_y = NULL;
  v->chroma = NULL;
}

void rb_video_close(RbVideo *v) {
  if (!v) return;
  free_scaler(v);
  sws_freeContext(v->sws);
  av_frame_free(&v->frame);
  av_packet_free(&v->p);
  avcodec_free_context(&v->sub);
  avcodec_free_context(&v->c);
  avformat_close_input(&v->f);
  free(v->cues);
  free(v);
}

/* The output size: the picture fitted into max_w x max_h in its display shape, never enlarged
 * (the renderer scales a small picture up), both sides even. */
static void output_size(RbVideo *v, int max_w, int max_h) {
  AVRational sar = av_guess_sample_aspect_ratio(v->f, v->f->streams[v->stream], NULL);
  int w = v->c->width, h = v->c->height;
  double shown_w = sar.num > 0 && sar.den > 0 ? (double)w * sar.num / sar.den : w;
  double scale = (double)max_w / shown_w;
  if ((double)max_h / h < scale) scale = (double)max_h / h;
  if (scale > 1 && shown_w <= w) scale = 1;
  v->out_w = ((int)(shown_w * scale + 0.5) + 1) & ~1;
  v->out_h = ((int)(h * scale + 0.5) + 1) & ~1;
  if (v->out_w < 2) v->out_w = 2;
  if (v->out_h < 2) v->out_h = 2;
  if (v->out_w > max_w) v->out_w = max_w & ~1;
  if (v->out_h > max_h) v->out_h = max_h & ~1;
  /* More source than screen: about four lines for three screen lines are plenty (1280x720 for
   * 480x270: every second line; 1920x1080: every third). Reading the lines from memory is the
   * larger half of the work. */
  v->step = h * 3 / (v->out_h * 4);
  if (v->step < 1) v->step = 1;
}

/* The two columns of a `from`-wide line whose mean is each of `out` output columns: the ones
 * either side of the output column's centre (the same one twice when nothing is shrunk). */
static void tap_plan(int from, int out, uint16_t *tap) {
  for (int x = 0; x < out; x++) {
    int centre = (int)(((int64_t)(2 * x + 1) * from) / (2 * out));
    int a = from > out && centre > 0 ? centre - 1 : centre, b = from > out ? centre : a;
    if (b > from - 1) b = from - 1;
    if (a > b) a = b;
    tap[2 * x] = (uint16_t)a;
    tap[2 * x + 1] = (uint16_t)b;
  }
}

static int make_scaler(RbVideo *v) {
  int w = v->c->width, cw = (w + 1) / 2, ow = v->out_w, ocw = ow / 2;
  free_scaler(v);
  /* Shrinking to less than half: columns are added in pairs as the lines are added up. */
  v->half = w * 2 >= ow * 5;
  v->tap_y = malloc((size_t)ow * 2 * sizeof(*v->tap_y));
  v->tap_c = malloc((size_t)ocw * 2 * sizeof(*v->tap_c));
  /* Whole 16-sample chunks are handled at a time: room for the last one's overhang. */
  v->acc_y = calloc((size_t)w + 32, sizeof(*v->acc_y));
  v->acc_u = calloc((size_t)cw + 32, sizeof(*v->acc_u));
  v->acc_v = calloc((size_t)cw + 32, sizeof(*v->acc_v));
  v->wide = malloc((size_t)w + 32);
  v->line_y = malloc((size_t)ow + 16);
  v->chroma = malloc((size_t)ocw * (size_t)(v->out_h / 2) * 2);
  if (!v->tap_y || !v->tap_c || !v->chroma || !v->acc_y || !v->acc_u || !v->acc_v || !v->wide ||
      !v->line_y)
    return AVERROR(ENOMEM);
  tap_plan(v->half ? (w + 1) / 2 : w, ow, v->tap_y);
  tap_plan(v->half ? (cw + 1) / 2 : cw, ocw, v->tap_c);
  return 0;
}

int rb_video_open(const char *path, int max_w, int max_h, int hardware, RbVideo **out,
                  RbVideoInfo *info) {
  RbVideo *v = calloc(1, sizeof(*v));
  const AVCodec *codec = NULL;
  int ret;
  *out = NULL;
  if (!v) return AVERROR(ENOMEM);
  v->hardware = hardware;
  v->sub_stream = -1;
  v->p = av_packet_alloc();
  v->frame = av_frame_alloc();
  v->cues = calloc(RB_CUES, sizeof(*v->cues));
  if (!v->p || !v->frame || !v->cues) { ret = AVERROR(ENOMEM); goto fail; }
  if ((ret = avformat_open_input(&v->f, path, NULL, NULL)) < 0) goto fail;
  if ((ret = avformat_find_stream_info(v->f, NULL)) < 0) goto fail;
  ret = av_find_best_stream(v->f, AVMEDIA_TYPE_VIDEO, -1, -1, &codec, 0);
  if (ret < 0 || (v->f->streams[ret]->disposition & AV_DISPOSITION_ATTACHED_PIC)) {
    ret = AVERROR_STREAM_NOT_FOUND;
    goto fail;
  }
  v->stream = ret;
  v->start_ms = v->f->start_time == AV_NOPTS_VALUE ? 0 : v->f->start_time / 1000;
  AVStream *st = v->f->streams[v->stream];
  if (!(v->c = avcodec_alloc_context3(codec))) { ret = AVERROR(ENOMEM); goto fail; }
  if ((ret = avcodec_parameters_to_context(v->c, st->codecpar)) < 0) goto fail;
  v->c->opaque = v;
  v->c->get_format = pick_format;
  v->c->pkt_timebase = st->time_base;
  /* Software decoding spreads over the four cores; the hardware path decodes in the calling thread. */
  v->c->thread_count = 4;
  if ((ret = avcodec_open2(v->c, codec, NULL)) < 0) goto fail;
  if (v->c->width < 16 || v->c->height < 16 || v->c->width > 4096 || v->c->height > 4096) {
    ret = AVERROR_PATCHWELCOME;
    goto fail;
  }
  for (unsigned i = 0; i < v->f->nb_streams; i++) {
    AVStream *s = v->f->streams[i];
    if ((int)i == v->stream) continue;
    if (v->sub_stream < 0 && s->codecpar->codec_type == AVMEDIA_TYPE_SUBTITLE &&
        text_subtitle(s->codecpar->codec_id)) {
      const AVCodec *sc = avcodec_find_decoder(s->codecpar->codec_id);
      v->sub = sc ? avcodec_alloc_context3(sc) : NULL;
      if (v->sub && avcodec_parameters_to_context(v->sub, s->codecpar) >= 0 &&
          avcodec_open2(v->sub, sc, NULL) >= 0) {
        v->sub_stream = (int)i;
        continue;
      }
      avcodec_free_context(&v->sub);
    }
    s->discard = AVDISCARD_ALL;
  }
  if (v->sub_stream < 0) load_subtitle_file(v, path);
  output_size(v, max_w, max_h);
  if ((ret = make_scaler(v)) < 0) goto fail;
  memset(info, 0, sizeof(*info));
  info->width = (uint32_t)v->c->width;
  info->height = (uint32_t)v->c->height;
  info->out_width = (uint32_t)v->out_w;
  info->out_height = (uint32_t)v->out_h;
  info->subtitles = v->sub_stream >= 0 || v->cue_count > 0;
  AVRational rate = av_guess_frame_rate(v->f, st, NULL);
  info->fps_milli = rate.num > 0 && rate.den > 0 ? (uint32_t)(1000LL * rate.num / rate.den) : 0;
  info->duration_ms = v->f->duration > 0 ? (uint64_t)(v->f->duration / 1000) : 0;
  snprintf(info->codec, sizeof(info->codec), "%s", avcodec_get_name(v->c->codec_id));
  *out = v;
  return 0;
fail:
  rb_video_close(v);
  return ret;
}

/* Decode the next picture and keep it for rb_video_convert. 1: a picture, `pts_ms` its time from
 * the start of the file; 0: the end; negative: an FFmpeg error. */
int rb_video_next(RbVideo *v, int64_t *pts_ms, uint32_t *hardware) {
  v->held = 0;
  for (;;) {
    int ret = avcodec_receive_frame(v->c, v->frame);
    if (ret >= 0) {
      int64_t ts = v->frame->best_effort_timestamp;
      *pts_ms = ts == AV_NOPTS_VALUE ? -1
          : av_rescale_q(ts, v->f->streams[v->stream]->time_base, (AVRational){1, 1000}) - v->start_ms;
      *hardware = v->frame->format == AV_PIX_FMT_DRM_PRIME;
      v->held = 1;
      return 1;
    }
    if (ret == AVERROR_EOF) return 0;
    if (ret != AVERROR(EAGAIN)) return ret;
    if (v->draining) return 0;
    ret = av_read_frame(v->f, v->p);
    if (ret < 0) {
      v->draining = 1;
      avcodec_send_packet(v->c, NULL);
      continue;
    }
    if (v->p->stream_index == v->stream) {
      ret = avcodec_send_packet(v->c, v->p);
      av_packet_unref(v->p);
      /* A damaged packet costs its picture, not the film. */
      if (ret < 0 && ret != AVERROR(EAGAIN) && ret != AVERROR_INVALIDDATA) return ret;
    } else {
      if (v->p->stream_index == v->sub_stream)
        add_cue(v, v->sub, v->p, v->f->streams[v->sub_stream]->time_base);
      av_packet_unref(v->p);
    }
  }
}

/* Skip pictures nothing refers to while the reader catches up (1), or decode everything (0). */
void rb_video_hurry(RbVideo *v, int on) {
  v->c->skip_frame = on ? AVDISCARD_NONREF : AVDISCARD_DEFAULT;
}

int rb_video_seek(RbVideo *v, int64_t ms) {
  int64_t ts = av_rescale_q(ms + v->start_ms, (AVRational){1, 1000}, v->f->streams[v->stream]->time_base);
  int ret = av_seek_frame(v->f, v->stream, ts, AVSEEK_FLAG_BACKWARD);
  if (ret < 0) return ret;
  avcodec_flush_buffers(v->c);
  v->draining = 0;
  v->held = 0;
  /* Cues of an embedded track are read again from here; a subtitle file's stay. */
  if (v->sub_stream >= 0) {
    avcodec_flush_buffers(v->sub);
    v->cue_count = 0;
  }
  return 0;
}

/* The subtitle text shown at `ms`, "" for none. Returns the text's length. */
int rb_video_subtitle(RbVideo *v, int64_t ms, char *out, int size) {
  if (size < 1) return 0;
  out[0] = 0;
  for (int i = v->cue_count - 1; i >= 0; i--)
    if (v->cues[i].start_ms <= ms && ms < v->cues[i].end_ms) {
      snprintf(out, (size_t)size, "%s", v->cues[i].text);
      break;
    }
  return (int)strlen(out);
}

/* --- pictures to RGBA ---
 *
 * One output row at a time: its source lines are added up column by column (sixteen samples per
 * step, NEON on the player), the column sums are gathered into the output columns and averaged,
 * and the row is converted to RGB eight pixels at a time. Colour arithmetic is in 1/64ths, the
 * same in the NEON and the plain version, so the host tests see what the player shows. */
#if defined(__ARM_NEON)
#include <arm_neon.h>
#endif

/* acc[0..16) += p[0..16) */
static inline void add16(uint16_t *acc, const uint8_t *p) {
#if defined(__ARM_NEON)
  uint8x16_t in = vld1q_u8(p);
  vst1q_u16(acc, vaddw_u8(vld1q_u16(acc), vget_low_u8(in)));
  vst1q_u16(acc + 8, vaddw_u8(vld1q_u16(acc + 8), vget_high_u8(in)));
#else
  for (int i = 0; i < 16; i++) acc[i] += p[i];
#endif
}

/* acc[0..8) += the eight pairs of p[0..16) */
static inline void add16_pairs(uint16_t *acc, const uint8_t *p) {
#if defined(__ARM_NEON)
  vst1q_u16(acc, vpadalq_u8(vld1q_u16(acc), vld1q_u8(p)));
#else
  for (int i = 0; i < 8; i++) acc[i] += p[2 * i] + p[2 * i + 1];
#endif
}

/* Sixteen interleaved CbCr bytes as four sums of two neighbours each. */
static inline void add_pairs_halved(uint16_t *u, uint16_t *w, const uint8_t *p) {
#if defined(__ARM_NEON)
  uint8x8x2_t in = vld2_u8(p);
  vst1_u16(u, vpadal_u8(vld1_u16(u), in.val[0]));
  vst1_u16(w, vpadal_u8(vld1_u16(w), in.val[1]));
#else
  for (int i = 0; i < 4; i++) {
    u[i] += p[4 * i] + p[4 * i + 2];
    w[i] += p[4 * i + 1] + p[4 * i + 3];
  }
#endif
}

/* Sixteen interleaved CbCr bytes: u[0..8) += Cb, v[0..8) += Cr. */
static inline void add_pairs(uint16_t *u, uint16_t *w, const uint8_t *p) {
#if defined(__ARM_NEON)
  uint8x8x2_t in = vld2_u8(p);
  vst1q_u16(u, vaddw_u8(vld1q_u16(u), in.val[0]));
  vst1q_u16(w, vaddw_u8(vld1q_u16(w), in.val[1]));
#else
  for (int i = 0; i < 8; i++) {
    u[i] += p[2 * i];
    w[i] += p[2 * i + 1];
  }
#endif
}

typedef struct {
  const AVFrame *f;
  int blocks; /* hardware layout: blocks across; 0 for planes */
} Source;

/* Add luma line `y` to acc_y (pairs of columns to one sum when `half`). The hardware layout
 * stores a line as 16-sample pieces of its blocks; a plane's line is read 16 at a time too (FFmpeg
 * pads its lines, and acc has room). */
static void add_luma_line(const RbVideo *v, const Source *s, int y, int w) {
  int chunks = (w + 15) / 16;
  const uint8_t *line;
  size_t pitch;
  if (s->blocks) {
    line = s->f->data[0] + (size_t)(y / 32 * s->blocks) * 512 + (size_t)(y % 32) * 16;
    pitch = 512;
  } else {
    line = s->f->data[0] + (size_t)y * (size_t)s->f->linesize[0];
    pitch = 16;
    /* A plane's last chunk may reach past the line's padding: finish it sample by sample. */
    if (w % 16) chunks--;
  }
  if (v->half)
    for (int chunk = 0; chunk < chunks; chunk++) add16_pairs(v->acc_y + chunk * 8, line + (size_t)chunk * pitch);
  else
    for (int chunk = 0; chunk < chunks; chunk++) add16(v->acc_y + chunk * 16, line + (size_t)chunk * pitch);
  for (int x = chunks * 16; x < w; x++) v->acc_y[v->half ? x / 2 : x] += line[x];
}

static void add_chroma_line(const RbVideo *v, const Source *s, int y, int cw) {
  if (s->blocks) {
    const uint8_t *band = s->f->data[1] + (size_t)(y / 16 * s->blocks) * 256 + (size_t)(y % 16) * 16;
    for (int chunk = 0; chunk * 8 < cw; chunk++) {
      if (v->half)
        add_pairs_halved(v->acc_u + chunk * 4, v->acc_v + chunk * 4, band + (size_t)chunk * 256);
      else
        add_pairs(v->acc_u + chunk * 8, v->acc_v + chunk * 8, band + (size_t)chunk * 256);
    }
    return;
  }
  const uint8_t *u = s->f->data[1] + (size_t)y * (size_t)s->f->linesize[1];
  const uint8_t *w = s->f->data[2] + (size_t)y * (size_t)s->f->linesize[2];
  int x = 0;
  for (; x + 16 <= cw; x += 16) {
    if (v->half) {
      add16_pairs(v->acc_u + x / 2, u + x);
      add16_pairs(v->acc_v + x / 2, w + x);
    } else {
      add16(v->acc_u + x, u + x);
      add16(v->acc_v + x, w + x);
    }
  }
  for (; x < cw; x++) {
    v->acc_u[v->half ? x / 2 : x] += u[x];
    v->acc_v[v->half ? x / 2 : x] += w[x];
  }
}

/* The sums of `rows` lines (of pairs when `half`) as one line of averages, `n` wide. */
static void average_line(const uint16_t *acc, int n, uint32_t samples, uint8_t *out) {
  uint32_t per = 65536 / samples;
  int x = 0;
#if defined(__ARM_NEON)
  if (samples > 1) {
    uint16x4_t k = vdup_n_u16((uint16_t)per);
    for (; x + 8 <= n; x += 8) {
      uint16x8_t a = vld1q_u16(acc + x);
      uint16x4_t lo = vrshrn_n_u32(vmull_u16(vget_low_u16(a), k), 16);
      uint16x4_t hi = vrshrn_n_u32(vmull_u16(vget_high_u16(a), k), 16);
      vst1_u8(out + x, vqmovn_u16(vcombine_u16(lo, hi)));
    }
  }
#endif
  for (; x < n; x++) {
    uint32_t value = samples > 1 ? (acc[x] * per + 32768) >> 16 : acc[x];
    out[x] = value > 255 ? 255 : (uint8_t)value;
  }
}

/* Output columns from an averaged line: each the mean of its two taps. */
static inline void pick(const uint8_t *line, const uint16_t *tap, uint8_t *out, int n, int stride) {
  for (int x = 0; x < n; x++) out[x * stride] = (uint8_t)((line[tap[2 * x]] + line[tap[2 * x + 1]] + 1) >> 1);
}

/* Colour in 1/64ths: BT.709 for high definition, BT.601 below, studio range unless full. */
typedef struct {
  int y_off, ky, rv, gu, gv, bu;
} Colour;

static inline uint8_t clip(int value) { return value < 0 ? 0 : value > 255 ? 255 : (uint8_t)value; }

/* One output row: `y` luma samples and half as many CbCr pairs to RGBA. `n` is even. */
static void row_to_rgba(const uint8_t *y, const uint8_t *chroma, int n, const Colour *c, uint8_t *out) {
  int x = 0;
#if defined(__ARM_NEON)
  uint8x8_t y_off = vdup_n_u8((uint8_t)c->y_off);
  int16x8_t ky = vdupq_n_s16((int16_t)c->ky), rv = vdupq_n_s16((int16_t)c->rv);
  int16x8_t gu = vdupq_n_s16((int16_t)c->gu), gv = vdupq_n_s16((int16_t)c->gv);
  int16x8_t bu = vdupq_n_s16((int16_t)c->bu), mid = vdupq_n_s16(128);
  for (; x + 8 <= n; x += 8) {
    int16x8_t luma = vmulq_s16(vreinterpretq_s16_u16(vmovl_u8(vqsub_u8(vld1_u8(y + x), y_off))), ky);
    /* Four CbCr pairs, each for two pixels. */
    uint8x8_t pairs = vld1_u8(chroma + x);
    uint8x8x2_t each = vuzp_u8(pairs, pairs);
    uint8x8_t u8 = vzip_u8(each.val[0], each.val[0]).val[0];
    uint8x8_t v8 = vzip_u8(each.val[1], each.val[1]).val[0];
    int16x8_t u = vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(u8)), mid);
    int16x8_t w = vsubq_s16(vreinterpretq_s16_u16(vmovl_u8(v8)), mid);
    uint8x8x4_t px;
    px.val[0] = vqrshrun_n_s16(vqaddq_s16(luma, vmulq_s16(w, rv)), 6);
    px.val[1] = vqrshrun_n_s16(vqsubq_s16(luma, vaddq_s16(vmulq_s16(u, gu), vmulq_s16(w, gv))), 6);
    px.val[2] = vqrshrun_n_s16(vqaddq_s16(luma, vmulq_s16(u, bu)), 6);
    px.val[3] = vdup_n_u8(255);
    vst4_u8(out + 4 * x, px);
  }
#endif
  for (; x < n; x++) {
    int luma = (y[x] > c->y_off ? y[x] - c->y_off : 0) * c->ky;
    int u = chroma[x & ~1] - 128, w = chroma[(x & ~1) + 1] - 128;
    int r = luma + w * c->rv, g = luma - (u * c->gu + w * c->gv), b = luma + u * c->bu;
    /* As the NEON version: saturate at 16 bits, round, shift. */
    r = r > 32767 ? 32767 : r;
    b = b > 32767 ? 32767 : b;
    out[4 * x] = clip((r + 32) >> 6);
    out[4 * x + 1] = clip((g + 32) >> 6);
    out[4 * x + 2] = clip((b + 32) >> 6);
    out[4 * x + 3] = 255;
  }
}

static int convert_420(RbVideo *v, const AVFrame *f, uint8_t *rgba) {
  Source s = {f, f->format == AV_PIX_FMT_DRM_PRIME ? f->linesize[0] / 16 : 0};
  int w = v->c->width, h = v->c->height, cw = (w + 1) / 2, ch = (h + 1) / 2;
  int ow = v->out_w, oh = v->out_h, ocw = ow / 2, och = oh / 2, step = v->step;
  if (f->width < w || f->height < h) return AVERROR(EINVAL);
  int aw = v->half ? (w + 1) / 2 : w, acw = v->half ? (cw + 1) / 2 : cw;
  /* Chroma first: averaged to half the output size. */
  for (int oy = 0; oy < och; oy++) {
    int y0 = (int)((int64_t)oy * ch / och), y1 = (int)((int64_t)(oy + 1) * ch / och), rows = 0;
    if (y1 <= y0) y1 = y0 + 1;
    memset(v->acc_u, 0, (size_t)(acw + 16) * sizeof(*v->acc_u));
    memset(v->acc_v, 0, (size_t)(acw + 16) * sizeof(*v->acc_v));
    for (int y = y0; y < y1; y += step, rows++) add_chroma_line(v, &s, y, cw);
    uint8_t *line = v->chroma + (size_t)oy * (size_t)ocw * 2;
    uint32_t samples = (uint32_t)rows << v->half;
    average_line(v->acc_u, acw, samples, v->wide);
    pick(v->wide, v->tap_c, line, ocw, 2);
    average_line(v->acc_v, acw, samples, v->wide);
    pick(v->wide, v->tap_c, line + 1, ocw, 2);
  }
  int hd = f->colorspace == AVCOL_SPC_BT709 || (f->colorspace == AVCOL_SPC_UNSPECIFIED && h > 576);
  int full = f->color_range == AVCOL_RANGE_JPEG || f->format == AV_PIX_FMT_YUVJ420P;
  Colour colour = {full ? 0 : 16, full ? 64 : 75, hd ? (full ? 101 : 115) : (full ? 90 : 102),
                   hd ? (full ? 12 : 14) : (full ? 22 : 25), hd ? (full ? 30 : 34) : (full ? 46 : 52),
                   hd ? (full ? 119 : 135) : (full ? 113 : 129)};
  for (int oy = 0; oy < oh; oy++) {
    int y0 = (int)((int64_t)oy * h / oh), y1 = (int)((int64_t)(oy + 1) * h / oh), rows = 0;
    if (y1 <= y0) y1 = y0 + 1;
    memset(v->acc_y, 0, (size_t)(aw + 16) * sizeof(*v->acc_y));
    for (int y = y0; y < y1; y += step, rows++) add_luma_line(v, &s, y, w);
    average_line(v->acc_y, aw, (uint32_t)rows << v->half, v->wide);
    pick(v->wide, v->tap_y, v->line_y, ow, 1);
    const uint8_t *chroma = v->chroma + (size_t)(oy / 2 < och ? oy / 2 : och - 1) * (size_t)ocw * 2;
    row_to_rgba(v->line_y, chroma, ow, &colour, rgba + (size_t)oy * (size_t)ow * 4);
  }
  return 0;
}

/* Write the held picture as out_width x out_height RGBA. */
int rb_video_convert(RbVideo *v, uint8_t *rgba) {
  const AVFrame *f = v->frame;
  if (!v->held) return AVERROR(EINVAL);
  if (f->format == AV_PIX_FMT_DRM_PRIME || f->format == AV_PIX_FMT_YUV420P ||
      f->format == AV_PIX_FMT_YUVJ420P)
    return convert_420(v, f, rgba);
  v->sws = sws_getCachedContext(v->sws, f->width, f->height, f->format, v->out_w, v->out_h,
                                AV_PIX_FMT_RGBA, SWS_BILINEAR, NULL, NULL, NULL);
  if (!v->sws) return AVERROR(ENOSYS);
  uint8_t *planes[4] = {rgba, NULL, NULL, NULL};
  int strides[4] = {v->out_w * 4, 0, 0, 0};
  sws_scale(v->sws, (const uint8_t *const *)f->data, f->linesize, 0, f->height, planes, strides);
  return 0;
}

/* A picture of the film for lists and covers: `side` x `side` RGBA, the picture centred on black,
 * taken a tenth into the film (past logos and black leaders). Software decoding only. */
int rb_video_thumbnail(const char *path, uint8_t *rgba, int side) {
  RbVideo *v = NULL;
  RbVideoInfo info;
  int64_t pts;
  uint32_t hardware;
  int ret = rb_video_open(path, side, side, 0, &v, &info);
  if (ret < 0) return ret;
  if (info.duration_ms > 20000) rb_video_seek(v, (int64_t)(info.duration_ms / 10));
  ret = rb_video_next(v, &pts, &hardware);
  if (ret == 0) ret = AVERROR_EOF;
  uint8_t *picture = ret > 0 ? malloc((size_t)v->out_w * (size_t)v->out_h * 4) : NULL;
  if (ret > 0 && !picture) ret = AVERROR(ENOMEM);
  if (ret > 0) ret = rb_video_convert(v, picture);
  if (ret >= 0) {
    int left = (side - v->out_w) / 2, top = (side - v->out_h) / 2;
    for (size_t i = 0; i < (size_t)side * (size_t)side; i++) {
      rgba[4 * i] = rgba[4 * i + 1] = rgba[4 * i + 2] = 0;
      rgba[4 * i + 3] = 255;
    }
    for (int y = 0; y < v->out_h; y++)
      memcpy(rgba + ((size_t)(top + y) * (size_t)side + (size_t)left) * 4,
             picture + (size_t)y * (size_t)v->out_w * 4, (size_t)v->out_w * 4);
    ret = 0;
  }
  free(picture);
  rb_video_close(v);
  return ret;
}
