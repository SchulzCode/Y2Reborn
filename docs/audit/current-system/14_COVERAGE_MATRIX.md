# Coverage of the requested audit

This index shows where each requested area is assessed. It is not a claim that every possible runtime case was exercised. Physical UNKNOWN items are deliberately retained rather than filled in from completion reports.

| Request | Main assessment | Principal findings |
| --- | --- | --- |
| 1 Repository quality | [Platform](03_Y2LINUX_AUDIT.md), [application](04_REBORN_AUDIT.md), [debt](11_TECHNICAL_DEBT.md) | F14/F30; module ownership and live-vs-legacy distinction |
| 2 Build/reproducibility | [Platform](03_Y2LINUX_AUDIT.md) | F13; pinned inputs vs incomplete clean acquisition/pairing |
| 3 Boot/recovery/flashing | [Platform](03_Y2LINUX_AUDIT.md), [map](02_SYSTEM_ARCHITECTURE.md) | G01/G02/F31 |
| 4 Kernel/DT | [Platform](03_Y2LINUX_AUDIT.md), [power](07_POWER_PLATFORM_AUDIT.md) | G03/F29; ownership/reservations/PM constraints |
| 5 Display/GPU | [UI engineering](08_UI_UX_AUDIT.md), [power](07_POWER_PLATFORM_AUDIT.md) | G04/F17/F22 |
| 6 Input | [UI/input](08_UI_UX_AUDIT.md) | G07/F20; semantic mapping vs backlog/loss handling |
| 7 Power/charging | [Power](07_POWER_PLATFORM_AUDIT.md) | F16/F17; current-code and physical-evidence distinction |
| 8 Storage | [Platform](03_Y2LINUX_AUDIT.md), [application](04_REBORN_AUDIT.md) | F05/F07/F26 |
| 9 Wi-Fi | [Connectivity](06_CONNECTIVITY_AUDIT.md) | F18; scan is not connection |
| 10 Bluetooth | [Connectivity](06_CONNECTIVITY_AUDIT.md) | F19; actual codecs, trust/reconnect/AVRCP |
| 11 Hardware audio path | [Audio](05_AUDIO_AUDIT.md) | AFE/I2S/DAC/mixer/PM trace |
| 12 32-bit/high-res claims | [Audio trace](05_AUDIO_AUDIT.md) | F10; S16/44100 output, DBL limiter, precision tiers |
| 13 FFmpeg | [Audio](05_AUDIO_AUDIT.md) | Verified 9.0.1/generated exact components; F09/F13 |
| 14 Audio processing | [Audio](05_AUDIO_AUDIT.md) | G05/F10/F12; one engine, extra transition conversions |
| 15 ReplayGain | [Audio](05_AUDIO_AUDIT.md) | F12; four tags, gain/headroom/validation gaps |
| 16 Gapless | [Audio](05_AUDIO_AUDIT.md), [tests](09_TEST_VALIDATION_AUDIT.md) | F11; limited valid continuity proof |
| 17 Crossfade | [Audio](05_AUDIO_AUDIT.md) | F04; offered durations exceed sink block contract |
| 18 Playback engine | [Audio](05_AUDIO_AUDIT.md), [application](04_REBORN_AUDIT.md) | F01–F04/F11; ownership/cancellation/buffering/errors |
| 19 Bluetooth audio path | [Connectivity](06_CONNECTIVITY_AUDIT.md), [audio](05_AUDIO_AUDIT.md) | Shared processed PCM; wireless-specific uncertainty |
| 20 Music library | [Application](04_REBORN_AUDIT.md) | G06/F05/F08/F09/F23 |
| 21 Database | [Application](04_REBORN_AUDIT.md) | G06/F05/F07; schema/transactions/errors |
| 22 Metadata/artwork | [Application](04_REBORN_AUDIT.md), [audio](05_AUDIO_AUDIT.md) | F24; bounds/cache/identity |
| 23 Queue/playback model | [Application](04_REBORN_AUDIT.md) | F03/F08 |
| 24 App state architecture | [Application](04_REBORN_AUDIT.md), [map](02_SYSTEM_ARCHITECTURE.md) | G08; useful existing boundary, concrete authority drift |
| 25 UI implementation | [UI engineering](08_UI_UX_AUDIT.md) | G04/G08/F21–F23 |
| 26 UX/input | [UI/UX](08_UI_UX_AUDIT.md) | F03/F08/F20/F21; no redesign |
| 27 Persistence | [Application](04_REBORN_AUDIT.md) | G09/F05–F07; per-setting ownership and durability |
| 28 Observability | [Application](04_REBORN_AUDIT.md) | G10/F25; useful bounds, misleading counters |
| 29 Error handling | [Application](04_REBORN_AUDIT.md), [audio](05_AUDIO_AUDIT.md) | Guarded/test unwraps vs real swallowed/ordered errors |
| 30 Memory/resources | [Application](04_REBORN_AUDIT.md), [audio](05_AUDIO_AUDIT.md), [checks](15_EXECUTED_CHECKS.md) | F01/F04/F23/F24; measured host leak signal, target UNKNOWN |
| 31 Performance | [Application](04_REBORN_AUDIT.md), [release](10_RELEASE_GAPS.md) | F23/F28; derived bounds, no invented Y2 numbers |
| 32 Battery/flash wear | [Application](04_REBORN_AUDIT.md), [power](07_POWER_PLATFORM_AUDIT.md) | F06/F24/F28 |
| 33 Security/privacy | [Platform](03_Y2LINUX_AUDIT.md), [application](04_REBORN_AUDIT.md) | G10/F32 |
| 34 Licensing | [Platform distribution section](03_Y2LINUX_AUDIT.md) | F15; actual assets/libraries/firmware, legal uncertainty explicit |
| 35 Test quality | [Validation review](09_TEST_VALIDATION_AUDIT.md) | F27; behavior/fixture quality, not counts |
| 36 Test gaps | [Validation review](09_TEST_VALIDATION_AUDIT.md) | Ten ordered risk-focused groups |
| 37 Documentation | [Platform](03_Y2LINUX_AUDIT.md), [validation](09_TEST_VALIDATION_AUDIT.md), [debt](11_TECHNICAL_DEBT.md) | F14/F27/F30; stale specific examples |
| 38 Release readiness | [Release gaps](10_RELEASE_GAPS.md), [roadmap](12_PRIORITY_ROADMAP.md) | Concrete alpha/stable gates, not blanket “not ready” |

Cross-cutting deliverables: [actual system map](02_SYSTEM_ARCHITECTURE.md), [master findings with severity/confidence/evidence](13_MASTER_FINDINGS.md), [missing-entirely matrix](10_RELEASE_GAPS.md), [drift/overengineering/underengineering](11_TECHNICAL_DEBT.md), and [Top 10 preserve / Top 10 next problems](01_EXECUTIVE_SUMMARY.md).
