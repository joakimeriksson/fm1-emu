#!/usr/bin/env python3
"""A picture of each firmware's screen for the page's cards and its wall: <page>/shots/<id>.webp (the
240x240 LCD, lossless) and <page>/shots/shots.json, what the page reads to know which firmwares have
one. The FM-1's page is web/, the SMK-37's web/smk37/.

    python3 tools/firmware_shots.py [--device fm1|smk37] [--jobs N] [ID...]

Each firmware in the page's firmwares.json plays a few seconds in fm1-live (--shot --record
--lcd-only, PLAY pressed and a chord held so its screen moves), and the busiest frame is kept: the
most edges and colours, so a splash screen or a blank one loses to a page full of parameters. The
packages: the FM-1's from reference/firmwares (tools/fetch_firmwares.mjs), the SMK-37's downloaded
into reference/smk37 from the catalog's package links. tools/firmware_shots.json can give a
firmware its own keys and time, or keep it off the page's wall (a picture of an error it shows
without its installer, say), with the reason.
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import urllib.request
from concurrent.futures import ThreadPoolExecutor

import numpy as np
from PIL import Image

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
LIVE = os.path.join(ROOT, "target", "release", "fm1-live")
# per device: the page, where its packages are, the keys that make its screen move
DEVICES = {
    "fm1": (os.path.join(ROOT, "web"), os.path.join(ROOT, "reference", "firmwares"),
            "1000:PLAY,1200:PLAY^,1500:C4,1500:E4,1500:G4,4500:C4^,4500:E4^,4500:G4^"),
    "smk37": (os.path.join(ROOT, "web", "smk37"), os.path.join(ROOT, "reference", "smk37"),
              "1000:SEQ PLAY,1200:SEQ PLAY^,1500:k12,1500:k16,1500:k19,4500:k12^,4500:k16^,4500:k19^"),
}
RUN_MS, FROM_MS, STEP = 6500, 1500, 4      # frames from 1.5 s on, every 4th (--record: one per 16 ms)


def score(frame):
    """How much the screen shows: its edges, how much of it is lit, its colours."""
    g = frame.astype(np.int16)
    lit = (g.max(axis=2) > 40).mean()
    edges = (np.abs(np.diff(g, axis=1)).sum(axis=2) > 60).mean() + (np.abs(np.diff(g, axis=0)).sum(axis=2) > 60).mean()
    colours = len(np.unique((frame[::2, ::2] // 32).reshape(-1, 3), axis=0))
    return edges * 2 + min(lit, 0.5) + min(colours, 40) / 80


def shoot(fid, opts, pkgs, keys, out):
    fwsc = os.path.join(pkgs, fid + ".fwsc")
    run_ms, from_ms = opts.get("ms", RUN_MS), opts.get("from", FROM_MS)
    with tempfile.TemporaryDirectory(prefix=f"shot-{fid}-") as rec:
        env = dict(os.environ, FM1_BENCH_KEYS=opts.get("keys", keys))
        r = subprocess.run([LIVE, "--fwsc", fwsc, "--shot", str(run_ms), "--record", rec, "--lcd-only"],
                           cwd=ROOT, env=env, capture_output=True, timeout=900)
        best = None
        for k in range(from_ms // 16, run_ms // 16 + 1, STEP):
            p = os.path.join(rec, f"{k:06d}.ppm")
            if not os.path.exists(p):
                continue
            frame = np.asarray(Image.open(p).convert("RGB"))
            s = score(frame)
            if best is None or s > best[0]:
                best = (s, frame)
        if best is None:
            return fid, f"no frames (fm1-live exited {r.returncode})"
    Image.fromarray(best[1]).save(os.path.join(out, fid + ".webp"), "WEBP", lossless=True, quality=100, method=6)
    return fid, "ok"


def main():
    args = sys.argv[1:]
    jobs, device = 4, "fm1"
    for flag in ("--jobs", "--device"):
        if flag in args:
            i = args.index(flag)
            if flag == "--jobs":
                jobs = int(args[i + 1])
            else:
                device = args[i + 1]
            del args[i:i + 2]
    if device not in DEVICES:
        sys.exit(f"--device fm1 or smk37, not {device}")
    page, pkgs, keys = DEVICES[device]
    OUT = os.path.join(page, "shots")
    if not os.path.exists(LIVE):
        sys.exit("build fm1-live first: cargo build --release -p fm1-live")
    catalog = json.load(open(os.path.join(page, "firmwares.json")))["firmwares"]
    own = json.load(open(os.path.join(ROOT, "tools", "firmware_shots.json")))[device]
    if device == "smk37":
        # the SMK-37's packages, from the catalog's links (the page downloads the same ones)
        os.makedirs(pkgs, exist_ok=True)
        for f in catalog:
            p = os.path.join(pkgs, f["id"] + ".fwsc")
            if f.get("package") and not os.path.exists(p) and (not args or f["id"] in args):
                try:
                    urllib.request.urlretrieve(f["package"], p)
                except OSError as e:
                    print(f"{f['id']}: no package ({e})", flush=True)
    have = [f["id"] for f in catalog if os.path.exists(os.path.join(pkgs, f["id"] + ".fwsc"))]
    ids = [i for i in have if not args or i in args]
    os.makedirs(OUT, exist_ok=True)
    failed = []
    with ThreadPoolExecutor(jobs) as ex:
        for fid, status in ex.map(lambda i: shoot(i, own.get(i, {}), pkgs, keys, OUT), ids):
            if status != "ok":
                failed.append(fid)
                print(f"{fid}: {status}", flush=True)
    # the list the page reads: each picture by the start of its hash (its URL's ?v=, so a new
    # picture is fetched again), and whether it belongs on the wall
    listed = {}
    for fid in sorted(have):
        p = os.path.join(OUT, fid + ".webp")
        if os.path.exists(p):
            e = {"v": hashlib.sha256(open(p, "rb").read()).hexdigest()[:8]}
            if own.get(fid, {}).get("wall") is False:
                e["wall"] = False
            listed[fid] = e
    for f in os.listdir(OUT):
        if f.endswith(".webp") and f[:-5] not in have:
            os.remove(os.path.join(OUT, f))          # (a firmware no longer in the catalog)
    with open(os.path.join(OUT, "shots.json"), "w") as fh:
        json.dump(listed, fh, indent=0, sort_keys=True)
        fh.write("\n")
    size = sum(os.path.getsize(os.path.join(OUT, f)) for f in os.listdir(OUT))
    print(f"{len(ids) - len(failed)} of {len(ids)} pictures taken; {len(listed)} in {os.path.relpath(OUT, ROOT)} ({size // 1024} KB)")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
