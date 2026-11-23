---
name: ez-context
description: Bootstraps and maintains a project's agent + human context layer — a root AGENTS.md router, a /context/ folder, and README.md — so any coding agent or human opening the repo gets accurate, right-sized context automatically. Self-deciding: detects whether to scaffold from scratch (bootstrap) or refresh what exists (reconcile). Use whenever setting up agent onboarding/context for a repo; when AGENTS.md, CLAUDE.md, or context docs are missing, thin, or stale; after finishing a feature, architectural change, or scope change; or when the user mentions agent context, AGENTS.md, context docs, a design system doc, or keeping docs in sync. Trigger even on loose phrasing like "document this project for agents", "set up context", or "the agent docs are out of date".
---

# ez-context

Build and maintain a repo's **context layer**: a lean root `AGENTS.md` router, a `/context/` folder of tiered docs, and a human-facing `README.md`. One source of truth on disk, wired so every coding agent (Claude Code, Codex, Gemini, Cursor, …) and every human reads the same thing.

This is one self-deciding skill, not a create command plus an update command. It inspects the repo and picks its own path.

## Operating principles (these govern every edit you make)

1. **Write only what's non-obvious.** Industry evals show auto-generated context files *hurt* — they inflate cost and lower task success by restating what the code, file tree, and existing README already make discoverable. Earn every line: intent, "why this exists," constraints, exact commands, gotchas, conventions that aren't visible from a glance. If an agent could learn it by reading the tree or a manifest, leave it out.
2. **Reconcile, never blindly replace.** Existing human-written content is merged into, never overwritten. Preserve voice and intent; change only what's wrong or stale.
3. **Dual-sourced context.** A codebase audit tells you *what exists*; a short user interview tells you *what it means, who it's for, why it matters*. README especially must not be generated from code facts alone. See [references/interview.md](references/interview.md).
4. **Conditional loading, applied recursively.** Every doc you produce declares *when it should load* in the router and defaults to **not loaded**. The router stays tiny; detail lives in docs that load only on a matching task.
5. **Minimal-diff maintenance.** On reconcile, touch only what is stale or directly implicated by the current change. The doc set must model the same restraint it asks of agents reading it.
6. **The router is lean.** `AGENTS.md` is a routing index + exact commands + hard constraints + gotchas — **not** prose architecture. Architecture and other depth live in `/context/` docs loaded on demand. (Eval data: an Architecture section in the router changes cost, not behavior.)

## Step 0 — Detect mode

Look at the repo, then choose:

- **Bootstrap** if there is no `/context/` folder, or no root `AGENTS.md`, or they exist but are essentially empty/placeholder.
- **Reconcile** if a populated `/context/` **and** a router already exist.

When ambiguous (e.g. an `AGENTS.md` exists but no `/context/`), treat it as **bootstrap** but feed the existing file in as reconcile input — adopt its real content, don't discard it.

State which mode you picked and why in one line before proceeding.

## The two paths

**→ Bootstrap:** scaffold the layer from scratch (audit → interview → write router + Tier-1 foundation + README, plus `design-system.md` if a UI exists → wire distribution). Full procedure: [references/bootstrap.md](references/bootstrap.md).

**→ Reconcile:** refresh an existing layer (staleness check + structural-drift check → update only what's implicated → re-check `design-system.md` on UI changes → fix the router's load map). Full procedure: [references/reconcile.md](references/reconcile.md).

Both paths share these building blocks — read the relevant one when you reach it:

| Concern | Reference |
|---|---|
| What to ask the human, and how few questions | [references/interview.md](references/interview.md) |
| `/context/` tiering, the AGENTS.md router format, doc frontmatter | [references/structure.md](references/structure.md) |
| Staleness + structural-drift detection (the freshness contract) | [references/staleness.md](references/staleness.md) |
| The text-only `design-system.md` template + its always-fresh rule | [references/design-system.md](references/design-system.md) |
| Source-of-truth layout, symlink-or-copy, wiring each agent's native file | [references/distribution.md](references/distribution.md) |

Templates to copy from live in [assets/templates/](assets/templates/). Helper script: `scripts/ezctx.py` (subcommands `detect`, `setup`, `check`) — see [references/distribution.md](references/distribution.md) and [references/staleness.md](references/staleness.md) for usage.

## Always finish with a summary

End either path with a short report: what was created or updated, what was intentionally left untouched, anything flagged for the human (a new area that needs a doc, an interview answer still missing, a stale doc you couldn't auto-resolve). Keep it to the changes that matter — model the minimal-diff principle in how you report, too.

## Self-promoting trigger

The router you write includes a line instructing any agent in the repo to re-run `/ez-context` after a feature, architectural change, or scope change. v1 is **invoked-only** — the skill never runs itself in the background; that reminder is how the trigger travels with the project instead of relying on someone remembering.
