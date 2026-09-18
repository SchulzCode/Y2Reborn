# Reborn

Native Linux music player for Y2Linux. Rust, ARMv7 hard-float glibc, FFmpeg,
ALSA/BlueALSA, SQLite, DRM/GBM/EGL/GLES2, evdev, wpa_supplicant and BlueZ.
No Android runtime or application architecture is used.

**REBORN BASELINE 01: implementation candidate; physical acceptance pending.**
Start with [architecture and operator contract](docs/architecture/baseline-01.md)
and [installation handoff](docs/validation/REBORN-BASELINE-01.md).
Earlier `docs/planning/M0-*` files are retained historical planning, superseded
by the owner's September 18, 2026 native Reborn implementation request.

```sh
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo build --workspace --locked --offline
python3 tests/host-daemon.py
Y2_BUILDROOT_OUTPUT=/path/to/production/buildroot tools/build/cross.sh
```

Rust 1.90.0 and its ARMv7 standard library must be provisioned before an offline
build. Cargo dependencies are pinned in Cargo.lock and checked into vendor/.
Native development libraries are required on a host; production libraries and
headers come from the pinned Buildroot output. There is no production bindgen.
The runtime does not require Python; Python tools run only on the host.

The normal service uses `/data/reborn` and `/run/reborn/control.sock`.
For an isolated host daemon use `--headless --data-dir DIR --socket PATH
--music-dir DIR --fixtures assets/fixtures`; headless mode accurately reports
hardware unavailable and cannot count as physical acceptance.
