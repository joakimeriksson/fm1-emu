#!/usr/bin/env python3
"""Play every catalog firmware with the JIT and interpreted, and check the audio against tests/catalog.json.

    tools/catalog_check.py [--live target/release/fm1-live] [--shard I/N] [--jobs N] [--update] [ID ...]

Each firmware in web/firmwares.json (fetched into reference/firmwares/ by tools/fetch_firmwares.mjs)
plays 8 s at 240 MHz with keys, MIDI and PLAY on a script, as a fm1-live --bench, booted from its
package as the page boots it (fm1-live --fwsc: the package's other sectors too, which some
firmwares need before they make a sound). It fails when
  - the JIT and the interpreter give different audio (the interpreter is the JIT's oracle),
  - the audio is short (the emulator stopped), or
  - the audio is not the one in tests/catalog.json: an emulator change that moves a firmware's sound
    must say so in the same pull request (--update writes the new hashes; review the list it prints).
A firmware that is missing (its site was down) is reported, not failed: the check is of the emulator.
One whose package is not the one hashed (its author published a new build) is still checked JIT
against interpreter; its audio is compared again after an --update.

M-VAVE's stock firmware and FM1 Open are not in the catalog (bring your own); the release checklist
runs them by hand.
"""
import concurrent.futures, hashlib, json, os, subprocess, sys, tempfile

args = sys.argv[1:]
def opt(name, default):
    if name in args:
        i = args.index(name)
        v = args[i + 1]
        del args[i:i + 2]
        return v
    return default
live = opt("--live", "target/release/fm1-live")
shard, shards = map(int, opt("--shard", "0/1").split("/"))
jobs = int(opt("--jobs", str(os.cpu_count() or 2)))
update = "--update" in args
ids = [a for a in args if not a.startswith("--")]

root = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
fw_dir = os.path.join(root, "reference", "firmwares")
golden_path = os.path.join(root, "tests", "catalog.json")
KEYS = "2000:C4,2000:m903c64,3500:C4^,3500:m803c00,5000:PLAY"
MS = 8000
SLACK = 4096                   # a run may end half a buffer later: the hash is of all but the last bytes
MIN = 44100 * 4                # under a second of audio (44.1 kHz stereo s16le) is no run

def sha(b): return hashlib.sha256(b).hexdigest()

def play(pkg, jit, tmp):
    wav = os.path.join(tmp, "jit.raw" if jit else "int.raw")
    env = dict(os.environ, FM1_BENCH_WAV=wav, FM1_BENCH_KEYS=KEYS)
    env.pop("FM1_NO_JIT", None)
    if not jit:
        env["FM1_NO_JIT"] = "1"
    r = subprocess.run([live, "--fwsc", pkg, "--mhz", "240", "--bench", str(MS)], env=env,
                       capture_output=True, text=True, timeout=600)
    speed = next((w[:-1] for line in r.stdout.splitlines() if "ms emulated" in line
                  for w in line.split() if w.endswith("x")), "?")
    audio = open(wav, "rb").read() if os.path.exists(wav) else b""
    return audio, speed

def check(fid, want):
    pkg = os.path.join(fw_dir, fid + ".fwsc")
    if not os.path.exists(pkg):
        return fid, "missing", None, "not fetched"
    pkg_sha = sha(open(pkg, "rb").read())[:16]
    with tempfile.TemporaryDirectory() as tmp:
        (j, js), (i, is_) = play(pkg, True, tmp), play(pkg, False, tmp)
    note = f"{js}x / {is_}x real time"
    n = min(len(j), len(i))
    if n < MIN:
        return fid, "FAIL", None, f"short audio: {len(j)} / {len(i)} bytes ({note})"
    if j[:n] != i[:n]:
        at = next(k for k in range(n) if j[k] != i[k])
        return fid, "FAIL", None, f"JIT and interpreter differ from byte {at} ({note})"
    # each firmware its own length (some start their sound late, some render slower than real time)
    cut = want["bytes"] if want and want["package"] == pkg_sha and not update else n - SLACK
    if n < cut:
        return fid, "FAIL", None, f"the audio ends at byte {n}, was past {cut} ({note})"
    got = {"package": pkg_sha, "bytes": cut, "audio": sha(j[:cut])[:16]}
    if not want:
        return fid, "new" if update else "FAIL", got, f"no hash in tests/catalog.json ({note})"
    if want["package"] != pkg_sha:
        return fid, "rebuilt", got, f"a new package upstream: JIT = interpreter, audio not compared ({note})"
    if got["audio"] != want["audio"]:
        return fid, "changed" if update else "FAIL", got, f"audio {got['audio']}, was {want['audio']} ({note})"
    return fid, "ok", got, note

golden = json.load(open(golden_path)) if os.path.exists(golden_path) else {}
cat = [f["id"] for f in json.load(open(os.path.join(root, "web", "firmwares.json")))["firmwares"] if not f.get("byo")]
todo = [fid for k, fid in enumerate(cat) if k % shards == shard and (not ids or fid in ids)]
with concurrent.futures.ThreadPoolExecutor(max(1, jobs // 2)) as pool:   # (two runs per firmware)
    results = list(pool.map(lambda fid: check(fid, golden.get(fid)), todo))

for fid, status, _, note in results:
    print(f"{fid:<14} {status:<8} {note}")
count = {s: sum(1 for r in results if r[1] == s) for s in ("ok", "rebuilt", "new", "changed", "missing", "FAIL")}
print(", ".join(f"{n} {s}" for s, n in count.items() if n) or "nothing to check")
orphans = sorted(k for k in golden if k not in cat)
if update:
    for fid, status, got, _ in results:
        if got:
            golden[fid] = got
    # a firmware gone from the catalog takes its hash with it (a whole, unfiltered run: a shard
    # or a run of some ids only says what it would drop)
    if orphans and not ids and shards == 1:
        print(f"dropped from tests/catalog.json (not in the catalog): {', '.join(orphans)}")
        for k in orphans:
            del golden[k]
    with open(golden_path, "w") as f:
        json.dump(dict(sorted(golden.items())), f, indent=1)
        f.write("\n")
if orphans and (not update or ids or shards > 1):
    print(f"in tests/catalog.json, not in the catalog: {', '.join(orphans)} (a whole --update drops them)")
sys.exit(1 if count["FAIL"] else 0)
