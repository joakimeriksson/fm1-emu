---
load_when: "MiniLab User program setup and input routing"
last_verified: 2026-11-23
source_refs:
  - crates/fm1-live/src/studio/minilab.json
  - crates/fm1-live/src/studio/controller_tests.rs
  - crates/fm1-live/src/ctl.rs
---
# MiniLab 3 FM-1 User program

The app defaults to a factory-compatible profile on the standard MiniLab MIDI port. Factory knobs use absolute values, and channel-10 pads 36–51 map to the same button order listed below. Absolute knobs stop at their MIDI endpoints.

Use Controller → Automatic for factory settings, Controller → MiniLab FM-1 User for the configured relative program, or Controller → Raw MIDI to bypass mapping. Changes apply live, release old holds, and apply for the current run; opening firmware or reopening from Finder selects Automatic. Other MiniLab ports remain raw.

The dedicated program maps physical FM-1 controls, so each firmware keeps its own page meanings and button combinations. It requires a MiniLab User program and explicit selection in the emulator. The factory profile remains available with its previous assignments; do not select FM-1 User while sending factory absolute knob values.

## Factory knob layout

| MiniLab knob | CC | FM-1 action |
|---|---|---|
| 1–4 | 74, 71, 76, 77 | K1–K4 |
| 5 | 93 | Master |
| 6 | 18 | Select |
| 7 | 19 | Presets |
| 8 | 16 | Algorithm |

The main encoder and master fader retain their assignments. This changes Automatic/factory mapping only; the separately configured relative User program below keeps its own layout.

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

Keep the four faders on their factory CCs (82, 83, 85, 17). They pass through as musical MIDI, as do pitch/modulation strips, Hold, pedal and other unmapped messages. Their effects depend on the firmware. The main encoder duplicates SELECT, Shift-turn duplicates PRESETS and click duplicates HOME. Duplicate HOME/SEL sources stay held until all matching sources are released.

## Select the profile

```sh
target/release/fm1-live --fwsc reference/firmwares/felucca.fwsc --midi "Minilab3 MIDI" --map "MiniLab FM-1 User"
```

The relative User program selection is explicit and applies to the current run. Opening firmware or reopening from Finder selects Automatic. It is not auto-selected because factory absolute knobs are incompatible with this User program. The standard performance port filter avoids MCU/HUI, DIN and DAW ports being mapped accidentally. F1 shows bindings; right-click uses upstream MIDI learn. User-learned profiles follow upstream storage behavior.

Channel-1 keyboard notes use the same panel key actions as the on-screen keyboard, including velocity and release. Both bundled profiles default to panel mode: MIDI F3 (53) is the first panel key; notes outside F3–G5 fold by octaves using upstream behavior. F2 cycles the upstream keyboard modes. Pedal, modulation, pitch bend and unmapped faders pass to firmware MIDI. Pads decode channel 16 before any --midi-ch remap. Relative knobs use the upstream relative-64 mode. Duplicate HOME/SCL assignments stay held until all sources release. Panic and input disconnect release mapped holds. Reconnect discovers the selected input without restarting the app. With --map auto, profile choice is still made at startup.

This fork bundles the profile only in standalone. Browser and plugin retain upstream profiles/behavior. No controller settings are uploaded by the app. Real MiniLab User-program and bank-switch acceptance remains pending.
