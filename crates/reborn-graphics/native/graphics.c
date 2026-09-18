/* SPDX-License-Identifier: MIT
 * GBM/KMS lifetime handling adapted from Y2Linux tools/graphics/gpu-check.c.
 * Rendering and retained handles remain confined to the UI thread. */
#define _POSIX_C_SOURCE 200809L
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <drm_fourcc.h>
#include <errno.h>
#include <fcntl.h>
#include <gbm.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <xf86drm.h>
#include <xf86drmMode.h>
static double now(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec + t.tv_nsec / 1e9;
}
#include "splash-handoff.h"
struct fb {
  uint32_t id;
  int fd;
};
static void destroy_fb(struct gbm_bo *bo, void *data) {
  (void)bo;
  struct fb *fb = data;
  drmModeRmFB(fb->fd, fb->id);
  free(fb);
}
static struct fb *framebuffer(int fd, struct gbm_bo *bo) {
  struct fb *fb = gbm_bo_get_user_data(bo);
  if (fb)
    return fb;
  if (gbm_bo_get_modifier(bo) != DRM_FORMAT_MOD_LINEAR &&
      gbm_bo_get_modifier(bo) != DRM_FORMAT_MOD_INVALID) {
    return NULL;
  }
  fb = calloc(1, sizeof(*fb));
  if (!fb)
    return NULL;
  fb->fd = fd;
  uint32_t handles[4] = {gbm_bo_get_handle(bo).u32};
  uint32_t pitches[4] = {gbm_bo_get_stride(bo)}, offsets[4] = {0};
  if (drmModeAddFB2(fd, gbm_bo_get_width(bo), gbm_bo_get_height(bo),
                    gbm_bo_get_format(bo), handles, pitches, offsets, &fb->id,
                    0)) {
    free(fb);
    return NULL;
  }
  gbm_bo_set_user_data(bo, fb, destroy_fb);
  return fb;
}
struct flip {
  bool waiting;
  unsigned count, sequence;
  double timestamp;
};
static void flipped(int fd, unsigned seq, unsigned sec, unsigned usec,
                    void *data) {
  (void)fd;
  struct flip *f = data;
  f->waiting = false;
  f->count++;
  f->sequence = seq;
  f->timestamp = sec + usec / 1e6;
}
static int wait_flip(int fd, struct flip *f) {
  double deadline = now() + 3;
  drmEventContext events = {.version = DRM_EVENT_CONTEXT_VERSION,
                            .page_flip_handler = flipped};
  while (f->waiting) {
    struct pollfd pollfd = {.fd = fd, .events = POLLIN};
    int ms = (int)((deadline - now()) * 1000);
    if (ms <= 0) {
      return -1;
    }
    int ret = poll(&pollfd, 1, ms);
    if (ret < 0 && errno == EINTR)
      continue;
    if (ret <= 0 || !(pollfd.revents & POLLIN)) {
      return -1;
    }
    if (drmHandleEvent(fd, &events))
      return -1;
  }
  return 0;
}
static _Thread_local char last_error[256];
const char *rb_graphics_error(void) { return last_error; }
typedef struct {
  int fd, width, height, modeset;
  uint32_t crtc, connector;
  drmModeCrtc *old;
  drmModeModeInfo mode;
  struct gbm_device *gbm;
  struct gbm_surface *surface;
  struct gbm_bo *current;
  EGLDisplay display;
  EGLSurface window;
  EGLContext context;
  GLuint program, vbo, white, font, art;
  struct flip flip;
  char info[1024];
} RbGraphics;
void rb_graphics_close(RbGraphics *g) {
  if (!g)
    return;
  if (g->modeset && g->old) {
    int r = g->old->mode_valid
                ? drmModeSetCrtc(g->fd, g->old->crtc_id, g->old->buffer_id,
                                 g->old->x, g->old->y, &g->connector, 1,
                                 &g->old->mode)
                : drmModeSetCrtc(g->fd, g->crtc, 0, 0, 0, NULL, 0, NULL);
    if (r)
      drmModeSetCrtc(g->fd, g->crtc, 0, 0, 0, NULL, 0, NULL);
  }
  if (g->context != EGL_NO_CONTEXT) {
    if (g->program)
      glDeleteProgram(g->program);
    if (g->vbo)
      glDeleteBuffers(1, &g->vbo);
    if (g->font)
      glDeleteTextures(1, &g->font);
    if (g->white)
      glDeleteTextures(1, &g->white);
    if (g->art)
      glDeleteTextures(1, &g->art);
  }
  if (g->current)
    gbm_surface_release_buffer(g->surface, g->current);
  if (g->display != EGL_NO_DISPLAY) {
    eglMakeCurrent(g->display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    if (g->window != EGL_NO_SURFACE)
      eglDestroySurface(g->display, g->window);
    if (g->context != EGL_NO_CONTEXT)
      eglDestroyContext(g->display, g->context);
    eglTerminate(g->display);
  }
  if (g->surface)
    gbm_surface_destroy(g->surface);
  if (g->gbm)
    gbm_device_destroy(g->gbm);
  drmModeFreeCrtc(g->old);
  if (g->fd >= 0) {
    drmDropMaster(g->fd);
    close(g->fd);
  }
  free(g);
}
static GLuint make_shader(GLenum type, const char *source) {
  GLuint s = glCreateShader(type);
  glShaderSource(s, 1, &source, NULL);
  glCompileShader(s);
  GLint ok;
  glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
  if (!ok) {
    glDeleteShader(s);
    return 0;
  }
  return s;
}
static GLuint texture(int w, int h, const uint8_t *p) {
  GLuint t;
  glGenTextures(1, &t);
  glBindTexture(GL_TEXTURE_2D, t);
  glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, w, h, 0, GL_RGBA, GL_UNSIGNED_BYTE,
               p);
  glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
  glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
  glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
  glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
  return t;
}
int rb_graphics_open(RbGraphics **out, const uint8_t *font) {
  *out = NULL;
  RbGraphics *g = calloc(1, sizeof(*g));
  if (!g)
    return -ENOMEM;
  g->fd = -1;
  int ret = -ENODEV;
  for (int i = 0; i < 16; i++) {
    char path[64];
    snprintf(path, sizeof(path), "/dev/dri/card%d", i);
    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (fd < 0)
      continue;
    drmVersionPtr v = drmGetVersion(fd);
    int found = v && v->name && !strcmp(v->name, "mediatek");
    drmFreeVersion(v);
    if (found) {
      g->fd = fd;
      break;
    }
    close(fd);
  }
  if (g->fd < 0)
    goto fail;
  /* Initialization/rendering can happen while the splash is DRM master.
   * Acquire master only when the first complete frame is ready to scan out. */
  drmModeRes *r = drmModeGetResources(g->fd);
  if (!r)
    goto fail;
  for (int i = 0; i < r->count_connectors && !g->connector; i++) {
    drmModeConnector *c = drmModeGetConnector(g->fd, r->connectors[i]);
    if (c && c->connection == DRM_MODE_CONNECTED && c->count_modes) {
      g->connector = c->connector_id;
      g->mode = c->modes[0];
      for (int k = 0; k < c->count_modes; k++)
        if (c->modes[k].type & DRM_MODE_TYPE_PREFERRED) {
          g->mode = c->modes[k];
          break;
        }
      for (int k = 0; k < c->count_encoders && !g->crtc; k++) {
        drmModeEncoder *e = drmModeGetEncoder(g->fd, c->encoders[k]);
        if (e) {
          if (e->crtc_id)
            g->crtc = e->crtc_id;
          else
            for (int j = 0; j < r->count_crtcs; j++)
              if (e->possible_crtcs & (1U << j)) {
                g->crtc = r->crtcs[j];
                break;
              }
          drmModeFreeEncoder(e);
        }
      }
    }
    drmModeFreeConnector(c);
  }
  drmModeFreeResources(r);
  if (!g->connector || !g->crtc)
    goto fail;
  g->old = drmModeGetCrtc(g->fd, g->crtc);
  g->width = g->mode.hdisplay;
  g->height = g->mode.vdisplay;
  if (g->width > 4096 || g->height > 4096)
    goto fail;
  g->gbm = gbm_create_device(g->fd);
  if (!g->gbm)
    goto fail;
  g->surface = gbm_surface_create(
      g->gbm, g->width, g->height, GBM_FORMAT_XRGB8888,
      GBM_BO_USE_SCANOUT | GBM_BO_USE_RENDERING | GBM_BO_USE_LINEAR);
  if (!g->surface)
    goto fail;
  PFNEGLGETPLATFORMDISPLAYEXTPROC get =
      (PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress(
          "eglGetPlatformDisplayEXT");
  if (!get)
    goto fail;
  g->display = get(EGL_PLATFORM_GBM_KHR, g->gbm, NULL);
  EGLint major, minor;
  if (g->display == EGL_NO_DISPLAY ||
      !eglInitialize(g->display, &major, &minor) ||
      !eglBindAPI(EGL_OPENGL_ES_API))
    goto fail;
  const EGLint attrs[] = {EGL_SURFACE_TYPE,
                          EGL_WINDOW_BIT,
                          EGL_RENDERABLE_TYPE,
                          EGL_OPENGL_ES2_BIT,
                          EGL_RED_SIZE,
                          8,
                          EGL_GREEN_SIZE,
                          8,
                          EGL_BLUE_SIZE,
                          8,
                          EGL_DEPTH_SIZE,
                          0,
                          EGL_STENCIL_SIZE,
                          0,
                          EGL_NONE};
  EGLConfig configs[64], config = NULL;
  EGLint n;
  if (!eglChooseConfig(g->display, attrs, configs, 64, &n))
    goto fail;
  for (int i = 0; i < n; i++) {
    EGLint visual;
    eglGetConfigAttrib(g->display, configs[i], EGL_NATIVE_VISUAL_ID, &visual);
    if (visual == (EGLint)GBM_FORMAT_XRGB8888) {
      config = configs[i];
      break;
    }
  }
  if (!config)
    goto fail;
  const EGLint ca[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
  g->context = eglCreateContext(g->display, config, EGL_NO_CONTEXT, ca);
  g->window = eglCreateWindowSurface(g->display, config,
                                     (EGLNativeWindowType)g->surface, NULL);
  if (g->context == EGL_NO_CONTEXT || g->window == EGL_NO_SURFACE ||
      !eglMakeCurrent(g->display, g->window, g->window, g->context))
    goto fail;
  const char *renderer = (const char *)glGetString(GL_RENDERER);
  if (!renderer || !strstr(renderer, "Mali400")) {
    ret = -ENOTSUP;
    goto fail;
  }
  snprintf(g->info, sizeof(g->info), "%s; EGL %s; %s; %s",
           eglQueryString(g->display, EGL_VENDOR),
           eglQueryString(g->display, EGL_VERSION), renderer,
           glGetString(GL_VERSION));
  eglSwapInterval(g->display, 0);
  const char *vs = "attribute vec2 p;attribute vec2 uv;uniform vec4 "
                   "box;uniform vec4 texbox;varying vec2 t;void "
                   "main(){gl_Position=vec4(p*box.zw+box.xy,0.,1.);t=uv*texbox."
                   "zw+texbox.xy;}";
  const char *fs =
      "precision mediump float;varying vec2 t;uniform sampler2D tex;uniform "
      "vec4 tint;void main(){gl_FragColor=texture2D(tex,t)*tint;}";
  GLuint v = make_shader(GL_VERTEX_SHADER, vs),
         f = make_shader(GL_FRAGMENT_SHADER, fs);
  if (!v || !f) {
    if (v)
      glDeleteShader(v);
    if (f)
      glDeleteShader(f);
    goto fail;
  }
  g->program = glCreateProgram();
  glAttachShader(g->program, v);
  glAttachShader(g->program, f);
  glBindAttribLocation(g->program, 0, "p");
  glBindAttribLocation(g->program, 1, "uv");
  glLinkProgram(g->program);
  glDeleteShader(v);
  glDeleteShader(f);
  GLint ok;
  glGetProgramiv(g->program, GL_LINK_STATUS, &ok);
  if (!ok)
    goto fail;
  glUseProgram(g->program);
  glUniform1i(glGetUniformLocation(g->program, "tex"), 0);
  const GLfloat verts[] = {-1, -1, 0, 1, 1, -1, 1, 1, -1, 1, 0, 0, 1, 1, 1, 0};
  glGenBuffers(1, &g->vbo);
  glBindBuffer(GL_ARRAY_BUFFER, g->vbo);
  glBufferData(GL_ARRAY_BUFFER, sizeof(verts), verts, GL_STATIC_DRAW);
  glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 4 * sizeof(GLfloat),
                        (void *)0);
  glVertexAttribPointer(1, 2, GL_FLOAT, GL_FALSE, 4 * sizeof(GLfloat),
                        (void *)(2 * sizeof(GLfloat)));
  glEnableVertexAttribArray(0);
  glEnableVertexAttribArray(1);
  glEnable(GL_BLEND);
  glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
  glViewport(0, 0, g->width, g->height);
  uint8_t white[] = {255, 255, 255, 255};
  g->white = texture(1, 1, white);
  g->font = texture(128, 64, font);
  g->art = texture(1, 1, white);
  if (glGetError() != GL_NO_ERROR)
    goto fail;
  *out = g;
  return 0;
fail:
  snprintf(last_error, sizeof(last_error), "errno=%d (%s), EGL=0x%x", errno,
           strerror(errno), eglGetError());
  rb_graphics_close(g);
  return ret;
}
const char *rb_graphics_info(RbGraphics *g) { return g->info; }
int rb_graphics_width(RbGraphics *g) { return g->width; }
int rb_graphics_height(RbGraphics *g) { return g->height; }
void rb_graphics_begin(RbGraphics *g) {
  (void)g;
  glClearColor(.035, .047, .07, 1);
  glClear(GL_COLOR_BUFFER_BIT);
}
void rb_graphics_quad(RbGraphics *g, float x, float y, float w, float h,
                      uint32_t color, int glyph, int art) {
  glBindTexture(GL_TEXTURE_2D, art ? g->art : glyph >= 0 ? g->font : g->white);
  glUniform4f(glGetUniformLocation(g->program, "box"),
              (2 * x + w) / g->width - 1, 1 - (2 * y + h) / g->height,
              w / g->width, h / g->height);
  if (glyph >= 0 && !art)
    glUniform4f(glGetUniformLocation(g->program, "texbox"), (glyph % 16) / 16.f,
                (glyph / 16) / 8.f, 1 / 16.f, 1 / 8.f);
  else
    glUniform4f(glGetUniformLocation(g->program, "texbox"), 0, 0, 1, 1);
  glUniform4f(glGetUniformLocation(g->program, "tint"),
              ((color >> 24) & 255) / 255.f, ((color >> 16) & 255) / 255.f,
              ((color >> 8) & 255) / 255.f, (color & 255) / 255.f);
  glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}
int rb_graphics_art(RbGraphics *g, const uint8_t *p) {
  glBindTexture(GL_TEXTURE_2D, g->art);
  glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 160, 160, 0, GL_RGBA,
               GL_UNSIGNED_BYTE, p);
  return glGetError() == GL_NO_ERROR ? 0 : -EIO;
}
int rb_graphics_test(RbGraphics *g) {
  GLuint t = texture(8, 8, NULL), fb;
  glGenFramebuffers(1, &fb);
  glBindFramebuffer(GL_FRAMEBUFFER, fb);
  glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, t,
                         0);
  int ok = glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE;
  uint8_t p[4] = {0};
  if (ok) {
    glClearColor(1, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    glReadPixels(0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, p);
    ok = p[0] > 240 && p[1] < 8 && p[2] < 8 && glGetError() == GL_NO_ERROR;
  }
  glBindFramebuffer(GL_FRAMEBUFFER, 0);
  glDeleteFramebuffers(1, &fb);
  glDeleteTextures(1, &t);
  glViewport(0, 0, g->width, g->height);
  return ok ? 0 : -EIO;
}
int rb_graphics_present(RbGraphics *g) {
  if (glGetError() != GL_NO_ERROR || !eglSwapBuffers(g->display, g->window))
    return -EIO;
  struct gbm_bo *bo = gbm_surface_lock_front_buffer(g->surface);
  if (!bo)
    return -ENOMEM;
  struct fb *f = framebuffer(g->fd, bo);
  if (!f) {
    gbm_surface_release_buffer(g->surface, bo);
    return -EIO;
  }
  if (!g->modeset) {
    int handoff = rb_splash_begin("/run/reborn-splash/control.sock");
    if (handoff < -1) {
      gbm_surface_release_buffer(g->surface, bo);
      return handoff + 1;
    }
    if (drmSetMaster(g->fd) || drmModeSetCrtc(g->fd, g->crtc, f->id, 0, 0, &g->connector, 1,
                       &g->mode)) {
      int error = errno;
      if (handoff >= 0) close(handoff);
      gbm_surface_release_buffer(g->surface, bo);
      return -error;
    }
    g->modeset = 1;
    if (handoff >= 0) {
      /* The old splash FB is destroyed after our acknowledgement. Never try
       * restoring that retired buffer on shutdown or context recreation. */
      drmModeFreeCrtc(g->old); g->old = NULL;
      rb_splash_presented(handoff);
    }
  } else {
    g->flip.waiting = true;
    if (drmModePageFlip(g->fd, g->crtc, f->id, DRM_MODE_PAGE_FLIP_EVENT,
                        &g->flip)) {
      gbm_surface_release_buffer(g->surface, bo);
      return -errno;
    }
    if (wait_flip(g->fd, &g->flip)) {
      drmModeSetCrtc(g->fd, g->crtc, 0, 0, 0, NULL, 0, NULL);
      gbm_surface_release_buffer(g->surface, bo);
      return -ETIMEDOUT;
    }
  }
  if (g->current)
    gbm_surface_release_buffer(g->surface, g->current);
  g->current = bo;
  return 0;
}
