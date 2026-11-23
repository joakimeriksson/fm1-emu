#!/usr/bin/env bash
# Custom behavior belongs in the standalone adapters, not shared runtime sources.
set -euo pipefail
cd "$(dirname "$0")/.."
revision=$(cat UPSTREAM_REVISION)
git merge-base --is-ancestor "$revision" HEAD
# Treat tracked edits, staged additions and untracked source files as fork changes.
python3 - "$revision" <<'CHECK_SCOPE'
from pathlib import Path
import subprocess
import sys

revision = sys.argv[1]
def git(*args):
    return subprocess.check_output(["git", *args])

owned = ("crates/fm1-live/src/studio/", "assets/icons/", "context/", ".agents/skills/ez-context/")
allowed = {
    "AGENTS.md", "README.md", "UPSTREAM_REVISION", "Cargo.lock", "package.json",
    ".github/workflows/standalone.yml",
    "crates/fm1-live/Cargo.toml", "crates/fm1-live/build.rs",
    "crates/fm1-live/assets/InterTight.ttf", "crates/fm1-live/assets/InterTight-OFL.txt",
    "crates/fm1-live/src/main.rs", "crates/fm1-live/src/ctl.rs",
    "crates/fm1-plugin/build.rs", "crates/fm1-plugin/wrap/CMakeLists.txt",
    "crates/fm1-plugin/wrap/icon.rc.in",
    "tools/build_plugin.sh", "tools/build_web.sh", "tools/check_context.py",
    "tools/check_icon_artwork.py", "tools/check_upstream.sh", "tools/generate-icons.swift",
    "tools/package_apps.sh", "tools/package_icons.py", "tools/package_macos_app.sh",
    "tools/package_web_icons.py", "tools/windows-icon.rs",
}
changed = git("diff", "--name-only", "-z", revision, "--").split(b"\0")
untracked = git("ls-files", "--others", "--exclude-standard", "-z").split(b"\0")
paths = sorted({p.decode() for p in changed + untracked if p})
errors = [f"outside fork scope: {p}" for p in paths if p not in allowed and not p.startswith(owned)]
for p in git("diff", "--diff-filter=D", "--name-only", "-z", revision, "--").split(b"\0"):
    if p:
        errors.append(f"upstream file deleted: {p.decode()}")
if not Path("README.md").read_bytes().endswith(git("show", f"{revision}:README.md")):
    errors.append("preserve the upstream README verbatim after the fork notice")
if errors:
    raise SystemExit("\n".join(errors) + "\nRead context/fork-policy.md; do not widen the allowlist to bypass this check.")
print("PASS: all fork changes stay within the approved paths; upstream files and README are retained")
CHECK_SCOPE
git diff --exit-code "$revision" -- \
  crates/fm1-isa crates/fm1-core crates/fm1-soc crates/fm1-map \
  crates/fm1-emu crates/fm1-gdbstub crates/fm1-web crates/fm1-plugin web \
  crates/fm1-live/src/panel.rs crates/fm1-live/src/labels.rs \
  crates/fm1-live/src/fit.rs crates/fm1-live/src/wav.rs \
  crates/fm1-live/src/panel_smk.rs crates/fm1-live/src/exclusive.rs \
  ':(exclude)crates/fm1-plugin/build.rs' \
  ':(exclude)crates/fm1-plugin/wrap/CMakeLists.txt' \
  ':(exclude)crates/fm1-plugin/wrap/icon.rc.in'
printf '%s\n' 'PASS: shared upstream runtime sources match UPSTREAM_REVISION'
