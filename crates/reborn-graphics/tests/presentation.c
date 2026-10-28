/* Exercise the actual presentation implementation without a DRM/GL device. */
#define eglSwapBuffers fake_swap
#define glGetError fake_gl_error
#define gbm_surface_lock_front_buffer fake_lock
#define gbm_bo_get_user_data fake_userdata
#define gbm_surface_release_buffer fake_release
#define drmSetMaster fake_master
#define drmModeSetCrtc fake_modeset
#define drmModePageFlip fake_flip
#define drmHandleEvent fake_event
#define connect fake_connect
#define poll fake_poll
#include "../native/graphics.c"
#include <assert.h>

static struct fb test_fb = {.id = 77, .fd = 1234};
static int tokens[3], next_token, released, modesets, flips;
static bool no_event, queue_error, modeset_error, not_master;
static void *completion;

EGLBoolean fake_swap(EGLDisplay display, EGLSurface surface) {
  (void)display; (void)surface; return EGL_TRUE;
}
GLenum fake_gl_error(void) { return GL_NO_ERROR; }
struct gbm_bo *fake_lock(struct gbm_surface *surface) {
  (void)surface; return (struct gbm_bo *)&tokens[next_token++ % 3];
}
void *fake_userdata(struct gbm_bo *bo) { (void)bo; return &test_fb; }
void fake_release(struct gbm_surface *surface, struct gbm_bo *bo) {
  (void)surface; assert(bo); released++;
}
int fake_master(int fd) { assert(fd == 1234); return 0; }
int fake_modeset(int fd, uint32_t crtc, uint32_t fb_id, uint32_t x, uint32_t y,
                 uint32_t *connectors, int count, drmModeModeInfoPtr mode) {
  (void)crtc; (void)x; (void)y; (void)connectors; (void)count; (void)mode;
  assert(fd == 1234); assert(fb_id == 77 || (!fb_id && !mode)); modesets++;
  if (not_master) { errno = EACCES; return -1; }
  if (modeset_error) { errno = EIO; return -1; }
  return 0;
}
int fake_flip(int fd, uint32_t crtc, uint32_t fb_id, uint32_t flags, void *data) {
  (void)crtc; assert(fd == 1234); assert(fb_id == 77);
  assert(flags == DRM_MODE_PAGE_FLIP_EVENT); flips++; completion = data;
  if (queue_error) { errno = EBUSY; return -1; }
  return 0;
}
int fake_event(int fd, drmEventContextPtr context) {
  assert(!no_event); context->page_flip_handler(fd, 42, 10, 0, completion);
  return 0;
}
int fake_connect(int fd, const struct sockaddr *address, socklen_t length) {
  (void)fd; (void)address; (void)length; errno = ENOENT; return -1;
}
int fake_poll(struct pollfd *fds, nfds_t count, int timeout) {
  assert(count == 1); assert(timeout > 0);
  if (no_event) return 0;
  fds[0].revents = POLLIN; return 1;
}

static RbGraphics context(void) {
  next_token = released = modesets = flips = 0;
  no_event = queue_error = modeset_error = not_master = false;
  return (RbGraphics){.fd = 1234};
}
int main(void) {
  RbGraphics g = context();
  no_event = true;
  assert(rb_graphics_present(&g) == -ETIMEDOUT);
  assert(modesets == 1 && flips == 1 && g.flip.count == 0);
  assert(g.failed && g.pending && !g.current && !released);
  assert(rb_graphics_present(&g) == -EIO && flips == 1);

  g = context(); queue_error = true;
  assert(rb_graphics_present(&g) == -EBUSY);
  assert(!g.flip.waiting && !g.flip.count && g.pending && !released);

  g = context(); modeset_error = true;
  assert(rb_graphics_present(&g) == -EIO);
  assert(!g.modeset && !flips && released == 1 && !g.pending);

  g = context();
  assert(rb_graphics_present(&g) == 0);
  assert(modesets == 1 && flips == 1 && g.flip.count == 1);
  assert(g.current && !g.pending && !g.failed && !released);
  assert(rb_graphics_present(&g) == 0);
  assert(modesets == 1 && flips == 2 && g.flip.count == 2 && released == 1);
  no_event = true;
  assert(rb_graphics_present(&g) == -ETIMEDOUT);
  assert(g.current && g.pending && g.failed && released == 1);

  /* F17 (3e55b45e): before Reborn's first frame the splash is DRM master; the
   * off has nothing of Reborn's to stop and must not fail the wake retry. */
  g = context(); not_master = true;
  assert(rb_graphics_sleep(&g, 1) == 0 && g.sleeping && modesets == 1);
  assert(rb_graphics_sleep(&g, 0) == 0 && !g.sleeping && modesets == 1);
  /* Once Reborn owns the scanout, a refused off is still an error. */
  g = context();
  assert(rb_graphics_present(&g) == 0 && g.modeset);
  not_master = true;
  assert(rb_graphics_sleep(&g, 1) == -EACCES && !g.sleeping);
  g = context(); modeset_error = true;
  assert(rb_graphics_sleep(&g, 1) == -EIO && !g.sleeping);
  return 0;
}
