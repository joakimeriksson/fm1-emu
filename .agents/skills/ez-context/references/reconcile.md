# Reconcile path

Run this when a populated `/context/` + router already exist (see Step 0 in SKILL.md). Goal: bring the layer back into truth by touching **only** what's stale or implicated by the current change — never a blanket rewrite.

Two independent checks run here, and they catch different failures:

- **Staleness check** — finds docs that *already exist* but have fallen behind the code they describe. (areas being watched)
- **Structural-drift check** — finds *new* code/areas that no existing doc covers yet, so nothing registers as "stale" because nothing was watching. (areas not yet watched)

You need both. Staleness only ever triggers an *update to an existing doc*; structural drift triggers *creating a new doc or section*.

## Checklist

```
Reconcile progress:
- [ ] a. Staleness check across existing /context/ docs
- [ ] b. Classify the current change → which docs are directly implicated now
- [ ] c. Structural-drift check (re-audit, diff against what's recorded)
- [ ] d. Update ONLY stale or implicated docs (+ new docs for drift)
- [ ] e. Re-check design-system.md if anything touched UI/design
- [ ] f. Update the router's routing/load map if doc scope changed
- [ ] g. Summary
```

## a. Staleness check

For each `/context/` doc, compare its `last_verified` against the latest change to its `source_refs`. Full mechanism (git-based, with non-git fallback) and the helper script: [references/staleness.md](staleness.md). Output is a list of docs classified fresh / stale / missing-refs. Stale → queue for update in step (d). Do not rewrite a fresh doc.

## b. Classify the current change

Independent of staleness, figure out what *this* request/change is about, so you update docs implicated *right now* even if their `source_refs` window hasn't tripped yet (e.g. you're mid-feature, nothing committed). Map the change to task types and therefore to the docs the router routes those task types to. A doc can be implicated by (b) without being flagged by (a), and vice-versa — handle the union.

## c. Structural-drift check

Re-run a stripped-down version of the bootstrap audit ([references/bootstrap.md](bootstrap.md) step a): top-level dirs, manifests/dependencies, frameworks, UI presence. Diff that against what the Tier-1 foundation doc currently records. If something new appears that isn't reflected anywhere in `/context/`:

- A whole new domain/subsystem (e.g. a public API where there was none) → flag for a **new Tier-2 doc** + a new routing row.
- A smaller new area that fits an existing doc → flag for a **new section** in that doc.

This is distinct from staleness: it's "this needs a doc that doesn't exist yet," not "this doc is out of date." Use `scripts/ezctx.py detect` plus a quick tree/manifest read; no new capability is needed beyond the audit you already know how to do.

## d. Update only what's implicated

Update the union of (a) stale docs, (b) currently-implicated docs, and (c) drift findings. For each:

- Minimal diff: change the wrong/missing parts, preserve the rest and its voice.
- Keep writing only non-obvious content — a reconcile is not an excuse to pad.
- Set `last_verified` to today on any doc you verify or change.
- For drift: create the new doc from the right template, give it frontmatter + `source_refs`, and add its routing row (step f).

Leave every fresh, unimplicated doc untouched.

## e. design-system.md freshness (hard contract)

If the change touched UI/design at all, `design-system.md` is **always** re-checked — it's too easy to neglect to leave on best-effort. Re-verify it against the UI/style `source_refs`, update what drifted, bump `last_verified`. If a UI surface was just introduced where none existed (a drift finding), create `design-system.md` now. See [references/design-system.md](design-system.md).

## f. Update the router map

If you added, removed, or changed the scope of any doc, update `AGENTS.md`: add/remove routing rows, fix `load_when` phrasing so it still matches the doc's frontmatter, refresh Commands/constraints if the audit in (c) revealed changes (new test command, new forbidden path). Keep the router lean — route to new depth, don't inline it.

## g. Summary

Report: which docs were stale, which were implicated by the change, what drift was found and what new doc/section it produced, what was deliberately left untouched (and why — "fresh, not implicated"), and anything you couldn't auto-resolve.
