/* SPDX-License-Identifier: MIT — single-owner ALSA output membrane. */
#define _POSIX_C_SOURCE 200809L
#include <alloca.h>
#include <alsa/asoundlib.h>
#include <fcntl.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <unistd.h>

static void (*log_callback)(const char *);
static void alsa_log(const char *file, int line, const char *function,
                     int error, const char *format, ...) {
  (void)file;
  (void)line;
  (void)function;
  (void)error;
  if (!log_callback)
    return;
  char buffer[1024];
  va_list args;
  va_start(args, format);
  vsnprintf(buffer, sizeof(buffer), format, args);
  va_end(args);
  log_callback(buffer);
}
void rb_alsa_logging(void (*callback)(const char *)) {
  log_callback = callback;
  snd_lib_error_set_handler(alsa_log);
}

typedef struct {
  uint32_t rate, period, buffer, format, channels;
  int fallback;
  int hardware_mixer_gain_cdb;
  char device[128];
  char fallback_reason[256];
  int hp_level_live; /* wired: level the stream got (0 Low..2 High), -1 unknown */
  int level_raised;  /* wired: higher than the previous wired stream's level */
} RbParams;
/* Pinned with RawParams in src/native.rs. */
_Static_assert(sizeof(RbParams) == 420 && offsetof(RbParams, device) == 28 &&
                   offsetof(RbParams, hp_level_live) == 412 &&
                   offsetof(RbParams, level_raised) == 416,
               "RbParams layout differs from RawParams");
typedef struct {
  snd_pcm_t *pcm;
  int lease;
  snd_mixer_t *mixer;
  long left, right;
  int sw;
  int restore;
  /* The output pin the stream switched on: "Headphone" or "Speaker". */
  const char *pin;
} RbSink;
const char *rb_alsa_error(int e) { return snd_strerror(e); }

/* Y2Linux wired output controls: the headphone level ("HP Output Level",
 * full scale 1.0/1.41/1.73 Vrms) and the DAC filter. The kernel writes the
 * level only while the amplifier is down (DS1155F2), so it is set before a
 * stream starts and "HP Output Level Live" says what the stream got. Each
 * level has its own Master (Reborn's table, rb_wired_masters); it is set for
 * the level the amplifier really runs at. Until that is known, and on kernels
 * without the control, Master is -24 dB (quiet at any full scale). */
#define RB_MASTER_CDB_LEGACY -2400
/* #28 speaker: the stock speaker volume's top step (cs43131_refresh_volume_for_
 * route, (15 - 15) * 4 + 6 half-dB = -3 dB) at the speaker's full scale. */
#define RB_MASTER_CDB_SPEAKER -300
static int wired_level, wired_slow, wired_phase, wired_nos;
static int wired_masters[3] = {RB_MASTER_CDB_LEGACY, RB_MASTER_CDB_LEGACY,
                               RB_MASTER_CDB_LEGACY};
static int wired_live = -1; /* level of the previous wired stream */
/* A higher level was chosen (not the startup setting): the next wired stream
 * fades in even when none has played since boot. */
static int wired_configured, wired_raise_pending;
static int ctl_id(snd_ctl_t *ctl, const char *name, snd_ctl_elem_id_t *id,
                  snd_ctl_elem_info_t *info) {
  snd_ctl_elem_id_set_interface(id, SND_CTL_ELEM_IFACE_MIXER);
  snd_ctl_elem_id_set_name(id, name);
  snd_ctl_elem_info_set_id(info, id);
  return snd_ctl_elem_info(ctl, info);
}
static int ctl_set(snd_ctl_t *ctl, const char *name, unsigned value) {
  snd_ctl_elem_id_t *id;
  snd_ctl_elem_info_t *info;
  snd_ctl_elem_value_t *v;
  snd_ctl_elem_id_alloca(&id);
  snd_ctl_elem_info_alloca(&info);
  snd_ctl_elem_value_alloca(&v);
  int r = ctl_id(ctl, name, id, info);
  if (r < 0)
    return r;
  snd_ctl_elem_value_set_id(v, id);
  switch (snd_ctl_elem_info_get_type(info)) {
  case SND_CTL_ELEM_TYPE_ENUMERATED:
    if (value >= snd_ctl_elem_info_get_items(info))
      return -EINVAL;
    snd_ctl_elem_value_set_enumerated(v, 0, value);
    break;
  case SND_CTL_ELEM_TYPE_BOOLEAN:
    for (unsigned i = 0; i < snd_ctl_elem_info_get_count(info); i++)
      snd_ctl_elem_value_set_boolean(v, i, value != 0);
    break;
  default:
    return -EINVAL;
  }
  return snd_ctl_elem_write(ctl, v);
}
static int ctl_enum(snd_ctl_t *ctl, const char *name) {
  snd_ctl_elem_id_t *id;
  snd_ctl_elem_info_t *info;
  snd_ctl_elem_value_t *v;
  snd_ctl_elem_id_alloca(&id);
  snd_ctl_elem_info_alloca(&info);
  snd_ctl_elem_value_alloca(&v);
  int r = ctl_id(ctl, name, id, info);
  if (r < 0)
    return r;
  if (snd_ctl_elem_info_get_type(info) != SND_CTL_ELEM_TYPE_ENUMERATED)
    return -EINVAL;
  snd_ctl_elem_value_set_id(v, id);
  if ((r = snd_ctl_elem_read(ctl, v)) < 0)
    return r;
  return (int)snd_ctl_elem_value_get_enumerated(v, 0);
}
/* 1: the level control exists and took the level; 0: an older kernel. The
 * filter controls are upstream ones, set where they exist. */
