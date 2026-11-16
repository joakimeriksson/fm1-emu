---
load_when: "always — foundation; first orientation for any task"
last_verified: 2026-11-16
source_refs:
  - Cargo.toml
  - package.json
  - AGENTS.md
---
# Overview

Faithful original-binary FM-1 emulation is the product’s defining requirement. The intended user composes in a standalone Mac instrument and DAW plugins, keeps more firmware/content on the computer than fits the device, and later uses the same workspace as a physical FM-1 companion.

The target is Apple Silicon, with standalone/VST3 first and an offline static browser version. The user reports an M5 Pro with 64 GB and macOS 27.2. Both native and browser must work offline once chosen assets are saved. Personal use comes first; possible public open-source distribution remains a future decision.

Rust CPU/SoC code, the native panel, WASM runtime and plugin infrastructure come from fm1-emu. Studio adds a dependency-free browser interface and local library. The first implementation deliberately keeps the original device page as a same-origin runtime surface, avoiding a second audio engine.

Hardware-sized memory remains the default; larger emulated capacity is later work. Performance MIDI and saved-flash recall are implemented with host-specific storage: standalone remembers its last firmware/launch options, plugins recall DAW project state, and browser Studio reselects within a bookmarked instance. Complete CPU/RAM recall remains unfinished. The shared SLOOP 3 project librarian has simulated transfer coverage; physical-device verification is pending. Never equate persisted flash with full volatile-state snapshots.

Research/specification preceded implementation and remain available as on-demand references. Day-to-day progress and verification live in status.md; do not rewrite research history to imply it included runtime testing.
