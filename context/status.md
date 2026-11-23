---
load_when: "verified behavior and remaining acceptance"
last_verified: 2026-11-23
source_refs:
  - crates/fm1-live/src/studio/**
  - UPSTREAM_REVISION
---
# Status

Fresh upstream baseline: `7042b5127ce4e509ca5041f2cdb657e8e59488b9`. Existing master remains the reference implementation. This branch contains the standalone customization only; browser/plugin/core changes from the earlier fork are not carried forward.

Implemented: fixed large-screen graphite panel, pressed and Shift-held controls, upstream MIDI learn overlays, native app menus/shortcuts, release-gated F4, remembered Mac launch settings and per-firmware flash, reset backup, MiniLab User profile via upstream fm1-map, input reconnection and disconnect cleanup.

Verified locally on 2026-11-23:

| Check | Result |
|---|---|
| Native, mapping, CPU and SoC suites | 293 passed, zero failures; includes nine custom standalone regressions |
| Real Felucca fixture | Original package booted for 3000 emulated ms; active LCD and audio peak 2806 over the last half second |
| Panel rendering | Inspected the actual 1300×950 Felucca output, including firmware display, labels and pressed keys |
| Standalone app | Release build and packaging passed; strict/deep ad-hoc signature verification passed |
| Required full rebuild | Native CLI, upstream browser and CLAP/AU/VST3 bundles built; all four native bundles passed strict/deep signature verification; no plugin installation |
| Shared upstream boundary | Protected runtime sources match UPSTREAM_REVISION exactly; icon-only plugin build hooks are documented exceptions |
| Documentation | Context metadata, references, routing and links passed |

The first native run lacked the optional Felucca fixture; the later run used a local ignored fixture link and exercised the upstream firmware-dependent standalone tests. No firmware is tracked. The headless smoke used upstream's shot/record path and did not open audio devices or change session preferences.

Pending manual acceptance: native menu open/cancel and recording shortcuts, actual Finder session reopening, reset/restart inside the app, physical MiniLab User program/relative controls/bank switching, and live controller reconnection. Deterministic tests cover mapper behavior and disconnect cleanup, not the physical controller. Windows/Linux and unchanged upstream browser/plugin hosts were not runtime-tested. This branch does not establish perceptual audio equivalence or a latency guarantee.

Global hotkeys and a persistent status bar item are outside the initial scope. Existing installed applications/plugins and the old master worktree remain unchanged.


## Retained app identity and rebuild contract

The standalone macOS app, complete original icon sources/exports, icon regeneration/portable archive tools and platform embedding hooks are retained. The original 33 icon source/export files (excluding the updated inventory prose) and all browser exports were compared byte-for-byte with the prior fork. App and plugin bundles use the original Assets.car and ICNS resources; browser branding is applied to build output without changing upstream web sources.

The standalone regression suite passed all 28 tests after restoring the native icon hooks. All local targets were rebuilt with plugin installation disabled. Resource-byte and plist metadata checks, strict/deep native signatures, browser manifest/favicon links, artwork validation and portable archive checks passed. Windows/Linux and mobile presentation and current Finder appearance remain separate manual acceptance; no artwork was regenerated or redesigned in this change.

AGENTS.md and development.md explicitly require rebuilding the standalone macOS app and every other local target after every change or merge, including documentation-only edits, before handoff. The icon workflow is routed through icons.md so future agents retain the editable source and platform distinctions.
