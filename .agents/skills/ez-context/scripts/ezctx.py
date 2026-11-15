#!/usr/bin/env python3
"""ez-context helper. Stdlib only — runs anywhere Python 3.8+ is available.

Subcommands:
  detect   Report which agents are present and whether the repo is under git.
  setup    Wire the skill + router across agents (symlink, falling back to copy).
  check    Compare each /context/ doc's last_verified to its source_refs (staleness).

The conversational work (interview, writing docs) stays in the skill body; this
script only does the deterministic, fiddly, idempotent parts.
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

# Agent dir -> native router filename to wire to AGENTS.md (None = reads AGENTS.md natively)
AGENTS = {
    ".claude": "CLAUDE.md",
    ".codex": None,
    ".gemini": "GEMINI.md",
    ".cursor": None,
}
SKILL = "ez-context"


# ---------- shared helpers ----------

def is_git(root: Path) -> bool:
    return subprocess.run(
        ["git", "-C", str(root), "rev-parse", "--is-inside-work-tree"],
        capture_output=True, text=True,
    ).returncode == 0


def link_or_copy(src: Path, dst: Path) -> str:
    """Point dst at src. Try symlink first; fall back to copy. Returns mechanism used."""
    if dst.is_symlink() or dst.exists():
        if dst.is_symlink():
            dst.unlink()
        elif dst.is_dir():
            shutil.rmtree(dst)
        else:
            dst.unlink()
    dst.parent.mkdir(parents=True, exist_ok=True)
    try:
        os.symlink(src, dst, target_is_directory=src.is_dir())
        return "symlink"
    except (OSError, NotImplementedError):
        if src.is_dir():
            shutil.copytree(src, dst)
        else:
            shutil.copy2(src, dst)
        return "copy"


# ---------- detect ----------

def cmd_detect(root: Path) -> int:
    print(f"repo: {root}")
    print(f"git:  {'yes' if is_git(root) else 'no (staleness falls back to mtimes)'}")
    found = [d for d in AGENTS if (root / d).is_dir()]
    print(f"agents detected: {', '.join(found) if found else 'none'}")
    print(f"source of truth: {'present' if (root / '.agents/skills' / SKILL).is_dir() else 'missing (.agents/skills/' + SKILL + ')'}")
    print(f"router (AGENTS.md): {'present' if (root / 'AGENTS.md').exists() else 'missing'}")
    print(f"context/: {'present' if (root / 'context').is_dir() else 'missing'}")
    return 0


# ---------- setup ----------

def cmd_setup(root: Path, source_from: Path | None) -> int:
    src = root / ".agents/skills" / SKILL
    if source_from:
        src.parent.mkdir(parents=True, exist_ok=True)
        if src.resolve() != source_from.resolve():
            if src.exists():
                shutil.rmtree(src)
            shutil.copytree(source_from, src)
            print(f"seeded source of truth from {source_from}")
    if not src.is_dir():
        print(f"ERROR: source of truth missing: {src}\n"
              f"Place the skill there first, or pass --from <path-to-skill>.", file=sys.stderr)
        return 1

    copied_targets = []
    for agent, native in AGENTS.items():
        adir = root / agent
        if not adir.is_dir():
            continue
        # 1. skill distribution
        mech = link_or_copy(src, adir / "skills" / SKILL)
        print(f"{agent}/skills/{SKILL}: {mech}")
        if mech == "copy":
            copied_targets.append(f"{agent}/skills/{SKILL}")
        # 2. router wiring (only if a router exists and the agent wants a native file)
        if native and (root / "AGENTS.md").exists():
            mech = link_or_copy(root / "AGENTS.md", root / native)
            print(f"{native} -> AGENTS.md: {mech}")
            if mech == "copy":
                copied_targets.append(native)

    _update_gitignore(root)

    if copied_targets:
        print("\nNote: these were COPIES, not symlinks (symlink unavailable here, "
              "e.g. Windows without Developer Mode). Re-run `setup` to resync after the "
              "source changes:\n  - " + "\n  - ".join(copied_targets))
    return 0


def _update_gitignore(root: Path) -> None:
    gi = root / ".gitignore"
    marker = "# ez-context: derived per-agent skill copies/symlinks"
    lines = [marker] + [f"{a}/skills/{SKILL}" for a in AGENTS]
    existing = gi.read_text(encoding="utf-8").splitlines() if gi.exists() else []
    to_add = [l for l in lines if l not in existing]
    if to_add:
        with gi.open("a", encoding="utf-8") as f:
            if existing and existing[-1].strip():
                f.write("\n")
            f.write("\n".join(to_add) + "\n")
        print(f".gitignore: added {len(to_add)} line(s)")


# ---------- check (staleness) ----------

def _parse_frontmatter(text: str) -> dict:
    """Minimal YAML-frontmatter reader for last_verified + source_refs (list). Stdlib only."""
    if not text.startswith("---"):
        return {}
    end = text.find("\n---", 3)
    if end == -1:
        return {}
    block = text[3:end].splitlines()
    out: dict = {}
    key = None
    for raw in block:
        line = raw.rstrip()
        if not line.strip() or line.strip().startswith("#"):
            continue
        if line.startswith("  - ") or line.startswith("- "):
            if key == "source_refs":
                out.setdefault("source_refs", []).append(line.split("-", 1)[1].strip().strip('"\''))
            continue
        if ":" in line:
            k, v = line.split(":", 1)
            key = k.strip()
            v = v.strip().strip('"\'')
            if key == "source_refs" and not v:
                out["source_refs"] = []
            elif v:
                out[key] = v
    return out


def _newest_git(root: Path, globs: list[str]) -> str | None:
    res = subprocess.run(
        ["git", "-C", str(root), "log", "-1", "--format=%cI", "--"] + globs,
        capture_output=True, text=True,
    )
    return res.stdout.strip() or None if res.returncode == 0 else None


def _newest_mtime(root: Path, globs: list[str]) -> str | None:
    newest = None
    for g in globs:
        for p in root.glob(g):
            if p.is_file():
                ts = p.stat().st_mtime
                newest = ts if newest is None else max(newest, ts)
    return datetime.fromtimestamp(newest, timezone.utc).isoformat() if newest else None


def cmd_check(root: Path) -> int:
    ctx = root / "context"
    if not ctx.is_dir():
        print("no context/ folder — nothing to check (bootstrap first)")
        return 0
    git = is_git(root)
    print(f"freshness check ({'git' if git else 'mtime fallback — best-effort'}):\n")
    any_stale = False
    for doc in sorted(ctx.rglob("*.md")):
        fm = _parse_frontmatter(doc.read_text(encoding="utf-8"))
        rel = doc.relative_to(root).as_posix()
        lv = fm.get("last_verified")
        refs = fm.get("source_refs") or []
        if not refs:
            print(f"  MISSING-REFS  {rel}  (no source_refs — verify manually)")
            continue
        newest = _newest_git(root, refs) if git else _newest_mtime(root, refs)
        if not newest:
            print(f"  MISSING-REFS  {rel}  (source_refs match nothing)")
            continue
        if lv and newest[:10] <= str(lv)[:10]:
            print(f"  FRESH         {rel}  (verified {lv}, newest src {newest[:10]})")
        else:
            any_stale = True
            print(f"  STALE         {rel}  (verified {lv}, newer src {newest[:10]}) -> update")
    if any_stale:
        print("\nStale docs found - see reconcile path step (d).")
    return 0


# ---------- main ----------

def main() -> int:
    p = argparse.ArgumentParser(prog="ezctx")
    p.add_argument("--path", default=".", help="repo root (default: cwd)")
    sub = p.add_subparsers(dest="cmd", required=True)
    sub.add_parser("detect")
    sp = sub.add_parser("setup")
    sp.add_argument("--from", dest="source_from", default=None,
                    help="path to skill source to seed .agents/skills/ez-context")
    sub.add_parser("check")
    args = p.parse_args()
    root = Path(args.path).resolve()

    if args.cmd == "detect":
        return cmd_detect(root)
    if args.cmd == "setup":
        return cmd_setup(root, Path(args.source_from).resolve() if args.source_from else None)
    if args.cmd == "check":
        return cmd_check(root)
    return 1


if __name__ == "__main__":
    sys.exit(main())
