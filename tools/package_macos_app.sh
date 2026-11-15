#!/usr/bin/env bash
# Build a local standalone .app without changing installed applications or plugins.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --locked -p fm1-live
app_bundle="target/apps/FM-1 Studio.app"
mkdir -p "$app_bundle/Contents/MacOS"
cp target/release/fm1-live "$app_bundle/Contents/MacOS/fm1-live"
cat > "$app_bundle/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>fm1-live</string>
<key>CFBundleIdentifier</key><string>org.fm1studio.standalone</string>
<key>CFBundleName</key><string>FM-1 Studio</string>
<key>CFBundleDisplayName</key><string>FM-1 Studio</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>11.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST
codesign --force --sign - "$app_bundle"
echo "Built $app_bundle (local ad-hoc signature; upstream native device UI)."
