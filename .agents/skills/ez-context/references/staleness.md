# Staleness & structural-drift detection (the freshness contract)

How the skill knows a doc has fallen behind the code. v1 ships one mechanism (git-commit comparison over `source_refs`); the frontmatter schema is built so a precision upgrade can be added later without breaking it.

## Contents
- v1 mechanism: source_refs + git comparison
- The helper script
- Non-git fallback
- design-system.md: the hard always-fresh rule
- What v1 deliberately does not do (v2 roadmap)

## v1 mechanism — `source_refs` + git comparison

Each `/context/` doc declares in frontmatter (see [references/structure.md](structure.md)):

```yaml
last_verified: 2026-06-20
source_refs:
  - src/components/**
  - tailwind.config.*
```

The check, per doc: find the most recent commit that touched any path matching `source_refs`, and compare its date to `last_verified`.

- newest source change **after** `last_verified` → **stale** → queue for update.
- newest source change **at or before** `last_verified` → **fresh** → leave untouched.
- doc has no `source_refs`, or globs match nothing → **missing-refs** → flag for the human; you can't verify it automatically.

The literal git query for a doc's globs:

```bash
git log -1 --format=%cI -- <glob1> <glob2> …
```

Compare that ISO timestamp to the doc's `last_verified`. This is pure git plumbing — zero model tokens spent on the check itself, deterministic, and CI-friendly later.

**Known limitation (accepted for v1):** it's commit-based, not content-based. An unrelated formatting commit inside a watched glob can produce a false "stale." That's fine — the worst case is one unnecessary re-verify, never an incorrect rewrite. (The v2 upgrade below removes most of these.)

## The helper script

`scripts/ezctx.py check` runs the comparison across every doc in `/context/` and prints a fresh/stale/missing-refs report:

```bash
python scripts/ezctx.py check            # checks ./context
python scripts/ezctx.py check --path some/repo
```

It reads each doc's frontmatter, runs the git query for its `source_refs`, and classifies it. Use its output to drive step (a) of the reconcile path. If `python` isn't available, run the `git log` query above per doc by hand — the logic is simple enough to do inline.

## Non-git fallback

If the repo isn't a git repo (the script detects this), fall back to file modification times: compare the newest mtime among files matching `source_refs` to the doc's `last_verified`. Less reliable than git (mtimes are noisy, reset on checkout) so treat its "stale" results as softer hints, and tell the human the project isn't under git so freshness is best-effort.

## design-system.md — the hard always-fresh rule

`design-system.md` is the one doc that does not get to be "best effort." It is too easy to neglect and too commonly the stalest piece in a repo. The contract:

- **Always evaluated** for relevance on reconcile (its staleness is always checked).
- **Only loaded** when the task is UI/design-related (conditional loading still applies — checking ≠ loading).
- **Always re-checked and brought current** whenever a change touches UI/design, regardless of what the git window says.

This always-check / conditionally-load / keep-fresh pattern is the template for every Tier-2 doc — design-system.md is just where getting it wrong is most visible, so it gets the explicit guarantee.

## What v1 does not do (v2 roadmap — do not build now)

Designed-for, not built:

- **Content-hash + citation verification (Recommendation 2).** Store a `content_hash` of each `source_ref` at verification time plus any explicit `repo/path:line` citations the doc makes; on reconcile, recompute hashes and re-resolve citations. Hash mismatch with no broken citation = likely cosmetic (low priority); broken citation = hard flag. This removes most of v1's false positives. Add as new frontmatter fields (`content_hash`, `citations:`) — the v1 schema already reserves them.
- **Git-hook auto-trigger.** A post-commit/pre-push hook that runs *only* the read-only staleness + structural-drift checks and prints a "your context may be behind" nudge — without invoking the interview/edit flow. Keeps the conversational parts invocation-driven. Optional, opt-in.
- **AST-based structural drift.** Per-language analyzers with severity scoring. Too heavy for a single self-contained skill; out of scope.

v1 = Recommendation 1 only, invoked-only trigger, self-promoting router line.
