---
load_when: "builds, upstream updates, scope and verification"
last_verified: 2026-11-23
source_refs:
  - UPSTREAM_REVISION
  - tools/package_macos_app.sh
  - tools/check_upstream.sh
  - .github/workflows/standalone.yml
---
# Development

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo test --release --locked -p fm1-live -p fm1-map -p fm1-core -p fm1-soc
tools/check_upstream.sh
npm run check:context
tools/package_macos_app.sh
```

## Rebuild after changes

Rebuild the standalone macOS app and every other local target after each change or merge, including documentation-only edits, before handoff. Stop on a failed command and report unavailable targets. Incremental builds are sufficient; do not delete caches.

```sh
export PATH="$HOME/.cargo/bin:$PATH"
set -e
tools/package_macos_app.sh
NO_INSTALL=1 tools/build_plugin.sh
npm run build:web
cargo build --release --locked -p fm1-live -p fm1-emu
```

Verify each app/CLAP/AU/VST3 bundle with `codesign --verify --deep --strict`. Output is under target/apps/, target/plugin/, target/release/ and web/dist/. The old browser Studio is deliberately absent; build:web produces upstream browser assets only. Never install plugins as a build side effect. A build does not establish interactive app, browser, DAW or hardware acceptance.

No npm install is needed. The app is target/apps/FM-1 Studio.app, ad-hoc signed for local use. Packaging does not install anything or download firmware. It registers only the built app with Launch Services to refresh its icon; public notarization is not implied. Open a local .fwsc through the file picker. CLI example:

```sh
target/release/fm1-live --fwsc reference/firmwares/felucca.fwsc --midi "Minilab3 MIDI" --map "MiniLab FM-1 User"
```

Firmware stays ignored under reference/. Tests with optional upstream fixtures can skip internally when files are missing; report actual firmware smoke runs separately. Default native audio remains upstream's implementation and latency policy. The previous fork's audio refactor is not ported without a separate comparison against this newer engine. This branch makes no new audio-latency guarantee.

## Updating upstream

1. Start with a clean worktree. Follow the account rules and run gh-whoami.
2. Fetch upstream/master and create an update branch from the maintained product branch.
3. Record the candidate hash, then merge that exact upstream commit. Resolve only the small host adapters and any genuine custom interface changes. Do not replace upstream engine files with old versions.
4. Update UPSTREAM_REVISION to the merged candidate. Run tools/check_upstream.sh; protected engine, mapping, shared panel, browser and plugin runtime sources must exactly match the candidate.
5. Run native tests, context checks and app packaging. Smoke-test a real firmware, MIDI mapping, open/cancel, recording, reset backup and Finder recall. Check upstream API changes against custom renderer/overlay geometry.
6. Commit and push the update branch, then open a PR. Do not rebase or force-push the public product history. No automatic production merges.

The boundary check also requires the recorded baseline to be an ancestor of HEAD. New upstream changes can still require adapter work; the goal is a small, inspectable difference, not a conflict-free promise.

## Migration scope

Carry forward the standalone Mac app, the full icon family and editable sources, mandatory rebuild instructions, visual styling, standalone menus/shortcuts, session recall, Shift-click holds and the MiniLab User program. Leave browser Studio/library, hardware tools, custom plugin UI/state and speculative transport/editor features out. Keep upstream-supported targets intact. Runtime fixes are considered individually; prefer upstream fixes and avoid importing old engine code.

The upstream README is retained with a fork notice, and its tests/licensing remain intact. Custom files live under studio/ wherever practical. Keep native adapter edits focused rather than reformatting upstream files.

## Icon maintenance

Read [the icon contract](icons.md). Keep source and generated exports together; use `python3 tools/check_icon_artwork.py` and `python3 tools/package_icons.py` to validate and export them. `npm run build:web` uses the branding wrapper; direct `web/build.sh` keeps upstream output. Verify the app and every plugin bundle (including nested CLAP copies) contain the expected ICNS/catalog bytes and icon metadata before signature checks. Windows resource build hooks and Linux window artwork are preserved, with runtime presentation unverified on those platforms.
