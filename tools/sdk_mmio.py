#!/usr/bin/env python3
"""What a JieLi SDK library does to the hardware: every register read and write in its code, the
value written as an expression of the function's arguments, struct fields and other registers.

  tools/sdk_mmio.py LIB.a MEMBER.c.o [--fn NAME] [--headers DIR] [--depth 8]
      LIB.a: an SDK library (cpu/wl82/liba/*.a, LLVM bitcode members; reference/ac79sdk/liba);
      MEMBER: the object in it (eq.c.o, src.c.o). Prints, function by function, each access:
        W JL_EQ->CON1 = ((ch.channels << 4) | 0x1)
        R JL_EQ->CON0 & 0x80000000 -> branch
      --headers: the SDK's register maps (WL82.h, csfr.h) for the registers' names
      (default reference/ac79sdk/h); --ll: keep the IR (MEMBER.ll) next to the library.
  tools/sdk_mmio.py LIB.a --list       the members and their functions

The members are LLVM 4 bitcode with debug info: llvm-dis (Homebrew's llvm, $LLVM_DIS) reads them,
with the debug info's upgrade check off (two members fail it). Struct fields are named from the
debug info (DW_TAG_member offsets against the IR struct's layout, pi32v2: 32-bit, natural alignment);
the registers from the headers' `typedef struct {..} X_TypeDef` and `#define JL_X_BASE (base +
map_adr(g, a))`. The expressions follow SSA definitions to --depth; phi and select show their arms."""
import argparse
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LLVM_DIS = os.environ.get("LLVM_DIS", "/opt/homebrew/opt/llvm/bin/llvm-dis")


def ar_members(path):
    """[(name, bytes)] of a GNU ar archive (long names in '//'; repeated names numbered)"""
    d = Path(path).read_bytes()
    if d[:8] != b"!<arch>\n":
        raise SystemExit(f"{path}: not an ar archive")
    i, names, out, seen = 8, b"", [], {}
    while i + 60 <= len(d):
        h = d[i:i + 60]
        name, size = h[:16].decode().strip(), int(h[48:58])
        body = d[i + 60:i + 60 + size]
        i += 60 + size + (size & 1)
        if name == "/":
            continue
        if name == "//":
            names = body
            continue
        if name.startswith("/") and name[1:].isdigit():
            o = int(name[1:])
            name = names[o:names.index(b"\n", o)].decode()
        name = name.rstrip("/")
        seen[name] = seen.get(name, 0) + 1
        out.append((name if seen[name] == 1 else f"{name}#{seen[name]}", body))
    return out


def to_ll(body, keep=None):
    """bitcode -> IR text"""
    tmp = Path(keep) if keep else Path(os.environ.get("TMPDIR", "/tmp")) / "sdk_mmio.bc"
    bc = tmp.with_suffix(".bc")
    bc.write_bytes(body)
    r = subprocess.run([LLVM_DIS, "-disable-auto-upgrade-debug-info", str(bc), "-o", "-"],
                       capture_output=True, text=True)
    if r.returncode:
        raise SystemExit(f"llvm-dis: {r.stderr.strip()}")
    if keep:
        tmp.with_suffix(".ll").write_text(r.stdout)
    bc.unlink()
    return r.stdout


# ---- registers from the SDK's headers -------------------------------------------------------

def registers(hdir):
    """{address: 'JL_X->FIELD'} from WL82.h / csfr.h"""
    regs = {}
    bases = {"psfr_base": 0x50000, "hsfr_base": 0x40000, "bsfr_base": 0x20000, "lsfr_base": 0x10000,
             "csfr_base": 0x1EE0000}
    for h in ("WL82.h", "csfr.h"):
        p = Path(hdir) / h
        if not p.exists():
            continue
        t = re.sub(r"//[^\n]*|/\*.*?\*/", "", p.read_text(errors="replace"), flags=re.S)
        types = {}
        for body, name in re.findall(r"typedef struct\s*\{(.*?)\}\s*(\w+)\s*;", t, re.S):
            fields = []
            for m in re.finditer(r"___(?:RW|RO|WO)\s+___[us](?:8|16|32)\((\w+)(?:\[(\d+)\])?\)", body):
                fields += [m.group(1)] if not m.group(2) else [f"{m.group(1)}[{k}]" for k in range(int(m.group(2)))]
            types[name] = fields
        defs = {n: (b, a) for n, b, a in re.findall(r"#define\s+(\w+_BASE)\s+\((\w+)\s*\+\s*map_adr\(([^)]*)\)\)", t)}
        for inst, ty, base in re.findall(r"#define\s+(\w+)\s+\(\((\w+)\s*\*\)\s*(\w+_BASE)\)", t):
            if base not in defs or ty not in types:
                continue
            b, a = defs[base]
            g, o = (int(x, 0) for x in a.split(","))
            at = bases.get(b, 0) + (64 * g + o) * 4
            for k, f in enumerate(types[ty]):
                regs[at + 4 * k] = f"{inst}->{f}"
    return regs


