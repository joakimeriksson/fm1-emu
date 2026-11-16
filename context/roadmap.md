---
load_when: "phasing, acceptance gates or future work"
last_verified: 2026-11-16
source_refs:
  - context/status.md
  - context/product-specification.md
---
# Roadmap

## Current increment: usable foundation

Build the real native and WASM binaries, establish the documented design system, and connect a local browser collection to the actual emulator panel. Verify real-package import, boot, saved-flash exchange and offline reopening. Keep upstream changes small.

## Next: requirements that determine the architecture

Prove silent controller firmware advances without generated audio; implement outgoing emulated MIDI and variable-length SysEx; characterize timing; test host clock/transport and offline bounce. Establish complete CPU/RAM/peripheral snapshot serialization before claiming complete session recall.

Browser working-flash isolation is implemented by instance/setup scope. Next validate multi-tab browser persistence end-to-end and implement volatile checkpoints; plugin instance isolation and complete recall need their own acceptance tests.

## Then: expanded editing and native integration

Prototype stock/DX7 and SLOOP adapters against emulated USB before physical transport. Add a native collection service and translate new UI tokens into the native GUI stack. Preserve original firmware execution and the measured hardware panel geometry.

## Physical companion

A project-only SLOOP 3.3.7 companion now uses shared browser/native-launch sources with protocol fixtures and simulated transfer tests. Next validate that adapter on physical hardware; sample/sound-bank transfer, additional firmware adapters and recovery integration remain future work. Actual live editing, backups/restores, firmware installation and optional bootloader changes require real-device acceptance. Installation remains explicitly confirmed and is never a recall side effect.

## Later

AU/Logic host acceptance, AAX, advanced mappings/automation, separate outputs and enhanced capacity. No milestone is complete merely because its control appears in the UI. Refer to the full product specification for acceptance details.
