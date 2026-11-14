#!/bin/sh
# Create a deterministic FAT16 test disk image for USB host emulation tests.
# Usage: create_test_disk.sh OUTPUT [16|32]
# FAT16: 32 MiB; FAT32: 64 MiB. Never overwrites an existing image.
# Requires: dd, mkfs.fat (dosfstools), mcopy (mtools).
#
# The image is reproducible: same content, same layout, same label.
# Tests can assert on known filenames, sizes, and byte content.
set -eu

missing=
for cmd in mkfs.fat mcopy python3; do
    command -v "$cmd" >/dev/null 2>&1 || missing="$missing $cmd"
done
if [ -n "$missing" ]; then
    echo "missing:$missing" >&2
    echo "install with one of:" >&2
    echo "  nix:  nix profile install nixpkgs#dosfstools nixpkgs#mtools nixpkgs#python3" >&2
    echo "  apt:  sudo apt install dosfstools mtools python3" >&2
    echo "  brew: brew install dosfstools mtools python3" >&2
    exit 1
fi

IMG=${1:?usage: create_test_disk.sh OUTPUT [16|32]}
FAT=${2:-16}
case "$FAT" in
    16) SIZE_MB=32 ;;
    32) SIZE_MB=64 ;;
    *) echo 'FAT must be 16 or 32' >&2; exit 2 ;;
esac
LABEL=MODDTEST
export TZ=UTC
export SOURCE_DATE_EPOCH=1735689600
export MTOOLSRC=/dev/null

mkdir -p "$(dirname "$IMG")"
TMPD=$(mktemp -d)
trap 'rm -rf "$TMPD"' EXIT

# -- image --
# Exclusive creation protects existing images, including work/udisk.img.
python3 - "$IMG" "$SIZE_MB" <<'PY'
import sys
with open(sys.argv[1], 'xb') as image:
    image.truncate(int(sys.argv[2]) * 1024 * 1024)
PY
mkfs.fat --invariant -i 4d4f4444 -F "$FAT" -n "$LABEL" "$IMG" >/dev/null

# -- file 1: README.TXT (233 bytes) --
cat > "$TMPD/readme.txt" << 'ENDFILE'
FM-1 USB Disk Test
==================

This is a test image for the fm1-emu USB host emulation.
The firmware's FatFS reads this FAT16 volume through the
emulated MUSB host controller and MSC Bulk-Only Transport.

Created: 2026-11-09
ENDFILE

# -- file 2: PRESETS.TXT (606 bytes) --
cat > "$TMPD/presets.txt" << 'ENDFILE'
[preset:lead-1]
name = Bright Saw Lead
algorithm = 7
osc1_wave = saw
osc1_detune = +3
osc2_wave = saw
osc2_detune = -3
filter_cutoff = 0.72
filter_resonance = 0.35
env_attack = 0.01
env_decay = 0.30
env_sustain = 0.60
env_release = 0.45
lfo_rate = 4.2
lfo_depth = 0.15
fx_reverb = 0.40
fx_delay = 0.25

[preset:pad-warm]
name = Warm Pad
algorithm = 3
osc1_wave = triangle
osc1_detune = +1
osc2_wave = sine
osc2_detune = -1
filter_cutoff = 0.45
filter_resonance = 0.20
env_attack = 0.80
env_decay = 0.50
env_sustain = 0.85
env_release = 1.20
lfo_rate = 0.8
lfo_depth = 0.08
fx_reverb = 0.65
fx_delay = 0.10
ENDFILE

# -- file 3: SETLIST.CSV (229 bytes) --
cat > "$TMPD/setlist.csv" << 'ENDFILE'
track,bpm,key,preset,notes
1,120,C,lead-1,opener - build from soft to full
2,95,Am,pad-warm,ambient interlude
3,140,F#m,lead-1,high energy section
4,110,Eb,pad-warm,breakdown - half time feel
5,128,G,lead-1,finale - all hands up
ENDFILE

python3 - "$TMPD" <<'PY'
import os
from pathlib import Path
import sys
p = Path(sys.argv[1])
# 33 sectors plus a short tail: crosses FAT16 and FAT32 cluster boundaries.
(p / 'pattern.bin').write_bytes(bytes((i * 17 + (i >> 9) * 31) & 255
                                     for i in range(33 * 512 + 137)))
for f in p.iterdir():
    os.utime(f, (1735689600, 1735689600))
PY
mcopy -m -i "$IMG" "$TMPD/readme.txt"  ::README.TXT
mcopy -m -i "$IMG" "$TMPD/presets.txt"  ::PRESETS.TXT
mcopy -m -i "$IMG" "$TMPD/setlist.csv"  ::SETLIST.CSV
mcopy -m -i "$IMG" "$TMPD/pattern.bin"  ::PATTERN.BIN

echo "udisk: $IMG ($SIZE_MB MiB FAT$FAT, label $LABEL)"
sha256sum "$IMG"
mdir -i "$IMG" :: 2>/dev/null || true