static int wired_apply(snd_ctl_t *ctl) {
  int r = ctl_set(ctl, "HP Output Level",
                  (unsigned)__atomic_load_n(&wired_level, __ATOMIC_RELAXED));
  (void)ctl_set(ctl, "PCM Filter Speed",
                (unsigned)__atomic_load_n(&wired_slow, __ATOMIC_RELAXED));
  (void)ctl_set(ctl, "PCM Phase Compensation",
                (unsigned)__atomic_load_n(&wired_phase, __ATOMIC_RELAXED));
  (void)ctl_set(ctl, "PCM Nonoversample Emulate",
                (unsigned)__atomic_load_n(&wired_nos, __ATOMIC_RELAXED));
  if (r == -ENOENT)
    return 0;
  return r < 0 ? r : 1;
}
/* The Master (centi-dB, -2400..0) each level plays with. */
int rb_wired_masters(int low, int medium, int high) {
  int masters[3] = {low, medium, high};
  for (int i = 0; i < 3; i++)
    if (masters[i] < RB_MASTER_CDB_LEGACY || masters[i] > 0)
      return -EINVAL;
  for (int i = 0; i < 3; i++)
    __atomic_store_n(&wired_masters[i], masters[i], __ATOMIC_RELAXED);
  return 0;
}
/* Remember the wired settings and apply them now; a level chosen during
 * playback reaches the output when its next stream starts. */
int rb_wired_configure(int level, int slow, int phase, int nos) {
  if (level < 0 || level > 2)
    return -EINVAL;
  int previous = __atomic_exchange_n(&wired_level, level, __ATOMIC_RELAXED);
  if (__atomic_exchange_n(&wired_configured, 1, __ATOMIC_RELAXED) && level > previous)
    __atomic_store_n(&wired_raise_pending, 1, __ATOMIC_RELAXED);
  __atomic_store_n(&wired_slow, slow != 0, __ATOMIC_RELAXED);
  __atomic_store_n(&wired_phase, phase != 0, __ATOMIC_RELAXED);
  __atomic_store_n(&wired_nos, nos != 0, __ATOMIC_RELAXED);
  snd_ctl_t *ctl = NULL;
  int r = snd_ctl_open(&ctl, "hw:Y2Audio", 0);
  if (r < 0)
    return r;
  r = wired_apply(ctl);
  snd_ctl_close(ctl);
  return r;
}
/* A wired stream started at `live`: 1 when it is louder than the previous
 * wired stream, or a higher level was chosen since (playback fades in). */
int rb_wired_note_live(int live) {
  int chosen = __atomic_exchange_n(&wired_raise_pending, 0, __ATOMIC_RELAXED);
  int raised = (wired_live >= 0 && live > wired_live) || (chosen && live > 0);
  wired_live = live;
  return raised;
}
/* The Master level a wired stream at the chosen level uses on this kernel. */
static int wired_master_cdb(void) {
  snd_ctl_t *ctl = NULL;
  if (snd_ctl_open(&ctl, "hw:Y2Audio", 0) < 0)
    return RB_MASTER_CDB_LEGACY;
  snd_ctl_elem_id_t *id;
  snd_ctl_elem_info_t *info;
  snd_ctl_elem_id_alloca(&id);
  snd_ctl_elem_info_alloca(&info);
  int present = ctl_id(ctl, "HP Output Level", id, info) >= 0;
  snd_ctl_close(ctl);
  return present ? __atomic_load_n(&wired_masters[__atomic_load_n(&wired_level, __ATOMIC_RELAXED)],
                                    __ATOMIC_RELAXED)
                 : RB_MASTER_CDB_LEGACY;
}

