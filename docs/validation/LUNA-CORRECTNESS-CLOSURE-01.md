# Luna correctness closure 01

Date: 2026-09-22. Software-only closure record for the finite findings in the
latest independent review. No physical Y2 was accessed, paired, configured,
flashed, or tested. This record distinguishes host/source evidence from
physical qualification.

## Starting state and boundary

| Repository | Starting HEAD | Starting worktree |
|---|---|---|
| Y2Linux | `8e5b53bc9fb934e1f668b9e7e9e34f0fc5e4130e` | clean; `main` ahead 29 |
| Y2Reborn | `8a1443daa25419834c800a7f9d707e66f80e0f90` | `main` ahead 37; pre-existing untracked `docs/audit/`, `docs/review/`, and `docs/architecture/bluetooth-codecs.md` preserved |

Y2Linux roadmap boundary audit: commit `8c485b6`. It records the retained
GPU-01, CONNECTIVITY-10, and M4 evidence limits, current open platform issues,
and that this work neither closes a platform milestone nor authorizes hardware
activity. The architecture and hardware exclusions in the request remain in
force. No candidate exists yet.

## R1 — render and persistence intent

**Status: FIXED**

- **Finding and symptom:** Runtime used one `dirty` boolean for frame redraw
  and session persistence. A render could suppress a position checkpoint; a
  save could suppress the redraw requested by a setting or service notice.
- **Current source confirmation:** `Runtime` previously cleared the same flag
  after `model.checkpoint()` and after drawing; its 15-second save condition
  also depended on that flag.
- **Implementation:** Split runtime dirtiness into `render` and `persistence`
  state. Draw success clears only `render`; checkpoint success clears only
  `persistence`; checkpoint failure retains persistence intent. Position and
  queue-boundary changes request persistence, which is serviced at the existing
  15-second cadence. Setting, playback, Bluetooth-loss and source changes keep
  their required redraw/save behavior. Removed a redundant second Bluetooth-loss
  checkpoint.
- **Files changed:** `app/reborn/src/main.rs`.
- **Tests added:** Seven runtime dirty-state tests for stopped screen-timeout
  save/redraw, render-before-save, save-before-render, Bluetooth-loss notice,
  position cadence after rendering, unchanged state, and failed checkpoint
  retry intent.
- **Tests run:** `cargo fmt --all -- --check`; `cargo test -p reborn --bin reborn
  --locked runtime_dirty_tests` — 7 passed.
- **Result:** Separate runtime intents; targeted host tests pass.
- **Commit:** `f081a78` (`Separate render and persistence dirty state`).
- **Remaining limitation:** This is software/runtime-state evidence. The
  current candidate's physical UI and persistence behavior remain
  `PHYSICAL_QUALIFICATION_PENDING`.
- **Evidence tier:** source inspection + fresh targeted host test.

## R2 — signed 24-bit PCM in a 32-bit container

**Status: FIXED**

- **Finding and symptom:** Rust represented `S24_LE` as four bytes, but the C
  media membrane mapped format code 3 to S16. Nonzero S32-to-S24 output was
  consequently corrupted; the previous zero-only test could not expose it.
