#!/usr/bin/env python3
"""Replay pi32v2-isa's FM-1 hardware captures through fm1-emu's CPU core with the whole recorded state.

Every single-instruction record of pi32v2-isa's hw/ runs (register-only format 1; format 2 with sp, rets,
the scratch memory and the exit taken; parallel twins with their second instruction) is set up in fm1-emu as it
was on the chip: the code at the recorded stub address, the 16 registers, the raw psr flags, sp, rets and the
scratch words. crates/fm1-soc/examples/hw_replay.rs steps it. Compared: registers, psr bits 3..0, sp, rets, the exit
(fell through or taken) and the scratch words that changed.

Not replayed: block openers (if/rep/tbb bodies), cases with set-up or read-back instructions, the application
runs (they ran in handlers), and anything else whose record does not say enough.

    git clone https://github.com/jvitkauskas/pi32v2-isa ../pi32v2-isa
    python3 tools/hw_replay.py --isa ../pi32v2-isa [--only ID,ID] [--show 3]

Writes target/hw-replay/hw-replay.json and .md (every form with a difference, its first cases); prints a
summary.
"""
import argparse
import collections
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPLAY = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "release/examples/hw_replay"
RUNS = ["fm1-2026-10-24", "fm1-2026-10-24-v2", "fm1-2026-10-24-v3", "fm1-2026-10-24-v4", "fm1-2026-10-24-v5",
        "fm1-2026-11-14-bit12"]
FMT1_SP = 0x01C38000