int rb_wired_device(char *out, int n) {
  int card = -1, r;
  while ((r = snd_card_next(&card)) >= 0 && card >= 0) {
    char name[32];
    snprintf(name, sizeof(name), "hw:%d", card);
    snd_ctl_t *ctl = NULL;
    if (snd_ctl_open(&ctl, name, 0) < 0)
      continue;
    snd_ctl_card_info_t *info;
    snd_ctl_card_info_alloca(&info);
    int match = snd_ctl_card_info(ctl, info) >= 0 &&
                !strcmp(snd_ctl_card_info_get_id(info), "Y2Audio");
    if (match)
      snprintf(out, (size_t)n, "hw:CARD=Y2Audio,DEV=0");
    snd_ctl_close(ctl);
    if (match)
      return 0;
  }
  return r < 0 ? r : -ENODEV;
}

/* True when the JSON array after `section` holds `token` as a whole element,
 * quoted ("S16_LE") or bare (44100): 8000 must not match inside 48000. */
static int profile_section_has(const char *json, const char *section,
                               const char *token) {
  const char *start = strstr(json, section);
  if (!start)
    return 0;
  start = strchr(start + strlen(section), '[');
  if (!start)
    return 0;
  const char *end = strchr(start, ']');
  if (!end)
    return 0;
  size_t length = strlen(token);
  for (const char *p = start + 1; p < end;) {
    while (p < end && (*p == ' ' || *p == ',' || *p == '"' || *p == '\n' ||
                       *p == '\t' || *p == '\r'))
      p++;
    const char *item = p;
    while (p < end && *p != ',' && *p != '"' && *p != ' ' && *p != '\n' &&
           *p != '\t' && *p != '\r')
      p++;
    if ((size_t)(p - item) == length && !strncmp(item, token, length))
      return 1;
  }
  return 0;
}
static int wired_profile_allows(int format, unsigned rate) {
  FILE *file = fopen("/etc/y2linux/audio-enabled.json", "rb");
  int enabled_profile = file != NULL;
  if (!file) file = fopen("/etc/y2linux/audio-qualified.json", "rb");
  if (!file)
    return 0;
  char json[8192];
  size_t n = fread(json, 1, sizeof(json) - 1, file);
  fclose(file);
  json[n] = 0;
  const char *format_name;
  switch (format) {
  case 1:
    format_name = "S16_LE";
    break;
  case 2:
    format_name = "S32_LE";
    break;
  case 3:
    format_name = "S24_LE";
    break;
  default:
    return 0;
  }
  char rate_name[16];
  snprintf(rate_name, sizeof(rate_name), "%u", rate);
  return profile_section_has(json, enabled_profile ? "\"enabled_formats\"" : "\"qualified_formats\"", format_name) &&
         profile_section_has(json, enabled_profile ? "\"enabled_rates\"" : "\"qualified_rates\"", rate_name);
}
static snd_pcm_format_t pcm_format(unsigned format) {
  switch (format) {
  case 1:
    return SND_PCM_FORMAT_S16_LE;
  case 2:
    return SND_PCM_FORMAT_S32_LE;
  case 3:
    return SND_PCM_FORMAT_S24_LE;
  default:
    return SND_PCM_FORMAT_UNKNOWN;
  }
}
int rb_alsa_format_info(unsigned format, int *valid_bits, int *physical_bits) {
  if (!valid_bits || !physical_bits)
    return -EINVAL;
  snd_pcm_format_t selected = pcm_format(format);
  if (selected == SND_PCM_FORMAT_UNKNOWN)
    return -EINVAL;
  int valid = snd_pcm_format_width(selected);
  int physical = snd_pcm_format_physical_width(selected);
  if (valid <= 0 || physical <= 0)
    return -EINVAL;
  *valid_bits = valid;
  *physical_bits = physical;
  return 0;
}
static int test_candidate(snd_pcm_t *pcm, unsigned rate, unsigned format) {
  snd_pcm_hw_params_t *p;
  snd_pcm_hw_params_alloca(&p);
  int r = snd_pcm_hw_params_any(pcm, p);
  if (r < 0)
    return r;
  if ((r = snd_pcm_hw_params_set_access(pcm, p,
                                         SND_PCM_ACCESS_RW_INTERLEAVED)) < 0)
    return r;
  if ((r = snd_pcm_hw_params_set_format(pcm, p, pcm_format(format))) < 0)
    return r;
  if ((r = snd_pcm_hw_params_set_channels(pcm, p, 2)) < 0)
    return r;
  return snd_pcm_hw_params_test_rate(pcm, p, rate, 0);
}
int rb_alsa_plan(const char *name, unsigned requested_rate,
                 unsigned preferred_format, int wired, int strict_format,
                 RbParams *params) {
  if (!name || !params || requested_rate < 8000 || requested_rate > 384000 ||
      (preferred_format != 1 && preferred_format != 2 && preferred_format != 3))
    return -EINVAL;
  memset(params, 0, sizeof(*params));
  snprintf(params->device, sizeof(params->device), "%s", name);
  snd_pcm_t *pcm = NULL;
  int r = snd_pcm_open(&pcm, name, SND_PCM_STREAM_PLAYBACK, SND_PCM_NONBLOCK);
  if (r < 0)
    return r;
  /* Each candidate must satisfy access/format/channels/rate together.
   * A 24-in-32 sink is useful when S32 is rejected, and must precede S16.
   * Observed Bluetooth PCM is strict: no substitute format is allowed. */
  unsigned candidates[3] = {preferred_format, 0, 0};
  unsigned count = 1;
  if (!strict_format) {
    const unsigned alternatives[] = {2, 3, 1};
    for (unsigned i = 0; i < 3; i++)
      if (alternatives[i] != preferred_format)
        candidates[count++] = alternatives[i];
  }
  unsigned selected = 0;
  int last = -EINVAL;
  for (unsigned i = 0; i < count; i++) {
    unsigned format = candidates[i];
    if (!format)
      continue;
    if (wired && !wired_profile_allows((int)format, requested_rate)) {
      last = -EOPNOTSUPP;
      continue;
    }
    last = test_candidate(pcm, requested_rate, format);
    if (last >= 0) {
      selected = format;
      break;
    }
  }
  snd_pcm_close(pcm);
  if (!selected)
    return last;
  params->rate = requested_rate;
  params->format = selected;
  params->channels = 2;
  params->period = 512;
  params->buffer = 4096;
  params->hardware_mixer_gain_cdb = wired ? wired_master_cdb() : INT32_MIN;
  params->hp_level_live = -1;
  params->fallback = selected != preferred_format;
  if (params->fallback)
    snprintf(params->fallback_reason, sizeof(params->fallback_reason),
             wired && preferred_format == 2 ? "preferred S32_LE is unavailable in the enabled hardware path"
                   : "preferred sink format unavailable; selected ALSA fallback");
  return 0;
}

