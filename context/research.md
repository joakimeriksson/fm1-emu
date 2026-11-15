---
load_when: "community projects, source selection or research evidence"
last_verified: 2026-11-15
source_refs:
  - context/status.md
  - Cargo.toml
---

> Preserved research-phase document. See status.md for implementation progress and verification.

# FM-1 on macOS: community research and recommendation

Research date: 2026-11-15. Status: research and specification, before implementation.

## Recommendation

Use **Joakim Eriksson’s fm1-emu as the execution foundation**, retain its shared device panel, and build a library and firmware-aware companion around it. Reuse hardware recovery work from **M-Flasher / m-bootloader** and study **SLOOP’s editor**, **FM1 Editor & Librarian**, and **Virtual FM-1** for their respective editing and synchronization workflows.

Original firmware binary execution is a hard requirement. Source-compiled instruments are useful references, but are not acceptable execution backends for this project. Supporting a new firmware must not require porting its synthesis engine.

This is a feasible foundation for an instrument, not evidence that arbitrary present or future firmware will work perfectly. Compatibility must be earned for exact package hashes and individual capabilities. Sound, timing, and physical-device interoperability remain unverified here.

## Research method and limits

Reviewed public project documentation, release information, selected implementation files, protocol descriptions, and browser API documentation. Visually inspected the fm1-emu panel, SLOOP editor’s disconnected interface, and X0X browser panel. Did not install or execute native downloads, build projects, audition audio, run upstream tests, or connect hardware. No fork or implementation was created.

“Documented” below means supported by an upstream source, not independently tested. “Recommendation” and “proposed” describe this project’s design decisions. Rankings assess fit to the agreed requirements, rather than download counts or stars.

