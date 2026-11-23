---
load_when: "visual styling, panel geometry and interaction"
last_verified: 2026-11-23
source_refs:
  - crates/fm1-live/src/studio/panel.rs
  - crates/fm1-live/src/studio/menus.rs
  - assets/icons/**
---
# Design system

The standalone ports the approved graphite instrument from the previous fork: charcoal body, quiet ivory labels, flat controls, enlarged original 240×240 firmware display and separate pressed-state edge rings and LED illumination. The firmware framebuffer is rendered unchanged, using nearest-neighbor scaling. No simulated firmware UI or invented status values.

The panel uses the measured artwork geometry inherited from that design, on a fixed 1300×950 canvas fitted by upstream's window code. This is the existing-artwork exception to the base-8 system. Any new controls/layout spacing use multiples of 8; 1 px borders and 2 px focus outlines are allowed exceptions.

Mouse key height retains upstream velocity/pressure behavior. Shift-click holds a key/button; another Shift-click or a plain click releases it. MIDI controller holds also appear pressed. Right-click remains upstream MIDI learn; F1 toggles its labels and F2 changes the controller's key-routing mode. There is no layout switch that competes with F2.

Menus: File (open, close), Audio (WAV recording), Instrument (reset), View (MIDI labels). Cmd+O opens, Cmd+W closes, Cmd+Shift+R records. Reset names the consequence and requires confirmation; existing saved flash is archived after shutdown. These are application menus, not a status item or global shortcut service. The macOS adapter uses minifb's CTRL flag for Command equivalents and attaches custom menus through AppKit while retaining minifb action items, keeping tiling/window-list commands out of File/Audio/Instrument/View.

Inter Tight is bundled with its SIL OFL license; there is no font download. The complete approved icon sources and platform exports are retained under assets/icons/. The standalone and every macOS plugin bundle use Assets.car plus ICNS fallback; browser exports are applied during packaging. Read [the icon contract](icons.md) before changing artwork or packaging. Do not edit compiled assets manually. Build output stays under target/.

The canvas retains upstream accessibility limits. Do not claim full keyboard or screen-reader accessibility from rendering tests. Real macOS menu, display scaling and physical-controller acceptance must be recorded separately.
