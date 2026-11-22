---
load_when: "setting up or changing the dedicated MiniLab 3 FM-1 controller program"
last_verified: 2026-11-22
source_refs:
  - crates/fm1-soc/src/controller.rs
  - crates/fm1-live/src/main.rs
  - crates/fm1-plugin/src/lib.rs
  - crates/fm1-plugin/src/engine.rs
  - crates/fm1-plugin/src/editor.rs
  - web/midi-input.js
  - web/main.js
  - web/index.html
  - web/studio/controller-profile.test.mjs
---
# MiniLab 3 FM-1 User program

The dedicated program maps physical FM-1 controls, so each firmware keeps its own page meanings and button combinations. It requires a MiniLab User program and explicit selection in the emulator. The factory profile remains available with its previous assignments; do not select FM-1 User while sending factory absolute knob values.

## Configure the controller

Use current Arturia MIDI Control Center and MiniLab firmware supporting relative encoders (1.1 or later). Recall/back up an existing User memory before replacing it. Create a local template named FM-1 and configure it as below, then store it to an unused User memory. Enable that memory and select it on MiniLab using Shift + Prog (pad 3). This software does not upload a template or change controller settings automatically.

Set the eight small knobs to CC output on channel 1, with the following CCs. Knob 1 uses Absolute, minimum 0, maximum 127. Knobs 2–8 use Relative 1 (binary offset: 63 decrements one step, 64 rests, 65 increments one step), not Relative 2/3 or an absolute 63–65 range. Repeated 65 values must continue turning the FM-1 encoder.

| Knob | CC | FM-1 | Mode |
|---|---|---|---|
| 1 | 74 | MASTER | Absolute |
| 2 | 71 | SELECT | Relative 1 |
| 3 | 76 | PRESETS | Relative 1 |
| 4 | 77 | ALGORITHM | Relative 1 |
| 5 | 93 | K1 | Relative 1 |
| 6 | 18 | K2 | Relative 1 |
| 7 | 19 | K3 | Relative 1 |
| 8 | 16 | K4 | Relative 1 |

For the main encoder, retain CC114 for turning and CC112 for Shift-turn, both on channel 1 with the factory 64-centered relative output. Configure click as CC115, channel 1, Gate, On 127, Off 0. Leave Shift as CC9 and Shift-click as CC113 (or turn Shift-click output off). Their MIDI messages are consumed without pressing FM-1 SEL; MiniLab's local Shift functions remain available.

In each pad bank, set every pad to Output CC, MIDI channel 16 (fixed, not Global), Type Gate, On Value 127, Off Value 0. Do not use Toggle or Mackie output. Configure separate CC numbers for banks A and B:

| Pad | Printed label | Bank A action / CC | Bank B action / CC |
|---|---|---|---|
| 1 | Arp | HOME / 20 | ENV / 28 |
| 2 | Pad | FX / 21 | LFO / 29 |
| 3 | Prog | EDIT / 22 | ARP / 30 |
| 4 | Loop | SEQ / 23 | SAVE / 31 |
| 5 | Stop | SCL/SEL / 24 | OCT− / 32 |
| 6 | Play | PLAY/STOP / 25 | OCT+ / 33 |
| 7 | Rec | REC / 26 | HOME / 34 |
| 8 | Tap | GLO / 27 | SCL/SEL / 35 |

Tap Shift + Pad 2 to switch A/B, then release Shift before pressing a command pad. Avoid the separate Transport Bank mode: it invokes MiniLab's local/transport functions instead of these custom pads. A normal pad press sends no musical note. Hold a pad while turning a knob or pressing another control to use firmware button combinations. SAVE and REC are the firmware's actual buttons; holding REC can clear patterns in some firmware. They do not start the emulator's WAV recorder or write to a physical FM-1.

Keep the four faders on their factory CCs (82, 83, 85, 17). They pass through as musical MIDI, as do keys, pitch/modulation strips, Hold, pedal and other unmapped messages. Their effects depend on the firmware. The main encoder duplicates SELECT, Shift-turn duplicates PRESETS and click duplicates HOME. Duplicate HOME/SEL sources stay held until all matching sources are released.

## Select the profile

| Host | Selection |
|---|---|
| Standalone | Add `--controller-fm1 --midi "Minilab3 MIDI"` to the launch command; the setting carries through firmware changes and remembered GUI launches |
| Browser | Choose the standard MiniLab MIDI input, then **Controller → FM-1 User · MiniLab 3** |
| Plugin | Route the standard MiniLab MIDI port to a dedicated instrument track; choose **Controller → MiniLab FM-1 User** in the plugin toolbar |

Plugins default to Raw MIDI, including older projects. The selected profile and last controller MASTER position are saved per plugin instance. Plugin profile mapping runs on the machine worker at the MIDI event's scheduled frame, before MIDI channel reassignment. Mapped messages are consumed, so a command cannot also change a firmware MIDI parameter. DAWs may intercept or transform CCs; pass them through unchanged and avoid sharing this track with another controller using the same CCs. Controller identity cannot be inferred from plugin MIDI.

The controller MASTER position takes over volume until host Master automation changes or the on-screen Master is adjusted. This is saved controller state, not a recorded host automation gesture. Switching the plugin to Raw clears the controller volume override and releases held profile buttons. Browser route/profile/channel changes and standalone disconnection release mapped buttons; received CC120/123 also releases dedicated profile holds. Rebooting clears the controller's transient held state.

For the old mapping, use the factory Arturia program with browser **Factory · MiniLab 3** or omit `--controller-fm1` in standalone. **Raw MIDI** / `--controller-raw` bypasses mapping. The dedicated program applies only to FM-1, not SMK-37.

## Octave and transpose

MiniLab Oct−/+ shifts outgoing keyboard notes by octaves; Shift + Oct−/+ transposes by semitones. Press both octave buttons to reset. Keep these local functions: do not transpose the received notes again in the emulator. MiniLab's local octave buttons are not treated as FM-1 panel presses. Bank B pads 5/6 send the actual FM-1 OCT−/+ buttons, including firmware menu/confirm and held-button behavior. They can affect firmware state independently of MiniLab's keyboard range.

## Evidence and limits

Automated routing tests cover relative repetition/direction, all 16 pad assignments, duplicate holds, panic release, performance passthrough, factory compatibility, and plugin mapping before channel reassignment. No physical-controller configuration, bank switching, relative-mode capture or actual DAW acceptance is claimed for this program. No installable proprietary MCC template is supplied; the tables above are the configuration contract.

References: [Arturia MIDI chart](https://support.arturia.com/hc/en-us/articles/6189475866396-MiniLab-3-General-Questions), [MiniLab 3 MCC manual](https://dl.arturia.net/products/minilab-3/manual/minilab-3-mcc_Manual_1_14_1_EN.pdf), [MiniLab cheatsheet](https://medias.arturia.net/images/products/minilab-3/cheatsheet-minilab-3.pdf), [relative encoder firmware update](https://ipn.arturia.com/products/hybrid-synths/minilab-3/update). The older MCC manual predates the relative-mode update; confirm outgoing 63/65 bytes with MIDI monitoring after setup.
