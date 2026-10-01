# Current Reborn state on the Y2

Updated 2026-10-01. **Software candidate (not flashed, not physically
qualified):** Reborn **Product UI v2** (0.2.0) on the CPU Final Fix03
platform. Smaller product surface, typed platform boundary, one-detent wheel,
Reborn boot mark and dark shutdown. See [Product UI v2](ui/REBORN-PRODUCT-UI-V2.md),
the [pass record](review/REBORN-PRODUCT-PASS-V2.md) and the
[Y2Linux receipt](../../Y2Linux/docs/validation/Y2-REBORN-PRODUCT-UI-V2.md).
Everything below describes the last physically observed application.

Physical observation from the 2026-09-27 CPU Final Fix01 run. This page
describes the exact already-flashed application, not a new image or software
build. The [paired Linux state](../../Y2Linux/docs/CURRENT_PLATFORM_STATE.md)
and [physical report](../../Y2Linux/docs/validation/Y2-CPU-FINAL-FIX01-PHYSICAL-QUALIFICATION.md)
are the authority for hardware/platform results.

| Identity | Last physical observation |
| --- | --- |
| Reborn compiled source | `36db1869c6bab1ad2d00b7d8e7807ea6ef5b3803` |
| Linux compiled source | `0de6e951b438bf0f2d701e23e476d2b2405ba3c6` |
| Kernel/root | `6.18.0-y2linux-cpu-final-fix01` / `2025.02.18-platform-v1.7` |
| Release | `1.0.0-cpu-final-fix01-candidate.1` |
| Latest observed boot | `81e422f3-43cd-4ad6-877f-692eb1191b36` |

The user interface, controls, artwork, scanner and platform workers generated
**accepted real workload leases** for Idle, PlaybackNormal, PlaybackHeavy,
Interactive, ArtworkDecode, LibraryScan, NetworkTransfer and Maintenance.
Interaction lasted roughly 250 ms, coalesced and expired; screen-off released it
immediately while playback's lease remained. The kernel governor selects MHz.
Isolated client tests also covered renew/expiry/close/overlap/killed-process
cleanup; actual Reborn process-death restart was not physically tested.

Bounded native 44.1-kHz and 24/96 -> 48-kHz silent fixtures played with no ALSA
XRUN, FFmpeg decode/filter errors or corrupt packets. Silent fixtures establish
software operation, not listening quality. Crossfade was enabled through the real
UI to produce PlaybackHeavy, then restored to zero; an actual track-transition
blend and independent EQ path remain untested. The 200-file scan was a small
fixture run, not a 20k-track endurance result. The measured `decoder_stalls`
counter represents a full producer queue in current source, not corruption or
an ALSA underrun. Source output precision and native high-rate hardware are
separate from decoding and conversion.

The Fix01 platform qualification **FAILS overall**: SLIDLE never entered;
1196/1300 MHz stayed safely gated; no same-boot full suspend restore was
demonstrated; a loaded USB transfer lost connectivity while the UI stayed
usable. Four independent small Wi-Fi roundtrips and the one-retry radio restore
passed. No claim about actual Bluetooth peer playback, visual legibility,
analog fidelity, battery life, full wake or public release is made here.

Current software ownership remains [media](architecture/reborn-audio-stack-ffmpeg9.md),
[live volume](architecture/live-volume.md), [platform API](../../Y2Linux/docs/architecture/platform-api-v1.md),
and [UI v1](ui/REBORN-UI-V1.md). The [Luna correctness closure](validation/LUNA-CORRECTNESS-CLOSURE-01.md),
[UI candidate report](ui/REBORN-UI-V1-REPORT.md) and
[older system audit](audit/current-system/00_SCOPE_AND_EVIDENCE.md) are dated
software/review records. Refer to the [document catalog](DOCUMENTATION_CATALOG.md)
before treating their older "current" or "next" text as today's policy.

Next CPU platform work is one coherent Fix02 plan: PM diagnostic safety,
PWRAP readiness, MMC runtime clocks/coordinator reachability and loaded USB
recovery. This page does not authorize those code changes, flashing or a new
application release. It preserves source, candidate and physical evidence tiers.
