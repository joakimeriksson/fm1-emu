---
load_when: "current progress, next steps or known limitations"
last_verified: 2026-11-16
source_refs:
  - assets/icons/README.md
  - tools/generate-icons.swift
  - web/studio/**
  - web/main.js
  - context/development.md
  - crates/fm1-host/src/lib.rs
  - crates/fm1-live/src/main.rs
  - crates/fm1-plugin/src/editor.rs
  - web/hardware/**
---
# Status

Verification sections record individual increments, not cumulative current test counts. Later sections describe `.area` support, the project-only Hardware Library and standalone session recall; retain their explicit acceptance limits when summarizing current capability.

## Implemented in the first build increment

- Account-owned fork based on upstream commit 60645a58d6cd1597ed565a3f00e4981964623232.
- Root README and agent router, tiered context, preserved research and product requirements.
- Base-8 visual system and browser Studio with Instrument, Library and Design system views.
- Validated, content-addressed firmware/setup imports and transactional IndexedDB storage.
- Search, package export, collection archive import/export and device persistent-save export.
- Same-origin bridge to the original WASM panel and binary engine; local mode suppresses automatic remote discovery.
- Build-generated offline asset cache; firmware bytes live separately in the library.
- Native standalone and plugin build paths retained without replacing the existing native UI.

## Initial verification (2026-11-15)

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
- Expanded editors, general MIDI/SysEx/clock/host transport and broad hardware transfers remain roadmap work. The project-only SLOOP 3 companion is implemented with physical verification pending.
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

Earlier report (Noise Colour mismatch resolved below): user reports HYP1 distortion versus author browser player. Raw URL uses 100 MHz and catalog pins 1.5.62, versus imported 1.5.65 at 240 MHz. Ten-second native 1.5.65 renders produced 428032/434944 frames at 100/240 MHz, peak 9908/11363 and zero clipped samples; not a matched-state or perceptual equivalence check. Do not call audio fidelity verified. macOS unblock instructions are item-specific; no quarantine/security changes were performed.

Native MiniLab increment: connected CoreMIDI inputs enumerate as Minilab3 MIDI, DIN THRU, MCU/HUI and ALV. Existing app was running Felucca and subscribed only at startup. Added a native MIDI discovery worker that reconnects on port changes, releases notes when a port disappears, and shuts down before firmware replacement. Thirty native tests pass. Physical key receipt/audio confirmation still pending user performance. Added F4/Fn-F4 firmware-change steps and explicit native flash persistence caveat.

Follow-up: native F4 now uses a release-triggered, non-repeating shortcut gate; regression covers held startup key, repeats and cancellation idle frames. Thirty-one native tests pass; the separately enabled macOS CoreMIDI hotplug test also passes with exact note-on/off byte delivery and disconnect panic. Physical MiniLab monitor captured channel-1 0x90 note-on and 0x80 note-off pairs. The affected original app had switched to HYP1, whose default keys change its generative scale; documented BY HAND mode separately from stuck-note defects. Rebuilt the native app; running processes need restart. Temporary duplicate test emulator was closed.


## Controls, HYP1 audio and GR!MM increment

Corrected the held-note diagnosis: the user had MiniLab Hold enabled. Added native Cmd/Ctrl+O alongside the release-gated F4 picker, browser Cmd/Ctrl+O, factory MiniLab panel mapping, fractional knob drawing and Shift fine drag. Native queue target starts at 12 ms (previously 50 ms on macOS); polling is up to 120 Hz. Device latency is not measured end-to-end. Native/browser profiles bypass mapped CC forwarding; plugin input remains raw. Pads and faders 1–3 retain firmware MIDI behavior.

Captured physical MiniLab CCs for all eight knobs and four faders, main encoder relative values, and channel-10 pad notes. Reproduced HYP1 startup piece 06 with Noise Colour Black. The source implements Black as crackle; the user confirmed White removes the difference. No denoising or binary patch was applied. HYP1 catalog now uses the author’s 1.5.65 package and new web sessions default to 240 MHz.

The 1.5.65 original binary rendered 655,360 identical stereo frames in native and WASM; the native interpreter matched the first 346,880 frames. A 30-second WASM benchmark recorded zero steady short quanta, with three of 9,996 render calls over the old 2.9 ms callback deadline. Moved rendering/JIT/LCD/snapshots into a worker; the AudioWorklet now consumes a bounded stereo queue. Browser playback and save/reboot verification are recorded separately from headless benchmarks. This is not proof of author-player or physical-device sample equivalence.

Downloaded latest published GR!MM release v2.0.2 (2026-11-03), original grimm.fwsc saved as reference/firmwares/grimm-2.0.2.fwsc. SHA-256 d85c0dd3d668f48157afad7b9a1eb2e49e376f931afd2349bebef9014eb1375d matches the release asset digest. Six-second original-binary boot/render completed with 257,280 audio frames. The package remains ignored; no hardware flash occurred.

Validation: 32 native tests, 99 SoC/package tests, 30 plugin tests and 12 Studio tests pass; the separate macOS CoreMIDI hotplug/note-release/disconnect test passes too. Standalone and plugin bundles rebuilt without installing over existing plugins. Browser HYP1 playback at 240 MHz, save, firmware update check and clock-change reboot succeeded. In the final 93-second observation there was one early output underrun; the queue grew from 11.6 to 17.4 ms and no further underruns occurred between 22 and 93 seconds. This is a bounded observation, not a zero-dropout guarantee.

## SLOOP 3 standalone application-area support

The standalone picker accepts `.area` alongside `.fwsc`; CLI accepts `--area FILE`. SLOOP 3.3.7 beta's original application area passes header/payload CRC validation and boots through its own RAM unpacker. A 12-second headless native run displayed its five-track panel and captured 529,411 stereo frames, peak 5,081 (s16), with scripted PLAY/note input. This is boot/render/non-silent output evidence, not hardware or full musical correctness. Browser Studio and plugins now support `.area` using the same Rust format detector and boot handoff. WASM SLOOP smoke produced 528,640 frames and 57,600 lit pixels over 12 seconds; a MIDI note raised RMS from 0 to 0.0034. JS collection/setup round trips and WASM raw-flash/XIP recall passed. Rebuilt standalone, browser and CLAP/AU/VST3 bundles; real DAW acceptance remains pending. Downloads stay ignored in `reference/firmwares/`.

## Hardware Library first increment

Implemented shared file-based SLOOP 3.3.7 project capture/restore over Web MIDI, exposed from Studio, standalone F6 and plugin toolbar via an embedded companion. Includes protocol/version checks, current-project safety download, explicit restore review, unchanged-project check and full saved-project read-back comparison. It does not transfer samples/user sounds or raw flash. Eight simulated protocol/transport tests and the native embedding test pass; served page inspected visually with write controls disabled before connection. No physical FM-1 connection or write was performed. Source includes user-friendly host comparison and hardware workflows. Full hardware acceptance and real DAW interactions remain pending.

Standalone session increment: last firmware/launch arguments and persistent flash restored on Finder launch, with content-hashed default memory files. Native reset archives previous memory after shutdown. Plugin reset reuses Cmd::Load with no saved sectors; browser selection resumes per instance without autoplay. Complete unsaved CPU/RAM recall and cross-host storage synchronization remain unimplemented.

### Session/menu acceptance still pending

Automated standalone/host tests pass (34 passed, one ignored), including preservation of pre-reset memory bytes; browser domain tests, builds and context checks passed. Finder last-session reopening, native menu interaction and plugin reset inside a real DAW still need manual acceptance. This does not establish capture of all live settings: unsaved RAM, held notes, playback position and window geometry are not checkpointed. Default standalone memory is shared by exact firmware hash; use separate explicit `--flash` paths for independent simultaneous runs.

## App icon packaging

The approved black FM-1 artwork is exported as macOS ICNS/iconset, Windows ICO, PNG sizes, browser favicons, opaque Apple touch icons and separate maskable web icons. Standalone and AU/VST3/CLAP local bundles build with icon resources attached before signing. Web builds copy the assets; Studio caches them offline. Windows standalone/CLAP/VST3 resource compilation and Linux X11/desktop integration are implemented but not runtime-verified on those platforms. Mobile home-screen appearance and real DAW artwork presentation remain unverified; DAWs own their plugin-list presentation.

Verified locally: native standalone and three plugin bundles build; all four pass deep/strict signature verification and reference the expected ICNS bytes. PNG sizes, ICO frame directories, web icon links, manifest paths and offline asset coverage pass inspection. Both fm1-host tests pass, including the self-contained native companion favicon. Context checks pass. No plugin installation or publication was performed. Regeneration and Linux desktop setup are documented in the [icon asset guide](../assets/icons/README.md).

### Flat charcoal icon revision

Replaced the earlier glass-rim artwork with the approved smooth charcoal revision: flatter controls, gentle top lighting, soft contact shadows and crisper shallow outer bevels. All platform exports and the portable icon archive are regenerated from this source. Platform verification limits above still apply.

macOS icon registration follow-up: the ICNS decoded correctly and CFBundleIconFile was present, but metadata-only checks missed the reported display issue. Re-registering the built app refreshed its resolved NSWorkspace artwork. Packaging now refreshes this one app after signing; no global icon cache reset is used. The resolved icon was rendered and visually inspected.

### Blank screen and native macOS enclosure

Replaced the icon display with blank unlit glass across exports. The native macOS Icon Composer source now separates isolated hardware from a solid charcoal body fill during export and omits any baked outer bevel/padding. Bundles include compiled Assets.car with CFBundleIconName plus ICNS fallback. This corrects the extra white system frame seen with the legacy-only icon. The earlier registration-only fix did not address that sizing issue. All icon formats and the reusable archive are regenerated; macOS artwork is checked through its resolved system rendering. Windows/Linux/mobile runtime appearance remains unverified.

The final source passes raw PNG checks for a uniform #292a2b body and unlit screen. All four local macOS bundles and web assets were rebuilt; native artwork is checked without the legacy white enclosure, and no controls are clipped. The archive contains the Icon Composer document and compiled catalog alongside other platform exports.

Final Finder acceptance: the rebuilt app was selected in Finder and its large preview visibly shows the blank-screen instrument filling the native rounded tile, with no extra white frame and no clipped controls. Native packaging derives CFBundleVersion from the compiled icon digest to invalidate stale artwork registration after changes.

### Flat-control refinement and preserved platform intent

Refined the isolated hardware master against the approved flat-charcoal UI reference, reducing glossy outlines and surface mottling on knobs, buttons and keyboard trays while retaining layout, ivory markers and a blank screen. The body remains a separate uniform #292a2b fill. All native, legacy, Windows, Linux and web exports are regenerated from the same master.

[icons.md](icons.md) now records the durable user intent, distinct platform assets, exact regeneration/build/package commands, macOS container/cache pitfalls and platform verification limits. It is routed from AGENTS.md and linked from the design/build docs. The portable archive now has a source-controlled `tools/package_icons.py` command, including the platform guide; the extracted package is refreshed with the ZIP.

Refinement acceptance: uniform-body/blank-screen checks, icon sizes/frames/links/offline coverage, archive/catalog byte comparisons, all four macOS bundle signatures, both fm1-host tests and context integrity passed. Finder Quick Look visibly shows the refined matte controls and blank screen filling the native enclosure without the extra white frame or cropped controls. Windows/Linux/mobile and real DAW presentation retain the unverified limits in icons.md.
