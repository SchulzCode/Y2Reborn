# Reborn documentation

Begin with [current Reborn state](CURRENT_REBORN_STATE.md) and the paired
[Y2Linux state](../../Y2Linux/docs/CURRENT_PLATFORM_STATE.md). The latest
[Fix01 physical qualification](../../Y2Linux/docs/validation/Y2-CPU-FINAL-FIX01-PHYSICAL-QUALIFICATION.md)
tests real workloads and playback but fails overall CPU-platform acceptance.

| Need | Start here |
| --- | --- |
| Exact current application and physical scope | [Current Reborn state](CURRENT_REBORN_STATE.md) |
| All pages by scope | [Complete catalog](DOCUMENTATION_CATALOG.md) |
| Media / live volume | [FFmpeg stack](architecture/reborn-audio-stack-ffmpeg9.md), [volume](architecture/live-volume.md) |
| UI source and software candidate | [Product UI v2](ui/REBORN-PRODUCT-UI-V2.md), [navigation v2](ui/REBORN-PRODUCT-NAVIGATION-V2.md), [pass v2](review/REBORN-PRODUCT-PASS-V2.md); historical [UI v1](ui/REBORN-UI-V1.md), [candidate report](ui/REBORN-UI-V1-REPORT.md) |
| Platform data/ownership | [Linux API](../../Y2Linux/docs/architecture/platform-api-v1.md) |
| Earlier correctness closure | [Luna closure](validation/LUNA-CORRECTNESS-CLOSURE-01.md) |
| Earlier system/codec audits | [Audit entry](audit/current-system/00_SCOPE_AND_EVIDENCE.md), [codec design](architecture/bluetooth-codecs.md) |

Historical audit, UI, validation and asset documents retain their recorded
date/candidate. A source audit or passed host test does not certify physical
Y2 behavior; Fix01's latest observed image differs from some older candidates.
Follow the shared [knowledge rules](../../Y2Linux/docs/KNOWLEDGE_BASE.md).
Raw private Y2 captures remain in Linux's ignored `out/`, not this repo.
