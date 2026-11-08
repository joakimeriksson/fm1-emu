#!/usr/bin/env bash
# Boot Felucca and render the home screen with every panel color theme.
# Output: work/themes/<name>.png (one per theme)
# Usage: tools/test_all_themes.sh [MS]   (default 3000 = 3 s boot)
set -euo pipefail
cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"

MS=${1:-3000}
BIN=reference/firmwares/felucca.xip.bin
EXE=target/release/fm1-live
THEMES=(default cool-gray black orange black-green purple white-blue)
DIR=work/themes

[ -f "$BIN" ] || { echo "missing $BIN"; exit 1; }
[ -f "$EXE" ] || { echo "build first: cargo build --release -p fm1-live"; exit 1; }

rm -rf "$DIR"
mkdir -p "$DIR"

ok=0
for t in "${THEMES[@]}"; do
  printf "%-14s " "$t"
  if "$EXE" --bin "$BIN" --shot "$MS" --theme "$t" >/dev/null 2>&1; then
    magick work/panel.ppm "$DIR/$t.png"
    rm work/panel.ppm
    sz=$(wc -c < "$DIR/$t.png")
    echo "ok  ${sz} bytes  $DIR/$t.png"
    ok=$((ok + 1))
  else
    echo "FAIL"
  fi
done

echo ""
echo "$ok/${#THEMES[@]} themes rendered in $DIR/"
[ "$ok" -eq "${#THEMES[@]}" ]
