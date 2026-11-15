---
load_when: "current progress, next steps or known limitations"
last_verified: 2026-11-15
source_refs:
  - web/studio/**
  - web/main.js
  - context/development.md
---
# Status

## Implemented in the first build increment

- Account-owned fork based on upstream commit 60645a58d6cd1597ed565a3f00e4981964623232.
- Root README and agent router, tiered context, preserved research and product requirements.
- Base-8 visual system and browser Studio with Instrument, Library and Design system views.
- Validated, content-addressed firmware/setup imports and transactional IndexedDB storage.
- Search, package export, collection archive import/export and device persistent-save export.
- Same-origin bridge to the original WASM panel and binary engine; local mode suppresses automatic remote discovery.
- Build-generated offline asset cache; firmware bytes live separately in the library.
- Native standalone and plugin build paths retained without replacing the existing native UI.

## Verification

Verified on 2026-11-15 on Apple M5 Pro, arm64, macOS 27.2, using Rust 1.100.0 and Node 24.21.0:

| Check | Result |
|---|---|
| Native fm1-live + diagnostic runner | Release builds passed; Mach-O arm64 |
| Standalone app | Packaged at target/apps/FM-1 Studio.app; strict/deep local signature verification passed |
| VST3 / AU / CLAP | Local bundles built; VST3 strict/deep signature verification passed; not installed |
| Steinberg VST3 validator | 47 passed, 0 failed |
| CPU/SoC regression suite | 167 passed, 0 failed |
| Plugin regression suite | 30 passed, 0 failed |
| Studio domain suite | 5 passed, 0 failed, including real firmware roundtrip |
| Browser/plugin persistent-save interchange | Byte-identical firmware and flash-sector roundtrip passed |
| Browser UI | Felucca download/import, library persistence, actual device boot and collection export passed |
| Actual browser collection export | Restored by the importer; exact firmware hash matched |
| Offline-origin test | Stopped localhost server, confirmed HTTP connection failure, reloaded Studio, reopened library firmware and booted it successfully from cached runtime/local storage |
| Documentation | Frontmatter, routing, source globs and local Markdown links passed |

Fixture: Felucca 1.8.2, SHA-256 770ea7c756088d08ac59a193f1ae78126476c3c8251a86938c837ead5ab0048a. The original package remains ignored in reference/firmwares. Native four-second execution generated 171264 stereo frames at 44100 Hz, peak 6525 in 16-bit PCM. This proves non-silent output, not hardware equivalence. The diagnostic also reported 136 unknown MMIO accesses at three addresses; preserve this as a fidelity investigation item.

The offline test removed the origin server, not all OS networking. Studio mode suppresses remote discovery; full browser/OS network-disconnection testing remains a broader acceptance case. Browser screenshot inspection covered the desktop layout; compact/zoom/screen-reader acceptance remains pending.

No physical hardware or real Ableton/Logic/Pro Tools session was tested. Current validation does not establish all-firmware compatibility, latency guarantees or sound equivalence. The first browser verification preceded the final decorative empty-state overflow and export-filename fixes; those received code/syntax/domain checks afterward.

## Known limitations

- Full volatile CPU/RAM snapshots are not implemented; unsaved edits need the firmware’s Save command.
- Reusing the exact same Studio instance URL shares its working scope; open the base URL for an independent instance.
- Native apps/plugins do not share Studio’s browser library or new UI yet.
- Expanded editors, general MIDI/SysEx/clock/host transport and hardware transfers remain roadmap work.
- Browser canvas keyboard accessibility needs a dedicated accessible layer.
- Collection archives currently allow at most 256 items / 24 MiB of imported binary content.
- Offline readiness is subject to browser storage policy; exported backups remain necessary.
- Existing upstream plugin IDs are retained; do not install automatically over an existing FM-1 plugin.

## State and controller increment

Implemented isolated browser working databases by instance UUID and setup ID, stable instance URLs, scoped setup resumption, and flush-before-switch coordination. MIDI inputs now route by exact port ID and issue panic messages when a controller disconnects or the route/channel changes. Eight Studio domain tests pass, including namespace separation, ambiguous-controller names and panic message coverage. These tests do not constitute end-to-end physical controller validation.

Full volatile recall remains incomplete. The inspected CPU and SoC do not expose a complete serializable checkpoint contract; implementing it requires an explicit schema across both cores, private CPU interrupt state, RAM banks, peripheral timers/DMA, flash and USB state, with JIT caches recreated rather than serialized. Do not mark the proposed two-instance unsaved-edit DAW acceptance test as passed.

The native scripted MIDI run produced a non-silent WAV, but the no-input comparison produced no WAV; the baseline comparison is inconclusive and is not counted as a passed MIDI equivalence test. Physical keyboard transport, browser multi-instance end-to-end tests and complete volatile recall remain pending.

## HYP1 compatibility check

On 2026-11-15 imported the author's HYP1 1.5.65 package from https://rustypawz.github.io/fm1-HYP1/firmware/hyp1-1.5.65.fwsc. SHA-256: da4940a44bed3b004eaf1194daffd9bec693d454652418f1235cb03a4159c149. The local catalog's 1.5.62 was not substituted. Firmware remains in ignored reference/firmwares.

Native fm1-live original-binary run reached 6000 ms with active LCD/LED output and non-zero audio (peak 215 over the final half second). Browser Studio imported the package, booted it at 240 MHz, displayed animated firmware UI, opened the Acid page and triggered breakdown from the panel. Observed transient headroom was roughly 3x realtime, not a latency or sustained-load benchmark. Screenshot evidence was saved outside the repository. No physical-device flashing, physical MIDI-controller test, exhaustive save/reload test, or sound equivalence claim is made.

Beginner workflows now live in user-guide.md and are linked from the README and agent router. Next implementation priority remains complete volatile checkpoints and actual DAW recall acceptance, followed by the shared native library/editor integration and MIDI/transport completion.

Audacity 4 investigation: system-wide FM-1 bundle has valid ad-hoc signing and quarantine attributes; its two executable hashes differ from the local build. Audacity registration cache reports Error/-3 (meaning unverified). No security settings or installed files changed. Published Audacity docs exclude VST instruments; direct FM-1 instrument playback was not established. See user-guide.md.

OPEN: user reports HYP1 distortion versus author browser player. Raw URL uses 100 MHz and catalog pins 1.5.62, versus imported 1.5.65 at 240 MHz. Ten-second native 1.5.65 renders produced 428032/434944 frames at 100/240 MHz, peak 9908/11363 and zero clipped samples; not a matched-state or perceptual equivalence check. Do not call audio fidelity verified. macOS unblock instructions are item-specific; no quarantine/security changes were performed.

Native MiniLab increment: connected CoreMIDI inputs enumerate as Minilab3 MIDI, DIN THRU, MCU/HUI and ALV. Existing app was running Felucca and subscribed only at startup. Added a native MIDI discovery worker that reconnects on port changes, releases notes when a port disappears, and shuts down before firmware replacement. Thirty native tests pass. Physical key receipt/audio confirmation still pending user performance. Added F4/Fn-F4 firmware-change steps and explicit native flash persistence caveat.

Follow-up: native F4 now uses a release-triggered, non-repeating shortcut gate; regression covers held startup key, repeats and cancellation idle frames. Thirty-one native tests pass; the separately enabled macOS CoreMIDI hotplug test also passes with exact note-on/off byte delivery and disconnect panic. Physical MiniLab monitor captured channel-1 0x90 note-on and 0x80 note-off pairs. The affected original app had switched to HYP1, whose default keys change its generative scale; documented BY HAND mode separately from stuck-note defects. Rebuilt the native app; running processes need restart. Temporary duplicate test emulator was closed.
