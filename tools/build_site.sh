#!/usr/bin/env bash
# The GitHub Pages site in _site (or $1): the emulator's page at the top, the FM-1 SID player
# (fm1-sid/web) under fm1-sid/ with the same wasm, Try Hard's installer under try-hard/ (with its package
# when try-hard/build.sh has built it).
#   tools/build_site.sh && (cd _site && python3 -m http.server 8642)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/_site}"
mkdir -p "$OUT" && OUT="$(cd "$OUT" && pwd)"
"$ROOT/web/build.sh" "$OUT" >/dev/null
mkdir -p "$OUT/fm1-sid"
(cd "$ROOT/fm1-sid/web" && cp -R $(ls | grep -v '^smoke\.mjs$') "$OUT/fm1-sid/")
cp "$OUT/fm1_web.wasm" "$OUT/fm1-sid/"
mkdir -p "$OUT/try-hard"
cp "$ROOT/try-hard/web/"* "$OUT/try-hard/"
if [ -f "$ROOT/try-hard/build/try-hard.fwsc" ]; then cp "$ROOT/try-hard/build/try-hard.fwsc" "$OUT/try-hard/"
else echo "(no try-hard/build/try-hard.fwsc: the installer has no package)"; fi
echo "$OUT: emulator + fm1-sid/ + try-hard/"
