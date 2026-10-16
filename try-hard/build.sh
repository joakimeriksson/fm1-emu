#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-only
# Build Try Hard: Dinghy (the platform: boot, USB, the updater, the panel, the screen) at a pinned commit, with
# firmware/tryhard.c as its app and Try Hard's own package identity.
#   try-hard/build.sh [--release X.Y]      -> try-hard/build/try-hard.fwsc
#   try-hard/build.sh --host [SCRIPT [FPS]] -> try-hard/build/host/*.png: the screen, on this computer (no FM-1)
#   try-hard/build.sh --test               the gears mesh (tests/mesh_test.c), on this computer
# The package needs Linux on x86-64 with JieLi's toolchain and three AC79 SDK files, as Dinghy's README says
# (JIELI_TOOLCHAIN, AC79_SDK); --host needs only a C compiler (CC, default cc) and Python with Pillow.
set -e
HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="$HERE/build"
DINGHY_REPO="${DINGHY_REPO:-https://github.com/rgrvmusic/fm1-dinghy.git}"
DINGHY_REV=929730019a9dc362c12a710c0b139cbfefb582e9
W="$OUT/dinghy"
PY="${PYTHON:-python3}"

mkdir -p "$OUT"
[ -d "$W/.git" ] || git clone -q "$DINGHY_REPO" "$W"
git -C "$W" cat-file -e "$DINGHY_REV^{commit}" 2>/dev/null || git -C "$W" fetch -q origin
git -C "$W" checkout -q -f "$DINGHY_REV"
cp "$HERE/firmware/tryhard.c" "$W/firmware/src/dinghy.c"
# the identity: FM-1_95N (N = the minor version), the installers' FM-1_9NN form; Dinghy's is 99N
sed -i.orig -e 's/^PRODUCT = "FM-1_990"/PRODUCT = "FM-1_950"/' -e 's/"FM-1_99" + m\[2\]/"FM-1_95" + m[2]/' \
    -e 's/f"dinghy-{a.release}.fwsc"/"dinghy.fwsc"/' "$W/tools/build.py"
grep -q '"FM-1_950"' "$W/tools/build.py" || { echo "build.sh: Dinghy's build.py changed, the identity was not set"; exit 1; }

if [ "$1" = "--test" ]; then                   # the gears mesh (tests/mesh_test.c)
    mkdir -p "$OUT/host"
    ${CC:-cc} -O2 -Wall -Wno-unused-function -DDINGHY_HOST -I"$W/firmware/src" -I"$W/firmware/gen" \
        -o "$OUT/host/mesh_test" "$HERE/tests/mesh_test.c"
    exec "$OUT/host/mesh_test"
fi
if [ "$1" = "--host" ]; then
    mkdir -p "$OUT/host"
    ${CC:-cc} -O2 -Wall -Wno-unused-function -DDINGHY_HOST -DAPP_VER='"host"' -I"$W/firmware/src" -I"$W/firmware/gen" \
        -o "$OUT/host/tryhard" "$W/firmware/src/dinghy.c"
    "$OUT/host/tryhard" "$OUT/host/tryhard" "${2:-$HERE/tests/host_script.txt}" ${3:-}
    "$PY" - "$OUT/host" <<'PY'
import sys
from pathlib import Path
from PIL import Image
for p in sorted(Path(sys.argv[1]).glob("tryhard-*.ppm")):
    Image.open(p).resize((480, 480), Image.LANCZOS).save(p.with_suffix(".png"))
    p.unlink()
    print("screen ->", p.with_suffix(".png"))
PY
    exit 0
fi

(cd "$W" && ./build.sh "$@")
cp "$W/build/dinghy.fwsc" "$OUT/try-hard.fwsc"
echo "package  $OUT/try-hard.fwsc"