- **Current source confirmation:** The Rust format model already specified
  24 valid bits and 32 physical bits. The ALSA C mapping selected
  `SND_PCM_FORMAT_S24_LE`, while FFmpeg conversion and decoder output treated
  code 3 as S16. ALSA documents `S24_LE` as signed 24-bit LE using the low three
  bytes in a 32-bit word; `S24_3LE` is a separate packed three-byte format.
  See the [ALSA PCM format reference](https://www.alsa-project.org/alsa-doc/alsa-lib/group___p_c_m.html)
  and [PCM transfer semantics](https://www.alsa-project.org/alsa-doc/alsa-lib/pcm.html).
- **Implementation:** Preserve the existing model's four-byte sample/eight-
  byte stereo-frame sizing and document the low-three-byte LE contract. FFmpeg
  converts through S32 Q31; native packing takes the signed top 24 bits, writes
  them into bytes 0–2, and zeros the padding byte. S24 input ignores padding,
  sign-extends the low 24 bits into Q31, and S24-to-S24 normalizes padding.
  Decoder and crossfade paths use the same packing boundary. ALSA code 3 maps
  to `SND_PCM_FORMAT_S24_LE`; unknown codes are rejected, and runtime format
  reporting checks ALSA valid/physical widths. Receipt text says
  `s24le-in-32` rather than claiming FFmpeg has a packed S24 sample format.
- **Files changed:** `crates/reborn-core/src/lib.rs`,
  `crates/reborn-media/native/media.c`, `crates/reborn-media/src/native.rs`,
  `crates/reborn-audio/native/audio.c`, `crates/reborn-audio/src/native.rs`.
- **Tests added:** Exact S32-to-S24 bytes for zero, ±1-LSB, half-scale, maximum
  positive and minimum negative values in distinct stereo channels; S24-to-S32
  sign extension with ignored padding; actual decoded FLTP-to-S24 output
  compared sample-for-sample with S32 top bits and frame counts; S24
  crossfade; ALSA valid/physical-width mapping; unsupported-format rejection;
  and core byte/valid-bit sizing.
- **Tests run:** `cargo fmt --all -- --check`; `cargo test --locked -p
  reborn-core -p reborn-media -p reborn-audio` — 32 passed, 0 failed.
- **Result:** Nonzero and extreme sample packing, conversion, stereo order,
  frame accounting, ALSA mapping, and fail-closed behavior pass on the host.
- **Commit:** `f90f9fb` (`Fix signed 24-bit PCM container conversion`).
- **Remaining limitation:** No optional Bluetooth codec was enabled. No Y2
  sample playback or high-resolution wired behavior is claimed;
  `PHYSICAL_QUALIFICATION_PENDING`.
- **Evidence tier:** source inspection + host native-library tests + local ALSA
  format-width query + official ALSA documentation.

## R3 — transactional sink reconfiguration

**Status: FIXED**

- **Finding and symptom:** Reconfiguration released the old sink, but planning
  or opening the replacement could fail after callers had already changed
  output, queue, playback position, or DSP settings. The open command was
  fire-and-forget, and the release's two-second timeout began only after a
  blocking command-channel send.
- **Current source confirmation:** `Runtime::load()` previously planned after
  release but mutated `Buffering` state and submitted the replacement without
  waiting for the sink actor's factory result. `Playback::stop_and_wait()` used
  blocking `send()` before calling `recv_timeout(2s)`.
- **Implementation:** Reconfiguration now runs release acknowledgement,
  sink planning, acknowledged worker open, decoder start enqueue, and only then
  model commit. The sink actor has a direct one-slot open-result reply; start
  failure invalidates its generation and queues release. A sink-open guard
  refuses a second open until the prior release is acknowledged. The two-second
  release deadline now covers retrying a full command queue and waiting for the
  release reply. Runtime action snapshots restore output, settings, queue,
  selection, and position on failure; after release starts, active playback is
  reported as `Error`, stale generations are invalidated, and the restored
  state is checkpointed. Planning/open failure never commits candidate output
  or settings.
- **Files changed:** `app/reborn/src/main.rs`, `app/reborn/src/playback.rs`.
- **Tests added:** Runtime tests inject plan failure after releasing a live fake
  sink, verify no replacement start, and verify single sink ownership on a
  successful replacement. Model recovery tests cover output, queue selection,
  seek position, volume, ReplayGain, and EQ rollback, plus pre-release failure.
  Playback tests inject sink-open failure, a full command queue, a disconnected
  receiver, and a full event queue that stalls the actor; they assert synchronous
  open failure, no `TrackStarted`, bounded release, and successful recovery once
  the stalled queue drains. Existing output-switch coverage now also verifies
  the sink actor rejects a second open without release acknowledgement.
- **Tests run:** `cargo fmt --all -- --check`; `cargo check -p reborn --locked`;
  `cargo test -p reborn --bin reborn --locked` — 21 passed, 0 failed.
- **Result:** Release/plan/start/commit/fail ordering is explicit and the
  failure-boundary tests pass. The prior logical state is preserved where safe;
  failed active transitions cannot report `Playing` without a sink.
- **Commit:** pending.
- **Remaining limitation:** ALSA planning and the bounded worker-open
  acknowledgement remain synchronous on the runtime thread; each stop/open
  boundary is bounded at two seconds. Actual BlueALSA and wired-device open
  timing remains `PHYSICAL_QUALIFICATION_PENDING`.
- **Evidence tier:** source inspection + fake sink/worker host tests. No physical
  sink was opened.

## R4–R10

Results will be recorded separately as each correction is reviewed and
committed. No status is claimed yet.
