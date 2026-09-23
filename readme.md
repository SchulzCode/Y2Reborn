# Reborn

Native Linux music player for the Innioasis Y2, running on
[Y2Linux](https://github.com/SchulzCode/Y2Linux). Rust, ARMv7 hard-float glibc,
FFmpeg, ALSA/BlueALSA, SQLite, DRM/GBM/EGL/GLES2, evdev, wpa_supplicant and BlueZ.
No Android runtime or application architecture is used.

**Platform v1 application integration is software validated; physical
qualification is pending.** The candidate pairs Reborn `6c8aa12` with Y2Linux
`d04b95a`. Its validation includes 155 passing host workspace tests, formatting
and strict clippy, fresh ARM builds, QEMU userspace checks and platform image
validation. These establish software evidence, not physical playback, target
performance or endurance acceptance. Later documentation commits do not change
the candidate's compiled identities.

Start with [Platform v1 integration](docs/validation/PLATFORM-V1-INTEGRATION.md)
and the [correctness closure](docs/validation/LUNA-CORRECTNESS-CLOSURE-01.md).
The [original architecture](docs/architecture/baseline-01.md) remains useful
background; its initial versions and platform gaps are superseded where the
later validation records describe completed changes.
Y2Linux's [current state](https://github.com/SchulzCode/Y2Linux/blob/main/docs/CURRENT_PLATFORM_STATE.md)
and [owner qualification sessions](https://github.com/SchulzCode/Y2Linux/blob/main/docs/validation/PLATFORM-V1-OWNER-QUALIFICATION.md)
define the paired image and remaining hardware gates.

- FFmpeg owns decoding, DSP and final PCM conversion. ALSA/BlueALSA sink changes
  preserve transport generations and commit application state only after a
  successful open.
- SQLite remains the library authority with one database writer. Scans validate
  source identity before pruning; platform SD generations distinguish removal,
  reinsertion and a different card at the same mountpoint.
- Library benchmarks exercise the real schema/scanner/UI at 1k, 10k and 20k
  tracks. Wheel navigation formats the visible page without cloning the entire
  catalog. Host measurements do not establish Y2 latency or memory budgets.
- Platform integration consumes network readiness and capabilities, acknowledges
  bounded shutdown after session/audio/database close, and reports boot readiness
  only after an actual first frame and runtime initialization.
- BlueZ AVRCP maps remote Play/Pause/Next/Previous to existing semantic actions
  and publishes the existing playback model. `y2-bt-reconnect` remains the single
  automatic device connection owner; codec reporting uses negotiated PCM state.

The internal product audio profile remains **S16 stereo at 44.1 kHz**. Software
24-bit conversion support does not establish high-resolution Y2 output. Bluetooth
provides manual SBC for qualification; Auto requires runtime, remote,
distribution and platform-qualification eligibility and currently fails closed.
Optional AAC/aptX/aptX HD/LDAC encoders are absent. Physical AVRCP, Bluetooth audio,
radio coexistence and long-run behavior remain owner qualification work.

For host development, provision Rust **1.90.0** and the native development
libraries, then run:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo build --workspace --locked --offline
python3 tests/host-daemon.py
```

Cargo dependencies are pinned in `Cargo.lock` and checked into `vendor/`.
The application uses FFmpeg **9.0.2** in the current production pair. Native
headers/libraries must match the selected build environment; see Y2Linux's
[source reconstruction guide](https://github.com/SchulzCode/Y2Linux/blob/main/docs/build/platform-v1-reconstruction.md)
for the pinned inputs and owner firmware requirements. The ARM build also needs
the `armv7-unknown-linux-gnueabihf` standard library and the paired Buildroot SDK:

```sh
Y2_BUILDROOT_OUTPUT=/path/to/production/buildroot tools/build/cross.sh
```

There is no production bindgen. Reborn itself does not require Python; its host
test tools do, and Y2Linux's platform services have their own runtime dependencies.

The normal service stores state under `/data/reborn`, reads internal music from
`/data/music` and exposes `/run/reborn/control.sock`. For an isolated host daemon,
use `--headless --data-dir DIR --socket PATH --music-dir DIR --fixtures
assets/fixtures` with private scratch paths. Headless mode reports unavailable
hardware and cannot supply physical first-frame or playback qualification.

The Platform v1 candidate is packaged by Y2Linux, preserving Y2DATA. Follow its
owner qualification plan before returning to general application feature work;
unsupported platform capabilities remain explicit rather than inferred by Reborn.
