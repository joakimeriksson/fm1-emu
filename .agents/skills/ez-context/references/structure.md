# Structure: `/context/` tiering, the router, and doc frontmatter

This defines the shapes everything else produces. Read it before bootstrap or reconcile.

## Contents
- Where things live
- The four tiers
- The `AGENTS.md` router format
- Per-doc frontmatter contract
- Coexisting with an existing `/docs`

## Where things live

```
repo-root/
├── AGENTS.md            # Tier 0 — the router. Always read by agents. Tiny.
├── README.md           # human-facing. Dual-sourced. Not an agent doc.
└── context/
    ├── overview.md      # Tier 1 — foundation. Project intent, stack, conventions.
    ├── architecture.md  # Tier 2 — loaded only for architecture/structure tasks.
    ├── design-system.md # Tier 2 — loaded only for UI/design tasks. Always-fresh.
    └── …                # more Tier 2/3 docs as the project grows.
```

`/context/` is always at repo root. Do **not** branch on whether a `/docs` exists — if one does, leave it alone and place agent context in `/context/` alongside it (see last section).

## The four tiers

Tiering is about *when a doc loads*, not how important it is.

- **Tier 0 — Router** (`AGENTS.md`): always loaded, kept tiny. Routing + commands + constraints + gotchas. Never prose architecture.
- **Tier 1 — Foundation** (`context/overview.md`): always loaded, rarely changes. Project intent/purpose, tech stack, top-level conventions, the "what is this and what are the non-obvious rules" layer. Keep it short — if it's growing sections that only matter sometimes, those sections are really Tier 2.
- **Tier 2 — Conditional/domain**: loaded only when the current task matches its trigger. `architecture.md`, `design-system.md`, `api.md`, per-subsystem files. This is where most depth lives.
- **Tier 3 — On-demand reference**: loaded only when explicitly needed. Deep specs, decision logs, migration notes.

A doc's tier is recorded by how the router routes to it (always vs. conditional vs. on-demand), and by its `load_when` frontmatter (below).

## The `AGENTS.md` router format

The router is a compressed index. Its job: let an agent, in a few hundred tokens, know what's true here and where to look for more. Copy [assets/templates/AGENTS.md.template](../assets/templates/AGENTS.md.template) and fill it. Target **under ~150 lines**; if it's growing, push content into a `/context/` doc and route to it instead.

Sections, in order:

1. **One-line project identity.** What this repo is. (Not a sales pitch — that's README's job.)
2. **Commands.** The exact, literal shell commands an agent runs: install, build, test, lint, run, typecheck. Imperative and specific — `pnpm test --run`, not "run the tests." Include only commands that are non-obvious or project-specific; skip if a glance at the manifest makes it obvious.
3. **Constraints / never-do.** Hard rules: files never to edit (generated, vendored), patterns the project forbids, "don't bump X," security boundaries.
4. **Non-standard patterns & gotchas.** The things that bite a newcomer: the unusual convention, the thing that looks wrong but is intentional, the footgun.
5. **Routing table.** Task type → doc(s) to read. This is the heart of conditional loading. One row per Tier-2/3 doc:

   ```
   | When the task involves… | Read |
   |---|---|
   | UI, styling, components, visual changes | context/design-system.md |
   | architecture, module boundaries, data flow | context/architecture.md |
   | the public API surface | context/api.md |
   ```

6. **Maintenance line (self-promoting trigger).** A literal instruction: *"After completing a feature, architectural change, or scope change, re-run `/ez-context` to keep this layer current."*

What does **not** go in the router: architecture prose, anything restating the file tree, anything in README, tutorials.

## Per-doc frontmatter contract

Every doc the skill writes under `/context/` (Tier 1+) carries YAML frontmatter. This is what makes conditional loading and staleness checking work. v1 schema:

```yaml
---
load_when: "UI work, styling, component or visual changes"   # human-readable trigger; matches a router routing row
last_verified: 2026-06-20                                     # ISO date (or short commit SHA) of last verification
source_refs:                                                  # globs this doc describes; staleness watches these
  - src/components/**
  - tailwind.config.*
# v2 (reserved, do not populate in v1): content_hash, citations
---
```

- `load_when` — the trigger phrase. Keep it consistent with the router's routing-table row so they don't drift.
- `last_verified` — set to today's date whenever you create or verify the doc.
- `source_refs` — the globs whose changes should make this doc suspect. Choose them tight enough to be meaningful, broad enough to cover the doc's subject. A foundation doc may watch manifests (`package.json`, `pyproject.toml`); `design-system.md` watches the UI/style globs; `api.md` watches route/handler globs.

The router itself (`AGENTS.md`) does not need `source_refs` — it's regenerated from the docs it routes to.

Leave the v2 fields out entirely in v1; the schema is designed so they can be added later without breaking anything.

## Coexisting with an existing `/docs`

If the repo already has a human `/docs`: don't touch it, don't migrate it, don't duplicate it. Put agent context in `/context/`, and from `README.md` (and, if useful, the router) cross-link to `/docs` rather than restating it. The point of `source_refs` and "write only what's non-obvious" is that `/context/` complements existing docs instead of competing with them.
