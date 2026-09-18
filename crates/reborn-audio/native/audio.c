/* SPDX-License-Identifier: MIT — single-owner ALSA output membrane. */
#define _POSIX_C_SOURCE 200809L
#include <alloca.h>
#include <alsa/asoundlib.h>
#include <fcntl.h>
#include <stdarg.h>
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
} RbParams;
typedef struct {
  snd_pcm_t *pcm;
  int lease;
  snd_mixer_t *mixer;
  long left, right;
  int sw;
  int restore;
} RbSink;
const char *rb_alsa_error(int e) { return snd_strerror(e); }

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

static int profile_section_has(const char *json, const char *section,
                               const char *token) {
  const char *start = strstr(json, section);
  if (!start)
    return 0;
  const char *end = strchr(start, ']');
  if (!end)
    return 0;
  char quoted[32];
  snprintf(quoted, sizeof(quoted), "\"%s\"", token);
  const char *found = strstr(start, quoted);
  if (!found)
    found = strstr(start, token);
  return found && found < end;
}
static int wired_profile_allows(int format, unsigned rate) {
  FILE *file = fopen("/etc/y2linux/audio-qualified.json", "rb");
  if (!file)
    return 0;
  char json[8192];
  size_t n = fread(json, 1, sizeof(json) - 1, file);
  fclose(file);
  json[n] = 0;
  const char *format_name = format == 2 ? "S32_LE" : "S16_LE";
  char rate_name[16];
  snprintf(rate_name, sizeof(rate_name), "%u", rate);
  return profile_section_has(json, "\"qualified_formats\"", format_name) &&
         profile_section_has(json, "\"qualified_rates\"", rate_name);
}
static snd_pcm_format_t pcm_format(unsigned format) {
  return format == 2 ? SND_PCM_FORMAT_S32_LE : SND_PCM_FORMAT_S16_LE;
}
static int test_candidate(snd_pcm_t *pcm, unsigned rate, unsigned format) {
  snd_pcm_hw_params_t *p;
  snd_pcm_hw_params_alloca(&p);
  int r = snd_pcm_hw_params_any(pcm, p);
  if (r < 0)
    return r;
  if ((r = snd_pcm_hw_params_test_access(pcm, p,
                                         SND_PCM_ACCESS_RW_INTERLEAVED)) < 0)
    return r;
  if ((r = snd_pcm_hw_params_test_format(pcm, p, pcm_format(format))) < 0)
    return r;
  if ((r = snd_pcm_hw_params_test_channels(pcm, p, 2)) < 0)
    return r;
  return snd_pcm_hw_params_test_rate(pcm, p, rate, 0);
}
int rb_alsa_plan(const char *name, unsigned requested_rate,
                 unsigned preferred_format, int wired, RbParams *params) {
  if (!name || !params || requested_rate < 8000 || requested_rate > 384000)
    return -EINVAL;
  memset(params, 0, sizeof(*params));
  snprintf(params->device, sizeof(params->device), "%s", name);
  snd_pcm_t *pcm = NULL;
  int r = snd_pcm_open(&pcm, name, SND_PCM_STREAM_PLAYBACK, SND_PCM_NONBLOCK);
  if (r < 0)
    return r;
  unsigned candidates[2] = {preferred_format == 1 ? 1 : 2, 1};
  if (candidates[0] == candidates[1])
    candidates[1] = 0;
  unsigned selected = 0;
  int last = -EINVAL;
  for (unsigned i = 0; i < 2; i++) {
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
  params->hardware_mixer_gain_cdb = wired ? -2400 : INT32_MIN;
  params->fallback = selected != (preferred_format == 1 ? 1u : 2u);
  if (params->fallback)
    snprintf(params->fallback_reason, sizeof(params->fallback_reason),
             wired ? "preferred S32_LE is not physically qualified for this rate"
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
      snd_mixer_elem_t *m = element(s, "Master"), *h = element(s, "Headphone");
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
int rb_sink_open(const char *name, unsigned rate, unsigned format, int wired,
                 RbSink **out, RbParams *params) {
  int r;
  *out = NULL;
  RbSink *s = calloc(1, sizeof(*s));
  if (!s)
    return -ENOMEM;
  s->lease = -1;
  if (access("/run/y2", F_OK) == 0) {
    s->lease = open("/run/y2/activity.lock", O_CREAT | O_RDWR | O_CLOEXEC, 0600);
    if (s->lease < 0 || flock(s->lease, LOCK_SH | LOCK_NB)) {
      r = -EBUSY;
      goto fail;
    }
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
    snd_mixer_elem_t *m = element(s, "Master"), *h = element(s, "Headphone");
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
    if ((r = snd_mixer_selem_set_playback_dB_all(m, -2400, 0)) < 0)
      goto fail;
    if ((r = snd_mixer_selem_set_playback_switch_all(h, 1)) < 0)
      goto fail;
  }
  params->rate = actual;
  params->period = period;
  params->buffer = buffer;
  params->format = format;
  params->channels = 2;
  params->hardware_mixer_gain_cdb = wired ? -2400 : INT32_MIN;
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
int rb_sink_delay(RbSink *s) {
  snd_pcm_sframes_t n = 0;
  int r = snd_pcm_delay(s->pcm, &n);
  return r < 0 ? r : (int)n;
}
