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
- **Commit:** `72f9995` (`Make sink reconfiguration transactional`).
- **Remaining limitation:** ALSA planning and the bounded worker-open
  acknowledgement remain synchronous on the runtime thread; each stop/open
  boundary is bounded at two seconds. This introduces a bounded wait for the
  actual sink-open result so settings cannot commit on command enqueue alone.
  Actual BlueALSA and wired-device open timing remains
  `PHYSICAL_QUALIFICATION_PENDING`.
- **Evidence tier:** source inspection + fake sink/worker host tests. No physical
  sink was opened.

## R4 — scan identity and pruning safety

**Status: FIXED**

- **Finding and symptom:** A changed file whose FFmpeg metadata open failed was
  counted as a failure but left the per-source `complete` flag true. Finish
  could therefore prune the previous valid row. SD scan safety also used only
  `root.is_dir()`, which remains true when `/media/sd` is an empty unmounted
  directory or a different filesystem has reused that path.
- **Current source confirmation:** `scan_sources()` continued after
  `Decoder::open()` errors without clearing `complete`, and Finish pruned all
  rows whose `seen` token did not match. The old SD completion guard checked
  only directory existence.
- **Implementation:** Decoder-open, metadata, traversal, and directory-entry
  read failures now make the source incomplete; any such incomplete scan skips
  pruning. SD `Source` snapshots carry the observed mount ID in addition to the
  UUID-backed source ID. The scanner verifies the same mount ID at traversal
  boundaries and again in the database writer immediately before Finish.
  Traversal uses an open directory descriptor through `/proc/self/fd`, so a
  replaced path cannot redirect the in-flight walk to a different filesystem.
  Missing UUID or mount identity fails closed. Finish reports whether pruning
  actually occurred; batch, finish, and storage-full errors propagate without
  marking the scan complete.
- **Files changed:** `crates/reborn-core/src/lib.rs`,
  `crates/reborn-library/src/lib.rs`, `crates/reborn-platform/src/storage.rs`,
  `app/reborn/src/main.rs`, `app/reborn/src/bin/reborn-preview.rs`.
- **Tests added:** Changed-file FFmpeg open failure retains the old row; an SD
  path that is unmounted or reused with a new mount ID is rejected; mid-scan
  identity loss and a database-writer identity recheck disable pruning; a safe
  complete scan still prunes. SQLite trigger fault injection covers batch
  failure, finish failure, and a simulated `SQLITE_FULL`/ENOSPC message; all
  preserve prior rows. Mountinfo parsing preserves mount IDs and escaped paths.
- **Tests run:** `cargo fmt --all -- --check`; `cargo check --workspace --locked`;
  `cargo test -p reborn-library -p reborn-platform -p reborn-core --locked` —
  49 passed, 0 failed.
- **Result:** A scan is prune-safe only after traversal, file metadata, stable
  source identity, batch persistence, and finish persistence all succeed.
- **Commit:** `6e8ea74` (`Prevent unsafe library scan pruning`).
- **Remaining limitation:** Unclassified decoder-open failures are treated as
  unsafe even if a file is permanently corrupt; row removal waits for a later
  identity-consistent successful scan. `/proc/self/mountinfo` and a usable
  filesystem UUID are required for SD pruning. Physical media removal and card
  replacement remain `PHYSICAL_QUALIFICATION_PENDING`.
- **Evidence tier:** source inspection + SQLite/file fault injection + host
  mountinfo parser tests. No SD card or physical Y2 was accessed.

## R5 — database recovery classification

**Status: FIXED**

- **Finding and symptom:** Startup treated nearly every SQLite open/setup error
  as corruption, moved the live database first, then moved `-wal` and `-shm`
  independently. A future schema, lock, read-only filesystem, or full disk
  could detach a valid database; a sidecar rename failure could leave a split
  database set.
- **Current source confirmation:** `Database::spawn()` previously quarantined
  any non-future-schema open error, while `open()` returned untyped strings and
  enabled WAL before checking the schema version.
- **Implementation:** SQLite failures are classified by primary/extended error
  code and operation stage into corruption, future schema, busy/locked,
  read-only, full disk, permission, I/O, WAL/SHM, migration, or setup failures.
  The schema version is read before changing journal mode. Only SQLite
  `CORRUPT`/`NOTADB` or a failed `quick_check(10)` reaches quarantine; all
  operational and newer-schema cases preserve the source DB and fail startup.
  Corruption recovery moves the DB and existing sidecars into one evidence
  directory. Rename failures roll back in reverse order. An `INCOMPLETE`
  marker lets the next startup restore a set left mid-transaction by process
  interruption before attempting SQLite open.
