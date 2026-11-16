# FM-1 icon asset inventory

The approved visual intent and platform-specific build instructions live in [the icon platform contract](../../context/icons.md). Read that guide before changing artwork, exports or bundle metadata.

The master `controls.png` is an isolated, text-free hardware overlay. Preserve the actual FM-1 layout, blank unlit screen, ivory markers, flat smooth charcoal control faces, gentle top light and soft local contact shadows. No surface texture, cloudy patches, metallic machining, labels or active/glowing controls. The body is composited separately as uniform #292a2b. `source.png` is the generated opaque full-bleed preview, not another master.

| Asset | Purpose |
|---|---|
| `FM1.icon/icon.json` + `FM1.icon/Assets/device.png` | Modern macOS source: opaque full-canvas artwork with no baked outer frame, bevel, rounded mask or padding; macOS supplies its enclosure |
| `macos/Assets.car` | Xcode-compiled macOS catalog, copied to standalone and each AU/VST3/CLAP bundle before signing |
| `FM1.icns` + `FM1.iconset/` | Legacy macOS fallback: 16–1024 px including standard/Retina slots |
| `FM1.ico` | Windows resources: PNG frames at 16/20/24/32/40/48/64/96/128/256 px |
| `png/fm1-SIZE.png` | Rounded transparent exports: 16/20/24/32/40/48/64/96/128/256/512/1024 px |
| `linux-64.argb` | Embedded X11 window artwork, generated from the 64 px export |
| `fm1-studio.desktop` | Optional Linux desktop entry; use `fm1-512.png` as `~/.local/share/icons/hicolor/512x512/apps/fm1-studio.png` and put the entry under `~/.local/share/applications` with fm1-live on PATH |
| `favicon-data-uri.txt` | Self-contained native Hardware Library favicon |
| `../../web/favicon.ico` | Browser favicon with 16/32/48 px frames |
| `../../web/icons/favicon-SIZE.png` | 16/32/48 px browser PNGs |
| `../../web/icons/apple-touch-icon-SIZE.png` | Opaque 120/152/167/180 px exports; HTML uses 180 px for downsampling |
| `../../web/icons/icon-SIZE.png` | 192/512 px manifest icons with purpose=any |
| `../../web/icons/maskable-SIZE.png` | Separate opaque 192/512 px variants with hardware inside the central mask-safe region |
| `../../web/site.webmanifest` | Shared browser/home-screen identity and relative icon URLs |

Regenerate once from the repository root on macOS with full Xcode/Icon Composer support; then validate and package:

```sh
swift tools/generate-icons.swift
python3 tools/check_icon_artwork.py
python3 tools/package_icons.py
```

CoreGraphics/ImageIO composite and resample in explicit sRGB; iconutil writes ICNS; actool compiles the native catalog. Generated assets are committed so ordinary Windows/Linux/web builds do not need macOS or an image-generation service. Do not hand-edit the exports independently.

Modern macOS bundles need both CFBundleIconName=FM1 with Assets.car and CFBundleIconFile with the ICNS fallback. An opaque native image preserves canvas bounds; a transparent control-only layer can be automatically cropped and enlarged. Standalone packaging derives an icon revision from the catalog digest and refreshes only that app's Launch Services registration after signing. Reopen already-running copies to refresh the Dock. Verify the actual Finder preview for full enclosure, no extra white frame, no clipping and a blank screen.

Plugin-list artwork is owned by each DAW. Linux Wayland appearance is compositor-dependent. Mobile formats here support browser/home-screen use, not native iOS/Android apps. The separate SMK-37 product and test/demo pages keep their own branding.

References: [Apple Icon Composer](https://developer.apple.com/icon-composer/), [bundle icon metadata](https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleiconfile), [manifest icons](https://www.w3.org/TR/appmanifest/#icons-member).