def patterns(d):
    out = {}
    if (d / "patterns.json").exists():
        out.update(json.loads((d / "patterns.json").read_text()))
    for extra in sorted(d.glob("*.patterns.json")):
        out.update(json.loads(extra.read_text()))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--isa", required=True, type=Path, help="a pi32v2-isa checkout")
    ap.add_argument("--only")
    ap.add_argument("--show", type=int, default=3)
    ap.add_argument("--out", default=ROOT / "target/hw-replay", type=Path)
    a = ap.parse_args()
    subprocess.run(["cargo", "build", "--release", "-q", "-p", "fm1-soc", "--example", "hw_replay"], cwd=ROOT, check=True)
    sys.path.insert(0, str(a.isa))
    sys.path.insert(0, str(a.isa / "tools/hwtest"))
    from pi32v2isa import table  # noqa: E402
    import gen  # noqa: E402
    tab = table.load(a.isa / "isa/pi32v2.toml")
    only = set(a.only.split(",")) if a.only else None

    cases, skipped = [], collections.Counter()
    for run in RUNS:
        d = a.isa / "hw" / run
        pats = patterns(d)
        for f in sorted(d.glob("*.jsonl")):
            for line in f.read_text().splitlines():
                if not line.strip():
                    continue
                r = json.loads(line)
                if r["id"] == "nop" or (only and r["id"] not in only):
                    continue
                insn = tab.by_id(r["id"])
                if insn.block or insn.body or r["id"] in ("tbb", "tbh") or any(k in r for k in ("before", "after", "entry", "model")):
                    skipped["block/setup"] += 1
                    continue
                code = bytes.fromhex(r["bytes"])
                if "pair_b" in r:
                    pb = r["pair_b"]
                    bi = tab.by_id(pb["id"])
                    code += gen.enc(tab, pb["id"], d=pb["d"], b=pb["b"], n=gen.solve(bi, "n", pb["n"]))
                v2 = "sp_in" in r
                if v2:
                    if r["batch"] not in pats:
                        skipped["no pattern"] += 1
                        continue
                    words = list(pats[r["batch"]])
                    k = (r["sp_in"] - 4 - r["scratch"]) // 4
                    if 0 <= k < len(words):
                        words[k] = r["stub"]
                    sp, rets, scratch, stub = r["sp_in"], r["rets_in"], r["scratch"], r["stub"]
                else:
                    words, sp, rets, scratch, stub = [], FMT1_SP, 0, FMT1_SP, 0x01C30000
                cases.append((f"{run}/{f.name}", r, code, stub, sp, rets, scratch, words))
    lines = []
    for _, r, code, stub, sp, rets, scratch, words in cases:
        regs = r["in"] if "in_values" not in r else r["in_values"]
        lines.append(" ".join([code.hex(), f"{stub:x}"] + [f"{v & 0xffffffff:x}" for v in regs] +
                              [f"{r['flags_in'] & 0xf:x}", f"{sp:x}", f"{rets:x}", f"{scratch:x}", f"{len(words):x}"] +
                              [f"{w & 0xffffffff:x}" for w in words]))
    chunks = [lines[k::8] for k in range(8)]
    # each case gets a fresh Cpu and Soc; the Cpu's ~13 MB instruction cache would otherwise be mapped and
    # unmapped every time: keep freed memory in the heap
    env = dict(os.environ, MALLOC_MMAP_THRESHOLD_="33554432", MALLOC_TRIM_THRESHOLD_="4294967296",
               MALLOC_TOP_PAD_="268435456")
    procs = [subprocess.Popen([str(REPLAY)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=env)
             for _ in chunks]
    outs = [p.communicate("\n".join(c) + "\n")[0].splitlines() for p, c in zip(procs, chunks)]
    results = [None] * len(lines)
    for k, o in enumerate(outs):
        results[k::8] = o
    assert len(results) == len(cases), (len(results), len(cases))

    per = collections.defaultdict(lambda: {"n": 0, "ok": 0, "kinds": collections.Counter(), "ex": []})
    for (src, r, code, stub, sp, rets, scratch, words), res in zip(cases, results):
        f = res.split()
        p = per[r["id"]]
        p["n"] += 1
        bad = []
        if f[0] == "err":
            bad.append(("error", f[1]))
        else:
            regs = [int(x, 16) for x in f[1:17]]
            psr, sp_o, rets_o, pc, cc, nch = (int(f[17], 16), int(f[18], 16), int(f[19], 16), int(f[20], 16),
                                              int(f[21]), int(f[22]))
            mem = {int(f[23 + 2 * k], 16): int(f[24 + 2 * k], 16) for k in range(nch)}
            for k in range(16):
                if regs[k] != r["out"][k] & 0xffffffff:
                    bad.append((f"r{k}", f"in {r['in'][k] & 0xffffffff:#x} hw {r['out'][k] & 0xffffffff:#x} emu {regs[k]:#x}"))
            if psr & 0xf != r["psr_out"] & 0xf:
                bad.append(("psr", f"in {r['flags_in'] & 0xf:04b} hw {r['psr_out'] & 0xf:04b} emu {psr & 0xf:04b}"))
            if "sp_in" in r:
                if sp_o != r["sp_out"]:
                    bad.append(("sp", f"hw {r['sp_out']:#x} emu {sp_o:#x}"))
                if rets_o != r["rets_out"]:
                    bad.append(("rets", f"hw {r['rets_out']:#x} emu {rets_o:#x}"))
                exit_emu = 0 if pc == stub + len(code) else 1
                if exit_emu != r["exit"]:
                    bad.append(("exit", f"hw {r['exit']} emu {exit_emu} (pc {pc:#x}, fall-through {stub + len(code):#x}, tgt {r.get('tgt')})"))
                # the runner's exit pushes over the 16 words below the final sp and the word at it, and the
                # case's own return slot is at sp_in - 4: neither side is compared there
                lo, hi = r["sp_out"] - 64, r["sp_out"] + 4
                skip = lambda k: lo <= k < hi or k == r["sp_in"] - 4
                hw_mem = {int(k, 16): v for k, v in r["mem"].items() if not skip(int(k, 16))}
                emu_mem = {k: v for k, v in mem.items() if not skip(k)}
                if hw_mem != emu_mem:
                    diff = {hex(k): (hw_mem.get(k), emu_mem.get(k)) for k in set(hw_mem) | set(emu_mem)
                            if hw_mem.get(k) != emu_mem.get(k)}
                    bad.append(("mem", f"hw/emu {dict(list(diff.items())[:3])}"))
        if bad:
            for kind, _ in bad:
                p["kinds"][kind.rstrip("0123456789") if kind.startswith("r") and kind[1:].isdigit() else kind] += 1
            if len(p["ex"]) < a.show:
                p["ex"].append({"src": src, "text": r["text"], "bytes": r["bytes"], "bad": bad[:4]})
        else:
            p["ok"] += 1

    forms = sorted(({"id": k, "group": tab.by_id(k).group, **{x: v[x] for x in ("n", "ok", "ex")},
                     "kinds": dict(v["kinds"])} for k, v in per.items()),
                   key=lambda x: (x["ok"] == x["n"], x["group"], x["id"]))
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out / "hw-replay.json").write_text(json.dumps(forms, indent=1))
    n = sum(x["n"] for x in forms)
    ok = sum(x["ok"] for x in forms)
    good = sum(1 for x in forms if x["ok"] == x["n"])
    md = [f"# fm1-emu against FM-1 hardware captures", "",
          f"{n} cases of {len(forms)} forms: {ok} cases agree; {good} forms agree on every case. Skipped: {dict(skipped)}.", "",
          "| id | group | ok/n | what differs | first case |", "|---|---|---|---|---|"]
    for x in forms:
        if x["ok"] == x["n"]:
            continue
        e = x["ex"][0]
        md.append(f"| `{x['id']}` | {x['group']} | {x['ok']}/{x['n']} | {', '.join(f'{k} {v}' for k, v in x['kinds'].items())} | "
                  f"`{e['text']}`: {'; '.join(f'{k}: {v}' for k, v in e['bad'][:2])} |")
    (a.out / "hw-replay.md").write_text("\n".join(md) + "\n")
    print(md[2])
    by = collections.Counter()
    for x in forms:
        for k, v in x["kinds"].items():
            by[k] += v
    print("differences by kind:", dict(by.most_common()))


if __name__ == "__main__":
    main()
