#!/usr/bin/env bash
# Pack the release's command-line programs (fm1-live, fm1-emu, built with
# `cargo build --release -p fm1-live -p fm1-emu`) for one platform:
# target/release-apps/fm1-emu-<version>-<platform>.tar.gz (on Windows .zip) with the two programs
# and a README.
#   tools/package_apps.sh macos-arm64|linux-arm64|linux-x86_64|windows-x86_64
# Not signed: the README says how to let macOS (or Windows SmartScreen) run them.
set -euo pipefail
cd "$(dirname "$0")/.."
platform=${1:?usage: tools/package_apps.sh PLATFORM}
version=$(sed -nE 's/^version = "(.*)"/\1/p' Cargo.toml | head -1)
name=fm1-emu-$version-$platform
out=target/release-apps
rm -rf "$out/$name" "$out/$name.tar.gz" "$out/$name.zip"
mkdir -p "$out/$name"
exe=
run=./fm1-live
case $platform in windows-*) exe=.exe run=fm1-live ;; esac
cp target/release/fm1-live$exe target/release/fm1-emu$exe "$out/$name/"

case $platform in
  macos-arm64)
    speed="It runs the FM-1's 240 MHz clock in real time with its AArch64 JIT."
    setup='These programs are not signed with an Apple Developer ID, so macOS blocks them until you
allow it. In this folder, run once:

    xattr -d com.apple.quarantine fm1-live fm1-emu'
    ;;
  linux-arm64)
    speed="It has the same AArch64 JIT as on a Mac; on a slower machine, if the sound stutters,
lower the clock: --mhz 160."
    setup='It needs ALSA and X11. Debian 12, Ubuntu 22.04:
    sudo apt install libasound2 libx11-6 libxkbcommon0 libxcursor1 libxrandr2 libxi6
Ubuntu 24.04 and later: libasound2t64 in place of libasound2.'
    ;;
  linux-x86_64)
    speed="There is no JIT for x86-64 yet (planned for the next release): it interprets, at a
quarter to half of the JIT's speed. The lighter firmwares keep up; the heavier ones (Felucca needs
about 220 M instructions a second at 240 MHz) may not, even on a fast PC. If the sound stutters,
lower the clock: --mhz 160, or 120."
    setup='It needs ALSA and X11. Debian 12, Ubuntu 22.04:
    sudo apt install libasound2 libx11-6 libxkbcommon0 libxcursor1 libxrandr2 libxi6
Ubuntu 24.04 and later: libasound2t64 in place of libasound2.'
    ;;
  windows-x86_64)
    speed="It has an x86-64 JIT. If the sound stutters, lower the clock: --mhz 160. The audio goes
to the default output (WASAPI, shared mode)."
    setup='These programs are not signed, so Windows SmartScreen may stop the first start: "More
info", then "Run anyway". Double-click fm1-live.exe to pick a firmware package, or run it from a
terminal (PowerShell, cmd) in this folder.'
    ;;
  *) echo "unknown platform $platform" >&2; exit 1 ;;
esac

cat > "$out/$name/README.txt" <<TXT
fm1-emu $version: the M-VAVE FM-1 emulator, for $platform
https://github.com/joakimeriksson/fm1-emu

fm1-live plays a firmware live: the FM-1's screen and panel in a window, the computer keyboard
as the keys, every MIDI input of the computer, audio out.
$speed

$setup

Play a firmware package (.fwsc): a community firmware (the list, with each author's link:
https://joakimeriksson.github.io/fm1-emu/ ; the packages: https://thefirmwarehub.com/) or
M-VAVE's own FM-1.fwsc:

    $run --fwsc felucca.fwsc
    $run --fwsc FM-1.fwsc --syx bank.syx     (M-VAVE's own, a DX7 bank loaded)
    $run --help

fm1-emu is the batch runner for tests and debugging: WAV and screen output, breakpoints, memory
dumps (see the repository's README).

MIT licensed. The firmwares are their authors'.
TXT

case $platform in
  windows-*)
    # (Windows' own zip: Explorer opens it; the runner's bash has no zip)
    (cd "$out" && powershell -NoProfile -Command "Compress-Archive -Path '$name' -DestinationPath '$name.zip'")
    rm -rf "$out/$name"
    echo "packaged $out/$name.zip"
    ;;
  *)
    tar -C "$out" -czf "$out/$name.tar.gz" "$name"
    rm -rf "$out/$name"
    echo "packaged $out/$name.tar.gz"
    ;;
esac
