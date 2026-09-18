# Reborn startup latency correction

The owner-installed audio candidate was inspected read-only after the report
that the startup splash remained visible for too long. The retained boot trace
showed the splash visible at `0.828 s`, Y2ROOT completing its handoff at
`12.246 s`, and the splash releasing/presenting at `52.047/52.060 s`.

The running Reborn process was created near `19.5 s`, but its first structured
application record appeared near `41.0 s`. Its executable directly required
all six FFmpeg shared libraries, so the ARM dynamic loader performed their
cold relocation and constructor work before `main`. The remaining application
trace took about `3.5 s` to reach the scan worker and `6.2 s` to create the
Mali400 EGL context; first frame presentation followed at about `11.0 s` from
the Reborn session start. A dirty FAT volume was also reported by the kernel,
but it is an independent storage-health issue rather than the Reborn loader
delay.

The correction keeps FFmpeg 9.0.1 authoritative and moves the media membrane
behind a lazy shared boundary. The `reborn` ELF has no FFmpeg `DT_NEEDED`
entries; `/usr/lib/reborn/libreborn_media.so` carries the six FFmpeg SONAMEs
and is loaded on the first decoder, artwork, conversion, crossfade or runtime
component request. Reborn now records millisecond startup phases, opens the
GPU context before the library scan, and starts from `S05reborn` after
`S02y2-data`. D-Bus, BlueZ, wpa_supplicant and connectivity remain worker
dependencies with retry behavior, so early UI startup does not wait for radios.

Host and ARM validation completed for the candidate source: the workspace
tests pass, the `reborn-media` crate passes strict Clippy, the ARM cross build
passes with the production Buildroot SDK, the Buildroot package installs the
shared membrane, and pinned QEMU runs the installed ARM binaries with the
actual FFmpeg `9.0.1` runtime manifest. The next startup timing measurement
requires the owner to install this root-only candidate; no physical retest is
claimed by QEMU or host ELF inspection.
