#!/usr/bin/env python3
"""Validate context structure and local documentation links without invoking git."""
from pathlib import Path
import re
root = Path(__file__).resolve().parent.parent
router = (root / "AGENTS.md").read_text()
errors = []
for doc in sorted((root / "context").glob("*.md")):
    text = doc.read_text()
    if not text.startswith("---\n"):
        errors.append(f"{doc.name}: missing frontmatter")
        continue
    front = text.split("---", 2)[1]
    for key in ("load_when:", "last_verified:", "source_refs:"):
        if key not in front:
            errors.append(f"{doc.name}: missing {key}")
    if f"context/{doc.name}" not in router:
        errors.append(f"{doc.name}: missing router entry")
    globs = re.findall(r"^  - (.+)$", front, re.M)
    for glob in globs:
        if not list(root.glob(glob.strip('"'))):
            errors.append(f"{doc.name}: unmatched source ref {glob}")
for doc in [root / "README.md", root / "AGENTS.md", *(root / "context").glob("*.md")]:
    text = doc.read_text()
    for target in re.findall(r"\]\(([^)]+)\)", text):
        if "://" in target or target.startswith(("#", "mailto:")):
            continue
        local = target.split("#", 1)[0].split(":", 1)[0]
        if local and not (doc.parent / local).exists():
            errors.append(f"{doc.relative_to(root)}: missing link {target}")
if errors:
    raise SystemExit("\n".join(errors))
print("PASS: context metadata, routing, source refs and local documentation links")
