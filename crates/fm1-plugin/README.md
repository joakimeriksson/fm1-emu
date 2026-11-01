# FM-1 as a plugin (AU, VST3, CLAP)

The emulator as an instrument in a DAW: load a firmware package (`.fwsc`) or a shared setup (`.fm1save`) in the plugin’s
window, play it from the host's MIDI, and play its panel with the mouse. It's the same panel as
on the page, so an SMK-37 package gets the SMK-37's.

| Format | For | Built by |
|---|---|---|
| AU (v2) | Logic, GarageBand, and other Mac hosts | clap-wrapper |
| VST3 | Ableton Live, Cubase, FL Studio, Reaper, Bitwig, Studio One… | clap-wrapper |
| CLAP | Bitwig, Reaper, FL Studio, Studio One… | nih-plug (Rust) |

One plugin is written in Rust as a CLAP with [nih-plug](https://github.com/robbert-vdh/nih-plug),
built without its VST3 bindings (they are GPL). [clap-wrapper](https://github.com/free-audio/clap-wrapper)
makes the AU and the VST3 of it, each carrying the CLAP in its bundle on macOS; on Windows the
VST3 loads the CLAP from the CLAP folder beside it. Every part is permissively licensed:

| Part | Licence |
|---|---|
| this plugin | MIT |
| nih-plug | ISC |
| clap-wrapper and the CLAP SDK | MIT |
| Apple's AudioUnitSDK | Apache-2.0 |
| Steinberg's VST3 SDK (3.8 and later) | MIT |

## Build and install (macOS)

```sh
tools/build_plugin.sh
```

You need Rust, CMake and Ninja (`brew install cmake ninja`). The script builds the CLAP, then the
AU and the VST3 (clap-wrapper downloads the SDKs it needs on the first run). It signs them ad hoc,
installs them in `~/Library/Audio/Plug-Ins/{Components,VST3,CLAP}` and runs Apple's AU validator
(`auval -v aumu Fm1s Fm1e`). Add `NO_INSTALL=1` to only build them, into `target/plugin/`.

A host that is already running doesn't see the new build: GarageBand and Logic look for plugins
when they start, so quit them (⌘Q) and open them again.

**Steinberg's validator and test host** are built from the VST3 SDK that clap-wrapper downloaded.
The SDK wants a full Xcode, so `XCODE_VERSION` stands in for one when only the Command Line Tools
are installed:

```sh
cd target/plugin
XCODE_VERSION=16.0 cmake -S build/cpm/vst3sdk -B vst3sdk-build -G Ninja -DXCODE_VERSION=16.0 \
  -DCMAKE_BUILD_TYPE=Release -DSMTG_ENABLE_VST3_PLUGIN_EXAMPLES=OFF -DSMTG_ENABLE_VSTGUI_SUPPORT=OFF
cmake --build vst3sdk-build --target validator editorhost
vst3sdk-build/bin/Release/validator ~/Library/Audio/Plug-Ins/VST3/FM-1.vst3   # 47 tests pass
open vst3sdk-build/bin/Release/editorhost.app --args ~/Library/Audio/Plug-Ins/VST3/FM-1.vst3
```

An ad hoc signature is enough on the Mac that built the plugin. To give it to others, it has to be
signed with an Apple Developer ID and notarized.

## Build and install (Windows, x86-64)

```powershell
tools\build_plugin.ps1
```

You need Rust (the MSVC toolchain), Visual Studio Build Tools with the C++ workload, CMake and
git. With winget:

```powershell
winget install Rustlang.Rustup Kitware.CMake Git.Git
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

The script builds the CLAP (the cdylib, `fm1_plugin.dll`, is `FM-1.clap`), then the VST3 around
it with clap-wrapper (CMake picks the newest Visual Studio it finds, the Build Tools included, so
no vcvars is needed; the SDKs are downloaded on the first run). It installs both in the per-user plugin folders, no administrator
needed: `%LOCALAPPDATA%\Programs\Common\VST3\FM-1.vst3` and `…\Common\CLAP\FM-1.clap`. Set
`$env:NO_INSTALL = 1` to only build them, into `target\plugin\`.

Unlike the macOS bundles, the Windows VST3 doesn't carry the CLAP: when a host loads it, it
loads `FM-1.clap` from a CLAP folder (`C:\Program Files\Common Files\CLAP`,
`%LOCALAPPDATA%\Programs\Common\CLAP`, or a folder in `CLAP_PATH`). So the two files go
together, and a VST3 host needs the CLAP installed even if it doesn't host CLAPs itself. A DAW
that doesn't scan the per-user VST3 folder gets `FM-1.vst3` in
`C:\Program Files\Common Files\VST3` instead (and `FM-1.clap` in `…\Common Files\CLAP`), or its
settings get the per-user folder added to the plugin paths.

**Steinberg's validator** is built from the SDK that clap-wrapper downloaded, and `CLAP_PATH`
lets it find the CLAP that was built but not installed:

```powershell
cmake -S target\plugin\build\cpm\vst3sdk -B target\plugin\vst3sdk-build -A x64 `
  -DSMTG_ENABLE_VST3_PLUGIN_EXAMPLES=OFF -DSMTG_ENABLE_VSTGUI_SUPPORT=OFF -DSMTG_ENABLE_VST3_HOSTING_EXAMPLES=ON
cmake --build target\plugin\vst3sdk-build --config Release --target validator
$env:CLAP_PATH = "$PWD\target\plugin"
target\plugin\vst3sdk-build\bin\Release\validator.exe target\plugin\FM-1.vst3   # 47 tests pass
```

The build is not code-signed; Windows hosts load unsigned plugins. REAPER and Waveform scan the
per-user folders by default, so the plugin shows up after their next plugin scan.

## Releases

The plugin is released with the rest of the emulator (`.github/workflows/release.yml`, on a tag
`v<version>`; [docs/releases/README.md](../../docs/releases/README.md) has the steps). Its
version, in this crate's `Cargo.toml`, is the workspace's. On an Apple silicon runner the
workflow:
1. runs the tests;
2. builds and installs the plugin and runs Apple's AU validator;
3. builds Steinberg's validator and runs it on the VST3;
4. packages the zip (`tools/package_plugin.sh`: the three plugins and an `INSTALL.txt`).

Without signing secrets the release is unsigned (`INSTALL.txt` gives the command that lets macOS
open it). With these repository secrets it's signed with a Developer ID and notarized:
- `MACOS_CERTIFICATE`: a "Developer ID Application" certificate as a base64 `.p12`;
- `MACOS_CERTIFICATE_PASSWORD`: its password;
- `APPLE_ID`, `APPLE_TEAM_ID` and `APPLE_APP_PASSWORD`: an app-specific password, for
  notarization.

Locally, `SIGN_IDENTITY="Developer ID Application: …" tools/build_plugin.sh` then
`tools/package_plugin.sh` does the same.

## Using it

- **Load firmware / save…** opens a `.fwsc` package or a shared `.fm1save` setup from the browser
  or another plugin instance. Importing a save restores its firmware, patches and settings,
  including when the same firmware is already running. For a browser backup that omits
  bring-your-own firmware, load the matching `.fwsc` first, then import the `.fm1save`.
  Invalid imports leave the current setup intact.
  On a Mac, keep the files outside Desktop, Documents, Downloads and iCloud Drive: macOS keeps a
  DAW's plugin host out of those folders without asking, and the load fails with "Operation not
  permitted". `~/Music` or any other folder in your home works.
- **Export save…** captures the running firmware and its latest flash changes as a `.fm1save`.
  Send it to someone else, import it in the browser, or open it in another VST3/AU/CLAP instance.
  It waits for flash writes to finish and writes the file in the background. Edits held only in
  firmware RAM still need the firmware’s own Save command first. A shared file includes the
  firmware, so its redistribution licence applies.
- **DAW projects** still keep the firmware and the flash sectors it writes, so your patches come
  back when the project reopens. Exporting a file also refreshes this project-state copy.
- **MIDI** from the host goes to the MIDI IN jack. **MIDI channel** moves channel messages to one
  channel; M-VAVE's own firmware listens on 2.
- **Master** is the panel's MASTER pot, and a host parameter you can automate.
- **CPU clock** is the emulated chip's clock: 240 MHz by default (the chip's nominal clock),
  or 120, 160, 320 or 400. Most of a firmware's cost grows with the clock, so a lower one lets a
  heavy firmware keep up on a slower computer (CS-1 and FM-106 run about 1.3x as fast at 160 MHz
  as at 240, 1.5x at 120, with the same sound), as long as the firmware still gets its work done.
  A higher one gives a firmware that runs out of time more; the emulator runs one
  instruction a cycle, while the chip runs some in pairs. Changing it restarts the firmware,
  keeping its flash.
- **The panel** works as on the page:
  - a key or button is held while pressed, and a drag across the keys plays a glissando;
  - a knob turns when you drag it up or down, or scroll over it;
  - on the SMK-37 the pads, faders and wheels work too.
- **The status bar** shows the emulation's speed against real time, and counts the frames that
  came late when the computer couldn't keep up.

## How it runs

The firmware makes its audio in DMA halves, and how long a half takes to emulate varies. So the
machine runs on a thread of its own, a little ahead of the host:
- **Frames:** it keeps a ring of its 44.1 kHz frames about one host block plus 1024 frames ahead
  of what the host has taken.
- **MIDI timing:** host MIDI arrives stamped with the frame it belongs to, and the machine takes
  it when it reaches that frame. A note sounds where the host placed it, and the delay is a
  constant the plugin reports to the host as its latency, so the host compensates for it.
- **Sample rate:** the output is resampled to the host's rate.

**A host that takes no audio:** some hosts show the window without processing audio, such as
Steinberg's test host, or a track that isn't playing. If the host hasn't taken any frames for
50 ms, the machine runs on in real time, as the device would. Its audio is dropped and not
counted, so the host's stamps still line up when it takes frames again.

**Load:** one instance takes about one CPU core. The slowest firmwares run at 1.4–1.7x real time
on an Apple Silicon Mac (most at 3–6x) at the default 240 MHz, so keep two or three heavy
instances per project. The CPU clock moves the cost (X0X on an M4 Pro: 2.3x real time at
240, 1.7x at 320, 1.3x at 400); a heavy firmware that comes late can run at 160 or 120.

`cargo test --release` (here) runs a firmware on the machine's thread. It checks that a stamped
note sounds on time, and that the firmware boots and draws its screen when nothing takes its
audio. It uses `reference/firmwares/felucca.fwsc` (`node tools/fetch_firmwares.mjs`);
`FM1_TEST_FW=<id>` picks another firmware for the second test.

## Tested

| Check | Result |
|---|---|
| Apple's AU validator (`auval`) | passes: rendering at 11.025–192 kHz, latency, state, the window |
| Steinberg's VST3 validator | 47 of 47 tests pass |
| GarageBand | AU: loads a firmware and runs; the window's redraw on reopening is fixed but not yet seen there |
| Steinberg's editorhost | VST3: the window draws, firmwares boot and react to the panel |
| Logic, Ableton, REAPER, Bitwig | not yet tried |
| Windows (x86-64) | builds with `tools\build_plugin.ps1`; the crate's 12 tests and Steinberg's validator (47 of 47) pass on Windows 11 |
| REAPER 7.84, Waveform Free 14 (Windows 11) | VST3: both find it in the per-user folder, load Felucca through the file dialog and play it from the panel and MIDI (3.4x real time on this PC) |
| Linux | not built yet: nih-plug and clap-wrapper support it, but the build scripts are macOS and Windows only |

## Testing portable saves locally

```sh
node tools/fetch_firmwares.mjs felucca
cargo test --manifest-path crates/fm1-plugin/Cargo.toml --release --locked
node tools/check_plugin_save_interop.mjs
env NO_INSTALL=1 tools/build_plugin.sh
```

The interoperability check uses the real browser codec at `web/persistence.js` and the real
plugin engine. Until the browser save PR is merged, pass a local copy of its `persistence.js`
as the first argument. The tests also cover a browser-generated format fixture with synthetic
bytes, rejected malformed saves, same-firmware replacement, live SPI snapshots and XIP restoration.
The build command creates local bundles in `target/plugin` without changing installed plugins.

The local AU smoke check loads the built bundle directly, renders audio and round-trips
project state without installing it:

```sh
clang++ -std=c++17 tools/check_plugin_au.cpp -framework AudioToolbox -framework CoreFoundation -o /tmp/fm1-check-au
/tmp/fm1-check-au "$PWD/target/plugin/FM-1.component"
```

This checks the AU API; the new file dialogs still need an interactive DAW check.
