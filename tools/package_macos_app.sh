#!/usr/bin/env bash
# Build a local standalone .app without changing installed applications or plugins.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --locked -p fm1-live
app_bundle="target/apps/FM-1 Studio.app"
mkdir -p "$app_bundle/Contents/MacOS" "$app_bundle/Contents/Resources"
cp assets/icons/FM1.icns "$app_bundle/Contents/Resources/FM1.icns"
cp assets/icons/macos/Assets.car "$app_bundle/Contents/Resources/Assets.car"
cp target/release/fm1-live "$app_bundle/Contents/MacOS/fm1-live"
# Give Launch Services a new bundle revision when compiled artwork changes.
icon_digest=$(shasum -a 256 assets/icons/macos/Assets.car | cut -c1-4)
bundle_version="1.$((16#${icon_digest:0:2} % 100)).$((16#${icon_digest:2:2} % 100))"
cat > "$app_bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIconName</key><string>FM1</string>
<key>CFBundleIconFile</key><string>FM1.icns</string>
<key>CFBundleExecutable</key><string>fm1-live</string>
<key>CFBundleIdentifier</key><string>org.fm1studio.standalone</string>
<key>CFBundleName</key><string>FM-1 Studio</string>
<key>CFBundleDisplayName</key><string>FM-1 Studio</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>$bundle_version</string>
<key>LSMinimumSystemVersion</key><string>11.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --sign - "$app_bundle"
# Rebuilt bundles can retain Finder/Dock's previous generic icon registration.
# Refresh only this local app; never reset global icon caches or restart Finder/Dock.
touch "$app_bundle"
lsregister=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
if [ -x "$lsregister" ]; then
  "$lsregister" -f "$PWD/$app_bundle" || echo "Warning: macOS app registration refresh failed." >&2
fi
echo "Built $app_bundle (local ad-hoc signature; standalone Studio panel)."
