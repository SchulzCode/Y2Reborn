/* SPDX-License-Identifier: MIT
 * Bounded local display handoff. The caller already owns its DRM fd and has
 * rendered a first backbuffer before READY. The splash keeps its FB alive until
 * PRESENTED, so releasing master never exposes fbcon or freed scanout memory. */
#include <sys/socket.h>
#include <sys/un.h>
#include <sys/time.h>

static int rb_splash_begin(const char *path) {
  int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
  if (fd < 0) return -errno - 1;
  struct sockaddr_un address = {.sun_family = AF_UNIX};
  if (strlen(path) >= sizeof(address.sun_path)) { close(fd); return -EINVAL - 1; }
  strcpy(address.sun_path, path);
  struct timeval timeout = {.tv_sec = 2};
  setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
  setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof(timeout));
  if (connect(fd, (struct sockaddr *)&address, sizeof(address))) {
    int error = errno; close(fd);
    return error == ENOENT || error == ECONNREFUSED ? -1 : -error - 1;
  }
  if (send(fd, "READY 1\n", 8, MSG_NOSIGNAL) != 8) { close(fd); return -EIO - 1; }
  char reply[11]; size_t used = 0;
  /* Absolute deadline, so a peer cannot extend this by trickling bytes. */
  double deadline = now() + 2;
  while (used < sizeof(reply)) {
    struct pollfd p = {.fd = fd, .events = POLLIN};
    int ms = (int)((deadline - now()) * 1000);
    if (ms <= 0 || poll(&p, 1, ms) <= 0) { close(fd); return -ETIMEDOUT - 1; }
    ssize_t n = recv(fd, reply + used, sizeof(reply) - used, 0);
    if (n <= 0) { close(fd); return -EPROTO - 1; }
    used += (size_t)n;
  }
  if (memcmp(reply, "RELEASED 1\n", sizeof(reply))) { close(fd); return -EPROTO - 1; }
  return fd;
}
static void rb_splash_presented(int fd) {
  if (fd >= 0) { (void)send(fd, "PRESENTED 1\n", 12, MSG_NOSIGNAL); close(fd); }
}