static snd_mixer_elem_t *element(RbSink *s, const char *name) {
  for (snd_mixer_elem_t *e = snd_mixer_first_elem(s->mixer); e;
       e = snd_mixer_elem_next(e))
    if (!strcmp(snd_mixer_selem_get_name(e), name))
      return e;
  return NULL;
}
void rb_sink_close(RbSink *s) {
  if (!s)
    return;
  if (s->pcm) {
    snd_pcm_drop(s->pcm);
    snd_pcm_close(s->pcm);
  }
  if (s->mixer) {
    if (s->restore) {
      snd_mixer_elem_t *m = element(s, "Master"), *h = element(s, s->pin);
      if (m) {
        snd_mixer_selem_set_playback_volume(m, SND_MIXER_SCHN_FRONT_LEFT,
                                            s->left);
        snd_mixer_selem_set_playback_volume(m, SND_MIXER_SCHN_FRONT_RIGHT,
                                            s->right);
      }
      if (h)
        snd_mixer_selem_set_playback_switch_all(h, s->sw);
    }
    snd_mixer_close(s->mixer);
  }
  if (s->lease >= 0)
    close(s->lease);
  free(s);
}
/* wired: 0 Bluetooth, 1 the headphone jack, 2 the internal speaker (#28). */
int rb_sink_open(const char *name, unsigned rate, unsigned format, int wired,
                 RbSink **out, RbParams *params) {
  int r, speaker = wired == 2;
  if (!name || !out || !params)
    return -EINVAL;
  *out = NULL;
  memset(params, 0, sizeof(*params));
  if (strlen(name) >= sizeof(params->device))
    return -ENAMETOOLONG;
  if (format != 1 && format != 2 && format != 3)
    return -EINVAL;
  RbSink *s = calloc(1, sizeof(*s));
  if (!s)
    return -ENOMEM;
  s->lease = -1;
  s->pin = speaker ? "Speaker" : "Headphone";
  if (access("/run/y2", F_OK) == 0) {
    s->lease = open("/run/y2/activity.lock", O_CREAT | O_RDWR | O_CLOEXEC, 0600);
    if (s->lease < 0 || flock(s->lease, LOCK_SH | LOCK_NB)) {
      r = -EBUSY;
      goto fail;
    }
  }
  /* The level must be in place before the stream powers the amplifier. */
  int levels = 0, master = speaker ? RB_MASTER_CDB_SPEAKER : RB_MASTER_CDB_LEGACY;
  params->hp_level_live = -1;
  if (wired) {
    snd_ctl_t *ctl = NULL;
    if ((r = snd_ctl_open(&ctl, "hw:Y2Audio", 0)) < 0)
      goto fail;
    r = wired_apply(ctl);
    snd_ctl_close(ctl);
    if (r < 0)
      goto fail;
    levels = r;
  }
  if ((r = snd_pcm_open(&s->pcm, name, SND_PCM_STREAM_PLAYBACK,
                        SND_PCM_NONBLOCK)) < 0)
    goto fail;
  snd_pcm_hw_params_t *p;
  snd_pcm_hw_params_alloca(&p);
  unsigned actual = rate;
  int dir = 0;
  snd_pcm_uframes_t period = 512, buffer = 4096;
  if ((r = snd_pcm_hw_params_any(s->pcm, p)) < 0)
    goto fail;
  if ((r = snd_pcm_hw_params_set_access(s->pcm, p,
                                        SND_PCM_ACCESS_RW_INTERLEAVED)) < 0)
    goto fail;
  if ((r = snd_pcm_hw_params_set_format(s->pcm, p, pcm_format(format))) < 0)
    goto fail;
  if ((r = snd_pcm_hw_params_set_channels(s->pcm, p, 2)) < 0)
    goto fail;
  if ((r = snd_pcm_hw_params_set_rate_near(s->pcm, p, &actual, &dir)) < 0)
    goto fail;
  if (actual != rate) {
    r = -EINVAL;
    goto fail;
  }
  if ((r = snd_pcm_hw_params_set_period_size_near(s->pcm, p, &period, &dir)) < 0)
    goto fail;
  if ((r = snd_pcm_hw_params_set_buffer_size_near(s->pcm, p, &buffer)) < 0)
    goto fail;
  if ((r = snd_pcm_hw_params(s->pcm, p)) < 0)
    goto fail;
  snd_pcm_sw_params_t *sw;
  snd_pcm_sw_params_alloca(&sw);
  if ((r = snd_pcm_sw_params_current(s->pcm, sw)) < 0)
    goto fail;
  snd_pcm_sw_params_set_start_threshold(s->pcm, sw, period * 2);
  snd_pcm_sw_params_set_avail_min(s->pcm, sw, period);
  if ((r = snd_pcm_sw_params(s->pcm, sw)) < 0)
    goto fail;
  if ((r = snd_pcm_prepare(s->pcm)) < 0)
    goto fail;
  if (wired) {
    if ((r = snd_mixer_open(&s->mixer, 0)) < 0)
      goto fail;
    if ((r = snd_mixer_attach(s->mixer, "hw:Y2Audio")) < 0)
      goto fail;
    if ((r = snd_mixer_selem_register(s->mixer, NULL, NULL)) < 0)
      goto fail;
    if ((r = snd_mixer_load(s->mixer)) < 0)
      goto fail;
    snd_mixer_elem_t *m = element(s, "Master"), *h = element(s, s->pin);
    if (!m || !h) {
      r = -ENOENT;
      goto fail;
    }
    if ((r = snd_mixer_selem_get_playback_volume(m, SND_MIXER_SCHN_FRONT_LEFT,
                                                 &s->left)) < 0)
      goto fail;
    if ((r = snd_mixer_selem_get_playback_volume(m, SND_MIXER_SCHN_FRONT_RIGHT,
                                                 &s->right)) < 0)
      goto fail;
    if ((r = snd_mixer_selem_get_playback_switch(h, SND_MIXER_SCHN_FRONT_LEFT,
                                                 &s->sw)) < 0)
      goto fail;
    s->restore = 1;
    if ((r = snd_mixer_selem_set_playback_dB_all(m, master, 0)) < 0)
      goto fail;
    if ((r = snd_mixer_selem_set_playback_switch_all(h, 1)) < 0)
      goto fail;
    /* The pin completes the path: the amplifier is up now, at this level. */
    snd_ctl_t *ctl = NULL;
    /* The speaker runs at its own full scale and Master, not a level's. */
    if (levels && !speaker && snd_ctl_open(&ctl, "hw:Y2Audio", 0) >= 0) {
      int live = ctl_enum(ctl, "HP Output Level Live");
      snd_ctl_close(ctl);
      if (live >= 0 && live <= 2) {
        params->hp_level_live = live;
        params->level_raised = rb_wired_note_live(live);
        /* Nothing is written yet: the level's own Master before any sound. */
        int own = __atomic_load_n(&wired_masters[live], __ATOMIC_RELAXED);
        if (snd_mixer_selem_set_playback_dB_all(m, own, 0) >= 0)
          master = own;
      }
    }
  }
  params->rate = actual;
  params->period = period;
  params->buffer = buffer;
  params->format = format;
  params->channels = 2;
  params->hardware_mixer_gain_cdb = wired ? master : INT32_MIN;
  snprintf(params->device, sizeof(params->device), "%s", name);
  *out = s;
  return 0;
fail:
  rb_sink_close(s);
  return r;
}
int rb_sink_write(RbSink *s, const uint8_t *p, int frames) {
  return (int)snd_pcm_writei(s->pcm, p, frames);
}
int rb_sink_wait(RbSink *s, int ms) { return snd_pcm_wait(s->pcm, ms); }
int rb_sink_recover(RbSink *s, int code) { return snd_pcm_recover(s->pcm, code, 1); }
int rb_sink_drop(RbSink *s) {
  int r = snd_pcm_drop(s->pcm);
  return r < 0 ? r : snd_pcm_prepare(s->pcm);
}
/* Pause in place (Pause on Reborn keeps the output open). With
 * `device_pause` the device's own pause where it has one (most codecs);
 * otherwise, and for BlueALSA (whose pause left the headphones silent after
 * resume, Beta 6), the buffered few milliseconds play out (drain, as at the
 * end of a song) and resume prepares the stream again, as a start does.
 * Returns 1 when the device paused itself, 0 otherwise, negative on error. */
