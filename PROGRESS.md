# F'Em 1 progress

## 2026-11-14 — Repository and checkpoint setup

- Foundation: joakimeriksson/fm1-emu at `31d9ac0`, including the fm1-sid submodule.
- Product name: **F'Em 1**; repository/internal name: `fem1`.
- Local development branch: `fem1`; original repository retained as `upstream`.
- Target fork: https://github.com/itsrobballen/fem1.
- Added AGENTS.md with milestone commits, pushes, and progress notes so work can
  resume across sessions. Aim for checkpoints about every 30 minutes during
  longer active work, and before ending a session.

Validation: clean starting checkout; submodule initialized at its recorded commit.
Documentation-only setup; no application behavior changed or build run.

Fork verified at https://github.com/ItsRobbAllen/fem1 (renamed from fm1-emu).
Origin points to this fork; upstream remains the original repository.
GitHub connector confirms write access. Local Git authentication still needs a
refresh (saved credential returned HTTP 401); use the connector to publish
checkpoints until local Git authentication is restored.

## Next steps

1. Verify Windows VST3 build prerequisites and build the existing plug-in.
2. Evaluate a synth, groovebox, and sampler firmware for sound, CPU use, timing,
   project recall, and offline rendering before deeper product changes.
