#!/usr/bin/env bash
# Build the browser version into web/dist (or $1): the wasm module and the page. The page
# fetches the firmwares from their authors' sites at run time (web/firmwares.json, web/fwsc.js),
# so nothing else is bundled.
#   web/build.sh && (cd web/dist && python3 -m http.server 8642)
#   -> http://localhost:8642  (?fw=x0x picks a firmware, ?mhz=120 the clock; /smk37/ the SMK-37's page)
# On macOS it uses rustup's stable toolchain for the wasm32 target (Homebrew's rustc has no
# wasm32 std; its rust-lld finds libLLVM through DYLD_FALLBACK_LIBRARY_PATH); elsewhere plain
# cargo with the wasm32-unknown-unknown target installed (rustup target add ...).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/web/dist}"
cd "$ROOT"
# one codegen unit and a higher inline threshold: 3-4% faster in node (esp32sim measured the
# same threshold; 8000 was slower there)
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
export RUSTFLAGS="${RUSTFLAGS:-} -Cllvm-args=-inline-threshold=2000"
# the function table exported and growable: the JIT installs compiled blocks in it (web/jit.js)
RUSTFLAGS="$RUSTFLAGS -Clink-arg=--export-table -Clink-arg=--growable-table"
TC="${RUSTUP_TC:-$HOME/.rustup/toolchains/stable-aarch64-apple-darwin}"
if [ -d "$TC" ]; then
  DYLD_FALLBACK_LIBRARY_PATH="$TC/lib" RUSTC="$TC/bin/rustc" "$TC/bin/cargo" build --release -p fm1-web \
    --target wasm32-unknown-unknown --target-dir target/wasm
else
  cargo build --release -p fm1-web --target wasm32-unknown-unknown --target-dir target/wasm
fi
rm -rf "$OUT"
mkdir -p "$OUT"
cp target/wasm/wasm32-unknown-unknown/release/fm1_web.wasm "$OUT/"
cp web/index.html web/style.css web/main.js web/persistence.js web/tryout.js web/worklet.js web/jit.js web/fwsc.js web/firmwares.json web/og.png "$OUT/"
# the technical report the pages link to: the paper (docs/paper), under the name its links have
cp docs/paper/fm1-paper.pdf "$OUT/fm1-emu-report.pdf"
# the SMK-37's page: its own index, catalog and preview, the same code and wasm beside them
mkdir -p "$OUT/smk37"
cp web/smk37/index.html web/smk37/firmwares.json web/smk37/og.png "$OUT/smk37/"
cp web/style.css web/main.js web/persistence.js web/tryout.js web/worklet.js web/jit.js web/fwsc.js "$OUT/fm1_web.wasm" "$OUT/smk37/"
# the developer page: no script, its own copy of the stylesheet
mkdir -p "$OUT/dev"
cp web/dev/index.html web/style.css "$OUT/dev/"
# the headline font, and the pictures of each catalog's screens (tools/firmware_shots.py), beside each page
for d in "$OUT" "$OUT/smk37" "$OUT/dev"; do mkdir -p "$d/fonts" && cp web/fonts/* "$d/fonts/"; done
[ -d web/shots ] && cp -R web/shots "$OUT/"
[ -d web/smk37/shots ] && cp -R web/smk37/shots "$OUT/smk37/"
# the release in the pages (the header's version, what's new, the downloads): from Cargo.toml's
# version and its notes, docs/releases/v<version>.md; without node the pages keep the committed copy
if command -v node >/dev/null; then
  node web/release.mjs "$OUT/index.html" "$OUT/smk37/index.html" "$OUT/dev/index.html"
  # every file the pages load under this build's name, so a browser's cached copy of one file is
  # never run with another build's (GitHub Pages lets each be kept 10 minutes): the commit, or
  # the time for a build outside git
  node web/stamp.mjs "$(git rev-parse --short HEAD 2>/dev/null || date +%s)" "$OUT" "$OUT/smk37" "$OUT/dev"
else
  echo "warning: no node; the pages keep the release blocks committed in web/index.html, unstamped" >&2
fi
ls -la "$OUT" | grep -v "^total"
