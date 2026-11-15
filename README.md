# FM-1 Studio

An original-firmware FM-1 instrument for your Mac, DAW and browser, with room for a larger collection around the device.

The aim is to run the same firmware you install on an M-VAVE FM-1: its sound, sequencer, screen and behavior. This fork builds on [Joakim Eriksson’s fm1-emu](https://github.com/joakimeriksson/fm1-emu). It adds a local-first workspace and library around the existing binary emulator.

**Early development.** Native standalone and plugin targets retain the upstream device panel. The new Studio interface is currently in the browser. Full session snapshots, expanded firmware editors and physical-device transfers are still planned. There is no hardware available for verification yet.

## What you can use now

- Build the native Apple Silicon standalone emulator and VST3/AU/CLAP bundles.
- Build a static browser studio with the same WASM emulation core and playable hardware view.
- Import original `.fwsc` firmware and `.fm1save` persistent setups into a local, hash-addressed collection.
- Search, reopen and export items, and back up/restore the collection.
- Cache browser runtime assets for offline use; imported firmware is stored locally.
- Download Felucca explicitly from the UI to try the instrument.

A firmware boot is not proof of sound or timing equivalence on hardware. Each firmware/version still needs compatibility testing. The browser and native/plugin targets have different MIDI capabilities; see [status](context/status.md).

## Build and run

Requires Node.js 22+, Rust with the `wasm32-unknown-unknown` target, Xcode Command Line Tools, CMake and Ninja on macOS. There are no npm dependencies to install for Studio. First builds download Rust/SDK dependencies.

```sh
export PATH="$HOME/.cargo/bin:$PATH"
rustup target add wasm32-unknown-unknown

# Static Studio
npm run build:web
npm run serve
```

Open [FM-1 Studio locally](http://127.0.0.1:8642/studio/). Import a firmware or choose **Get Felucca**, then click **Start** on the device. Play the panel with mouse/touch or its documented keyboard controls.

After **Ready offline** appears, reload once so the service worker controls the page. Imported packages and dependencies are local; test reopening the page offline before relying on it. Browser data can be cleared or evicted: export backups to files.

```sh
# Native standalone
tools/package_macos_app.sh
cargo build --release --locked -p fm1-emu
target/release/fm1-live --fwsc /path/to/firmware.fwsc --flash /path/to/working-flash.bin

# Build plugin bundles without installing over existing plugins
NO_INSTALL=1 tools/build_plugin.sh
```

The standalone application is produced at `target/apps/FM-1 Studio.app`. It is ad-hoc signed for local use and retains the upstream native panel. Double-click it and select a firmware package.

Plugin bundles are produced under `target/plugin/`. The current build retains upstream identifiers; install intentionally and avoid replacing another FM-1 installation. Signing/notarization and real DAW validation remain separate from a successful build. See the [upstream plugin guide](crates/fm1-plugin/README.md).

## Library and saves

Firmware is retained byte-for-byte, identified by SHA-256. Different versions coexist. Studio validates the package before adding it, and collection imports are committed together after validation.

- `.fwsc`: original firmware package.
- `.fm1save`: firmware identity plus saved flash changes, interoperable with upstream tools where supported.
- `.fm1library`: Studio collection archive; not a device backup or flashable firmware.

Saved flash does **not** preserve unsaved RAM edits. Use the firmware’s Save command before exporting. Full volatile machine snapshots remain an explicit engineering gate. Different setups of the same firmware currently share the device page’s working flash; export before replacing a setup.

The library remains on this browser origin. It does not sync automatically with native apps or another browser. Keep exported backups separate from browser storage. Initial collection archives are bounded to 256 items and 24 MiB of imported binary content.

## Design

The proposed visual language uses an eight-pixel layout grid, warm paper surfaces, graphite typography, cobalt actions and restrained signal accents. System fonts and locally packaged assets keep the interface independent of remote services.

[Design system](context/design-system.md) documents token values, grid rules, component states, accessibility, responsive layouts and native adoption. The Studio **Design system** view previews the implemented tokens. Native emulated hardware geometry remains upstream until deliberately adapted.

## Development and verification

```sh
# Optional published firmware fixture (downloaded into ignored reference/)
node tools/fetch_firmwares.mjs felucca

FM1_TEST_FWSC=reference/firmwares/felucca.fwsc npm run test:studio
cargo test --release --locked -p fm1-core -p fm1-soc
npm run check:context
```

See [development](context/development.md) for build outputs and validation commands. [Status](context/status.md) separates measured results from pending work.

## Project context

Start with [overview](context/overview.md). Agents read [AGENTS.md](AGENTS.md), which routes task-specific context rather than loading every document.

- [Architecture and boundaries](context/architecture.md)
- [Product specification](context/product-specification.md)
- [Research and project comparisons](context/research.md)
- [Roadmap and acceptance gates](context/roadmap.md)
- [Original upstream README](docs/UPSTREAM-README.md)

Context follows the supplied ez-context skill, preserved in `.agents/skills/ez-context/`. Existing upstream technical documentation remains under `docs/`.

## Attribution and licensing

Emulator code is MIT; see [LICENSE](LICENSE). This fork retains upstream authorship and technical documentation. Firmware packages, patches, samples, fonts and other third-party material retain their own licenses. Downloading a firmware does not authorize redistributing it.

This is an independent project, not affiliated with M-VAVE.
