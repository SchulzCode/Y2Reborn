# Y2Linux + Y2Reborn: current-system audit

<!-- knowledge-base-scope: historical-audit/review -->
> **Historical record.** The dates, candidate identity, "current" claims,
> next steps and permissions below belong to this recorded boundary. See
> [current state](../../CURRENT_REBORN_STATE.md) for the latest physically observed result.

Audit date: 2026-09-22. This is one product review, not two independent repository reviews. No production code, configuration, firmware, Git identity, milestones or hardware state was changed. No build, flash, device connection, charging experiment or production commit was performed. Recommendations are proposals, not implementation or hardware authorization.

## Exact scope

| Repository | Audited HEAD | Initial worktree |
| --- | --- | --- |
| `/home/luca/Dokumente/Code/Y2Linux` | `be7e64c5dd5c6bdfe39c35e8fbd70e6b4b2c4717` | Clean |
| `/home/luca/Dokumente/Code/Y2Reborn` | `011884b7ef13187171225f252c213afeeb6da851` | Clean |

The application, native C boundaries, platform integration, production build/packaging scripts, kernel policy/board drivers and representative patches/tests were inspected. The imported Wi-Fi implementation and upstream dependencies were reviewed through integration seams and relevant source, not subjected to an exhaustive line-by-line vulnerability audit. This review covers all 38 requested areas; it is not certification of every driver or physical behavior.

Repository source, cached host binaries, retained cross-build outputs, package metadata and historical physical observations are separate evidence classes. A package naming current commits does not prove those commits are running on a device. A retained full Buildroot tree does not necessarily describe the latest reused image byte-for-byte.

The [standing milestone rule](../../../../Y2Linux/docs/planning/roadmap-gap-audit.md#standing-milestone-boundary-rule) was read. This audit does not activate/close a milestone or expand hardware/memory/production scope. Its correction roadmap remains here for owner decision. Before any such boundary is crossed, repeat the standing evidence/gap audit and update the authoritative roadmap; this document is not permission to implement its recommendations. Unchanged ROM/recovery provenance was not repeated.

## Reading order

Start with [executive summary](01_EXECUTIVE_SUMMARY.md), [master findings](13_MASTER_FINDINGS.md), and [priority roadmap](12_PRIORITY_ROADMAP.md). Consult:

1. [Actual architecture](02_SYSTEM_ARCHITECTURE.md)
2. [Platform/build/recovery/security/licensing](03_Y2LINUX_AUDIT.md)
3. [Application/library/state/resources](04_REBORN_AUDIT.md)
4. [Audio/FFmpeg/high-resolution/processing](05_AUDIO_AUDIT.md)
5. [Connectivity](06_CONNECTIVITY_AUDIT.md)
6. [Power/platform](07_POWER_PLATFORM_AUDIT.md)
7. [UI and input/UX](08_UI_UX_AUDIT.md)
8. [Tests and physical evidence](09_TEST_VALIDATION_AUDIT.md)
9. [Release gaps and entirely missing behavior](10_RELEASE_GAPS.md)
10. [Technical debt](11_TECHNICAL_DEBT.md)
11. [Request coverage](14_COVERAGE_MATRIX.md)
12. [Executed checks](15_EXECUTED_CHECKS.md)

## Vocabulary

Evidence labels apply to a precisely scoped claim, not an entire subsystem:

| Label | Meaning |
| --- | --- |
| VERIFIED | Implementation and meaningful validation strongly support this claim; the validation tier must still be stated. |
| IMPLEMENTED | Code exists; integration/physical proof is incomplete. |
| PARTIAL | Only some required behavior exists. |
| WEAK | Existing behavior has concrete architecture/correctness/quality concerns. |
| MISSING | No meaningful implementation of the specified behavior was found in the inspected production path. |
| UNKNOWN | Evidence does not support a responsible conclusion. |
| DEPRECATED / DEAD | Superseded or inactive path; not automatically safe to delete. |

Confidence is HIGH, MEDIUM or LOW. HIGH confidence in a code defect does not mean the defect was physically reproduced. The master table's GOOD / IMPROVE / PARTIAL / MISSING / UNKNOWN / DEAD/LEGACY classification is a decision category, separate from the evidence label.

Validation tiers are: implemented in source; built; host unit/fixture tested; integration tested; physically qualified on Y2; production-ready. None implies the next automatically. No complete subsystem receives a blanket production-ready designation in this audit.

## Evidence authorities and limits

* Production platform intent: `Y2Linux/tools/production/build.py`, `buildroot/configs/y2_production_defconfig`, `kernel/config/production.config`, `kernel/dts/innioasis-y2.dts`, `kernel/patches/manifest.json`, production initramfs and overlays.
* Application authority: `Y2Reborn/app/reborn/src/{main,playback}.rs`, `crates/*`, locked/vendored Cargo inputs, and the Buildroot `reborn` package. Preview binaries are not production rendering evidence.
* Retained complete native dependency build: `Y2Linux/out/y2linux-reborn-audio-final/buildroot`. Its generated FFmpeg component headers and ELF boundaries were checked, not rebuilt.
* Latest retained package inspected: `Y2Linux/out/y2linux-reborn-ui-polish-02`; paired build receipts under the corresponding `-build` directory. `versions.json` names both audited HEADs. `userspace-source.json` explicitly describes retention of existing root/data images and rebuilding rescue programs. Read-only `debugfs` confirmed the image's wired qualification profile and media membrane presence; no exhaustive payload-to-HEAD proof is claimed.
* Physical display/GPU/audio evidence: [GPU01 records](../../../../Y2Linux/docs/hardware-evidence/2026-09-18-gpu01/README.md), including raw rendering/audio/suspend text and owner observations. This proves a narrower, older baseline than the complete current Reborn product.
* Physical connectivity evidence: [CONNECTIVITY-10](../../../../Y2Linux/docs/hardware-evidence/2026-09-17-m5-connectivity10/README.md) and its sanitized observations. Adapter/scanning success is not association or audio qualification.
* Power acceptance: local `Y2Linux/out/m4-owner-acceptance.md` records owner acceptance of POWER-03, explicitly not a new measured charging/thermal series. Its source/image identity predates later charger changes.
* Reborn validation reports distinguish candidates from installation; notably [UI-POLISH-02](../../validation/REBORN-UI-POLISH-02.md) explicitly leaves physical viewing pending. A commit title mentioning physical legibility is not a deployment receipt.

Local `out/` and `target/` files are available evidence, not durable tracked source. Important observations from them are transcribed into this audit. Historical documentation is cited for what it actually records, not treated as current truth.

## Architectural history relevant to interpretation

Git history shows progression from rescue/boot and conservative storage support through native ASoC, charging/power, CONSYS Wi-Fi/Bluetooth and Lima, followed by Reborn and rapid UI/audio revisions. Reborn's FFmpeg transition (`65c6ea0`), lazy media loading (`e3e9320`), startup work (`7d61cdf`), wheel correction (`a1f37eb`), UI reset (`bd28e10`), control wiring (`c2ab6cb`), radio forgetting (`404cb42`) and current polish are distinct transitions. Linux charger change `3dc0a8b` materially changes deep-recovery charging policy after older acceptance evidence.

The authorship/model used previously is not evidence of quality. Findings below follow code and validation, not the user's concern about Luna or the confidence of earlier reports.
