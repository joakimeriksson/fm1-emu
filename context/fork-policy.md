---
load_when: "before any code, build, documentation or upstream-update change"
last_verified: 2026-11-23
source_refs:
  - AGENTS.md
  - UPSTREAM_REVISION
  - tools/check_upstream.sh
  - .github/workflows/standalone.yml
---
# Fork policy

The goal is a useful standalone instrument with a small, inspectable difference from fm1-emu. A fresh branch alone does not lower update costs: every maintained change needs an owner and a reason.

## Approved product boundary

| Area | Owned changes | Upstream boundary |
|---|---|---|
| Visual UI | Graphite panel, labels, pressed/held states | Keep the shared panel and firmware display/runtime unchanged |
| Standalone Mac QOL | Application menus, shortcuts, session preferences | Use existing host actions; no second engine or global plugin state |
| App identity | Complete original icon sources/exports and packaging | Resource/build hooks only; retain upstream plugin behavior |
| MiniLab mapping | Factory/User JSON profiles, port routing, profile selection | Reuse fm1-map; no parallel MIDI mapping engine or controller uploads |
| Maintenance | Context, scoped CI and full local rebuilds | Preserve upstream targets, tests, licenses and README |

Browser Studio/library, a hardware companion, firmware editors, synthesis changes, plugin behavior changes and additional hosts are not approved product scope. Retaining upstream targets is different from customizing or deleting them.

## Choose the smallest extension

```text
Requested behavior
    ↓
Already provided upstream? → use it
    ↓ no
Expressible as profile/theme/asset data? → use data
    ↓ no
Small standalone adapter using upstream API? → keep it under studio/
    ↓ no
Report the boundary conflict and propose an upstreamable change
or a specific scope decision; do not silently expand the fork
```

For example, a MiniLab knob emitting factory absolute CCs needs a matching profile and enabled host routing. It does not justify changing CPU/SoC code or replacing the mapper. Likewise, a missing .area loader is a compatibility gap to decide explicitly, not permission to copy the old fork's engine wholesale.

## Enforced file boundary

`tools/check_upstream.sh` is the executable allowlist. It checks the whole maintained difference from UPSTREAM_REVISION, not just the latest commit. It includes unstaged, staged and untracked files, rejects removal of upstream files, and requires the upstream README to remain verbatim below the fork notice. CI runs the same command.

Owned paths are studio/ under fm1-live, assets/icons/, context/ and the retained context skill. The finite upstream integration list covers standalone main/controller/build hooks, native icon resources, packaging and the custom CI check. The script also verifies protected shared runtime trees against the pin exactly. Keep this list narrow; passing it does not excuse unrelated behavior inside an allowed file.

Do not suppress a check, move unrelated code into studio/, rewrite published history, advance the pin to conceal custom changes, or add an allowlist entry to bypass a failure. The pin changes only to an actual upstream commit merged through the documented update process. A new product area or upstream hook needs explicit user authorization unless already authorized in the session.

## Work and review contract

Before edits: inspect current changes, read the applicable context, run the boundary check, and identify which approved area owns the fix. Keep core fixes separate and propose them upstream when appropriate; retire local workarounds when upstream supplies the behavior.

Before handoff: run the boundary/context checks, relevant regressions and [all required local rebuilds](development.md#rebuild-after-changes), including the standalone Mac app after documentation-only edits. Do not install plugins. Test input selection/routing through the actual host adapter, not only a mapper instantiated directly by a unit test. Record physical MIDI, app, DAW and hardware acceptance separately from builds or replayed messages.

A PR should make the maintained difference reviewable: its approved area, why each upstream-file edit is needed, verification evidence and unresolved acceptance/compatibility gaps. Adding a reusable abstraction, dependency or configuration setting needs a current use that upstream facilities or a small adapter cannot meet. Avoid speculative features and unrelated refactors.