- **Files changed:** `crates/reborn-library/src/lib.rs`.
- **Tests added:** Bad-header and damaged-page databases quarantine and rebuild;
  future schema remains intact; locked, read-only, disk-full, permission,
  generic I/O, WAL/SHM, and migration failures retain their distinct class.
  Sidecar-set success, injected WAL/SHM rename failures with full rollback, and
  interrupted-quarantine recovery are exercised at the filesystem boundary.
- **Tests run:** `cargo fmt --all`; `cargo test -p reborn-library --locked` —
  23 passed, 0 failed. Workspace/package check follows with the host gate.
- **Result:** Only genuine corruption is quarantined; newer and operational
  failures preserve the existing DB, and sidecar handling has rollback plus a
  startup recovery path.
- **Commit:** `d61e6fe` (`Classify database recovery failures safely`).
- **Remaining limitation:** The startup integrity probe reads the SQLite
  database and busy handling may wait up to two seconds. Host fixtures verify
  classification and recovery; target filesystem behavior remains
  `PHYSICAL_QUALIFICATION_PENDING`.
- **Evidence tier:** source inspection + SQLite fault injection + temporary
  filesystem tests. No Y2 storage was accessed.

## R6 — crossfade input-window recovery

**Status: FIXED**

- **Finding and symptom:** `read_window()` consumed several next-track decoder
  blocks into a local vector. If a later decoder read failed, `?` returned the
  error and dropped that local prefix even though the decoder had advanced.
- **Current source confirmation:** The previous helper returned
  `Result<Option<Pcm>, String>` and did not return accumulated PCM on error.
- **Implementation:** Window failures now carry both the error and any PCM
  already consumed. The transition recovery drains the old-track suffix first,
  then queues the consumed next-track prefix before the authoritative boundary;
  subsequent decoder reads continue after that prefix. The existing
  `send_stream()` chunking remains the sink write boundary.
- **Files changed:** `app/reborn/src/playback.rs`.
- **Tests added:** Injected failure before input, after the first block, after
  multiple blocks, and one frame short of 5-, 10-, and 15-second windows, with
  sample-by-sample continuity checks. A recovery-order test verifies the old
  suffix precedes the new prefix, and a large partial-prefix case verifies
  every sink chunk stays at or below 524,288 bytes without losing samples.
- **Tests run:** `cargo fmt --all`; `cargo test -p reborn --bin reborn --locked
  playback::tests` — 13 passed, 0 failed.
- **Result:** Every consumed input frame has an owner after a later read error;
  the old suffix and next prefix remain ordered and sink writes stay bounded.
- **Commit:** `48ddaaa` (`Preserve crossfade input after read errors`).
- **Remaining limitation:** Fault injection exercises the real window assembly
  and stream chunker with deterministic PCM; physical 5/10/15-second playback
  remains `PHYSICAL_QUALIFICATION_PENDING`.
- **Evidence tier:** source inspection + runtime helper fault injection + host
  playback tests. No physical output was used.

## R7 — filtered single-track context actions

**Status: FIXED**

- **Finding and symptom:** In a filtered view, context-menu Play could start the
  selected track plus the rest of the filtered collection, while Play Next and
  Add to Queue enqueued every filter match.
- **Current source confirmation:** The context handler routed Play through the
  collection-aware `play_effect()` and special-cased filtered queue actions to
  `PlayNextCollection`/`AddToQueueCollection`.
- **Implementation:** Context rows are resolved against the focused source row.
  A `track:*` row exposes single-track actions; Play emits `Play(index)`, and
  Play Next/Add emit their single-index effects regardless of active album,
  artist, or folder filter. Explicit Album collection rows still emit
  collection effects. Album/artist collection context actions retain their
  collection semantics.
- **Files changed:** `crates/reborn-ui/src/lib.rs`,
  `crates/reborn-ui/src/screens.rs`.
- **Tests added:** Album screen, artist screen, and folder-filtered tracks each
  select the second matching track and verify Play, Play Next, and Add affect
  only that track. Explicit album Play, Play Next, and Add continue to target
  both album members.
- **Tests run:** `cargo fmt --all`; `cargo test -p reborn-ui --locked` — 12
  passed, 0 failed.
- **Result:** Context action intent stays on the selected queue candidate while
  explicit collection actions remain collection-wide.
- **Commit:** pending.
- **Remaining limitation:** Verification is at the UI Action/Effect boundary;
  actual queue mutation and physical button interaction remain
  `PHYSICAL_QUALIFICATION_PENDING`.
- **Evidence tier:** source inspection + host UI action tests.

## R8–R10

Results will be recorded separately as each correction is reviewed and
committed. No status is claimed yet.
