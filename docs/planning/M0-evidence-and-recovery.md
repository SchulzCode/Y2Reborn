# M0 — Evidence & Recovery Baseline

Planning baseline: 2026-09-08. Status: **NOT PASSED**. No device operations were performed during this planning session.

Published milestone: [M0 — Evidence & Recovery Baseline](https://github.com/SchulzCode/Y2PlayerNative/milestone/1). First issue batch:

- [Y2E-101](https://github.com/SchulzCode/Y2PlayerNative/issues/1) — Inventory existing stock artifacts and establish evidence provenance
- [Y2E-105](https://github.com/SchulzCode/Y2PlayerNative/issues/2) — Capture the current non-root ADB system and access baseline
- [Y2E-110](https://github.com/SchulzCode/Y2PlayerNative/issues/3) — Reconcile stock scatter and runtime partition metadata
- [Y2E-115](https://github.com/SchulzCode/Y2PlayerNative/issues/4) — Inspect stock boot structure and config/device-tree availability offline
- [Y2E-120](https://github.com/SchulzCode/Y2PlayerNative/issues/5) — Assess recovery capability and propose the next safe acquisition boundary

Only Y2E-101 is labeled `state:luna-ready`; the four dependent tasks are `state:blocked`. GitHub issue bodies contain explicit predecessor links and the full execution contract. Local copies are in `docs/planning/issues/`.

The immediate boundary is **provenance, access, boot/storage layout, and a reviewed acquisition/recovery gap report**. It is not a Linux boot attempt. Only the five tasks below are scheduled; the later evidence areas are requirements, not a speculative issue backlog.

## Current repository state and ownership

- `SchulzCode/Y2PlayerNative`: initial commit `f00bb946d6d2196bd66e221161e126ed02834d67`, containing only `readme.md`; the supplied `YY2PlayerNativeBlueprint.md` is untracked and remains untouched.
- Android reference: `/home/luca/Dokumente/Code/Y2Player`, clean working tree at `870f2b33e49129990bc0a98eeb34d58ed42542b6` when inspected. Ignored firmware/research artifacts are separate from this Git revision and need their own hashes and provenance.
- `SchulzCode/Y2Linux` could not be resolved using the authenticated account. This does not prove it does not exist. Stage M0 issues and reviewed text in Y2PlayerNative for now; retain stable Y2E IDs. The eventual platform evidence owner remains Y2Linux, as the blueprint requires. Do not create another repository or duplicate issues as part of this batch.
- Git author/committer resolve to the existing configured user, and GitHub authentication resolves to `SchulzCode`. Preserve both. Before any future commit, verify identity again; stop on missing/incorrect configuration, never change it or add model attribution.
- Linux 6.18 is listed as longterm by [kernel.org](https://www.kernel.org/category/releases.html), checked 2026-09-08. Its designation says nothing about complete MT6582/Y2 board support. Exact kernel revision and upstream support audit remain later work.

## Reference architecture and behavior

The Android application is an API-19 HOME launcher with a custom drawn, wheel-oriented 480 × 360 interface. Inspect `AppContainer.kt`, `core/state/`, `queue/`, `input/`, `library/`, `playback/`, and their tests for behavior and boundaries, not a source-port structure.

Useful references include explicit queue ordering and shuffle passes; seek/previous/repeat rules; gapless and crossfade cancellation; ReplayGain fallback and peak handling; volume transition safety; route-loss pause; storage removal and USB export coordination; incremental metadata scanning; lazy bounded artwork; audiobook progress; playlists; backup/import; search; screen-off controls; listening history; and themes. `RELEASE_2.5.md` documents search, hardware-dependent FM, alphabet navigation, and expanded timers. The README still describes 2.3 and denies search, and the old input map denies acceleration: neither is authoritative for current behavior. Resolve behavior against source/tests and current release records.

`docs/ARCHITECTURE.md` and `docs/PLAYBACK_ENGINE.md` describe the service-to-engine boundary: one audio owner thread, bounded/coalescing commands, two decoder handles for transitions, explicit decoded/submitted/played accounting, cancellation, and persistent PCM buffers. Source inspection confirms the FFmpeg engine and fixed 44,100-Hz `PcmFormat`. C FFmpeg decoding and metadata probing cross JNI; DSP remains float until PCM16 AudioTrack output. SQLite indexes the library; other persistence policies and formats need intentional redesign for the native SQLite authority.

Retain the blueprint's Rust state/action/reducer/effect core, C decoder/DSP ABI, independent output sinks, and standard Linux platform adapters. JNI, Android services, wake locks, AudioTrack, vendor HAL APIs and permission tricks are reference context only. Existing test cases can become behavioral specifications; native implementations and tests are designed separately. Native Last.fm, modern Bluetooth, LVGL/DRM and direct ALSA are targets, not proven inherited capabilities.

## Hardware evidence already located

All archive paths below are relative to the Android repository. A confirmed archived observation is scoped to that capture, not automatically to today's connected device or every Y2 revision. This session inspected selected raw records and hashed four firmware inputs; it did not revalidate every archive or reproduce prior experiments.

| Status and fact | Evidence inspected | Implication / remaining uncertainty |
| --- | --- | --- |
| CONFIRMED in archived capture: MT6582, Cortex-A7 class ARMv7/NEON/VFPv4, stock-family Linux 3.4.67 | `out/hardware-snapshots/2026-07-29_004935/hardware/proc-cpuinfo.txt`, `proc-version.txt` | Re-identify current device and build; hardware revision remains UNKNOWN. |
| CONFIRMED in archive: I2C `1-0030` binds `cs43131_dac`, `1-0058` binds `aw87559_pa`, and `0-0051` names `APT32F` | Same capture, `hardware/i2c-identities.txt` | Driver identity does not prove complete wiring, reset/IRQ GPIOs, supplies, clocks or analog routing. |
| CONFIRMED in archive: ALSA reports no soundcards | Same capture, `audio/alsa-cards.txt` | A vendor control node is not a usable ALSA PCM sink. |
| INFERRED board data path: AFE DL1 through second I2S; PMIC analog role unresolved | `docs/Y2_AUDIO_PATH_PHASE4_SECOND_I2S.md`, `out/afe-runtime/2026-07-30_135519/` | Preserve raw register captures and exact binary hashes. Do not promote disassembly predictions into electrical proof or high-resolution support. |
| CONFIRMED in archive: `mtk-kpd`, `ACCDET`, `mtk-tpd`, `mtk-tpd-kpd`, AVRCP input devices | Snapshot `hardware/input-devices.txt` | Android keycodes are not Linux scan codes. Panel/touch/controller and wheel wiring remain to be established. |
| CONFIRMED in archive: platform node `pmic_mt6323` | Snapshot `hardware/platform-devices.txt` | Battery/charger calibration, thermal behavior, regulators, suspend and wake paths need evidence. |
| CONFIRMED static evidence: WMT/STP nodes, firmware loader and vendor Bluetooth artifacts | `reverse/ramdisk/init.project.rc`, `reverse/system/`, `docs/BLUETOOTH_STACK_INVESTIGATION.md` | Loaded hardware, transport, firmware selection and Linux compatibility are separate questions; firmware presence does not prove a radio exists/enables. |
| Historical FM report records chip ID `0x6627` and software tuning; reception and board population vary | `docs/FM_RADIO_FEASIBILITY.md`, `RELEASE_2.5.md` | Current unit's RF circuitry/reception is UNKNOWN; do not infer usable FM from chip-ID or successful open/tune. |
| CONFIRMED negative archive observations: no exported DT paths/config.gz; dmesg and cmdline denied | Snapshot `hardware/device-tree.txt`, `pulled/proc__config.gz.error.txt`, `logs/dmesg.txt`, `manifest.csv` | No exported DT is not proof of no embedded DTB. Legacy board files/ATAGs remain possible. ADB exit code 0 sometimes accompanies denial text. |

Older DAC and Bluetooth reports are partially superseded by later runtime reports. Record contradictory statements with their evidence dates and scope instead of silently selecting the most optimistic claim. The referenced `tools/collect-hardware-snapshot.ps1` is absent from the current checkout; do not blindly execute old collection instructions or resurrect an unreviewed collector.

## Recovery inputs and concrete gaps

`OriginalFirmware/` contains boot/recovery/system images, stock scatter, MBR/EBRs, LK and preloader files. Host SHA-256 checks on 2026-09-08:

| Input | SHA-256 |
| --- | --- |
| `boot.img` | `8131020fafddb864d252b13f6bc7e83584487bd182d371d7e51f6244e2412f02` |
| `recovery.img` | `1c14b5316bfc0542b4b3b3c6c75208ff6da99e7302ef7c397aaa77e33cc6292c` |
| `system.img` | `ca05f82cbb5374dc8c41163aa8c7276de2f8af4c56d7ada7b6f2c581330a8455` |
| `MT6582_Android_scatter.txt` | `e5fe03e9f3219cc9b5ead27892f2ddb722acc16adce3306d27feb87893cd977e` |

These establish local file identity, not vendor authenticity, compatibility with this unit, or successful restoration. Boot/system hashes match the earlier hardware report. The secure-ADB boot builder changes a stock copy; generated system images can contain HAL/FM/key-layout changes. Keep vendor package, extracted derivatives, modified builds and current-device dumps separately identified.

The scatter separates `EMMC_BOOT_1` from `EMMC_USER`; its MBR has linear offset `0x1400000` but physical offset `0`. BOOTIMG has linear `0x3180000`, physical `0x1d80000`, size `0x1000000`. Never use one column as another or turn these package values directly into a readback/flash command. Variable/sentinel entries such as FAT/BMTPOOL require interpretation, not arithmetic assumptions.

NVRAM, PRO_INFO, PROTECT_F/PROTECT_S, SECCFG and MISC have `file_name: NONE`; the package is not a personalized-device backup. `reverse/ramdisk/fstab` mounts protect partitions and uses vendor `/emmc@...` aliases. Identify actual nodes and calibration ownership before reading partition contents. No same-device restoration proof, independent calibration backup, current flashing-tool compatibility or current USB/preloader recovery path was established in this inspection. **Recovery remains UNKNOWN.**

## Evidence needed before a Linux experiment

This is the M0 coverage contract. Later tasks are written only after the first batch's review.

| Area | Required evidence and satisfactory outcome |
| --- | --- |
| Host/current system | Host/tool versions, UTC times, private device identity, authorization and shell privilege, build fingerprint/kernel/cmdline, mount state and known modifications; unavailable/denied results preserved. |
| Boot/storage | Boot chain, entry modes, console candidates, boot header/wrappers/load addresses, memory/reservations, partition names/nodes/start/length/units/region, eMMC boot regions versus user area, SD and USB ownership; reconcile scatter with runtime evidence before acquisition. |
| Kernel/config/DT | Exact stock image lineage, embedded config if present, DTB/DTBO candidates and structural checks or bounded negative search, ramdisk/init/fstab, modules and load order. Record board-file/ATAG uncertainty without inventing a DTS. |
| Audio/FM | Bound devices, HAL/policy/driver identities, archived route evidence, power/mute/clock/control ordering, separate speaker/headphone/FM paths; explicitly track reset/supplies/I2S/analog unknowns. No raw device ioctls or bus scans. |
| Display/input | Panel identity/interface/timings/rotation/backlight, touch and wheel/button controllers, Linux input capabilities and later bounded events, IRQ/reset/wake relationships; observed resolution alone is insufficient. |
| Power | PMIC, battery/charger/thermal identities, voltage/current units, charging limits/calibration and baseline readings; cpufreq/idle/suspend/wake evidence. No forced suspend or charger experiments in the first batch. |
| Wi-Fi/Bluetooth/firmware | Controller/transport/driver/module/firmware identity, load sequence, rfkill and calibration provenance. Keep board-specific parameters, radio addresses and pairing/network secrets private. No enable/pair/scan in this batch. |
| Recovery | Verified compatible stock package and per-device boot/recovery/calibration backups, independent backup copy and checksum verification; exact host/tool/DA/scatter and connection procedure; recovery independent of working Android; bounded stock restore rehearsal with readback and functional checks. |

## First batch and dependency boundary

| Task | One outcome | Dependencies |
| --- | --- | --- |
| Y2E-101 | Reusable artifact/provenance manifest | None; first executable task |
| Y2E-105 | Fresh non-root system/access baseline | Y2E-101 reviewed |
| Y2E-110 | Runtime/package boot partition reconciliation | Y2E-101, Y2E-105 reviewed |
| Y2E-115 | Offline stock boot structure/config/DT report | Y2E-101, Y2E-110 reviewed |
| Y2E-120 | Recovery capability and acquisition-gap decision packet | Y2E-101, Y2E-105, Y2E-110, Y2E-115 reviewed |

Run one task at a time. Only Y2E-101 starts ready. A closed issue alone does not satisfy a dependency: review its evidence and stop conditions before marking the next task ready. A documented denial or missing artifact can be a complete observation while still blocking acquisition and the M0 gate. Do not automatically continue into future subsystem or backup tasks.

Evidence location for this staging phase: reviewed text in `docs/knowledge/` and `docs/recovery/`; raw captures in an owner-controlled private directory outside Git. Raw records retain exact commands, host and remote statuses, stdout/stderr, UTC time, byte lengths and SHA-256. Public text uses a non-identifying capture ID and artifact hashes. Do not publish firmware, calibration, MAC/serial IDs, credentials or personal media. Never treat filenames, checksums or reported command success alone as proof of artifact completeness.

## M0 gates and rolling-wave review

1. **First boundary:** review the five reports, classify trustworthy/missing evidence and decide a concrete safe backup acquisition method. If ADB permissions prevent reading boot/calibration, stop; no rooting, exploits, patched boot or generic MTK tool workaround. An inaccessible path is useful evidence.
2. **Next waves, not yet issues:** separately scope passive subsystem captures, missing stock-kernel evidence, exact per-partition backups and backup verification. Investigate preloader/BROM/stock-recovery reachability in a separately reviewed operator procedure; normal Android USB enumeration is not proof of emergency recovery.
3. **Recovery rehearsal:** only after verified backups, compatible restore inputs/tooling, independent copies and an exact reviewed procedure. Obtain explicit authorization for the bounded stock restoration operation. Keep preloader, LK, partition tables and calibration outside ordinary restore writes. Do not deliberately brick or erase a device to prove recovery. If a required tool/mode cannot preserve that boundary, report it and redesign the procedure before proceeding.
4. **M0 PASS:** provenance reconciled; boot/partition/memory evidence supports a concrete later boot plan; subsystem inventory and critical unknowns are recorded; exact device backups are integrity-checked and independently retained; recovery is reachable independently of working Android; an authorized stock boot/recovery restoration rehearsal is recorded with target regions, readback, normal boot and peripheral/calibration checks. Re-evaluate recovery if the device/revision/tool/image changes. All bring-up-critical unknowns must be resolved or explicitly block the relevant next action.

The architect records the gate decision. Documentation-only recovery, flashing-tool detection, successful image builds, a package download or this first batch closing **cannot** produce M0 PASS. No experimental kernel boot, DTS work, application scaffolding or M1 issue creation is authorized by these tasks. Recovery proof is itself a controlled exception before the experimentation gate, not a circular requirement to have already passed that gate.
