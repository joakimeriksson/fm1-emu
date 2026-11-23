---
load_when: "icon artwork, platform icon assets, native bundle branding, favicon or home-screen icons"
last_verified: 2026-11-23
source_refs:
  - assets/icons/**
  - tools/generate-icons.swift
  - tools/check_icon_artwork.py
  - tools/package_icons.py
  - tools/package_macos_app.sh
  - tools/build_plugin.sh
  - tools/windows-icon.rs
  - crates/fm1-live/src/studio/app_icon.rs
  - crates/fm1-plugin/build.rs
  - crates/fm1-plugin/wrap/CMakeLists.txt
  - tools/build_web.sh
  - tools/package_web_icons.py
---
# Icon identity and platform contract

## User-approved intent

One FM-1 identity, with platform-specific containers. Preserve the actual hardware arrangement: four left knobs in a 2×2 cluster, four upper-right knobs, paired octave buttons, center-left screen, right 2×6 button bank and lower staggered capsule touch keys. Do not return to a conventional piano keyboard.

The icon is text-free. The screen stays blank, dark and unlit: no waveform, algorithm diagram, patch name, meter, colored bar or glow. Ivory control markers are retained. No button or key is shown active.

The approved material reference is the user's flat charcoal instrument UI. Body, knobs, buttons and keyboard use clean smooth charcoal surfaces, gentle top light, restrained edge definition and soft local contact shadows. No grain, mottling, cloudy patches, brushed metal, machining marks or shiny chrome. The body is a separate uniform #292a2b fill; never ask image generation to repaint that background. The isolated controls remain the artwork master. The labeled UI reference establishes materials and lighting only: do not copy its labels, screen content or glowing/active controls into the icon.

Modern macOS supplies its own outer enclosure. Its artwork must fill the entire opaque square with **no baked outer bevel, frame, rounded corners or transparent padding**. Do not reuse the pre-rounded legacy export as the native Icon Composer layer: that previously produced a small device inside an extra white system frame. Conversely, Windows/Linux/ordinary web exports need their own rounded silhouette. Maskable and Apple touch exports have opaque backgrounds for platform masking. These are intentional differences, not asset drift.

## Source and generated artifacts

- `assets/icons/controls.png`: canonical isolated hardware, including its blank screen. Change this for material refinements; preserve layout and transparency.
- `tools/generate-icons.swift`: composes the solid body in explicit sRGB, resamples all sizes, writes Linux ARGB and the inline companion favicon, and compiles Icon Composer assets.
- `assets/icons/source.png`: generated full-bleed preview; not a second editable master.
- `assets/icons/FM1.icon`: real Icon Composer document. Its raster image is the generated opaque full-canvas composition; opacity preserves layout bounds and prevents automatic cropping of isolated controls. No glass effect is baked into the layer.
- `assets/icons/macos/Assets.car`: native macOS compiled catalog. Commit this and other generated exports so Windows/Linux/web builds do not require Xcode.
- `assets/icons/FM1.icns`, `FM1.iconset`, `FM1.ico`, `png/`, `linux-64.argb`, and `assets/icons/web/icons/`: platform exports, not independent designs. Exact dimensions are listed in [the asset inventory](../assets/icons/README.md).

## Regenerate, validate and package

From the repository root, on a Mac with full Xcode and Icon Composer support:

```sh
swift tools/generate-icons.swift
python3 tools/check_icon_artwork.py
python3 tools/package_icons.py
```

The packager writes `target/FM-1-icons.zip` and refreshes `target/FM-1-icons/`. Neither step publishes or installs anything. Commit source and generated exports together. The companion favicon export is retained as an asset; there is no companion runtime in this branch. After every change, rebuild the standalone app and all other local targets using [the mandatory rebuild checklist](development.md#rebuild-after-changes).

## Build each platform

| Platform | Inputs and integration | Build/repackage command |
|---|---|---|
| macOS standalone `.app` | Assets.car + ICNS fallback, CFBundleIconName=FM1 and CFBundleIconFile set before signing | `tools/package_macos_app.sh` |
| macOS AU/VST3/CLAP | Same catalog and fallback in each bundle, including nested CLAP; sign after copying | `NO_INSTALL=1 tools/build_plugin.sh` |
| Windows standalone | Multi-resolution ICO embedded by Rust build script using MSVC/Windows SDK rc.exe; window icon uses that resource | `cargo build --release --locked -p fm1-live -p fm1-emu` on Windows |
| Windows VST3/CLAP | ICO in the CLAP DLL and CMake VST3 wrapper; no AU target | PowerShell: `$env:NO_INSTALL='1'; .\tools\build_plugin.ps1` |
| Linux standalone | Embedded ARGB for X11; PNG and optional desktop launcher for desktop integration | `cargo build --release --locked -p fm1-live -p fm1-emu`, then `tools/package_apps.sh linux-arm64` or `linux-x86_64` on the matching build machine |
| Browser, Android/iOS home screen | ICO/PNG favicons, Apple touch PNGs, any/maskable manifest PNGs and relative HTML links | `npm run build:web` |

`tools/package_apps.sh` packages command-line program releases, not the standalone macOS `.app`; those are separate paths. Windows program archives can be packaged with `tools/package_apps.sh windows-x86_64` from the supported Windows release shell. Linux desktop installation details are in the asset inventory; packaging does not change system folders. Wayland and DAW plugin browsers control their own presentation. Mobile support here means browser/home-screen assets, not native iOS/Android binaries.

`npm run build:web` calls tools/build_web.sh: the unmodified upstream build runs first, then tools/package_web_icons.py applies committed icons and relative HTML links to web/dist/. Source browser exports and manifest live under assets/icons/web/. Direct web/build.sh produces upstream output without this overlay. Browser Studio and Hardware Library remain outside this fork's scope; their icon exports are retained for reuse. SMK-37 and test/demo pages keep their upstream identity.

## Acceptance and known limits

The artwork check validates uniform body pixels, a blank screen and the full-bleed native layer. Also check dimensions/ICO frames, manifest URLs and offline coverage; compare bundled ICNS/catalog bytes; verify macOS signatures. Inspect the **actual Finder preview**, not only the ICNS or an intermediate image. Confirm full-size enclosure, no white frame, no cropped controls, and a blank screen.

Standalone packaging derives a bundle revision from the catalog digest, touches the signed bundle and re-registers only that app with Launch Services. This refreshes artwork caches without restarting Finder/Dock or clearing global caches. Already-running app copies may need reopening.

Earlier revisions established Finder appearance. Recheck this branch after refinements and record current build, signature and resource checks in [status](status.md); historical Finder acceptance does not establish the current bundle's display. Windows/Linux runtime presentation, mobile home-screen appearance and real DAW plugin-list artwork remain unverified. Do not describe a Mac build or raster preview as acceptance on those hosts.
