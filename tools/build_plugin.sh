#!/usr/bin/env bash
# Build the FM-1 plugin (macOS): the CLAP from Rust (crates/fm1-plugin), then the AU and the
# VST3 around it (clap-wrapper, crates/fm1-plugin/wrap), the CLAP carried inside each bundle so
# each stands alone. Signs them (ad hoc unless SIGN_IDENTITY names a Developer ID certificate in
# the keychain), installs them in ~/Library/Audio/Plug-Ins and runs Apple's AU validator.
#   tools/build_plugin.sh            # build, install, validate
#   NO_INSTALL=1 tools/build_plugin.sh
#   SIGN_IDENTITY="Developer ID Application: ..." tools/build_plugin.sh
#   UNIVERSAL=1 tools/build_plugin.sh   # for Apple silicon and Intel Macs in one build (the release's;
#                                       # rustup target add aarch64-apple-darwin x86_64-apple-darwin)
# tools/package_plugin.sh zips what this builds for a release.
set -euo pipefail
cd "$(dirname "$0")/.."
root=$PWD
out=$root/target/plugin
mkdir -p "$out"
# a Developer ID signature takes the hardened runtime and a timestamp (notarization wants both);
# the CLAP inside each bundle is signed before it goes in, so the bundles are signed without --deep
identity=${SIGN_IDENTITY:--}
sign() {
  if [ "$identity" = "-" ]; then codesign -s - -f "$1"
  else codesign -s "$identity" -f --options runtime --timestamp "$1"; fi
}

# the CLAP: a bundle around the cdylib
if [ -n "${UNIVERSAL:-}" ]; then
  (cd crates/fm1-plugin && cargo build --release --target aarch64-apple-darwin --target x86_64-apple-darwin)
  dylibs=(crates/fm1-plugin/target/{aarch64,x86_64}-apple-darwin/release/libfm1_plugin.dylib)
else
  (cd crates/fm1-plugin && cargo build --release)
  dylibs=(crates/fm1-plugin/target/release/libfm1_plugin.dylib)
fi
clap=$out/FM-1.clap
rm -rf "$clap"
mkdir -p "$clap/Contents/MacOS"
lipo -create -output "$clap/Contents/MacOS/FM-1" "${dylibs[@]}"
version=$(sed -nE 's/^version = "(.*)"/\1/p' crates/fm1-plugin/Cargo.toml | head -1)
cat > "$clap/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>FM-1</string>
  <key>CFBundleIdentifier</key><string>io.github.joakimeriksson.fm1-emu.clap</string>
  <key>CFBundleName</key><string>FM-1</string>
  <key>CFBundlePackageType</key><string>BNDL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
</dict>
</plist>
EOF
sign "$clap"

# the AU around it
cmake -S crates/fm1-plugin/wrap -B "$out/build" -G Ninja -DCMAKE_BUILD_TYPE=Release -DFM1_CLAP="$clap" \
  -DCMAKE_OSX_ARCHITECTURES="$(lipo -archs "$clap/Contents/MacOS/FM-1" | tr ' ' ';')" >/dev/null
cmake --build "$out/build"
au=$(find "$out/build" -maxdepth 2 -name "FM-1.component" -type d | head -1)
rm -rf "$out/FM-1.component"
cp -R "$au" "$out/FM-1.component"
# the Info.plist that names the AU to hosts (clap-wrapper's copy of it runs before the bundle's
# default one is written, under Ninja)
cp "$out/build/fm1_auv2-build-helper-output/auv2_Info.plist" "$out/FM-1.component/Contents/Info.plist"
rm -rf "$out/FM-1.component/Contents/PlugIns"
mkdir -p "$out/FM-1.component/Contents/PlugIns"
cp -R "$clap" "$out/FM-1.component/Contents/PlugIns/"
sign "$out/FM-1.component"
vst3=$(find "$out/build" -maxdepth 2 -name "FM-1.vst3" -type d | head -1)
rm -rf "$out/FM-1.vst3"
cp -R "$vst3" "$out/FM-1.vst3"
rm -rf "$out/FM-1.vst3/Contents/PlugIns"
mkdir -p "$out/FM-1.vst3/Contents/PlugIns"
cp -R "$clap" "$out/FM-1.vst3/Contents/PlugIns/"
sign "$out/FM-1.vst3"
echo "built $clap, $out/FM-1.component and $out/FM-1.vst3"

[ -n "${NO_INSTALL:-}" ] && exit 0
dst=~/Library/Audio/Plug-Ins
mkdir -p "$dst/CLAP" "$dst/Components" "$dst/VST3"
rm -rf "$dst/CLAP/FM-1.clap" "$dst/Components/FM-1.component" "$dst/VST3/FM-1.vst3"
cp -R "$clap" "$dst/CLAP/"
cp -R "$out/FM-1.component" "$dst/Components/"
cp -R "$out/FM-1.vst3" "$dst/VST3/"
# (the AU host caches components: a new build is seen once it is told to look again)
killall -9 AudioComponentRegistrar 2>/dev/null || true
echo "installed in $dst"
auval -v aumu Fm1s Fm1e
