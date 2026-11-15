---
load_when: "building, testing, installation or verification evidence"
last_verified: 2026-11-15
source_refs:
  - package.json
  - tools/build_studio.sh
  - tools/package-studio.mjs
  - tools/serve-studio.mjs
  - tools/build_plugin.sh
  - Cargo.toml
---
# Development

The canonical fork is under the selected account’s GitHub directory. Follow the root account instructions before git/gh work; never move a cloned repository to the old research workspace.

Build native targets with locked dependencies. Build plugin bundles with NO_INSTALL=1 so a local development build cannot overwrite an existing installation. The script ad-hoc signs local bundles; this does not establish public distribution or DAW compatibility.

Studio requires only Node built-ins; npm install is unnecessary. npm run build:web first invokes the upstream WASM build, then packages Studio and creates a content-derived offline cache manifest. It does not download firmware. npm run serve binds to 127.0.0.1 and serves only real files inside web/dist, with explicit WASM/JS MIME types and traversal checks.

## Verification ladder

1. Node tests exercise import rejection, collection validation, hashes and real-package round trips.
2. Rust CPU/SoC tests protect the execution foundation.
3. Real firmware smoke runs prove a specific package boots/renders.
4. Browser checks cover import, device boot, collection export/restore, persistence, responsive layout and a network-disabled cold reopening.
5. Plugin validators and real DAW checks are separate. A build alone earns no host-compatibility label.
6. Hardware acceptance remains pending until a physical unit is available.

Useful local fixture: node tools/fetch_firmwares.mjs felucca. Downloads stay in ignored reference/firmwares and must not be committed. Run FM1_TEST_FWSC=reference/firmwares/felucca.fwsc npm run test:studio to enable the package roundtrip test.

Native diagnostic: target/release/fm1-emu --fwsc reference/firmwares/felucca.fwsc --steps 1000000000 --ms 4000 --keys '2000-3500:n7' --wav target/felucca-smoke.wav --quiet. Keep diagnostic output in ignored target/, not tracked context.

The supplied ez-context helper can check staleness from source_refs. npm run check:context checks routing, metadata and local Markdown targets without depending on git history. Neither checker proves a document’s prose correct; manually reconcile the design doc for every visual change.

Standalone macOS app packaging: tools/package_macos_app.sh. It builds the native executable, creates target/apps/FM-1 Studio.app and ad-hoc signs the bundle. Public distribution/notarization is not implied.

The MiniLab diagnostic example accepts an optional monitoring duration: `cargo run --release -p fm1-live --example midi_probe -- 120`. It receives only; it never configures or writes to a controller. Studio tests now cover the realtime queue’s bounded prefill/stale-boot/pause behavior and factory controller mapping. Browser asset packaging includes the render worker/runtime, with worker URLs stamped alongside imports. Keep direct-loop WASM benchmarks separate from end-to-end worker/output-underrun verification.
