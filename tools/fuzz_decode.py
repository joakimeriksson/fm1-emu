#!/usr/bin/env python3
"""Fuzz the emulator's decoder against the vendor disassembler.

    tools/fuzz_decode.py [--sleigh DIR] [--per N] [--seed S] [--no-corpus] [--reuse]

Makes test instructions, has the vendor toolchain's objdump decode them (in Docker, as
tools/probe_build.sh runs it) and the emulator too (`fm1-emu --check-decode`), and reports where
they disagree:
  gap        objdump decodes it, the emulator doesn't (a class narrower than the encoding)
  length     both decode it, to different lengths (the emulator would run on mid-instruction)
  no-sem     the emulator decodes it but has no semantics for it
  junk       the emulator decodes what objdump calls unknown
then runs tools/audit_decode.py over the listing (operands: registers, offsets, targets).

The instructions come from
  - the corpus classes' samples (isa/fm1.yaml), each bit flipped in turn: the encodings next to
    the ones the firmwares use, where a class fitted only to the corpus stops short;
  - with --sleigh DIR, the constructors of a Ghidra SLEIGH description of the ISA (e.g.
    github.com/edco/ghidra-jieli's data/languages): N random encodings of each, its fixed fields
    set, the rest random.
Each goes in a 32-byte slot padded with nops. Output in work/fuzz/.
"""
import collections, hashlib, os, random, re, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "work", "fuzz")
# the newest JieLi toolchain under reference/toolchain (docs/recon.md names the archive)
TC = "/fm1/reference/toolchain/" + sorted(n for n in os.listdir(os.path.join(ROOT, "reference", "toolchain")) if n.startswith("jieli-linux-toolchains-"))[-1]
SLOT = 32

args = sys.argv[1:]
opt = lambda k, d=None: args[args.index(k) + 1] if k in args else d
sleigh = opt("--sleigh")
per = int(opt("--per", "6"))
rng = random.Random(int(opt("--seed", "1")))
os.makedirs(OUT, exist_ok=True)

cands = []                                   # (source, bytes)


def add(src, bs):
    cands.append((src, bytes(bs)))


