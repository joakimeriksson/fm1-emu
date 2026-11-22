---
load_when: "building, testing, installation or verification evidence"
last_verified: 2026-11-22
source_refs:
  - context/icons.md
  - assets/icons/README.md
  - tools/generate-icons.swift
  - package.json
  - tools/build_studio.sh
  - tools/package-studio.mjs
  - tools/serve-studio.mjs
  - tools/build_plugin.sh
  - Cargo.toml
  - tools/check_firmware_alignment.sh
  - crates/fm1-host/src/lib.rs
  - crates/fm1-host/src/audio.rs
  - crates/fm1-host/tests/audio_callback.rs
---
# Development

The canonical fork is under the selected account’s GitHub directory. Follow the root account instructions before git/gh work; never move a cloned repository to the old research workspace.

Build native targets with locked dependencies. Build plugin bundles with NO_INSTALL=1 so a local development build cannot overwrite an existing installation. The script ad-hoc signs local bundles; this does not establish public distribution or DAW compatibility.

Studio requires only Node built-ins; npm install is unnecessary. npm run build:web first invokes the upstream WASM build, then packages Studio and creates a content-derived offline cache manifest. It does not download firmware. npm run serve binds to 127.0.0.1 and serves only real files inside web/dist, with explicit WASM/JS MIME types and traversal checks.

## Verification ladder

1. Node tests exercise import rejection, collection validation, hashes and real-package round trips.
2. Rust CPU/SoC tests protect the execution foundation.
3. Real firmware smoke runs prove a specific package boots/renders.
4. Browser checks cover import, device boot, collection export/restore, persistence, responsive layout and a network-disabled cold reopening.
5. Plugin validators and real DAW checks are separate. A build alone earns no host-compatibility label.
6. Hardware acceptance remains pending until a physical unit is available.

Useful local fixture: node tools/fetch_firmwares.mjs felucca. All downloaded firmware belongs in ignored `reference/firmwares/`, including manually obtained packages and versioned subfolders for multi-file releases; do not create a separate top-level firmware folder or commit downloads. Run FM1_TEST_FWSC=reference/firmwares/felucca.fwsc npm run test:studio to enable the package roundtrip test.

Native diagnostic: target/release/fm1-emu --fwsc reference/firmwares/felucca.fwsc --steps 1000000000 --ms 4000 --keys '2000-3500:n7' --wav target/felucca-smoke.wav --quiet. Keep diagnostic output in ignored target/, not tracked context.

The supplied ez-context helper can check staleness from source_refs. npm run check:context checks routing, metadata and local Markdown targets without depending on git history. Neither checker proves a document’s prose correct; manually reconcile the design doc for every visual change.

Standalone macOS app packaging: tools/package_macos_app.sh. It builds the native executable, creates target/apps/FM-1 Studio.app and ad-hoc signs the bundle. Public distribution/notarization is not implied.

The MiniLab diagnostic example accepts an optional monitoring duration: `cargo run --release -p fm1-live --example midi_probe -- 120`. It receives only; it never configures or writes to a controller. Studio tests now cover the realtime queue’s bounded prefill/stale-boot/pause behavior and factory controller mapping. Browser asset packaging includes the render worker/runtime, with worker URLs stamped alongside imports. Keep direct-loop WASM benchmarks separate from end-to-end worker/output-underrun verification.

## Cross-host firmware acceptance

When changing firmware formats, boot, flash persistence or execution, audit all three adapters: `fm1-live`, `fm1-plugin` and `fm1-web`. Put parsing, detection, boot parameters and flash behavior in `fm1-soc`; adapters own UI, threading and transport only. UI allowlists, drag/drop, collection import ordering, export filenames and `.fm1save` recall must also accept the same formats. Do not implement synthesis or an alternate firmware port to work around execution errors.

Run `npm run check:firmware` with local Felucca `.fwsc` and SLOOP 3 `.area` fixtures. Override `FM1_TEST_FWSC`/`FM1_TEST_AREA` if stored elsewhere. Missing fixtures fail clearly; this command never downloads binaries. It runs all Rust host tests (plugin workspace separately), JS collection/save tests, a browser build, actual WASM display/audio smoke and flash recall for both formats, and browser/plugin portable-save interoperability. SLOOP’s plugin test boots the original bytes, checks display output and saved-flash recall. Downloaded fixtures remain ignored.

After acceptance, rebuild `tools/package_macos_app.sh` and `NO_INSTALL=1 tools/build_plugin.sh`. Verify browser import/start after closing old service-worker tabs and exercise the plugins in actual DAWs when available. Headless engine checks do not establish real DAW or hardware acceptance. Update user-guide, architecture and status together; document any intentional surface difference and its verification limit before considering the task done.

For hardware work, read [hardware-library.md](hardware-library.md). Run `npm run test:hardware` and the `fm1-host` tests; `check:firmware` includes both. Keep new firmware protocols behind explicit capabilities/version checks. Test transfer failures without hardware, then record physical acceptance separately. Shared companion changes require rebuilding all native bundles as well as the browser assets because native hosts embed the same sources.

Session lifecycle: standalone Finder recall is a host preference, never a global instrument override for plugins. Browser selection belongs to its instance URL; sound still needs a gesture. Reset must mean current firmware's initial persistent state on every host, with explicit confirmation and no physical-device writes. Standalone archives its previous flash after orderly shutdown; plugin/browser users export a setup before reset. Do not describe saved-flash recall as complete CPU/RAM session recovery.

## App identity and icons

Follow [the icon platform contract](icons.md) for source/generated assets, platform-specific commands and acceptance. Regenerate on macOS with full Xcode, validate, then rebuild standalone, plugins and web because the native companion embeds its favicon. Windows/Linux builds consume committed exports. Use NO_INSTALL=1 for plugin verification. The standalone macOS app and command-line release archives are separate packaging paths.

## Realtime playback checks

`cargo test --release --locked -p fm1-host -p fm1-live` covers stereo order, ring wraparound, conversion phase at 32/44.1/48/96 kHz, prefill, dropout fades, allocation-free playback and bounded recording overflow/finalization. `node --test web/studio/audio-buffer.test.mjs` covers queue bounds, stale boots, pause flush, recovery and latency decay. These deterministic tests do not establish device latency or sustained zero-underrun operation. Use `FM1_TEST_FWSC` with the plugin engine tests to exercise a local Felucca fixture rather than skipping it.

For actual output-device playback without changing open sessions or saved flash, run `cargo run --release --locked -p fm1-live --example audio_probe -- reference/firmwares/sloop/sloop3-3.3.7.area 30 12`. The diagnostic holds three keys after four seconds, requests 128 device frames, uses the standalone adaptive queue from the specified starting target and reports output underruns, device errors and worst emulation chunk duration. It separates the first ten seconds from later underruns but does not establish perceptual equivalence. Do not run concurrent playback probes when comparing settings.