# ---- the IR -----------------------------------------------------------------------------------

def strip(line):
    return re.sub(r",\s*!(?:dbg|tbaa|srcloc|range|prof)\s*!\d+", "", line).strip()


SIZES = {"i1": 1, "i8": 1, "i16": 2, "i32": 4, "i64": 8, "float": 4, "double": 8, "ptr": 4}


class Module:
    def __init__(self, ll):
        self.ll = ll
        self.structs = {}                       # name -> [field type strings]
        for n, body in re.findall(r"^%(struct\.[\w.]+|union\.[\w.]+) = type \{(.*)\}$", ll, re.M):
            self.structs[n] = split_types(body)
        self.meta = dict(re.findall(r"^!(\d+) = (.*)$", ll, re.M))
        self.names = {}                         # struct name -> {llvm field index: member name}
        for n in self.structs:
            self.names[n] = self.field_names(n)

    def size_align(self, t):
        t = t.strip()
        if t in SIZES:
            return SIZES[t], min(SIZES[t], 4) if t != "i64" and t != "double" else 8
        m = re.match(r"\[(\d+) x (.*)\]$", t)
        if m:
            s, a = self.size_align(m.group(2))
            return int(m.group(1)) * s, a
        m = re.match(r"%([\w.]+)$", t)
        if m and m.group(1) in self.structs:
            off, al = 0, 1
            for f in self.structs[m.group(1)]:
                s, a = self.size_align(f)
                off = (off + a - 1) // a * a + s
                al = max(al, a)
            return (off + al - 1) // al * al, al
        if t.startswith("<{"):                  # packed
            return sum(self.size_align(f)[0] for f in split_types(t[2:-2])), 1
        return 4, 4

    def offsets(self, n):
        out, off = [], 0
        for f in self.structs[n]:
            s, a = self.size_align(f)
            off = (off + a - 1) // a * a
            out.append(off)
            off += s
        return out

    def field_names(self, n):
        """the debug info's members at each IR field's offset"""
        short = n.split(".", 1)[1].split(".")[0]
        comp = None
        for k, v in self.meta.items():
            if "DICompositeType(" in v and f'name: "{short}"' in v and "elements:" in v:
                comp = v
                break
        if not comp:
            return {}
        el = re.search(r"elements: !(\d+)", comp).group(1)
        ids = re.findall(r"!(\d+)", self.meta.get(el, ""))
        members = []
        for i in ids:
            v = self.meta.get(i, "")
            nm = re.search(r'name: "(\w+)"', v)
            off = re.search(r"offset: (\d+)", v)
            if nm:
                members.append((int(off.group(1)) if off else 0, nm.group(1)))
        offs = self.offsets(n)
        out = {}
        for k, o in enumerate(offs):
            end = offs[k + 1] if k + 1 < len(offs) else 1 << 30
            hit = [m for b, m in members if o * 8 <= b < end * 8]
            if hit:
                out[k] = hit[0] if len(hit) == 1 else "{" + ",".join(hit) + "}"
        return out

    def functions(self):
        for m in re.finditer(r"^define [^\n]*?@([\w.]+)\((.*?)\)[^\n]*\{\n(.*?)^\}", self.ll, re.S | re.M):
            yield m.group(1), m.group(2), m.group(3)


def split_types(s):
    out, depth, cur = [], 0, ""
    for ch in s:
        if ch in "{[<(":
            depth += 1
        elif ch in "}]>)":
            depth -= 1
        if ch == "," and depth == 0:
            out.append(cur.strip())
            cur = ""
        else:
            cur += ch
    if cur.strip():
        out.append(cur.strip())
    return out


