#!/usr/bin/env python3
"""The lowest clock each catalog firmware plays at without running out of time, into web/firmwares.json.

    tools/clock_floors.py [--live target/release/fm1-live] [--jobs N] [ID ...]

Each firmware (fetched into reference/firmwares/ by tools/fetch_firmwares.mjs) plays MS as a
fm1-live --bench with KEYS, at each of the page's clocks from the top down, until a clock where
it leaves DIRTY audio halves or more unfilled (starvedHalves, fm1-soc's Refill), where fm1-live
gives no result (halted, timed out), or where it is silent: a silent run leaves no half unfilled,
so its 0 proves nothing. Its minimum is the lowest clock above that one: top down, because a
firmware can starve at a higher clock and not at a lower one. The entry gets "minMhz" and
"minMhzPackage", the first 16 hex digits of the SHA-256 of the package it was measured on, as
tests/catalog.json keeps them: the page ignores the minimum on any other package. No field when
the lowest clock is enough, or when the top one is not (the page's live count tells that one).
A firmware silent at the top clock with these keys (a tracker with no song, a MIDI tool) is said
so and gets no field.

A package that is not the one in tests/catalog.json (fetched before a catalog change) is not
measured: its minimum would name a package the page does not run.
"""
import array, concurrent.futures, hashlib, json, os, re, subprocess, sys, tempfile

args = sys.argv[1:]
def opt(name, default):
    if name in args:
        i = args.index(name)
        v = args[i + 1]
        del args[i:i + 2]
        return v
    return default
live = opt("--live", "target/release/fm1-live")
jobs = int(opt("--jobs", str(os.cpu_count() or 2)))
ids = [a for a in args if not a.startswith("--")]

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
fw_dir = os.path.join(root, "reference", "firmwares")
cat_path = os.path.join(root, "web", "firmwares.json")
CLOCKS = [int(c) for c in re.search(r"const CLOCKS = \[([\d, ]+)\]", open(os.path.join(root, "web", "main.js")).read())[1].split(",")]
# (the MIDI note for firmwares whose panel keys do something else; PLAY early enough to leave
# the rest of the run to what it starts, the heaviest part for some)
KEYS = ("3000:C4@100,3000:E4@100,3000:G4@100,3000:m903c64,8000:C4^,8000:E4^,8000:G4^,"
        "8000:m803c00,9000:PLAY,9100:PLAY^")
MS = 20000
DIRTY = 3                      # one or two starved halves in a run are a click, not a sound that breaks up
SILENT = 8                     # peak to peak of the whole run, in steps of the 16-bit output

def starved(pkg, mhz):
    """The starved halves and whether the run was silent; None when fm1-live failed."""
    with tempfile.TemporaryDirectory() as tmp:
        out, wav = os.path.join(tmp, "bench.json"), os.path.join(tmp, "audio.raw")
        env = dict(os.environ, FM1_BENCH_KEYS=KEYS, FM1_BENCH_JSON=out, FM1_BENCH_WAV=wav)
        try:
            subprocess.run([live, "--fwsc", pkg, "--mhz", str(mhz), "--bench", str(MS)], env=env,
                           capture_output=True, timeout=600)
        except subprocess.TimeoutExpired:
            return None, False
        if not os.path.exists(out):
            return None, False
        a = array.array("h", open(wav, "rb").read() if os.path.exists(wav) else b"")
        return json.load(open(out))["starvedHalves"], not a or max(a) - min(a) <= SILENT

def measure(fid, want):
    pkg = os.path.join(fw_dir, fid + ".fwsc")
    if not os.path.exists(pkg):
        return None, None, "not fetched"
    pkg_sha = hashlib.sha256(open(pkg, "rb").read()).hexdigest()[:16]
    if pkg_sha != want:
        return None, None, f"package {pkg_sha} is not tests/catalog.json's {want}: fetch it again"
    counts, lowest = [], None
    for mhz in reversed(CLOCKS):
        n, silent = starved(pkg, mhz)
        counts.append(f"{mhz}:{'silent' if silent and n is not None else n}")
        if n is None and lowest is None:
            return None, None, f"fm1-live gave no result at {mhz} MHz"
        if silent and lowest is None:
            return None, pkg_sha, f"SILENT at {mhz} MHz with these keys"
        if n is None or silent or n >= DIRTY:
            break
        lowest = mhz
    return lowest, pkg_sha, " ".join(counts)

# web/firmwares.json as tools/hub_catalog.mjs lays it out (fm1-live's reader scans for that layout)
def text_of(doc):
    s = lambda v: json.dumps(v, ensure_ascii=False)
    fields = lambda f, keys: ", ".join(f'"{k}": {s(f[k])}' for k in keys if k in f)
    head, own = ["id", "name", "version", "author", "license"], ["about", "repo"]
    def entry(f):
        rest = [k for k in f if k not in head and k not in own]
        lines = [f"    {{ {fields(f, head)},"] + [f"      {fields(f, [k])}," for k in own if k in f] + [f"      {fields(f, rest)} }}"]
        return re.sub(r",\n      \s*}$", " }", "\n".join(lines))
    skipped = json.dumps(doc["skipped"], ensure_ascii=False, separators=(",", ":"))
    return (f'{{\n  "note": {s(doc["note"])},\n  "default": {s(doc["default"])},\n  "skipped": {skipped},\n'
            '  "firmwares": [\n' + ",\n".join(entry(f) for f in doc["firmwares"]) + "\n  ]\n}\n")

# (no fm1-live is not a firmware that gives no result: every one would keep the minimum it has, and the run would pass)
if not os.access(live, os.X_OK):
    sys.exit(f"{live}: no fm1-live to run (cargo build --release -p fm1-live, or --live PATH)")
text = open(cat_path, encoding="utf-8").read()
doc = json.loads(text)
if text_of(doc) != text:
    sys.exit("web/firmwares.json is not laid out as tools/hub_catalog.mjs writes it: run that first")
golden = json.load(open(os.path.join(root, "tests", "catalog.json")))
todo = [f for f in doc["firmwares"] if not f.get("byo") and (not ids or f["id"] in ids)]
with concurrent.futures.ThreadPoolExecutor(max(1, jobs)) as pool:
    results = list(pool.map(lambda f: measure(f["id"], golden.get(f["id"], {}).get("package")), todo))

for f, (mhz, pkg_sha, note) in zip(todo, results):
    kept = f" (keeps its minimum of {f['minMhz']} MHz, measured on {f['minMhzPackage']})" if not pkg_sha and "minMhz" in f else ""
    print(f"{f['id']:<14} {mhz or '-':<4} {note}{kept}")
    if not pkg_sha:
        continue
    if mhz and mhz > CLOCKS[0]:
        f.update({"minMhz": mhz, "minMhzPackage": pkg_sha})
    else:
        f.pop("minMhz", None)
        f.pop("minMhzPackage", None)
open(cat_path, "w", encoding="utf-8").write(text_of(doc))
