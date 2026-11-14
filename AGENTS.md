# F'Em 1 development

F'Em 1 is a firmware-loading virtual instrument based on joakimeriksson/fm1-emu.
Use `fem1` for repository/internal identifiers. Preserve upstream attribution and
license notices. Review firmware licenses separately before bundling firmware.

## Commit checkpoints

- Commit after each coherent, working milestone. During longer work, aim for a
  checkpoint about every 30 minutes when there is a reviewable change.
- Before ending a work session, checkpoint completed work and update PROGRESS.md
  with changes, validation, limitations, and the next concrete step.
- Review the diff and run checks appropriate to the change before committing.
  Stage explicit paths; keep unrelated user changes out of commits.
- Push checkpoint commits to the current development branch on `origin`
  (`itsrobballen/fem1`). Keep `upstream` pointing to joakimeriksson/fm1-emu.
- Never force-push or push to upstream as part of this workflow.
- If publishing is unavailable, retain local commits and record the exact blocker
  in PROGRESS.md. Do not claim a local checkpoint has been backed up remotely.
- If local Git authentication fails but the GitHub connector has write access,
  publish through its Git object operations and fetch/reconcile the remote commit
  locally without discarding local history. Record the fallback in PROGRESS.md.
- Avoid empty timer commits. This cadence applies while actively working; it does
  not require unattended scheduled commits.

## Resume

Read PROGRESS.md, inspect Git status and recent commits, then continue from the
recorded next step. Preserve the `fm1-sid` submodule unless a task requires changes.
