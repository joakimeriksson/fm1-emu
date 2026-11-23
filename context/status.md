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

Remaining manual acceptance is narrowed by the interactive checks below: final menu layout/recording shortcut/Shift-click checks, reset/restart inside the app, physical MiniLab User program/relative controls/bank switching, and live controller reconnection. Deterministic tests cover mapper behavior and disconnect cleanup, not the physical controller. Windows/Linux and unchanged upstream browser/plugin hosts were not runtime-tested. This branch does not establish perceptual audio equivalence or a latency guarantee.

Global hotkeys and a persistent status bar item are outside the initial scope. Existing installed applications/plugins and the old master worktree remain unchanged.


## Retained app identity and rebuild contract

The standalone macOS app, complete original icon sources/exports, icon regeneration/portable archive tools and platform embedding hooks are retained. The original 33 icon source/export files (excluding the updated inventory prose) and all browser exports were compared byte-for-byte with the prior fork. App and plugin bundles use the original Assets.car and ICNS resources; browser branding is applied to build output without changing upstream web sources.

The standalone regression suite passed all 28 tests after restoring the native icon hooks. All local targets were rebuilt with plugin installation disabled. Resource-byte and plist metadata checks, strict/deep native signatures, browser manifest/favicon links, artwork validation and portable archive checks passed. Windows/Linux and mobile presentation and current Finder appearance remain separate manual acceptance; no artwork was regenerated or redesigned in this change.

AGENTS.md and development.md explicitly require rebuilding the standalone macOS app and every other local target after every change or merge, including documentation-only edits, before handoff. The icon workflow is routed through icons.md so future agents retain the editable source and platform distinctions.


## Interactive launch acceptance

Live testing on 2026-11-23 found and fixed three host integration problems:

- Saved launch arguments were checked only for file existence. Recall now validates the full launch with the upstream loader; incompatible firmware or state returns to the picker without changing those files. Regression tests cover an existing unsupported input and a compatible local firmware.
- Finder startup spawned a child instrument while the registered launcher waited. macOS now uses the existing exec-based restart path and retains one registered app process. Windows retains upstream startup behavior.
- minifb maps MENU_KEY_CTRL to Command on macOS, while MENU_KEY_COMMAND alone produces no modifier. The menu flags are corrected. AppKit attaches the minifb action menus directly to avoid minifb assigning every menu the system Window role.

| Live check | Evidence |
|---|---|
| Finder icon | Approved blank-screen preview, full enclosure, no white frame or clipped controls |
| Finder reopening | User-selected local Felucca 1.8.3 reopened without the picker after the single-process fix |
| Instrument window | Actual graphite panel and original firmware display inspected |
| Keyboard and recording | A held A key produced a finalized 44.1 kHz stereo WAV with non-zero audio (peak 6615) |
| Audio menu | Recording started and stopped using its menu action |
| Firmware selection | File-menu open/cancel and release-gated F4 observed working |
| Command shortcuts | Cmd+O opened the picker after correcting the flag; Cmd+W closed the app; final Cmd+Shift+R recording validation remains pending |
| Clean menu contents | Audio displayed only its recording action after native attachment; final File-menu order needs visual recheck |
| Regression suite | 30 standalone tests passed with the recall regressions |

The automation connection then returned cgWindowNotFound for both the instrument and Finder, including after resetting the connection. This prevented final visual checks of the last menu-order adjustment and Shift-click holds. The instrument process was still executing its renderer. Do not turn this tool limitation into a successful visual acceptance claim. Real MiniLab controls, audible output judged by the user, firmware-owned save/reset and complete session-state recall remain unverified.

Plain .area input, including the user's previous SLOOP session, is rejected by this upstream baseline. Support from the earlier fork has not been ported and remains a migration blocker for that workflow before merging. The previous launch arguments were copied to ignored target/app-acceptance/ before selecting test firmware; original firmware and SLOOP flash were not changed.

## MiniLab activation fix and scope enforcement

Physical input monitoring captured absolute CC74 and channel-10 note 36 from the standard MiniLab port, confirming factory-style output. Automatic mode now selects factory-compatible profile data; the relative User program remains explicitly selectable in the native Controller menu. Profile changes apply live through upstream Mapper::set_profile, release holds and persist without restarting firmware. Five new regressions cover captured messages, both factory pad banks, late connection/port exclusion, live switching and launch preference preservation. All 35 standalone tests pass. Full physical bank/program acceptance remains pending after relaunch.

AGENTS.md now requires the fork policy before edits. The existing CI gate checks the full delta and untracked files against a finite allowlist, rejects upstream-file deletion and preserves the upstream README. The shared runtime remains upstream-identical.

## Factory knob order

Moved Master from knob 8 to knob 5, followed by Select, Presets and Algorithm on knobs 6–8. Knobs 1–4, the main encoder, fader and pad bindings retain their assignments. The change is profile data only; the relative User program is unchanged. A routing regression covers every factory knob.

## MiniLab keyboard routing

Both bundled MiniLab profiles now route channel-1 notes through upstream panel-key mapping instead of firmware MIDI IN, matching on-screen key actions. A host-routing regression checks C4 pitch position, velocity, note-off and zero-velocity release without duplicate MIDI delivery. Audible crackle resolution still needs confirmation on the rebuilt app with the physical controller. No synthesis or upstream mapper code changed.
