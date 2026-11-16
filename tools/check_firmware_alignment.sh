#!/usr/bin/env bash
# Offline cross-host acceptance: requires local original binaries; never installs plugins.
set -euo pipefail
cd "$(dirname "$0")/.."
export FM1_TEST_FWSC="${FM1_TEST_FWSC:-reference/firmwares/felucca.fwsc}"
export FM1_TEST_AREA="${FM1_TEST_AREA:-reference/firmwares/sloop3-3.3.7-beta/sloop3-3.3.7.area}"
for fixture in "$FM1_TEST_FWSC" "$FM1_TEST_AREA"; do
  if [ ! -f "$fixture" ]; then echo "Missing local firmware fixture: $fixture" >&2; exit 1; fi
done
export FM1_TEST_AREA="$(cd "$(dirname "$FM1_TEST_AREA")" && pwd)/$(basename "$FM1_TEST_AREA")"
cargo test --release --locked -p fm1-soc -p fm1-live -p fm1-web -p fm1-host
cargo test --release --locked --manifest-path crates/fm1-plugin/Cargo.toml
npm run test:studio
npm run test:hardware
npm run build:web
for fixture in "$FM1_TEST_FWSC" "$FM1_TEST_AREA"; do
  FM1_SMOKE_SECONDS=12 node web/smoke.mjs web/dist "$fixture"
  node web/persistence.wasm.mjs web/dist "$fixture"
  node tools/check_plugin_save_interop.mjs web/persistence.js "$fixture"
done
npm run check:context