# ---- the corpus classes, bit by bit
if "--no-corpus" not in args:
    # (the file's own regular layout: `  - name: x` opens a class, `    samples: [..]` lists its)
    isa, name = [], None
    for l in open(os.path.join(ROOT, "isa", "fm1.yaml")):
        if l.startswith("  - name: "):
            name = l.split(": ", 1)[1].strip()
        elif l.startswith("    samples: "):
            isa.append({"name": name, "samples": re.findall(r'"([0-9a-f]+)"', l)})
    seen = set()
    for e in isa:
        for s in e["samples"][:3]:
            b = bytes.fromhex(s)
            for bit in range(8 * len(b)):
                m = bytearray(b)
                m[bit // 8] ^= 1 << (bit % 8)
                if bytes(m) not in seen:
                    seen.add(bytes(m))
                    add("flip:" + e["name"], m)

# ---- a SLEIGH description's constructors
if sleigh:
    files = [os.path.join(sleigh, f) for f in sorted(os.listdir(sleigh)) if f.startswith("pi32v2") and f.endswith((".sinc", ".slaspec"))]
    text = "\n".join(open(f, errors="replace").read() for f in files)
    fields = {}                                  # name -> (lo, hi) within its 16-bit token
    for tok in re.finditer(r"define token \w+\(16\)(.*?);", text, re.S):
        for m in re.finditer(r"^\s*(\w+)\s*=\s*\((\d+)\s*,\s*(\d+)\)", re.sub(r"#.*", "", tok[1]), re.M):
            fields.setdefault(m[1], (int(m[2]), int(m[3])))
    fields["group"] = (13, 15)                   # (the context's copy of the prefix)
    # constructors: `[table]:display is constraints {` (a line each)
    tables = collections.defaultdict(list)
    for line in text.splitlines():
        m = re.match(r"^\s*(\w*):(.*?)\bis\b(.*?)(?:\[|\{|$)", line)
        if m and not line.lstrip().startswith("#"):
            tables[m[1]].append((m[2].strip(), m[3].strip(), line))

    def realise(cons, off, hw, depth=0):
        """apply a constraint text at halfword offset off to hw (a dict halfword -> [value, fixed mask])"""
        for k, seg in enumerate(cons.split(";")):
            for term in re.split(r"&", seg):
                term = term.strip().strip("()").strip()
                if not term or term == "...":
                    continue
                m = re.match(r"^(\w+)\s*(=|!=|<=|>=|<|>)\s*(\w+)$", term)
                name = m[1] if m else term
                if name in fields:
                    lo, hi = fields[name]
                    w = hi - lo + 1
                    v, mask = hw.setdefault(off + k, [rng.getrandbits(16), 0])
                    if m:
                        want = int(m[3], 0)
                        ok = {"=": lambda x: x == want, "!=": lambda x: x != want, "<": lambda x: x < want,
                              ">": lambda x: x > want, "<=": lambda x: x <= want, ">=": lambda x: x >= want}[m[2]]
                        cur = (v >> lo) & ((1 << w) - 1)
                        x = want if m[2] == "=" else next((y for y in [cur] + [rng.getrandbits(w) for _ in range(64)] if ok(y)), cur)
                        v = (v & ~(((1 << w) - 1) << lo)) | (x << lo)
                        mask |= ((1 << w) - 1) << lo
                    hw[off + k] = [v, mask]
                elif name in tables and depth < 6:
                    sub = rng.choice(tables[name])
                    realise(sub[1], off + k, hw, depth + 1)

    n = 0
    for disp, cons, line in tables[""]:
        if "parainst" in cons or "cdtinst" in cons or "instruction" in cons.split():
            continue                             # (the pair / block / repeat wrappers)
        mn = disp.split()[0] if disp else "?"
        for _ in range(per):
            hw = {}
            realise(cons, 0, hw)
            if 0 not in hw:
                break
            ln = max(hw) + 1
            add("sleigh:" + mn, b"".join(hw.get(i, [0])[0].to_bytes(2, "little") for i in range(ln)))
            n += 1
    print(f"{n} encodings from {len(tables[''])} SLEIGH constructors")

print(f"{len(cands)} test instructions")

# ---- the vendor objdump
with open(os.path.join(OUT, "cands.S"), "w") as f:
    f.write(".text\n.globl _start\n_start:\n")
    for _, b in cands:
        f.write(".byte " + ",".join(str(x) for x in (b + bytes(SLOT - len(b)))) + "\n")
with open(os.path.join(OUT, "cands.txt"), "w") as f:
    for _, b in cands:
        f.write((b + bytes(SLOT - len(b))).hex() + "\n")
# (--reuse: the last run's listing, when it was made from the same instructions: the listing is
# matched to them slot by slot)
digest = hashlib.sha256(open(os.path.join(OUT, "cands.S"), "rb").read()).hexdigest()
stamp = os.path.join(OUT, "cands_objdump.sha256")
if "--reuse" in args:
    if not os.path.exists(stamp) or open(stamp).read().strip() != digest:
        sys.exit("--reuse: the last listing was made from other instructions (another --seed, --per, "
                 "--sleigh or corpus); run without --reuse")
else:
    subprocess.run(["docker", "run", "--rm", "--platform", "linux/amd64", "-v", f"{ROOT}:/fm1", "-w", "/fm1", "debian:bookworm-slim",
                    "bash", "-c", f"'{TC}/pi32v2/bin/clang' -target pi32v2 -c work/fuzz/cands.S -o work/fuzz/cands.o && "
                    f"'{TC}/common/bin/objdump' -d work/fuzz/cands.o > work/fuzz/cands_objdump.txt"], check=True)
    open(stamp, "w").write(digest + "\n")
line_re = re.compile(r"^\s*([0-9a-f]+):\s+((?:[0-9a-f]{2} )+)\s*\t?\s*(.*)$")
vendor = {}
for l in open(os.path.join(OUT, "cands_objdump.txt"), errors="replace"):
    m = line_re.match(l)
    if m and int(m[1], 16) % SLOT == 0:
        vendor[int(m[1], 16) // SLOT] = (len(m[2].split()), m[3].strip())

# ---- the emulator
emu = subprocess.run([os.path.join(ROOT, "target/release/fm1-emu"), "--check-decode", os.path.join(OUT, "cands.txt")],
                     capture_output=True, text=True, check=True).stdout
ours = {}
for l in emu.splitlines():
    i, ln, name, st = l.split("|", 3)
    ours[int(i)] = (int(ln), name, st)


def shape(t):
    t = re.sub(r"\s*<[^>]*>\s*$", "", t)
    t = re.sub(r"\b(r\d+_r\d+)\b", "D", t)
    t = re.sub(r"\br\d+\b", "R", t)
    return re.sub(r"-?0x[0-9a-fA-F]+|-?\b\d+\b", "N", t).strip()


# the shapes the firmwares use (the corpus listings, isa/corpora.txt): a gap or a missing semantics
# in one of these is a class fitted too narrowly; in another, an instruction no firmware has used
used = collections.Counter()
for path in open(os.path.join(ROOT, "isa", "corpora.txt")).read().split():
    path = os.path.join(ROOT, path)
    if os.path.exists(path):
        for l in open(path, errors="replace"):
            m = line_re.match(l)
            if m:
                used[shape(m[3].strip())] += 1
print(f"{len(used)} instruction shapes in the corpus listings")

kinds = collections.defaultdict(lambda: collections.defaultdict(list))
for i, (src, b) in enumerate(cands):
    if i not in vendor:
        continue
    vlen, vt = vendor[i]
    olen, oname, st = ours[i]
    unknown = "unknown" in vt
    if unknown and olen == 0:
        continue
    sh = shape(vt)
    where = "" if unknown or sh in used else " (unused)"
    if unknown:
        kinds["junk"][oname].append((b, vt, src, oname))
    elif olen == 0:
        kinds["gap" + where][sh].append((b, vt, src, oname))
    elif olen != vlen:
        kinds["length"][sh].append((b, vt, src, f"{oname} len {olen}"))
    elif st != "ok":
        kinds["no-sem" + where][sh].append((b, vt, src, f"{oname}: {st}"))

with open(os.path.join(OUT, "report.txt"), "w") as f:
    for kind in ("length", "gap", "no-sem", "junk", "gap (unused)", "no-sem (unused)"):
        groups = kinds[kind]
        line = f"== {kind}: {sum(len(v) for v in groups.values())} instructions, {len(groups)} shapes"
        print(line)
        f.write(line + "\n")
        for k, items in sorted(groups.items(), key=lambda kv: -len(kv[1])):
            f.write(f"  [{len(items):4}] {k}\n")
            for b, vt, src, o in items[:3]:
                f.write(f"         {b.hex(' '):24} {vt[:56]:56} {src:28} {o}\n")
print(f"(details: {os.path.relpath(OUT, ROOT)}/report.txt)")

# ---- operands, where both decode
audit = subprocess.run([sys.executable, os.path.join(ROOT, "tools/audit_decode.py"), os.path.join(OUT, "cands_objdump.txt")],
                       capture_output=True, text=True)
open(os.path.join(OUT, "audit.txt"), "w").write(audit.stdout + audit.stderr)
print(audit.stdout.splitlines()[0] if audit.stdout else audit.stderr[-400:])
