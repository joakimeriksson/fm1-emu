# FM-1 Studio standalone

<!-- >>> github-two-accounts >>> -->
## GitHub accounts on this Mac
Two GitHub accounts are used here and must never be mixed or linked. Before any git, gh or GitHub work, read
~/GitHub/AGENTS.md and follow it exactly. The folder decides the account: repos live only in ~/GitHub/<account>/.
Run `gh-whoami` in a repo before committing or pushing. If anything says STOP, stop and tell the user.
<!-- <<< github-two-accounts <<< -->

Read context/overview.md before development.

## Commands

Add Rust to the shell when needed: `export PATH="$HOME/.cargo/bin:$PATH"`.

- Build standalone: `tools/package_macos_app.sh`
- Native regressions: `cargo test --release --locked -p fm1-live -p fm1-map -p fm1-core -p fm1-soc`
- Context integrity: `npm run check:context`
- Upstream boundary: `tools/check_upstream.sh`
- Plugin verification, if implicated: `NO_INSTALL=1 tools/build_plugin.sh`

## Constraints

- Execute original firmware binaries. Do not replace the upstream engine or synthesis.
- Custom scope: standalone Mac app, complete icon sources/exports and build branding, visual panel, macOS application menus/shortcuts, session preferences and MiniLab mapping. Preserve upstream targets and tests.
- Keep machine, mapping engine, shared panel, browser and plugin runtime sources identical to UPSTREAM_REVISION. Change host adapters only where required; see context/architecture.md.
- Do not install plugins or alter existing applications during builds.
- Keep parsing, file I/O and UI work outside audio callbacks.
- Never commit firmware, user memory, backups, machine-specific paths or generated build output.
- Firmware flash saves are not CPU/RAM snapshots. No hardware writes.
- Use the base-8 system for new UI geometry; the ported instrument artwork retains its measured proportions.

## Fork scope

Before editing, read context/fork-policy.md. Product changes are limited to the visual UI, standalone macOS QOL and branding, and MiniLab mapping. Keep the existing app, icon sources/exports and mandatory rebuild workflow.

- Prefer upstream behavior and APIs, then profile/theme data, then a small host adapter. Keep owned code under crates/fm1-live/src/studio/.
- Do not modify the shared engine, firmware loader, mapper, browser runtime or plugin runtime to fix a local host issue. Report compatibility gaps and propose an upstreamable fix or an explicitly authorized scope decision.
- Do not add hosts, libraries, hardware tools, editors, frameworks or configuration without a concrete requested use. Keep upstream targets, tests, licenses and README intact.
- Run tools/check_upstream.sh before and after changes. It compares against UPSTREAM_REVISION, including untracked files. Never edit the pin or widen the allowlist merely to make it pass.
- New scope or an additional upstream integration file needs explicit user authorization. Honor an authorization already given; do not ask again.
- Every PR names its allowed scope, explains any upstream-file edit, and records tests and remaining manual acceptance.

## Context

| Task | Read |
|---|---|
| Always | context/overview.md |
| Before any change or upstream update | context/fork-policy.md |
| Boundaries and host integration | context/architecture.md |
| Visual or interaction changes | context/design-system.md |
| App icons, platform artwork and bundle branding | context/icons.md |
| Builds, tests, scope and upstream updates | context/development.md |
| MiniLab setup and routing | context/minilab.md |
| Verified state and remaining acceptance | context/status.md |

After every change or merge, including documentation-only edits, rebuild the standalone macOS app and all other local targets before handoff: macOS app, CLAP/AU/VST3, upstream browser assets and native CLI. Keep plugin installation disabled; report any failed or unavailable target. Follow the checklist in context/development.md.

Keep affected context and README accurate in the same change. Reconcile using `.agents/skills/ez-context/SKILL.md`; read the design system for every UI change. Run `npm run check:context` after documentation edits. Build/test evidence never establishes physical-controller, DAW or hardware acceptance.
