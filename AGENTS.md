# FM-1 Studio — agent router

Faithful original-binary FM-1 emulation, native Apple Silicon instruments, and an offline browser studio.

<!-- >>> github-two-accounts >>> -->
## GitHub accounts on this Mac
Two GitHub accounts are used here and must never be mixed or linked. Before any git, gh or GitHub work, read
~/GitHub/AGENTS.md and follow it exactly. The folder decides the account: repos live only in ~/GitHub/<account>/.
Run `gh-whoami` in a repo before committing or pushing. If anything says STOP, stop and tell the user.
<!-- <<< github-two-accounts <<< -->

## Commands

Add Rust to the current shell when needed: `export PATH="$HOME/.cargo/bin:$PATH"`.

- Standalone app bundle: `tools/package_macos_app.sh`
- Native: `cargo build --release --locked -p fm1-live -p fm1-emu`
- Browser + offline Studio: `npm run build:web`
- Preview: `npm run serve`, then open `http://127.0.0.1:8642/studio/`
- Studio tests with real package: `FM1_TEST_FWSC=reference/firmwares/felucca.fwsc npm run test:studio`
- CPU/board tests: `cargo test --release --locked -p fm1-core -p fm1-soc`
- Plugin build without installation: `NO_INSTALL=1 tools/build_plugin.sh`
- Context integrity: `npm run check:context`

## Constraints

- Execute original firmware binaries. Never substitute a source-port or approximate synthesis backend.
- Never claim physical-device verification without hardware. No hardware writes on project recall.
- Firmware loading/boot/save changes must cover standalone, plugin and browser together. Use `fm1_soc::firmware_from_bytes` + `boot_firmware`; follow the cross-host checklist in context/development.md and run `npm run check:firmware`. Record unverified host behavior explicitly.
- Hardware librarian changes belong in the shared companion (web/hardware + fm1-host), with explicit destination/version checks, backup/review/read-back and simulated failure tests. Read context/hardware-library.md; never perform hardware writes on emulator or DAW recall.
- Preserve firmware bytes and hashes; do not commit downloaded firmware, user saves or device backups.
- All new layout spacing and control sizes use the base-8 system. Read the design doc before UI edits.
- Keep file I/O, network, parsing and UI work outside realtime audio callbacks.
- Preserve upstream attribution, licenses, APIs and existing tests. Do not hand-edit generated build output.
- Plugin identifiers currently remain upstream-compatible. Never install over existing plugins during a build.
- Avoid absolute user paths in tracked documents, fixtures, logs and screenshots.

## Gotchas

- `web/main.js` is the upstream-compatible device page. Studio hosts it same-origin with `studio=1`.
- Studio mode suppresses catalog discovery and remote package fetches; firmware must arrive from the local library.
- Persistent flash saves are not complete CPU/RAM snapshots, hardware backups or universally compatible songs.
- The new Studio UI is browser-only initially; native targets retain the upstream panel.
- Native/plugin/core and browser builds have separate Cargo target directories.

## Conditional context

| When the task involves… | Read |
|---|---|
| Always — foundation; first orientation for any task | context/overview.md |
| Architecture, runtime boundaries, storage or bridge changes | context/architecture.md |
| UI work, styling, components, visual or design changes | context/design-system.md |
| Icon artwork, app/plugin branding, favicons or platform icon builds | context/icons.md |
| Building, testing, installation or verification evidence | context/development.md |
| Using Studio, changing firmware, controller setup, backups or beginner troubleshooting | context/user-guide.md |
| USB hardware librarian, project transfers, firmware adapters or companion launch | context/hardware-library.md |
| Current progress, next steps or known limitations | context/status.md |
| Product requirements or scope decisions | context/product-specification.md |
| Community projects, source selection or research evidence | context/research.md |
| Phasing, acceptance gates or future work | context/roadmap.md |

## Maintenance

Keep `context/`, this router and the root README up to date **in the same change as the implementation**, before reporting the task complete. This applies to features, bug fixes, architecture, scope, build commands and user workflows.

- Reconcile using `/ez-context` (the local skill is `.agents/skills/ez-context/SKILL.md`). Update only affected docs; add a focused doc and routing entry when a new subsystem needs one.
- Compare docs with the actual working tree, including uncommitted changes. A freshness check based on commit dates does not prove the prose matches current behavior.
- Update beginner instructions when controls or workflows change. Record what was tested, what remains unverified and known limitations; never turn a build or simulated test into a hardware/DAW acceptance claim. Update `last_verified` and `source_refs` only for docs actually reviewed.
- Re-check `context/design-system.md` on every UI change, even if a staleness check passes. Run `npm run check:context` after documentation changes and resolve reported routing, metadata or link errors before completion.
