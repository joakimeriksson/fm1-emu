# ez-context (skill)

A single, self-deciding agent skill that bootstraps and maintains a project's **agent + human context layer**: a lean root `AGENTS.md` router, a tiered `/context/` folder, and a dual-sourced `README.md`. One source of truth on disk, wired so every coding agent (Claude Code, Codex, Gemini, Cursor, …) and every human reads the same thing — without anyone hand-writing or remembering to update it.

> This README documents the skill *for maintainers*. It is **not** the `README.md` the skill generates for a target repo.

## What it does

- **Self-deciding, one entry point.** Inspects the repo and chooses **bootstrap** (scaffold from scratch) or **reconcile** (refresh what exists) — no separate create/update commands.
- **Bootstrap:** codebase audit + short user interview → lean `AGENTS.md`, Tier-1 `context/overview.md`, dual-sourced `README.md`, and (only if a UI surface exists) a text-only `context/design-system.md`; then wires distribution.
- **Reconcile:** git-based staleness check + structural-drift re-audit → updates **only** stale or implicated docs, creates new docs for newly-uncovered areas, always re-checks `design-system.md` on UI changes, never blanket-rewrites.

## Layout

```
ez-context/
├── SKILL.md                  # lean entry: principles, mode detection, path pointers
├── references/
│   ├── structure.md          # tiers, AGENTS.md router format, per-doc frontmatter contract
│   ├── bootstrap.md          # audit → interview → scaffold → distribute
│   ├── reconcile.md          # staleness + structural-drift, minimal-diff updates
│   ├── interview.md          # the 3–6 question bank + narrowing rules
│   ├── staleness.md          # git-diff freshness, non-git fallback, v2 roadmap
│   ├── design-system.md      # text-only 5-part template + always-fresh contract
│   └── distribution.md       # source-of-truth, symlink-or-copy, native-file wiring
├── assets/templates/         # AGENTS.md / overview.md / design-system.md starters
├── scripts/ezctx.py          # detect | setup | check  (stdlib-only, Python 3.8+)
└── evals/evals.json          # the 3 test-case prompts
```

`SKILL.md` is deliberately short and routes to one-level-deep references (per Anthropic skill-authoring guidance). Edit the relevant reference, not `SKILL.md`, when changing how a path behaves.

## Key design decisions (and why)

These are the load-bearing choices. Revisit them here before changing behavior.

1. **Write only what's non-obvious.** Industry evals show auto-generated context files *lower* task success and *raise* cost by restating what the repo already makes discoverable. The skill's #1 operating principle is to earn every line (intent, constraints, gotchas, exact commands) and never restate the file tree or manifests. If you loosen this, you reintroduce the exact failure mode the skill exists to avoid.
2. **Lean router, no architecture prose.** `AGENTS.md` = routing table + commands + constraints + gotchas + the self-promote line. Architecture lives in a Tier-2 `context/architecture.md` loaded on demand. (Eval data: an Architecture section in the router changes cost, not behavior.)
3. **Conditional loading, applied recursively.** Every `/context/` doc declares `load_when` and defaults to *not loaded*; the router routes task-type → doc. Checking a doc (staleness) is separate from loading it.
4. **Dual-sourced context.** Audit = what exists; interview = intent/audience/why/tone. README must never be generated from code facts alone. Interview is capped at ~3–6 questions, narrowed by the audit, batched.
5. **Reconcile = minimal diff.** Two independent checks: **staleness** (existing docs vs. their `source_refs`) and **structural drift** (re-audit finds *new, unwatched* areas). Staleness only updates existing docs; drift creates new ones. Fresh, unimplicated docs are left untouched.
6. **`design-system.md` has a hard always-fresh contract.** Always evaluated, conditionally loaded, always re-checked on UI change — it's the most commonly stalest doc, so it doesn't get "best effort."
7. **Agent-agnostic distribution.** Source of truth is `.agents/skills/ez-context/`; per-agent dirs get symlinks, **falling back to copy** (chosen because symlinks need Developer Mode/admin on Windows). Native files (`CLAUDE.md`, `GEMINI.md`, `.cursorrules`) are wired to `AGENTS.md` so every agent reads one router.

## Freshness mechanism (v1) and frontmatter

Each `/context/` doc carries:

```yaml
---
load_when: "human-readable trigger; matches a router routing row"
last_verified: 2026-06-20         # ISO date or short commit SHA
source_refs:                      # globs whose changes make this doc suspect
  - src/components/**
---
```

`scripts/ezctx.py check` runs `git log -1 --format=%cI -- <source_refs>` per doc and compares to `last_verified` → fresh / stale / missing-refs. Non-git repos fall back to file mtimes (best-effort). The schema reserves `content_hash` and `citations:` for the v2 precision upgrade — **do not populate them in v1**; just keep them unbroken.

## Helper script

```bash
python scripts/ezctx.py detect [--path REPO]   # which agents present, git?, what already exists
python scripts/ezctx.py setup  [--path REPO] [--from PATH_TO_SKILL]   # symlink-or-copy + wire natives + .gitignore
python scripts/ezctx.py check  [--path REPO]   # staleness report over /context/
```

Stdlib-only, no external deps. `setup` is idempotent and reports per-target whether it used a symlink or a copy (copies need a re-run to resync — surfaced in the output).

## Scope: v1 vs. later

**In v1:** Recommendation 1 staleness (git-diff over `source_refs`), invoked-only trigger, self-promoting router line, symlink-or-copy distribution.

**Deliberately deferred (documented in `references/staleness.md`, not built):**
- Content-hash + citation verification (Recommendation 2) — removes most false-positive stale flags.
- Git-hook auto-trigger — read-only staleness/drift nudge on commit/push.
- AST-based structural drift — too heavy for one self-contained skill.

## Testing / evals

Test cases live in `evals/evals.json`; the harness output and a standalone review viewer are in the sibling `../ez-context-workspace/`. Because the bootstrap path interviews the user, eval prompts pre-bake the interview answers so non-interactive subagent runs don't stall. Iteration 1: with-skill 23/23 assertions vs. baseline 9/23 (pass rate 1.00 vs 0.42).

To re-run after edits, see the skill-creator workflow; regenerate the viewer with `eval-viewer/generate_review.py` rather than hand-writing HTML.

## Known limitations / watch-list for future tweaks

- **Commit-based staleness is coarse** — an unrelated formatting commit inside a watched glob can show a false "stale." Accepted for v1 (worst case: one needless re-verify). Rec-2 fixes this.
- **Distribution copies can drift** on Windows without Developer Mode — re-run `setup` to resync; the script tells you which targets are copies.
- **Interview is only as good as the human's answers** — if declined, README/intent sections are flagged "needs human input" rather than fabricated. Verify this still holds if you touch `references/interview.md`.
- **`load_when` and the router routing row are two places that can drift** — keep them phrased consistently (called out in `structure.md`).
