# Distribution: one source of truth, every agent

Makes the context layer and the skill itself agent-agnostic. Two separate wirings:

1. **Skill distribution** — the `ez-context` skill lives once and is discoverable by every installed agent.
2. **Router wiring** — every agent's native context file points at the one `AGENTS.md`, so they all read the same router.

Both are idempotent — safe to re-run. Helper: `scripts/ezctx.py` (`detect`, `setup`).

## Source of truth

Canonical skill location is `.agents/skills/ez-context/` — **not** inside `.claude/`. Only `.agents/skills/` is tracked in git; the per-agent copies/symlinks are derived and git-ignored.

```
repo-root/
├── .agents/skills/ez-context/   # tracked source of truth
├── .claude/skills/ez-context    # symlink → ../../.agents/skills/ez-context  (or copy)
├── .codex/skills/ez-context     # symlink or copy
└── .gemini/skills/ez-context    # symlink or copy
```

## Skill distribution — symlink, with copy fallback

`scripts/ezctx.py detect` reports which agents are present (looks for `.claude/`, `.codex/`, `.gemini/`, `.cursor/`, and `AGENTS.md` consumers). `scripts/ezctx.py setup` then, for each detected agent's `skills/` dir:

1. **Try a symlink** into `.agents/skills/ez-context` (`ln -sfn` semantics; on Windows, a directory symlink/junction).
2. **If the symlink fails** — the common case on stock Windows without Developer Mode or admin — **fall back to copying** the folder, and record that re-running `setup` is how you resync after the source changes.

The script chooses per-target and reports which mechanism it used for each, so the user knows whether copies will drift. This is why the chosen strategy is symlink-first-copy-fallback rather than pure symlinks: it works on every platform without requiring elevated permissions.

## Router wiring — native files point at AGENTS.md

`AGENTS.md` is the one router, but some agents still prefer their own filename. For each detected agent, wire its native file to the router so they can't diverge:

- `CLAUDE.md` → `AGENTS.md`
- `GEMINI.md` → `AGENTS.md`
- `.cursorrules` → `AGENTS.md` (or the agent's documented import directive)

Same symlink-or-copy fallback applies. Preferred form where the agent supports it is an `@import`/include directive (a one-line file that imports `AGENTS.md`), which sidesteps symlink permissions entirely and survives on Windows; fall back to a symlink, then to a copy. If you must copy, note it diverges and re-running `setup` resyncs.

If an agent natively reads `AGENTS.md` already, no wiring is needed for it — don't create a redundant file.

## .gitignore

Ensure derived artifacts are ignored while the source stays tracked. Add (idempotently — don't duplicate lines):

```gitignore
# ez-context: track the source of truth, ignore derived per-agent copies/symlinks
.claude/skills/ez-context
.codex/skills/ez-context
.gemini/skills/ez-context
```

If router files were *copied* (not symlinked/imported) into native filenames, those copies are regenerable too, but leave `CLAUDE.md`/`GEMINI.md` tracking to the user's preference — don't silently gitignore a file a human might expect in the tree. Default: only ignore the skill copies/symlinks under the agent dirs.

## Windows note

You're frequently running this on Windows. `ln -sfn`-style symlinks need Developer Mode or an elevated shell there; junctions work for directories but not files. The script handles this by attempting the symlink/junction and falling back to copy automatically — you don't need to pre-detect the platform, but do surface in the summary which targets ended up as copies so the user knows what needs a re-run to stay in sync.

## Keeping it tool-neutral

The skill body and the docs it produces stay plain markdown + a portable Python helper. No agent-specific syntax leaks into `/context/` or the router beyond the thin native-file shims described here.
