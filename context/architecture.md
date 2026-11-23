---
load_when: "boundaries, standalone host integration and persistence"
last_verified: 2026-11-23
source_refs:
  - crates/fm1-live/src/main.rs
  - crates/fm1-live/src/ctl.rs
  - crates/fm1-live/src/studio/**
---
# Architecture

```text
Standalone main + controller adapter (small upstream edits)
  ├─ studio/panel.rs          approved renderer, upstream panel interface
  ├─ studio/menus.rs          macOS menu IDs and shortcuts
  ├─ studio/shortcuts.rs      release-gated firmware picker
  ├─ studio/session.rs        launch preferences and flash paths
  ├─ studio/midi.rs           discovery, disconnect cleanup and shutdown
  └─ studio/minilab.json      bindings interpreted by upstream fm1-map
                 ↓
Unmodified upstream machine, firmware loader and audio infrastructure
```

The shared upstream panel.rs stays untouched for browser/plugin users. Only the standalone aliases studio::panel. The custom renderer exposes upstream's size, geometry, hit-testing and drawing interface; MIDI learn overlays use the same coordinates as the rendered controls. There is one fixed large-screen layout and no extra layout settings or alternate runtime.

The MiniLab profile is a bundled choice in the standalone controller adapter, not a modified mapping engine. It is selected explicitly because relative User settings differ from factory settings. User profiles with the same name override the bundled profile. Mapped commands are consumed before MIDI channel reassignment. CC120/123 releases mapper holds before forwarding panic. A discovery thread reconnects input ports and releases mapped controls plus sustain/notes on disconnect. Auto profile selection still happens at startup; explicitly selected profiles also work when the controller arrives later. As upstream does, the host shares one mapper across mapped inputs; use a dedicated MiniLab input filter.

Menu items and keyboard shortcuts use the existing open/record/close handlers. F4 opens on release after an idle frame. Command shortcuts suppress musical key input. Shift-click toggles a held panel key/button; a plain click releases a latched control. No global hotkeys or persistent status bar item are included.

On macOS, successful GUI launches remember arguments under ~/Library/Application Support/FM-1 Studio/sessions. Stored file arguments use absolute paths so Finder does not depend on a terminal directory. A missing remembered firmware returns to the picker. Default flash filenames are keyed by original firmware bytes; explicit --flash paths take precedence. Same-firmware simultaneous runs share default memory; use separate --flash paths for isolation. Diagnostics do not opt into automatic session memory.

Reset shuts down emulation, lets upstream save flash, archives that file, then restarts. An archive failure cancels restart. Flash recall requires firmware-owned Save and does not restore unsaved RAM, held notes or play position. Existing legacy sessions are not migrated automatically. No parser, firmware boot or hardware-transfer code is changed.