BIN = {"or": "|", "and": "&", "xor": "^", "shl": "<<", "lshr": ">>", "ashr": ">>s", "add": "+", "sub": "-",
       "mul": "*", "udiv": "/u", "sdiv": "/", "urem": "%u", "srem": "%", "fadd": "+f", "fsub": "-f",
       "fmul": "*f", "fdiv": "/f"}
CMP = {"eq": "==", "ne": "!=", "ugt": ">u", "uge": ">=u", "ult": "<u", "ule": "<=u", "sgt": ">", "sge": ">=",
       "slt": "<", "sle": "<=", "oeq": "==", "olt": "<", "ogt": ">", "oge": ">=", "ole": "<=", "une": "!="}


class Fn:
    def __init__(self, mod, regs, name, args, body):
        self.mod, self.regs, self.name = mod, regs, name
        self.args = re.findall(r"%([\w.]+)\s*$", "") or []
        self.defs = {}
        self.lines = []
        for raw in body.split("\n"):
            line = strip(raw)
            if not line or line.startswith("#dbg") or "@llvm.dbg." in line:
                continue
            self.lines.append(line)
            m = re.match(r"%([\w.]+) = (.*)$", line)
            if m:
                self.defs[m.group(1)] = m.group(2)

    def reg(self, s):
        m = re.search(r"inttoptr \(i32 (\d+) to ptr\)", s)
        if not m:
            return None
        a = int(m.group(1))
        return self.regs.get(a, f"[{a:#x}]")

    def val(self, v, d):
        v = v.strip()
        v = re.sub(r"^(?:i\d+|float|double|ptr)\s+(?:(?:noundef|nonnull|zeroext|signext)\s+)*", "", v)
        if re.fullmatch(r"-?\d+", v):
            n = int(v)
            return str(n) if -10 <= n <= 10 else (hex(n) if n >= 0 else f"-{hex(-n)}")
        if re.fullmatch(r"-?[\d.]+e[+-]\d+|0x[0-9A-F]+", v):
            return v
        r = self.reg(v)
        if r:
            return r
        if v.startswith("@"):
            return v[1:]
        if not v.startswith("%"):
            return v
        name = v[1:]
        if name not in self.defs:
            return name                         # an argument
        if d <= 0:
            return f"%{name}"
        return self.expr(name, self.defs[name], d - 1)

    def addr(self, p, d):
        """a pointer as a place: a register, a struct field, a global"""
        p = p.strip()
        r = self.reg(p)
        if r:
            return r
        if p.startswith("@"):
            return p[1:]
        name = p.lstrip("%")
        dfn = self.defs.get(name)
        if dfn and dfn.startswith("getelementptr"):
            m = re.match(r"getelementptr (?:inbounds )?(%[\w.]+|\[[^\]]+\]|i\d+|ptr|float), ptr ([^,]+)((?:, i\d+ [^,]+)*)", dfn)
            if m:
                ty, base, idx = m.group(1), m.group(2), re.findall(r"i\d+ ([^,]+)", m.group(3))
                b = self.addr(base, d - 1) if d > 0 else base
                if ty.startswith("%") and ty[1:] in self.mod.structs and len(idx) >= 2:
                    nm = self.mod.names.get(ty[1:], {}).get(int(idx[1]), f"f{idx[1]}") if idx[1].isdigit() else idx[1]
                    rest = "".join(f"[{self.val(i, d - 1)}]" for i in idx[2:])
                    pre = "" if idx[0] == "0" else f"[{self.val(idx[0], d - 1)}]"
                    return f"{b}{pre}.{nm}{rest}"
                return f"{b}[{', '.join(self.val(i, d - 1) for i in idx)}]"
        if dfn and dfn.startswith(("bitcast", "phi", "select")):
            return self.expr(name, dfn, d - 1) if d > 0 else name
        return name if not dfn else f"*({self.val(p, d)})"

    def expr(self, name, dfn, d):
        m = re.match(r"(\w+) (?:(?:nuw|nsw|exact|disjoint|fast|nnan|ninf|contract|afn|reassoc|arcp) )*(?:i\d+|float|double) ([^,]+), (.+)$", dfn)
        if m and m.group(1) in BIN:
            return f"({self.val(m.group(2), d)} {BIN[m.group(1)]} {self.val(m.group(3), d)})"
        m = re.match(r"load (?:volatile )?([^,]+), ptr ([^,]+)", dfn)
        if m:
            return self.addr(m.group(2), d)
        m = re.match(r"(?:zext|sext|trunc|bitcast|ptrtoint|inttoptr|fptosi|fptoui|sitofp|uitofp|fpext|fptrunc) (?:nneg )?(.+?) to (.+)$", dfn)
        if m:
            op = dfn.split()[0]
            inner = self.val(m.group(1), d)
            return inner if op in ("zext", "sext", "bitcast", "ptrtoint", "inttoptr") else f"{op}<{m.group(2)}>({inner})"
        m = re.match(r"[if]cmp (\w+) (?:i\d+|float|ptr|double) ([^,]+), (.+)$", dfn)
        if m:
            return f"({self.val(m.group(2), d)} {CMP.get(m.group(1), m.group(1))} {self.val(m.group(3), d)})"
        m = re.match(r"select (?:i1 )?([^,]+), (?:\w+ )?([^,]+), (?:\w+ )?(.+)$", dfn)
        if m:
            return f"({self.val(m.group(1), d)} ? {self.val(m.group(2), d)} : {self.val(m.group(3), d)})"
        m = re.match(r"phi \w+ (.+)$", dfn)
        if m:
            arms = re.findall(r"\[ ([^,]+), %[\w.]+ \]", m.group(1))
            return "phi(" + " | ".join(self.val(a, d - 1) for a in arms) + ")"
        m = re.match(r"(?:tail )?call [^@%]*([@%][\w.]+)\((.*)\)$", dfn)
        if m:
            return f"{m.group(1)[1:]}({', '.join(self.val(a, d - 1) for a in split_types(m.group(2)))})"
        if dfn.startswith("getelementptr"):
            return "&" + self.addr("%" + name, d)
        if dfn.startswith("alloca"):
            return f"local {name}"
        return f"%{name}"

    def accesses(self, depth):
        out = []
        for line in self.lines:
            m = re.match(r"store volatile [^,]*? ([^,]+), ptr (inttoptr \(i32 \d+ to ptr\))", line)
            if m:
                out.append(f"W {self.reg(m.group(2))} = {self.val(m.group(1), depth)}")
                continue
            m = re.match(r"%([\w.]+) = load volatile [^,]+, ptr (inttoptr \(i32 \d+ to ptr\))", line)
            if m:
                users = [l for l in self.lines if re.search(rf"%{re.escape(m.group(1))}\b", l) and not l.startswith(f"%{m.group(1)} =")]
                use = ""
                for u in users[:2]:
                    um = re.match(r"%([\w.]+) = (.*)$", u)
                    if um:
                        use += " -> " + self.expr(um.group(1), um.group(2), 1)
                out.append(f"R {self.reg(m.group(2))}{use}")
            m = re.search(r"inttoptr \(i32 (\d+) to ptr\)", line)
            if m and "volatile" not in line and int(m.group(1)) in self.regs:
                out.append(f"? {line}")
        return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("lib")
    ap.add_argument("member", nargs="?")
    ap.add_argument("--fn", action="append", help="only these functions")
    ap.add_argument("--headers", default=str(ROOT / "reference/ac79sdk/h"))
    ap.add_argument("--depth", type=int, default=8)
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--ll", action="store_true", help="keep MEMBER.ll next to the library")
    ap.add_argument("--all", action="store_true", help="also the functions with no register access")
    a = ap.parse_args()
    members = ar_members(a.lib)
    if a.list or not a.member:
        for n, b in members:
            kind = "bitcode" if b[:4] == b"BC\xc0\xde" else "elf" if b[:4] == b"\x7fELF" else "?"
            print(f"{n:40s} {len(b):8d} {kind}")
        return
    body = dict(members).get(a.member)
    if body is None:
        raise SystemExit(f"{a.member}: not in {a.lib}")
    keep = Path(a.lib).parent / a.member.replace(".c.o", "") if a.ll else None
    mod = Module(to_ll(body, keep))
    regs = registers(a.headers)
    for name, args, fbody in mod.functions():
        if a.fn and name not in a.fn:
            continue
        f = Fn(mod, regs, name, args, fbody)
        acc = f.accesses(a.depth)
        if acc or a.all:
            print(f"\n{name}({args.strip()[:120]})")
            for x in acc:
                print("  " + x)


if __name__ == "__main__":
    main()
