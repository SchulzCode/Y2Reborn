# Reborn

Native Rust/FFmpeg music player for the Innioasis Y2 on
[Y2Linux](https://github.com/SchulzCode/Y2Linux). Both repositories' `main`
branches carry the latest published development source and documentation.

**Latest physically observed application:** Reborn
`36db1869c6bab1ad2d00b7d8e7807ea6ef5b3803` paired with Y2Linux
`0de6e951b438bf0f2d701e23e476d2b2405ba3c6`, kernel
`6.18.0-y2linux-cpu-final-fix01`, root `2025.02.18-platform-v1.7`.
The 2026-09-27 CPU Final Fix01 run confirms real Idle, PlaybackNormal,
PlaybackHeavy, Interactive, ArtworkDecode, LibraryScan, NetworkTransfer and
Maintenance leases. Bounded S16 44.1 and 24/96 -> 48 wired fixtures have zero
XRUN/decode/filter errors. Real input boost and immediate screen-off release work.
This is not a full player/audio/endurance acceptance. The paired platform still
fails SLIDLE, high-OPP admission, staged recovery and loaded USB reliability.

Start with [current Reborn state](docs/CURRENT_REBORN_STATE.md), the
[physical report](https://github.com/SchulzCode/Y2Linux/blob/main/docs/validation/Y2-CPU-FINAL-FIX01-PHYSICAL-QUALIFICATION.md),
and [documentation index](docs/README.md). Earlier Platform v1, Luna and UI
candidate reports retain their original software/evidence scope.

Reborn owns UI state, one SQLite library writer, FFmpeg decode/DSP, ALSA/BlueALSA
sink lifetime and semantic workload leases. The kernel/schedutil retains CPU
frequency authority. The production media path uses FFmpeg **9.0.2**; preferred
wired output is S16 stereo at enabled 44.1/48 kHz. High-rate sources may be
decoded and converted; native wide/high-rate Y2 output remains gated.
Crossfade-configured playback produces a heavy lease, but an actual end-of-track
blend and independent EQ run were not physically qualified by Fix01.

Normal Bluetooth endpoints remain SBC-only. Private optional encoder sources
can be compiled, but AAC/aptX/aptX-HD/LDAC and Auto selection remain under
runtime, peer, distribution and physical gates. Fix01 confirmed an isolated
CONSYS retry and controller service, not peer playback after full resume.

For host development use Rust **1.90.0**, matching native libraries and pinned
`Cargo.lock`/`vendor/`:

```sh
cargo fmt --all -- --check
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
```

An ARM release needs the paired Buildroot SDK and the recorded image/source
pair; see [source reconstruction](https://github.com/SchulzCode/Y2Linux/blob/main/docs/build/platform-v1-reconstruction.md).
The player uses `/data/reborn`, `/data/music` and `/run/reborn/control.sock`.
Host/headless tests do not establish physical Y2 rendering, audio quality or
recovery. No firmware, keys, bonds or private evidence belong in this repo.
