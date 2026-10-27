# Reborn documentation

Reborn 1.0.0-beta.8.41 is the player in **Y2Linux 1.0 Beta 8.41** (Baseline 159, 2026-10-27, tag `baseline-159`), built
with Rust 1.99. Start with [current Reborn state](CURRENT_REBORN_STATE.md), the
[Product UI v4](ui/REBORN-PRODUCT-UI-V4.md) design and the paired
[Y2Linux state](https://github.com/SchulzCode/Y2Linux/blob/main/docs/CURRENT_PLATFORM_STATE.md).
To build and install it, use Y2Linux's
[BUILDING](https://github.com/SchulzCode/Y2Linux/blob/main/docs/BUILDING.md) and
[INSTALL](https://github.com/SchulzCode/Y2Linux/blob/main/docs/INSTALL.md) guides.

| Need | Start here |
| --- | --- |
| Exact current application and physical scope | [Current Reborn state](CURRENT_REBORN_STATE.md) |
| Skins: format, screens, elements, installing | [Reborn skins](skins.md) |
| Responsiveness, rendering, startup and idle measurements | [Performance pass](validation/REBORN-PLATFORM-V1-PERFORMANCE.md) |
| All pages by scope | [Complete catalog](DOCUMENTATION_CATALOG.md) |
| Media / live volume | [FFmpeg stack](architecture/reborn-audio-stack-ffmpeg9.md), [volume](architecture/live-volume.md) |
| Platform 1.0 metadata and ingest evidence | [FFmpeg/metadata/library pass](validation/REBORN-PLATFORM-V1-FFMPEG-METADATA.md) |
| UI source and software candidate | [Product UI v4](ui/REBORN-PRODUCT-UI-V4.md) (current, with the Beta 7.2–7.6 refinements); earlier [Product UI v3](ui/REBORN-PRODUCT-UI-V3.md), [Product UI v2](ui/REBORN-PRODUCT-UI-V2.md), [navigation v2](ui/REBORN-PRODUCT-NAVIGATION-V2.md), [pass v2](review/REBORN-PRODUCT-PASS-V2.md); historical [UI v1](ui/REBORN-UI-V1.md), [candidate report](ui/REBORN-UI-V1-REPORT.md) |
| Boot progress and display hand-off | [Boot hand-off](architecture/boot-handoff.md) |
| SQLite / large-library performance | [Platform 1.0 database pass](validation/REBORN-PLATFORM-V1-LIBRARY-DB.md) (host and bounded native Y2 through 50k; ARM build) |
| Platform data/ownership | [Linux API](https://github.com/SchulzCode/Y2Linux/blob/main/docs/architecture/platform-api-v1.md) |
| Earlier correctness closure | [Luna closure](validation/LUNA-CORRECTNESS-CLOSURE-01.md) |
| Earlier system/codec audits | [Audit entry](audit/current-system/00_SCOPE_AND_EVIDENCE.md), [codec design](architecture/bluetooth-codecs.md) |

Historical audit, UI, validation and asset documents retain their recorded
date/candidate. A source audit or passed host test does not certify physical
Y2 behavior; each physical report identifies its tested image and artifact.
Follow the shared [knowledge rules](https://github.com/SchulzCode/Y2Linux/blob/main/docs/KNOWLEDGE_BASE.md).
Raw private Y2 captures remain in ignored `out/`; sanitized performance summaries
are retained beside their report.