The sources move quickly. The [v0.5.1 release](https://github.com/joakimeriksson/fm1-emu/releases/tag/v0.5.1) identifies commit `c7e47c4`, published November 14, and Apple Silicon standalone and AU/VST3/CLAP downloads. The release page showed additional commits on master. Findings from master must not be assumed present in that release.

## Execution projects compared

| Project | Execution model and documented strengths | Fit and decision |
|---|---|---|
| [fm1-emu](https://github.com/joakimeriksson/fm1-emu) | Runs original firmware; native and browser targets; existing plugin path | **First choice.** Best overall alignment and lowest architectural duplication among reviewed candidates. |
| [Simon Johansson’s fm1-emulator](https://github.com/simonjohansson/fm1-emulator) | QEMU CPU/board model; Apple Silicon desktop app; documented stock V15 and Felucca stress tests | **Independent reference.** Useful for comparing failures and developing diagnostics. No documented DAW plugin distribution in its README. |
| [shcherbakov7 browser emulator](https://github.com/shcherbakov7/m-wave-fm1-browser-emulator) | Rust/WASM binary emulator; dual cores and board devices; interpreter/JIT differential checks | **Secondary research reference.** Its documented USB-MIDI/Bluetooth limitations and performance headroom make it a weaker product base. |
| [Harbor](https://github.com/markterm/harbor) | Android application using fm1-emu, unchanged packages, USB/Bluetooth MIDI and inter-app routes | **Integration reference.** Relevant routing and packaging ideas; Android is outside scope. Not an independent replacement core. |
| [FM1VST](https://github.com/Quixotic7/FM1VST) | Builds firmware C source against a host HAL; modular cores; JUCE plugin and hardware-style panel | **Not an execution candidate.** Good source-port architecture, but does not meet arbitrary original-binary execution. |
| [Virtual FM-1](https://github.com/jbschooley/Virtual-FM-1) | Replica/source-port instrument with firmware-specific preset and project sync | **Companion-design reference.** Especially useful version-aware pull/push workflows; not a universal binary emulator. |
| [VIBE FM conversion platform](https://thefirmwarehub.com/convert/) | Converts open firmware source to a WASM-based instrument ecosystem | **Not an execution candidate.** Requires source conversion. Published latency/CPU claims are not comparable measurements of our target. |
| [FM-1 Mobile](https://github.com/hb3p8/fm1-mobile-emu) | Touch-oriented source-to-WASM firmware collection; explicitly cannot load `.fwsc` packages | **UI/offline reference only.** Fails the binary-loading requirement. |

Simon’s [README](https://github.com/simonjohansson/fm1-emulator/blob/main/README.md) documents native desktop execution, headless tests and a firmware console, with tests on an M3 Pro. Its accepted package formats are narrower than a generic “all FM-1 files” claim. Shcherbakov’s [README](https://github.com/shcherbakov7/m-wave-fm1-browser-emulator/blob/main/README.md) reports interpreter/JIT verification and board emulation, but also sub-real-time results on its specified server CPU. These measurements do not predict performance on the user’s Mac.

Harbor’s [project description](https://github.com/markterm/harbor#readme) confirms fm1-emu ancestry and Android MIDI routes. Its plain-Rust `harbor-engine` handles the worker, audio ring, resampling, MIDI parsing, automatic clock and portable saves separately from Android bindings: inspect this component before duplicating integration work. Its app code’s redistribution terms were not established in this review; do not copy it based on the emulator’s MIT license alone.

The [FM1VST README](https://github.com/Quixotic7/FM1VST#readme) explicitly describes C-source compilation and a host HAL. [Virtual FM-1](https://github.com/jbschooley/Virtual-FM-1#readme) mixes source-compiled firmware with a recreated stock instrument and acknowledges approximated effects and version-specific sync limitations. Both are valuable projects with different goals from ours.

## Why fm1-emu wins, and where it falls short

Its [workspace manifest](https://github.com/joakimeriksson/fm1-emu/blob/master/Cargo.toml) separates CPU, SoC, native runner and web crates, with the plugin in a separate workspace. This is a good starting boundary for sharing one emulation core across products.

The [main README](https://github.com/joakimeriksson/fm1-emu#readme) documents native JIT execution, persistent flash, and an emulated USB route exposed as a virtual MIDI device on macOS/Linux. It also describes a strict mode that models hardware timing more closely but uses interpretation and runs slower. **Executing the same binary is not equivalent to cycle-accurate execution.**

The [stock-firmware technical note](https://github.com/joakimeriksson/fm1-emu/blob/master/docs/stock-firmware.md) now reports V13, V15 and V16 execution with both cores. The [landing page](https://joakimeriksson.github.io/fm1-emu/) still names V13/V15. Treat V16 as documented on master and pending exact-release verification. Firmware packages also omit some persistent content held on physical devices; bringing a firmware file alone does not guarantee a factory-identical instrument.

The [plugin README](https://github.com/joakimeriksson/fm1-emu/blob/master/crates/fm1-plugin/README.md) documents a CLAP implementation with AU/VST3 wrappers, portable saves, latency compensation and selectable 6/12/23 ms lead. It reports validator success but lists Logic and Ableton as not yet tried. Live latency includes more than the lead, and its published machine measurements are not M5 Pro benchmarks. The AU wrapper has a documented dynamic-latency notification limitation.

Selected [plugin implementation](https://github.com/joakimeriksson/fm1-emu/blob/master/crates/fm1-plugin/src/lib.rs) shows stereo output, channel-message filtering, no custom SysEx type, linear output interpolation, and no host transport/tempo handling in the reviewed process function. Clock/system messages are rejected by its channel-message length check. **Existing MIDI note input therefore does not establish our required clock, SysEx, MIDI output or transport behavior.**

The [engine implementation](https://github.com/joakimeriksson/fm1-emu/blob/master/crates/fm1-plugin/src/engine.rs) runs emulation on a worker and passes timestamped short messages through queues. Flash export waits for writes; a host-state copy is refreshed periodically. This needs an audit for save freshness, unsaved RAM, offline rendering and non-audio/controller firmware. The worker’s progress must not depend solely on generated audio frames for firmware that intentionally produces no sound.

The [web wrapper](https://github.com/joakimeriksson/fm1-emu/blob/master/crates/fm1-web/src/lib.rs) shares panel code with native and runs emulation through an AudioWorklet. The [browser persistence implementation](https://github.com/joakimeriksson/fm1-emu/blob/master/web/persistence.js) already hashes packages and tracks changed flash sectors. Reuse its save compatibility rather than inventing a disconnected web format.

## Interface recommendation

There are two separate UI jobs: operating any firmware through the physical controls, and editing musical data through a firmware-specific protocol.

**Universal Device view:** keep fm1-emu’s common native/web panel and firmware-rendered LCD as the first implementation. This minimizes the risk of a beautiful replacement panel mapping inputs incorrectly. Its canvas-style panel needs accessibility and keyboard interaction evaluation.

**Expanded Editor view:** use SLOOP as the strongest reviewed reference for the groovebox workflow. Its [editor](https://isod89.github.io/sloop-fm1/webapp/editor/) exposes sound, sequencer, tracks, song, library, samples and settings sections. The disconnected view was inspected; connected editing was not. Its [documentation](https://github.com/isod89/sloop-fm1#the-web-editor) describes backup, MIDI import/export, and a source-compiled audio preview. We should connect editing to the binary emulator, not replace execution with that preview.

SLOOP’s [editor protocol](https://github.com/isod89/sloop-fm1/blob/main/web/EDITOR_PROTOCOL.md) evolves by protocol version and supports descriptors, live notifications and backup commands. That supports an adapter architecture, not a universal hard-coded SLOOP schema.

**X0X:** its [browser interface](https://charlesvestal.github.io/fm1-x0x/emu/) is a particularly useful interaction reference: focusable controls, keyboard operation, held-button combinations, screen-reader support and break-slot loading. The [project](https://github.com/charlesvestal/fm1-x0x) is a specific groovebox, not an interchangeable editor for every firmware. Reuse interaction ideas and appropriately licensed components; keep binary execution underneath.

**Stock/DX7 editor:** [FM1 Editor & Librarian](https://github.com/benny-sparra/fm1-dx7-patch-importer) is the best reviewed focused reference. It documents full standard DX7 voice editing, effects, bank organization and SysEx exchange; its source is MIT, with separate third-party image and bank terms. Its backup is app-specific, while `.syx` carries voice data. This distinction belongs in our import UI.

No reviewed project establishes a universal expanded editor for every firmware. New work is required to combine these workflows coherently.

## Hardware tools and integration

The M-Flasher link in the initial notes pointed to the emulator. The actual project is [jvitkauskas/m-flasher](https://github.com/jvitkauskas/m-flasher).

Its [README](https://github.com/jvitkauskas/m-flasher#readme) documents full-flash backup/restore, `.fwsc` installation, flash maps, CLI and desktop interfaces, and optional bootloader replacement. It is MIT. This is the strongest reviewed recovery foundation; it is not a live musical-data editor.

Its [development notes](https://github.com/jvitkauskas/m-flasher/blob/main/docs/development.md) distinguish normal USB-MIDI from ROM download mode. On macOS the latter uses libusb and a privileged helper to take the device from the disk driver. That mechanism should stay outside the DAW audio/plugin process. Its [safety design](https://github.com/jvitkauskas/m-flasher/blob/main/docs/safety.md) covers validated backups, board/key checks, sector-limited writes, factory-data preservation and read-back verification. It also documents stock V16/V15 persistent-data incompatibilities. Firmware rollback alone is insufficient protection for user data.

[m-bootloader](https://github.com/jvitkauskas/m-bootloader) adds recovery and MIDI-mode operations. Its current README also describes full-flash browser backup through MIDI, including a RAM-stage path. **This makes broader browser backup worth investigating**; it does not prove that every stock unit and firmware combination can use it. Installing the bootloader remains optional and separately confirmed.

MIDI editing, musical-object transfers, full backups, firmware install and bootloader install require separate capabilities and verification. A full hardware backup is not automatically a valid emulator save, and an emulator save is not automatically safe to flash onto a physical unit.

## Firmware examples and validation coverage

| Family / example | Evidence and value to this project |
|---|---|
| Stock V13/V15/V16 | Exact-package boot, dual-core behavior and persistent-bank compatibility. Use versioned evidence above; never infer compatibility from a shared download filename. |
| Felucca and SLOOP | Initial musical-data adapter targets. SLOOP documents USB/TRS input, USB note output and clock following in its [MIDI guide](https://github.com/isod89/sloop-fm1#midi-and-usb-audio). |
| X0X | Busy dual-core groovebox and sample-slot workflow; important CPU and transport stress case. |
| [386DX](https://github.com/seajaysec/386-DX-FM1-KaraokeBot) | Its own documentation confirms `.kar`/`.mid` song import and eight device slots. Good test of firmware-native import; not proof all firmware can interpret MIDI files. |
| [CORTEX](https://github.com/guiatihe333/cortex) | Project documentation confirms MIDI-file songs and a browser simulator. Include song import plus display/audio activity as an exploratory test. |
| [CTL-1](https://github.com/charlesvestal/fm1-ctl) | Current primary README describes controller, arp, Euclidean sequencer, USB/Bluetooth and local GM sound. It now identifies MPL-2.0, whereas the curated index calls it GPL-3.0. Inspect pinned licensing files before reuse. |
| [Berlinan firmware](https://github.com/jiripisa/fm1-berlinan) | Silent USB-MIDI sequencer: mandatory test that the emulator progresses and emits MIDI without audio. Distinct from the author’s similarly named macOS sequencer. |
| Album | Keep as a research/clock-interoperability lead from the interview. Its specific two-device clock behavior was not independently verified in this review. |

Bluetooth and DIN/TRS support in a firmware do not automatically establish emulator support for those peripherals. Initially use host MIDI routes where possible; do not promise emulated RF or Bluetooth audio.

## Browser and DAW boundaries

| Surface | Realistic initial role |
|---|---|
| Native standalone | Main complete instrument and companion; CoreAudio/CoreMIDI; external recovery helper where needed. |
| Ableton Live | Primary VST3 composition target. Live documents [VST/AU support](https://help.ableton.com/hc/en-us/articles/209068929-Using-AU-and-VST-plug-ins-on-macOS) and [routing VST MIDI output](https://help.ableton.com/hc/en-us/articles/209070189-Accessing-the-MIDI-output-of-a-VST-plug-in); test actual wrapper behavior. |
| Logic Pro | AU path documented for later; [Logic hosts Audio Units](https://support.apple.com/en-am/guide/logicpro/lgcp22a0dab0/mac), so VST3 alone does not cover it. Retaining upstream AU buildability is sensible. |
| Pro Tools | AAX later. [Avid’s SDK program](https://developer.avid.com/aax/) has its own development/testing/signing requirements; no AAX implementation was found in the selected foundation. Initial exchange through rendered audio and MIDI, not a claimed direct plugin. |
| Audacity | Audio import/recording workflow. Its [support documentation](https://support.audacityteam.org/audio-editing/using-realtime-effects) describes effect hosting, and its [manual](https://manual.audacityteam.org/man/effect_menu_vst.html) excludes VST instruments. Verify installed-version capabilities before promising an instrument workflow. |
| Static browser app | Same firmware core and principal views, locally stored libraries, file imports/downloads and supported MIDI. No backend required for musical work. |

Browser constraints come from both our implementation and platform APIs. [Web MIDI](https://developer.mozilla.org/en-US/docs/Web/API/Web_MIDI_API) requires a secure context and permissions; test SysEx explicitly. Chrome/Edge are the recommended first hardware targets. The emulator currently documents no Safari Web MIDI and no web MIDI output. [WebUSB](https://developer.mozilla.org/en-US/docs/Web/API/WebUSB_API) is not universal and does not establish access to the Mac ROM-recovery path. [Browser storage](https://developer.mozilla.org/en-US/docs/Web/API/Storage_API/Storage_quotas_and_eviction_criteria) has quotas and eviction behavior; exports remain essential. Static hosting does not mean opening `index.html` with `file://` will support every feature.

## Reuse and new-work decision

Reuse the emulator/core/panel/plugin path, portable-save compatibility and existing package parsing. Study or adapt editor protocols and hardware operations with their licenses intact. Avoid merging a large collection of firmware DSP ports into the product.

New work is concentrated in: a unified library; save-state completeness; bidirectional emulated MIDI/USB and SysEx; host clock/transport and silent-firmware scheduling; firmware capability adapters; a desktop hardware coordinator; validated conversion between backup formats; and testing on the user’s actual Mac and DAWs.

The most consequential unresolved issues are universal future compatibility, JIT versus strict hardware timing, complete volatile-state recall, silent firmware, offline DAW rendering, and physical-device verification. Resolve these early rather than polishing the library around assumptions.
