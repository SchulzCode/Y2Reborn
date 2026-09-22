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
- **Commit:** pending.
- **Remaining limitation:** This is software/runtime-state evidence. The
  current candidate's physical UI and persistence behavior remain
  `PHYSICAL_QUALIFICATION_PENDING`.
- **Evidence tier:** source inspection + fresh targeted host test.

## R2–R10

Results will be recorded separately as each correction is reviewed and
committed. No status is claimed yet.
