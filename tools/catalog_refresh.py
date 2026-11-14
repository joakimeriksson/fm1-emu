#!/usr/bin/env python3
"""The page's catalog brought up to the Hub's, keeping only what plays here.

    tools/catalog_refresh.py [--jobs N] [--summary FILE.md]

1. tools/hub_catalog.mjs rewrites web/firmwares.json from the FM-1 Firmware Hub's catalog.
2. What moved to a newer package, or is new, against the last commit's web/firmwares.json, is
   fetched (tools/fetch_firmwares.mjs) and played (tools/catalog_check.py --update: the JIT and
   the interpreter must agree, and the audio must last), which records its hash in
   tests/catalog.json.
3. What fails is held back: a firmware that moved keeps the package it had (its entry and hash as
   they were), a new one stays out of the file (the page still lists it among the Hub's newcomers,
   marked as not yet run here, web/fwsc.js hubNewcomers). A firmware gone from the Hub leaves
   tests/catalog.json too.
4. docs/FIRMWARES.md, the list with authors the README links to, follows the file
   (tools/readme_firmwares.mjs).
5. The summary (markdown, for a pull request) says what moved, what is new, what was held back
   and why. Exit 0 whether or not anything failed (a held-back firmware is not an error: the file
   stays one that plays); 1 only when the tools themselves could not run.

Run by .github/workflows/catalog-refresh.yml every day; by hand the same way.
"""
import json
import os
import re
import subprocess
import sys

root = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
CAT = os.path.join(root, "web", "firmwares.json")
GOLDEN = os.path.join(root, "tests", "catalog.json")
args = sys.argv[1:]
jobs = args[args.index("--jobs") + 1] if "--jobs" in args else str(os.cpu_count() or 2)
summary_path = args[args.index("--summary") + 1] if "--summary" in args else None


def sh(cmd, **kw):
    print("+", " ".join(cmd), flush=True)
    return subprocess.run(cmd, cwd=root, text=True, **kw)


def entries(text):
    return {f["id"]: f for f in json.loads(text)["firmwares"]}


before_text = sh(["git", "show", "HEAD:web/firmwares.json"], capture_output=True, check=True).stdout
before = entries(before_text)
if sh(["node", "tools/hub_catalog.mjs"]).returncode != 0:
    sys.exit("hub_catalog.mjs failed")
after_doc = json.load(open(CAT))
after = {f["id"]: f for f in after_doc["firmwares"]}

moved = [i for i in after if i in before and after[i].get("package") != before[i].get("package")]
new = [i for i in after if i not in before]
gone = [i for i in before if i not in after]
playable = [i for i in moved + new if not after[i].get("byo")]

results = {}
if playable:
    sh(["node", "tools/fetch_firmwares.mjs", *playable], check=True)
    r = sh([sys.executable, "tools/catalog_check.py", "--jobs", jobs, "--update", *playable], capture_output=True)
    print(r.stdout, r.stderr, sep="")
    for line in r.stdout.splitlines():
        m = re.match(r"(\S+)\s+(ok|rebuilt|new|changed|missing|FAIL)\s+(.*)", line)
        if m and m.group(1) in after:
            results[m.group(1)] = (m.group(2), m.group(3))
    if not results:
        sys.exit("catalog_check.py reported nothing")

held = {i: results[i][1] for i in playable if results.get(i, ("missing", "not played"))[0] in ("FAIL", "missing")}
if held or gone:
    golden = json.load(open(GOLDEN))
    for i in gone:
        golden.pop(i, None)
    # (a held-back firmware's hash: catalog_check --update records none for a failure, so the
    # one in tests/catalog.json is still its old package's)
    with open(GOLDEN, "w") as f:
        json.dump(dict(sorted(golden.items())), f, indent=1)
        f.write("\n")
if held:
    kept = []
    for f in after_doc["firmwares"]:
        if f["id"] not in held:
            kept.append(f)
        elif f["id"] in before:
            kept.append(before[f["id"]])    # the package it had, which plays
    after_doc["firmwares"] = kept
    # (the file's own layout: hub_catalog.mjs writes one entry per line pair; json.dumps would
    # reflow it, so the held-back entries are swapped in the text)
    text = open(CAT).read()
    for i in held:
        old = re.search(r'\n    \{ "id": "%s",.*?\},?(?=\n)' % re.escape(i), text, re.S)
        if not old:
            continue
        if i in before:
            prev = re.search(r'\n    \{ "id": "%s",.*?\},?(?=\n)' % re.escape(i), before_text, re.S)
            rep = prev.group(0).rstrip(",") + ("," if old.group(0).endswith(",") else "")
            text = text.replace(old.group(0), rep)
        else:
            text = text.replace(old.group(0), "")
    text = re.sub(r",(\n  \])", r"\1", text)   # a dropped last entry leaves a trailing comma
    json.loads(text)                          # still JSON
    open(CAT, "w").write(text)


if sh(["node", "tools/readme_firmwares.mjs"]).returncode != 0:
    sys.exit("readme_firmwares.mjs failed")


def ver(f):
    return f.get("version") or "?"


ok_moved = [i for i in moved if i not in held]
ok_new = [i for i in new if i not in held]
lines = [f"**{len(ok_moved)} moved to a newer package, {len(ok_new)} new, {len(held)} held back, {len(gone)} gone from the Hub** "
         f"({len(before)} firmwares before, {len(before) + len(ok_new) - len(gone)} now). Each moved or new one was played "
         "8 s with the JIT and interpreted (`tools/catalog_check.py`): the two agree, and its audio hash is in `tests/catalog.json`.", ""]
if ok_moved:
    lines += ["**Moved:** " + ", ".join(f"{after[i]['name']} {ver(before[i])} → {ver(after[i])}" for i in ok_moved), ""]
if ok_new:
    lines += ["**New:** " + ", ".join(f"{after[i]['name']} {ver(after[i])}" for i in ok_new), ""]
if held:
    lines += ["**Held back** (the page keeps the package that plays, or lists a new one as not yet run here):"]
    lines += [f"- {after[i]['name']} {ver(after[i])}: {why}" for i, why in held.items()]
    lines += [""]
if gone:
    lines += ["**Gone from the Hub:** " + ", ".join(before[i]["name"] for i in gone), ""]
summary = "\n".join(lines) if (moved or new or gone) else "The catalog is current with the Hub."
print("\n" + summary)
if summary_path:
    open(summary_path, "w").write(summary + "\n")
