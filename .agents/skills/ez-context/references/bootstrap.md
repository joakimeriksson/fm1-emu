# Bootstrap path

Run this when there is no usable context layer yet (see Step 0 in SKILL.md). Goal: stand up the router + `/context/` + README from a combination of what the code shows and what the human tells you — writing only non-obvious content.

Read [references/structure.md](structure.md) first if you haven't; it defines the router, tiers, and frontmatter you'll produce here.

## Checklist

```
Bootstrap progress:
- [ ] a. Codebase audit
- [ ] b. User interview (gaps the audit can't fill)
- [ ] c. Write router (AGENTS.md) + Tier-1 foundation (context/overview.md)
- [ ] d. Write README.md (dual-sourced)
- [ ] e. Write design-system.md — only if a UI surface exists
- [ ] f. Add load-condition frontmatter + routing rows for every doc
- [ ] g. Set up distribution (source of truth, symlink-or-copy, wire native files)
- [ ] h. Summary
```

## a. Codebase audit

Inspect, don't assume. Gather what an agent could *eventually* discover but you want to encode once:

- **Stack & manifests:** languages, frameworks, package manager(s). Read `package.json`/`pyproject.toml`/`go.mod`/`Cargo.toml`/etc. for scripts (these become the router's Commands), declared deps, and engines.
- **Structure:** top-level directories and what each is for. Note monorepo/workspaces.
- **Conventions already in code:** lint/format config, test layout, naming patterns, an existing `.editorconfig`/CI config that encodes rules.
- **Existing context:** any `README.md`, `CLAUDE.md`, `AGENTS.md`, `.cursorrules`, `/docs`. These are reconcile inputs, not things to overwrite — adopt their real content.
- **UI surface?** Look for a frontend: a web framework (React/Vue/Svelte/Next/etc.), a `components/` dir, CSS/Tailwind/styling config, a design-tokens file, mobile UI. This decides step (e). If unsure, it's a question for the interview.

Record findings compactly — they feed the docs *and* narrow the interview.

## b. User interview

The audit cannot tell you intent, audience, "why," tone, what's out of scope, or roadmap. Ask the human — but briefly. Full guidance and the question bank: [references/interview.md](references/interview.md). The rule: use audit findings to *narrow* what you ask (never ask "what language is this?"), batch the questions, cap at ~3–6, and never replace the interview with code-derived guesses for README.

If the human declines to answer or isn't available, say so and proceed with what you have — but mark README and any intent-dependent sections as "needs human input" rather than inventing intent.

## c. Router + Tier-1 foundation

Write `context/overview.md` (Tier 1) and `AGENTS.md` (Tier 0) together, synthesizing audit + interview.

- **`context/overview.md`** — foundation doc per [references/structure.md](structure.md): project intent/purpose (from interview), stack (from audit), top-level conventions, the non-obvious rules. Add frontmatter (`load_when: "always — foundation"`, `last_verified`, `source_refs` watching the manifests). Keep it short; push anything task-specific into a Tier-2 doc instead.
- **`AGENTS.md`** — fill [assets/templates/AGENTS.md.template](../assets/templates/AGENTS.md.template). Commands come straight from the manifest scripts. Constraints/gotchas come from audit + interview. Add a routing row for every Tier-2/3 doc you create. Include the maintenance/self-promote line.
- If audit found real architecture worth recording, write `context/architecture.md` (Tier 2) and route to it — do **not** inline architecture prose into the router.

## d. README.md (dual-sourced)

If `README.md` exists, reconcile into it (preserve voice; fix only what's wrong/missing). If it does **not** exist, you must still not generate it from code facts alone — weave interview answers (who it's for, what problem it solves, tone) together with audit facts (install/usage/stack) into a first-class, human-toned README. A README built from audit alone reads accurate but generic; the interview is what makes it represent the project. Cross-link to `/context/` and any existing `/docs`.

## e. design-system.md (UI only)

Only if a UI surface was detected. Create it text-only from [references/design-system.md](design-system.md), seeded by what the audit found (existing tokens/theme/components) and the lighter design question from the interview ("is there an existing brand/style direction to encode, or should one be proposed?"). Give it `source_refs` over the UI/style globs and route to it. If no UI surface exists, skip silently — don't create an empty design doc.

## f. Load-condition metadata

Confirm every `/context/` doc has correct frontmatter and a matching routing row in the router, with `load_when` and the routing row phrased consistently so they don't drift. This is what makes the whole set conditionally loaded rather than dead weight.

## g. Distribution

Set up the source-of-truth layout, symlink-or-copy the skill into detected agent dirs, and wire each agent's native context file to the router. Procedure and the helper script: [references/distribution.md](distribution.md). Safe to run on a repo that's partially set up — it's idempotent.

## h. Summary

Report what you created, what you adopted from pre-existing files, what you skipped (e.g. "no UI surface → no design-system.md"), and anything still needing human input.