int rb_sink_pause(RbSink *s, int on, int device_pause) {
  snd_pcm_state_t state = snd_pcm_state(s->pcm);
  if (on) {
    if (device_pause && state == SND_PCM_STATE_RUNNING && snd_pcm_pause(s->pcm, 1) == 0)
      return 1;
    if (state == SND_PCM_STATE_PAUSED)
      return 1;
    if (state != SND_PCM_STATE_RUNNING)
      return 0;
    /* Non-blocking: -EAGAIN means the drain continues by itself. */
    int r = snd_pcm_drain(s->pcm);
    /* BlueALSA 5.0 gives up waiting after ~100 ms plus the queued periods,
     * stops the stream and leaves it in SETUP with -EIO ("Drain timed out").
     * The stream is stopped as a pause wants, and resume prepares it again:
     * not a failure (it logged pause_failed on every Bluetooth pause). */
    if (r == -EIO && !device_pause && snd_pcm_state(s->pcm) == SND_PCM_STATE_SETUP)
      return 0;
    return r < 0 && r != -EAGAIN ? r : 0;
  }
  if (state == SND_PCM_STATE_PAUSED)
    return snd_pcm_pause(s->pcm, 0);
  if (state == SND_PCM_STATE_DRAINING) {
    int r = snd_pcm_drop(s->pcm);
    if (r < 0)
      return r;
    state = snd_pcm_state(s->pcm);
  }
  if (state == SND_PCM_STATE_SETUP || state == SND_PCM_STATE_XRUN)
    return snd_pcm_prepare(s->pcm);
  return 0;
}
int rb_sink_delay(RbSink *s) {
  snd_pcm_sframes_t n = 0;
  int r = snd_pcm_delay(s->pcm, &n);
  return r < 0 ? r : (int)n;
}
