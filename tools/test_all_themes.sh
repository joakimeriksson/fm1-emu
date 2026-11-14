#!/usr/bin/env bash
# Boot Felucca and render the home screen with every panel color theme.
# Output: work/themes/<name>.ppm (and .png if ImageMagick is available)
# Usage: tools/test_all_themes.sh [MS]   (default 3000 = 3 s boot)
set -euo pipefail
cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"

MS=${1:-3000}
BIN=reference/firmwares/felucca.xip.bin
EXE=target/release/fm1-live

[ -f "$BIN" ] || { echo "missing $BIN"; exit 1; }
[ -f "$EXE" ] || { echo "build first: cargo build --release -p fm1-live"; exit 1; }

THEMES=$("$EXE" --theme INVALID 2>&1 | grep -oP 'available: \K.*' || true)
if [ -z "$THEMES" ]; then
  THEMES="default cool-gray black orange black-green purple white-blue"
fi
IFS=', ' read -ra LIST <<< "$THEMES"

HAS_MAGICK=0
command -v magick >/dev/null 2>&1 && HAS_MAGICK=1

DIR=work/themes
mkdir -p "$DIR"

ok=0
for t in "${LIST[@]}"; do
  [ -z "$t" ] && continue
  printf "%-14s " "$t"
  PPM="$DIR/$t.ppm"
  if FM1_BENCH_PANEL="$PPM" "$EXE" --bin "$BIN" --shot "$MS" --theme "$t" >/dev/null 2>&1; then
    if [ "$HAS_MAGICK" -eq 1 ]; then
      magick "$PPM" "$DIR/$t.png"
      sz=$(wc -c < "$DIR/$t.png")
      echo "ok  ${sz} bytes  $DIR/$t.png"
    else
      sz=$(wc -c < "$PPM")
      echo "ok  ${sz} bytes  $PPM"
    fi
    ok=$((ok + 1))
  else
    echo "FAIL"
  fi
done

echo ""
echo "$ok/${#LIST[@]} themes rendered in $DIR/"
[ "$ok" -eq "${#LIST[@]}" ]
