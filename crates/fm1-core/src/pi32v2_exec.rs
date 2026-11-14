//! The forms of pi32v2-isa's table (fm1-isa's `pi32v2_table`) that fm1-isa's corpus classes do not have:
//! the DSP lane operations, the memory multiply-accumulates, the flag branches and others. Their
//! semantics are pi32v2-isa's hardware models (tools/hwtest/semantics/*.py, checked against an FM-1's
//! results), one arm each.
//!
//! A word the table decodes as one of these runs here (`decode`, `Cpu::execute`'s ROUTE_PI); every
//! other word keeps fm1-isa's classes. As in the models, every input is read before any result is
//! written (`R`, the registers before the instruction), and the writes are recorded (`Cpu::pi_wrote`)
//! for a dual-issue pair's rule.

use crate::{Bus, CoreError, Cpu, F_C, F_N, F_V, F_Z};
use fm1_isa::pi32v2_table::{self as pt, id, Form, FORMS};
use std::sync::OnceLock;

/// A form's operands, read from its window. The arms name the fields as the base form does; a pair
/// head's twin may name them otherwise (`{d}` for `{r}`): `field_as` finds the one in the same place of
/// the syntax.
pub(crate) struct Ops {
    pub win: u64,
    pub form: &'static Form,
    pub base: &'static Form,
}

impl Ops {
    fn f(&self, name: &str) -> &'static pt::Field {
        field_as(self.form, self.base, name).unwrap_or_else(|| panic!("{}: no field {name}", self.form.id))
    }
    /// A register field's register (a pair's even one).
    fn r(&self, name: &str) -> usize {
        (self.f(name).value(self.win) & 15) as usize
    }
    /// A field's value (`Field::value`).
    fn v(&self, name: &str) -> i64 {
        self.f(name).value(self.win)
    }
    /// A field's raw bits.
    fn raw(&self, name: &str) -> u32 {
        self.f(name).raw(self.win)
    }
    /// An enum field's text.
    fn t(&self, name: &str) -> &'static str {
        self.f(name).text(self.win)
    }
}

/// `form`'s field for `base`'s field `name`: the same name, or the one in the same place of the syntax.
fn field_as(form: &'static Form, base: &'static Form, name: &str) -> Option<&'static pt::Field> {
    form.field(name).or_else(|| {
        let k = pt::placeholders(base.syntax).iter().position(|&p| p == name)?;
        form.field(pt::placeholders(form.syntax).get(k)?)
    })
}

/// What an arm does besides writing registers.
pub(crate) enum Done {
    /// on to the next instruction
    Next,
    /// a jump
    Jump(u32),
}

/// The register state a form runs on: the inputs as they were (`r`), the results (`cpu`).
struct X<'a> {
    cpu: &'a mut Cpu,
    r: [u32; 16],
    f: u32,
}

impl X<'_> {
    /// Write register n, recording it.
    fn w(&mut self, n: usize, v: u32) {
        self.cpu.regs[n & 15] = v;
        self.cpu.pi_wrote[n & 15] = u32::MAX;
    }
    /// Write a pair (n even): the low word to n, the high one to n + 1.
    fn wp(&mut self, n: usize, v: u64) {
        self.w(n, v as u32);
        self.w(n + 1, (v >> 32) as u32);
    }
    /// Write the half `part` (".l" or ".h") of register n.
    fn wh(&mut self, n: usize, part: &str, v: u32) {
        let sh = if part == ".h" { 16 } else { 0 };
        let m = 0xffffu32 << sh;
        let n = n & 15;
        self.cpu.regs[n] = (self.cpu.regs[n] & !m) | ((v & 0xffff) << sh);
        self.cpu.pi_wrote[n] |= m;
    }
    /// The pair at n (even), as it was.
    fn pair(&self, n: usize) -> u64 {
        self.r[n & 15] as u64 | (self.r[(n + 1) & 15] as u64) << 32
    }
    fn flags(&mut self, f: u32) {
        self.cpu.flags = f & 0xf;
    }
}

// ---- helpers (the models' own: q.py, r.py, _r_util.py) -----------------------------------------------

/// x's low `bits` bits as a signed number.
fn sx(x: u64, bits: u32) -> i64 {
    if bits >= 64 {
        return x as i64;
    }
    let x = x & mask(bits);
    if bits > 0 && x >> (bits - 1) & 1 != 0 { x as i64 - (1i64 << bits) } else { x as i64 }
}

fn mask(bits: u32) -> u64 {
    if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 }
}

/// x clamped to the signed or unsigned range of `bits` bits.
fn clip(x: i64, bits: u32, signed: bool) -> i64 {
    if signed { x.clamp(-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1) } else { x.clamp(0, (1i64 << bits) - 1) }
}

/// The `bits`-bit lanes of x, lowest first.
fn lanes(x: u64, bits: u32, total: u32) -> Vec<u64> {
    (0..total / bits).map(|k| (x >> (k * bits)) & mask(bits)).collect()
}

fn join(xs: &[i64], bits: u32) -> u64 {
    xs.iter().enumerate().fold(0, |a, (k, &x)| a | ((x as u64) & mask(bits)) << (k as u32 * bits))
}

/// N and Z of a `bits`-bit result over the old flags' C and V.
fn nz(f: u32, x: u64, bits: u32) -> u32 {
    let x = x & mask(bits);
    (f & (F_C | F_V)) | if x == 0 { F_Z } else { 0 } | if x >> (bits - 1) & 1 != 0 { F_N } else { 0 }
}

fn usat(v: i64, bits: u32) -> i64 {
    v.clamp(0, (1i64 << bits) - 1)
}

fn ssat(v: i64, bits: u32) -> i64 {
    v.clamp(-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1)
}

fn sat(v: i64, signed: bool, bits: u32) -> u64 {
    (if signed { ssat(v, bits) } else { usat(v, bits) }) as u64 & mask(bits)
}

/// The half of v (`.h` or `.l`).
fn half(v: u32, high: bool) -> u32 {
    if high { v >> 16 } else { v & 0xffff }
}

/// One lane of an add / subtract with a mode such as `(ssat,x2)`: a and b raw `bits`-bit lanes; the raw
/// lane result (r.py `lane`).
fn lane(a: u64, b: u64, sign: i64, mode: &str, bits: u32) -> u64 {
    let parts: Vec<&str> = mode.trim_start_matches('(').trim_end_matches(')').split(',').collect();
    let rnd = parts.contains(&"rnd");
    let x2 = parts.contains(&"x2");
    let kind = parts.iter().copied().find(|p| *p != "rnd" && *p != "x2").unwrap_or("");
    let signed = matches!(kind, "ssat" | "savg");
    let (a, b) = if signed { (sx(a, bits), sx(b, bits)) } else { (a as i64, b as i64) };
    let mut t = a + sign * b;
    if matches!(kind, "usat" | "ssat") {
        if x2 {
            t *= 2;
            // a doubled difference that overflows gives 0, not the maximum (the unit)
            if sign < 0 && !signed && t >= 1i64 << bits {
                t = 0;
            }
        }
        t = if signed { ssat(t, bits) } else { usat(t, bits) };
    } else {
        t = (t + rnd as i64) >> 1;
    }
    t as u64 & mask(bits)
}

/// A raw `bits`-bit lane shifted by n: lsl, lsr, asr, shl3 (r.py `shift_lane`).
fn shift_lane(kind: &str, v: u64, n: u32, bits: u32) -> u64 {
    let m = mask(bits);
    match kind {
        "lsl" => if n < bits { (v << n) & m } else { 0 },
        "lsr" => if n < bits { v >> n } else { 0 },
        "asr" => (sx(v, bits) >> n.min(bits)) as u64 & m,
        _ => (((sx(v, bits) as i128) << n.min(64)).clamp(-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)) as u64 & m,
    }
}

/// The four bytes of v, lowest first.
fn bytes4(v: u32) -> [i64; 4] {
    [0, 8, 16, 24].map(|k| ((v >> k) & 0xff) as i64)
}

/// A pair's four 16-bit lanes.
fn lanes16(lo: u32, hi: u32) -> [i64; 4] {
    [(lo & 0xffff) as i64, (lo >> 16) as i64, (hi & 0xffff) as i64, (hi >> 16) as i64]
}

/// Four 16-bit lanes into the pair at d.
fn pack16(x: &mut X, ls: [i64; 4], d: usize) {
    let l = ls.map(|v| (v as u64 & 0xffff) as u32);
    x.w(d, l[0] | l[1] << 16);
    x.w(d + 1, l[2] | l[3] << 16);
}

fn sb(v: i64) -> i64 {
    if v & 0x80 != 0 { v - 256 } else { v }
}

/// (signed, doubled) of a multiply mode.
fn mode_parts(mode: &str) -> (bool, bool) {
    (mode.contains("ssat"), mode.contains("x2"))
}

/// The +1 / -1 signs of `+|-` text.
fn signs(text: &str) -> Vec<i64> {
    text.split('|').map(|c| if c == "+" { 1 } else { -1 }).collect()
}

/// A 16 x 16 product as a mode says: signed for ssat, doubled for x2 (r.py `_mulsat`).
fn mulsat(a: u32, b: u32, mode: &str) -> (i64, bool) {
    let (sg, x2) = mode_parts(mode);
    let (a, b) = if sg { (sx(a as u64, 16), sx(b as u64, 16)) } else { (a as i64, b as i64) };
    (a * b * if x2 { 2 } else { 1 }, sg)
}

/// The (high, low) products of a `*|*` form (r.py `_products`).
fn products(x: &X, o: &Ops, mode: &str, imm: bool) -> ([i64; 2], bool) {
    let (sg, x2) = mode_parts(mode);
    let s = x.r[o.r("s")];
    let mut out = [0i64; 2];
    for (k, (sel_s, sel_t)) in [("n", "p"), ("m", "q")].into_iter().enumerate() {
        let a = half(s, o.t(sel_s) == ".h") as u64;
        let a = if sg { sx(a, 16) } else { a as i64 };
        let b = if imm {
            o.v("p")
        } else {
            let b = half(x.r[o.r("t")], o.t(sel_t) == ".h") as u64;
            if sg { sx(b, 16) } else { b as i64 }
        };
        out[k] = a * b * if x2 { 2 } else { 1 };
    }
    (out, sg)
}

/// mul2 / madd.mul2 / msub.mul2 and their kin (r.py `_mk_mul2`): acc 0, 1 or -1.
fn mul2(x: &mut X, o: &Ops, pair: bool, imm: bool, acc: i64, mode: Option<&str>) {
    let m = mode.unwrap_or_else(|| o.t("i"));
    let ([hi, lo], sg) = products(x, o, m, imm);
    let d = o.r("r");
    if pair {
        for (reg, p) in [(d + 1, hi), (d, lo)] {
            let base = if sg { x.r[reg] as i32 as i64 } else { x.r[reg] as i64 };
            let v = if acc != 0 { sat(base + acc * p, sg, 32) } else { sat(p, sg, 32) };
            x.w(reg, v as u32);
        }
    } else {
        let mut parts = [0u64; 2];
        for (k, (shift, p)) in [(16, hi), (0, lo)].into_iter().enumerate() {
            // (the accumulator lane read unsigned, as in the halfword accumulators)
            let lane_acc = (x.r[d] >> shift & 0xffff) as i64;
            parts[k] = if acc != 0 { sat(lane_acc + acc * p, sg, 16) } else { sat(p, sg, 16) };
        }
        x.w(d, (parts[0] << 16 | parts[1]) as u32);
    }
}

/// The special registers by number (pi32v2-isa's `sr` fields and masks).
const SR: [&str; 16] = ["reti", "rete", "retx", "rets", "sr4", "psr", "cnum", "uspn", "sspn", "icfgn", "icfgs", "icfg",
    "usp", "ssp", "sp", "pc"];
const SR_RETI: u32 = 0;
const SR_PSR: u32 = 5;

/// Special register n as an instruction reads it: the core's own (`sreg_get_named`); pc the instruction's
/// address; uspn, icfgn and icfgs read 0 on an FM-1 (pi32v2-isa k2.py), which fm1-emu does not otherwise keep.
fn sr_value(cpu: &Cpu, n: usize) -> u32 {
    match SR[n & 15] {
        "pc" => cpu.pc,
        "uspn" | "icfgn" | "icfgs" => 0,
        name => crate::sreg_get_named(cpu, name),
    }
}

/// Special register n written as an instruction writes it (v4.py): the return registers keep bits 26..1,
/// psr its flags; icfg, usp, ssp and sp through the core's own setters. sr4, cnum, the n / s banks and
/// pc keep nothing (they read 0 after a write; a write to pc fell through).
fn sr_write(cpu: &mut Cpu, n: usize, v: u32) {
    match SR[n & 15] {
        name @ ("reti" | "rete" | "retx" | "rets") => cpu.set_sreg_named(name, v & crate::RETS_MASK),
        "psr" => cpu.set_sreg_named("psr", v & 0xf),
        name @ ("icfg" | "usp" | "ssp" | "sp") => cpu.set_sreg_named(name, v),
        _ => {}
    }
}

/// Push words, the first at the lowest address (sp ends at it).
fn push_words<B: Bus>(cpu: &mut Cpu, bus: &mut B, vals: &[u32]) {
    let sp = cpu.sp.wrapping_sub(4 * vals.len() as u32);
    for (k, &v) in vals.iter().enumerate() {
        bus.write32(sp.wrapping_add(4 * k as u32), v);
    }
    cpu.sp = sp;
}

/// The `bits`-bit value (carry, x) rotated right by c: (the low bits - 1, the new carry). p.py `_rorc` /
/// `shift64`'s rorc.
fn rorc(x: u64, cin: u64, c: u32, bits: u32) -> (u64, u64) {
    let m: u128 = (1u128 << bits) - 1;
    let v: u128 = (cin as u128) << (bits - 1) | x as u128;
    let k = (bits - c % bits) % bits;
    let v = if k == 0 { v } else { ((v << k) | (v >> (bits - k))) & m };
    ((v & ((1u128 << (bits - 1)) - 1)) as u64, (v >> (bits - 1)) as u64 & 1)
}

/// A finite float's bits rounded to binary16, nearest even (s.py `encode` at 5 / 10 bits): past the range
/// the infinity of its sign.
fn f32_to_f16(b: u32) -> u16 {
    let sign = ((b >> 16) & 0x8000) as u16;
    let (exp, man) = ((b >> 23) & 0xff, b & 0x7f_ffff);
    if exp == 0 && man == 0 {
        return sign;
    }
    // the value is m * 2^e
    let (m, e) = if exp == 0 { (man as u64, -149i32) } else { ((man | 0x80_0000) as u64, exp as i32 - 150) };
    let exponent = 63 - m.leading_zeros() as i32 + e; // floor(log2)
    let shift = exponent.max(-14) - 10;
    let mut n = if e >= shift {
        m << (e - shift)
    } else {
        // (m < 2^24: a shift of 64 or more leaves less than half the last place, 0)
        let k = (shift - e) as u32;
        if k >= 64 {
            0
        } else {
            let (q, rem, half) = (m >> k, m & ((1u64 << k) - 1), 1u64 << (k - 1));
            if rem > half || (rem == half && q & 1 != 0) { q + 1 } else { q }
        }
    };
    let mut ex = exponent;
    if n >= 1 << 11 {
        n >>= 1;
        ex += 1;
    }
    let expbits = if n >= 1 << 10 { (ex + 15).max(1) } else { 0 };
    if expbits >= 31 {
        return sign | 0x7c00;
    }
    sign | (expbits as u16) << 10 | (n & 0x3ff) as u16
}

/// A finite binary16's bits as a float's (exact).
fn f16_to_f32(h: u16) -> u32 {
    let sign = ((h >> 15) as u32) << 31;
    let (exp, man) = (((h >> 10) & 0x1f) as u32, (h & 0x3ff) as u32);
    if exp == 0 {
        // (a subnormal: man * 2^-24, exact in a float)
        sign | (man as f32 * f32::powi(2.0, -24)).to_bits()
    } else {
        sign | (exp + 112) << 23 | man << 13
    }
}

/// 0, 1 or -1 for a mul / madd / msub id.
fn acc_of(id: &str) -> i64 {
    if id.starts_with("madd") { 1 } else if id.starts_with("msub") { -1 } else { 0 }
}

/// The two-lane add / subtract mode of an id suffix (r.py `_mode_of`).
fn mode_of(suffix: &str) -> String {
    let parts: Vec<&str> = suffix.split('.').collect();
    if parts.last() == Some(&"x2") {
        format!("({},x2)", parts[parts.len() - 2])
    } else if parts[0] == "rnd" {
        format!("(rnd,{})", parts[1])
    } else {
        format!("({})", parts[0])
    }
}

/// One form's semantics; None: not one fm1-core runs.
fn exec<B: Bus>(cpu: &mut Cpu, bus: &mut B, base: usize, o: &Ops, next_pc: u32) -> Option<Result<Done, CoreError>> {
    let mut x = X { r: cpu.regs, f: cpu.flags, cpu };
    let fid = FORMS[base].id;
    match base {
        // ==== r.py: DSP arithmetic and multiplies ====
        // halfword add / subtract
        id::ADD_RHRHI | id::SUB_RHRHI | id::ADD_RHRHRH | id::SUB_RHRHRH => {
            let sign = if fid.starts_with("add") { 1 } else { -1 };
            let a = half(x.r[o.r("s")], o.t("m") == ".h") as i64;
            let b = if fid.ends_with("rhrhi") { o.v("p") } else { half(x.r[o.r("t")], o.t("p") == ".h") as i64 };
            let d = o.r("r");
            x.wh(d, o.t("n"), (a + sign * b) as u64 as u32);
        }
        id::ADD_RHRHRHE | id::SUB_RHRHRHE => {
            let sign = if fid.starts_with("add") { 1 } else { -1 };
            let a = half(x.r[o.r("s")], o.t("m") == ".h") as u64;
            let b = half(x.r[o.r("t")], o.t("p") == ".h") as u64;
            let v = lane(a, b, sign, o.t("q"), 16);
            let d = o.r("r");
            x.wh(d, o.t("n"), v as u32);
        }
        id::ADD_RRRE | id::SUB_RRRE => {
            let sign = if fid.starts_with("add") { 1 } else { -1 };
            let (a, b) = (x.r[o.r("s")], x.r[o.r("t")]);
            let signed = o.t("n") == "(ssat)";
            let (a, b) = if signed { (a as i32 as i64, b as i32 as i64) } else { (a as i64, b as i64) };
            let t = a + sign * b;
            let t = if signed { ssat(t, 32) } else { usat(t, 32) };
            x.w(o.r("r"), t as u32);
        }
        // halfword shifts, by an immediate or a register's low 8 bits
        id::LSL_RHRHI | id::LSR_RHRHI | id::ASR_RHRHI | id::SHL3_RHRHI
        | id::LSL_RHRHR | id::LSR_RHRHR | id::ASR_RHRHR | id::SHL3_RHRHR => {
            let kind = fid.split('.').next().unwrap_or("");
            let a = half(x.r[o.r("s")], o.t("m") == ".h") as u64;
            let n = if fid.ends_with('i') { o.v("p") as u32 } else { x.r[o.r("t")] & 0xff };
            let v = shift_lane(kind, a, n, 16);
            let d = o.r("r");
            x.wh(d, o.t("n"), v as u32);
        }
        // both halves shifted
        id::LSL_V_RRI | id::LSR_V_RRI | id::ASR_V_RRI | id::SHL3_V_RRI
        | id::LSL_V_RRR | id::LSR_V_RRR | id::ASR_V_RRR | id::SHL3_V_RRR => {
            let kind = fid.split('.').next().unwrap_or("");
            let n = if fid.ends_with("rri") { o.v("n") as u32 } else { x.r[o.r("t")] & 0xff };
            let a = x.r[o.r("s")] as u64;
            let v = shift_lane(kind, a & 0xffff, n, 16) | shift_lane(kind, a >> 16, n, 16) << 16;
            x.w(o.r("r"), v as u32);
        }
        // four byte lanes
        id::ADD4_PPR => {
            let s = o.r("s");
            let acc = lanes16(x.r[s], x.r[s + 1]);
            let b = bytes4(x.r[o.r("t")]);
            pack16(&mut x, [0, 1, 2, 3].map(|k| acc[k] + b[k]), o.r("r"));
        }
        id::ADD4_PRR => {
            let sign = if o.t("n").starts_with('+') { 1 } else { -1 };
            let (a, b) = (bytes4(x.r[o.r("s")]), bytes4(x.r[o.r("t")]));
            pack16(&mut x, [0, 1, 2, 3].map(|k| a[k] + sign * b[k]), o.r("r"));
        }
        // (`sum.acc.*.rrr`: instruction revision 3 forms this chip runs as add4 (rnd, uavg))
        id::ADD4_RRR_RND_UAVG | id::SUM_ACC_USAT_RRR | id::SUM_ACC_SSAT_RRR | id::SUM_ACC_MSAT_RRR => {
            let (a, b) = (bytes4(x.r[o.r("s")]), bytes4(x.r[o.r("t")]));
            let v = (0..4).fold(0u32, |v, k| v | (((a[k] + b[k] + 1) >> 1) as u32) << (8 * k));
            x.w(o.r("r"), v);
        }
        // two lanes of halves with a mode
        _ if fid.contains(".rrhrhrhrh.") && ["addadd", "addsub", "subadd", "subsub"].iter().any(|p| fid.starts_with(p)) => {
            let (op, suffix) = fid.split_once(".rrhrhrhrh.").unwrap();
            let sg = match op { "addadd" => (1, 1), "addsub" => (1, -1), "subadd" => (-1, 1), _ => (-1, -1) };
            let mode = mode_of(suffix);
            let (s, t) = (x.r[o.r("s")], x.r[o.r("t")]);
            let hi = lane(half(s, o.t("n") == ".h") as u64, half(t, o.t("p") == ".h") as u64, sg.0, &mode, 16);
            let lo = lane(half(s, o.t("m") == ".h") as u64, half(t, o.t("q") == ".h") as u64, sg.1, &mode, 16);
            x.w(o.r("r"), (hi << 16 | lo) as u32);
        }
        id::ADDSUB_RHRHI_SAT => {
            let sign = if o.t("o") == "+" { 1 } else { -1 };
            let v = lane(half(x.r[o.r("s")], o.t("m") == ".h") as u64, o.v("p") as u64, sign, o.t("q"), 16);
            let d = o.r("r");
            x.wh(d, o.t("n"), v as u32);
        }
        id::ADDSUB_RHRHI_MODE => {
            let sg = signs(o.t("p"));
            let s = x.r[o.r("s")];
            let q = o.v("q") as u64;
            let hi = lane(half(s, o.t("n") == ".h") as u64, q, sg[0], o.t("i"), 16);
            let lo = lane(half(s, o.t("m") == ".h") as u64, q, sg[1], o.t("i"), 16);
            x.w(o.r("r"), (hi << 16 | lo) as u32);
        }
        id::ADDSUB_PQQ_SAT => {
            let sg = signs(o.t("n"));
            let (sr, mr) = (o.raw("s"), o.raw("m"));
            let regs = |r: u32| (2 * (r >> 2) + (r >> 1 & 1), 2 * (r >> 2) + (r & 1));
            let ((a0, a1), (b0, b1)) = (regs(sr), regs(mr));
            let hi = lane(x.r[a0 as usize] as u64, x.r[b0 as usize] as u64, sg[0], o.t("p"), 32);
            let lo = lane(x.r[a1 as usize] as u64, x.r[b1 as usize] as u64, sg[1], o.t("p"), 32);
            let d = o.r("r");
            x.w(d + 1, hi as u32);
            x.w(d, lo as u32);
        }
        id::ADDSUB_PPP_MODE => {
            let mut sg = signs(o.t("n"));
            sg.reverse(); // (printed from the top lane down)
            let (s, t) = (o.r("s"), o.r("t"));
            let (a, b) = (lanes16(x.r[s], x.r[s + 1]), lanes16(x.r[t], x.r[t + 1]));
            let m = o.t("m");
            let v = [0, 1, 2, 3].map(|k| lane(a[k] as u64, b[k] as u64, sg[k], m, 16) as i64);
            pack16(&mut x, v, o.r("r"));
        }
        // 16 x 16 multiplies
        id::MUL_RHRHRHE => {
            let (p, sg) = mulsat(half(x.r[o.r("s")], o.t("m") == ".h"), half(x.r[o.r("t")], o.t("p") == ".h"), o.t("q"));
            let d = o.r("r");
            x.wh(d, o.t("n"), sat(p, sg, 16) as u32);
        }
        id::MUL_RRHRHE => {
            let (p, sg) = mulsat(half(x.r[o.r("s")], o.t("n") == ".h"), half(x.r[o.r("t")], o.t("m") == ".h"), o.t("p"));
            x.w(o.r("r"), sat(p, sg, 32) as u32);
        }
        id::MADD_RHRHRHE | id::MSUB_RHRHRHE => {
            let sign = if fid.starts_with("madd") { 1 } else { -1 };
            let (p, sg) = mulsat(half(x.r[o.r("s")], o.t("m") == ".h"), half(x.r[o.r("t")], o.t("p") == ".h"), o.t("q"));
            let d = o.r("r");
            // (the accumulator lane is read as unsigned even for ssat)
            let a = half(x.r[d], o.t("n") == ".h") as i64;
            x.wh(d, o.t("n"), sat(a + sign * p, sg, 16) as u32);
        }
        id::MADD_RRHRHE | id::MSUB_RRHRHE => {
            let sign = if fid.starts_with("madd") { 1 } else { -1 };
            let (p, sg) = mulsat(half(x.r[o.r("s")], o.t("n") == ".h"), half(x.r[o.r("t")], o.t("m") == ".h"), o.t("p"));
            let d = o.r("r");
            let a = if sg { x.r[d] as i32 as i64 } else { x.r[d] as i64 };
            x.w(d, sat(a + sign * p, sg, 32) as u32);
        }
        id::MUL_RRHI_SSAT | id::MADD_RRHI_SSAT | id::MSUB_RRHI_SSAT | id::MUL_RRHI_USAT | id::MADD_RRHI_USAT | id::MSUB_RRHI_USAT => {
            let acc = if fid.starts_with("madd") { 1 } else if fid.starts_with("msub") { -1 } else { 0 };
            let signed = fid.ends_with("ssat");
            let a = half(x.r[o.r("s")], o.t("n") == ".h") as u64;
            let a = if signed { sx(a, 16) } else { a as i64 };
            let mut p = a * o.v("m");
            let d = o.r("r");
            if acc != 0 {
                let base = if signed { x.r[d] as i32 as i64 } else { x.r[d] as i64 };
                p = base + acc * p;
            }
            x.w(d, sat(p, signed, 32) as u32);
        }
        // two parallel 16 x 16 multiplies
        id::MUL2_RRHRHRHRHE | id::MADD_MUL2_RRHRHRHRHE | id::MSUB_MUL2_RRHRHRHRHE => {
            mul2(&mut x, o, false, false, acc_of(fid), None);
        }
        id::MUL2_PRHRHRHRHE | id::MADD_MUL2_PRHRHRHRHE | id::MSUB_MUL2_PRHRHRHRHE
        | id::MUL2_PRHRHRHRHE_B12 | id::MADD_MUL2_PRHRHRHRHE_B12 | id::MSUB_MUL2_PRHRHRHRHE_B12 => {
            mul2(&mut x, o, true, false, acc_of(fid), None);
        }
        id::MUL2_PRHRHI_SSAT | id::MADD_MUL2_PRHRHI_SSAT | id::MSUB_MUL2_PRHRHI_SSAT => {
            mul2(&mut x, o, true, true, acc_of(fid), Some("(ssat)"));
        }
        id::MUL2_PRHRHI_USAT | id::MADD_MUL2_PRHRHI_USAT | id::MSUB_MUL2_PRHRHI_USAT => {
            mul2(&mut x, o, true, true, acc_of(fid), Some("(usat)"));
        }
        // sum(... *|* ...) into a pair: this chip runs them as the mul2 forms, (usat) for (u), (ssat) for (s)
        id::SUM_S_PRHRHRHRH | id::SUM_ACC_S_PRHRHRHRH | id::SUM_NACC_S_PRHRHRHRH => {
            let acc = if fid.starts_with("sum.acc") { 1 } else if fid.starts_with("sum.nacc") { -1 } else { 0 };
            mul2(&mut x, o, true, false, acc, Some("(ssat)"));
        }
        id::SUM_U_PRHRHRHRH | id::SUM_ACC_U_PRHRHRHRH | id::SUM_NACC_U_PRHRHRHRH => {
            let acc = if fid.starts_with("sum.acc") { 1 } else if fid.starts_with("sum.nacc") { -1 } else { 0 };
            mul2(&mut x, o, true, false, acc, Some("(usat)"));
        }
        // four byte lanes multiplied
        id::MUL4_U_PRR | id::MUL4_M_PRR => {
            let (a, b) = (bytes4(x.r[o.r("s")]), bytes4(x.r[o.r("t")]));
            let m = base == id::MUL4_M_PRR;
            pack16(&mut x, [0, 1, 2, 3].map(|k| a[k] * if m { sb(b[k]) } else { b[k] }), o.r("r"));
        }
        id::MUL4_M_PRI => {
            let a = bytes4(x.r[o.r("s")]);
            let n = o.v("n");
            pack16(&mut x, a.map(|v| v * n), o.r("r"));
        }
        id::MADD_MUL4_U_PRR | id::MADD_MUL4_M_PRR => {
            let d = o.r("r");
            let acc = lanes16(x.r[d], x.r[d + 1]);
            let (a, b) = (bytes4(x.r[o.r("s")]), bytes4(x.r[o.r("t")]));
            let m = base == id::MADD_MUL4_M_PRR;
            pack16(&mut x, [0, 1, 2, 3].map(|k| acc[k] + a[k] * if m { sb(b[k]) } else { b[k] }), d);
        }
        // ==== q.py: lane compares, min / max, saturation, packing, shifts by a signed count ====
        id::ABS_RHRH | id::NEGS_RHRH => {
            let v = sx(half(x.r[o.r("s")], o.t("m") == ".h") as u64, 16);
            let y = clip(if base == id::ABS_RHRH { v.abs() } else { -v }, 16, true);
            x.wh(o.r("r"), o.t("n"), y as u32);
        }
        id::ABS_V_RR | id::NEGS_V_RR | id::NEGS_RR => {
            let bits = if base == id::NEGS_RR { 32 } else { 16 };
            let abs = base == id::ABS_V_RR;
            let ys: Vec<i64> = lanes(x.r[o.r("s")] as u64, bits, 32).into_iter()
                .map(|v| { let v = sx(v, bits); clip(if abs { v.abs() } else { -v }, bits, true) }).collect();
            x.w(o.r("r"), join(&ys, bits) as u32);
        }
        id::UMAX_RHRHRH | id::SMAX_RHRHRH | id::UMIN_RHRHRH | id::SMIN_RHRHRH
        | id::UMAX_V_RRR | id::SMAX_V_RRR | id::UMIN_V_RRR | id::SMIN_V_RRR => {
            let h = fid.contains("rhrhrh");
            let (a, b) = if h {
                (half(x.r[o.r("s")], o.t("m") == ".h") as u64, half(x.r[o.r("t")], o.t("p") == ".h") as u64)
            } else {
                (x.r[o.r("s")] as u64, x.r[o.r("t")] as u64)
            };
            let (signed, max) = (fid.starts_with('s'), fid[1..].starts_with("max"));
            let tot = if h { 16 } else { 32 };
            let ys: Vec<i64> = lanes(a, 16, tot).into_iter().zip(lanes(b, 16, tot))
                .map(|(p, q)| {
                    let (p, q) = if signed { (sx(p, 16), sx(q, 16)) } else { (p as i64, q as i64) };
                    if max { p.max(q) } else { p.min(q) }
                }).collect();
            let y = join(&ys, 16) as u32;
            if h { x.wh(o.r("r"), o.t("n"), y) } else { x.w(o.r("r"), y) }
        }
        _ if fid.ends_with(".v") && ["all.", "alls.", "any.", "anys.", "cmp.", "cmps."].iter().any(|p| fid.starts_with(p))
            && !fid.contains("unknown") => {
            let mut it = fid.split('.');
            let (family, cond, shape) = (it.next().unwrap(), it.next().unwrap(), it.next().unwrap());
            let pairs = shape == "ppp";
            let (bits, total) = if pairs { (16, 64) } else { (8, 32) };
            let (a, b) = if pairs { (x.pair(o.r("s")), x.pair(o.r("t"))) } else { (x.r[o.r("s")] as u64, x.r[o.r("t")] as u64) };
            let signed = family.ends_with('s');
            let m: Vec<bool> = lanes(a, bits, total).into_iter().zip(lanes(b, bits, total))
                .map(|(p, q)| {
                    let (p, q) = if signed { (sx(p, bits), sx(q, bits)) } else { (p as i64, q as i64) };
                    match cond { "eq" => p == q, "ne" => p != q, "ge" => p >= q, "gt" => p > q, "le" => p <= q, _ => p < q }
                }).collect();
            let y = if family.starts_with("all") {
                if m.iter().all(|&v| v) { mask(total) } else { 0 }
            } else if family.starts_with("any") {
                if m.iter().any(|&v| v) { mask(total) } else { 0 }
            } else {
                join(&m.iter().map(|&v| -(v as i64)).collect::<Vec<_>>(), bits)
            };
            if pairs { x.wp(o.r("r"), y) } else { x.w(o.r("r"), y as u32) }
        }
        id::SAT8_U_RHRH | id::SAT8_S_RHRH => {
            let signed = base == id::SAT8_S_RHRH;
            let v = half(x.r[o.r("s")], o.t("m") == ".h") as u64;
            let v = if signed { sx(v, 16) } else { v as i64 };
            let y = clip(v, 8, signed);
            x.wh(o.r("r"), o.t("n"), y as u32);
            let f = nz(x.f, y as u64, 16);
            x.flags(f);
        }
        // (revision-3 spellings this chip runs as sat16)
        id::SAT8_U_RR | id::SAT8_S_RR => {
            let signed = base == id::SAT8_S_RR;
            let v = x.r[o.r("s")];
            let v = if signed { v as i32 as i64 } else { v as i64 };
            let y = clip(v, 16, signed);
            x.w(o.r("r"), y as u32);
            let f = nz(x.f, y as u64, 32);
            x.flags(f);
        }
        id::VSAT16_U_RP | id::VSAT16_S_RP | id::PACKB_RP | id::PACKB_RPE | id::PACKB_RP_STOU => {
            let src = if fid.starts_with("vsat16") { "p" } else { "s" };
            let mode = match base {
                id::VSAT16_U_RP => "(usat)",
                id::VSAT16_S_RP => "(ssat)",
                id::PACKB_RP => "truncate",
                id::PACKB_RP_STOU => "stou",
                _ => o.t("n"),
            };
            let xs: Vec<i64> = lanes(x.pair(o.r(src)), 16, 64).into_iter().map(|v| match mode {
                "(usat)" => clip(v as i64, 8, false),
                "(ssat)" => clip(sx(v, 16), 8, true),
                "stou" => clip(sx(v, 16), 8, false),
                _ => v as i64,
            }).collect();
            x.w(o.r("r"), join(&xs, 8) as u32);
        }
        id::UNPACKB_U_PR | id::UNPACKB_S_PR | id::UNPACK4_U_PR | id::UNPACK4_S_PR => {
            let signed = fid.contains(".s.");
            let dest = if fid.starts_with("unpack4") { "p" } else { "r" };
            let xs: Vec<i64> = lanes(x.r[o.r("s")] as u64, 8, 32).into_iter()
                .map(|v| if signed { sx(v, 8) } else { v as i64 }).collect();
            x.wp(o.r(dest), join(&xs, 16));
        }
        id::MTS0_PRR | id::MTS1_PRR => {
            let bits = if base == id::MTS0_PRR { 8 } else { 16 };
            // (low lane first: t0, s0, t1, s1, ...)
            let xs: Vec<i64> = lanes(x.r[o.r("s")] as u64, bits, 32).into_iter().zip(lanes(x.r[o.r("t")] as u64, bits, 32))
                .flat_map(|(a, b)| [b as i64, a as i64]).collect();
            x.wp(o.r("r"), join(&xs, bits));
        }
        id::SAD4_ACC_PRR => {
            // (each lane saturates on its own, without carry)
            let d = o.r("r");
            let acc = lanes(x.pair(d), 16, 64);
            let (a, b) = (lanes(x.r[o.r("s")] as u64, 8, 32), lanes(x.r[o.r("t")] as u64, 8, 32));
            let xs: Vec<i64> = (0..4).map(|k| (acc[k] as i64 + (a[k] as i64 - b[k] as i64).abs()).min(65535)).collect();
            x.wp(d, join(&xs, 16));
        }
        id::ALIGN8_RRR | id::ALIGN16_RRR | id::ALIGN24_RRR => {
            let n = match base { id::ALIGN8_RRR => 8, id::ALIGN16_RRR => 16, _ => 24 };
            let v = ((x.r[o.r("s")] as u64) << 32 | x.r[o.r("t")] as u64) >> n;
            x.w(o.r("r"), v as u32);
        }
        id::PACK_RRHRH => {
            let v = half(x.r[o.r("s")], o.t("n") == ".h") << 16 | half(x.r[o.r("t")], o.t("m") == ".h");
            x.w(o.r("r"), v);
        }
        // `><`: a shift by a signed count, left for a positive one
        id::XCHG_PR | id::XCHG_PR_SAT | id::XCHG_RHRHR | id::XCHG_RHRHR_SAT | id::XCHG_RRR_SAT | id::XCHG_V_RRR
        | id::XCHG_RRR_SAT_V => {
            let shape = match base {
                id::XCHG_PR | id::XCHG_PR_SAT => "pr",
                id::XCHG_RHRHR | id::XCHG_RHRHR_SAT => "rhrhr",
                id::XCHG_V_RRR | id::XCHG_RRR_SAT_V => "v",
                _ => "rrr",
            };
            let sat_ = fid.contains("sat");
            let bits: u32 = match shape { "pr" => 64, "rhrhr" | "v" => 16, _ => 32 };
            let count = x.r[o.r(if shape == "pr" { "s" } else { "t" })] as u64;
            let n = sx(count, match bits { 16 => 5, 32 => 6, _ => 7 });
            let src = match shape {
                "pr" => x.pair(o.r("r")),
                "rhrhr" => half(x.r[o.r("s")], o.t("m") == ".h") as u64,
                _ => x.r[o.r("s")] as u64,
            };
            let one = |v: u64| -> i64 {
                let val: i128 = if sat_ { sx(v, bits) as i128 } else { v as i128 };
                let val = if n >= 0 { val << n } else { val >> -n };
                if sat_ { val.clamp(-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1) as i64 } else { (val as u128 & mask(bits) as u128) as i64 }
            };
            let total = match shape { "pr" => 64, "v" => 32, _ => bits };
            let y = join(&lanes(src, bits, total).into_iter().map(one).collect::<Vec<_>>(), bits);
            let carry = if n != 0 {
                let sh = if n > 0 { bits as i64 - n } else { -n - 1 };
                (src >> sh) & 1
            } else {
                ((x.f >> 1) & 1) as u64
            };
            let shifted = (x.f & !F_C) | (carry as u32) << 1;
            match shape {
                "pr" => { x.wp(o.r("r"), y); x.flags(shifted); }
                "rhrhr" => x.wh(o.r("r"), o.t("n"), y as u32),
                "v" => x.w(o.r("r"), y as u32),
                _ => { x.w(o.r("r"), y as u32); let f = nz(shifted, y, 32); x.flags(f); }
            }
        }
        // ==== d2.py: multiplies of loaded operands, with post-increments ====
        // (`hh.i`: these encodings reset the FM-1, nothing to model; `hh.rh` runs as `hw.rh`, its
        // second load 32 bits)
        _ if (fid.contains(".mem1.") || fid.contains(".mem2.")) && !fid.contains(".hh.i.") => {
            let p: Vec<&str> = fid.split('.').collect();
            let (op, kind, size, addr, sign) = (p[0], p[1], p[2], p[3], p[4]);
            let w0 = if size.starts_with('h') { 16 } else { 32 };
            let rd = |bus: &mut B, a: u32, bits: u32| if bits == 16 { bus.read16(a) as u64 } else { bus.read32(a) as u64 };
            let s = o.r("s");
            let a = rd(bus, x.r[s], w0);
            let b;
            if kind == "mem1" {
                let (bv, inc) = if addr == "i" { (x.r[o.r("t")], o.v("n") as u32) } else { (x.r[o.r("u")], x.r[o.r("t")]) };
                b = bv as u64;
                x.w(s, x.r[s].wrapping_add(inc));
            } else {
                let (second, inc_a, inc_b) = if addr == "i" {
                    (o.r("t"), o.v("n") as u32, o.v("m") as u32)
                } else {
                    let t = x.r[o.r("t")];
                    (o.r("u"), sx((t >> 16) as u64, 16) as u32, sx(t as u64, 16) as u32)
                };
                b = rd(bus, x.r[second], 32);
                // (shared bases keep the first operand's increment)
                x.w(second, x.r[second].wrapping_add(inc_b));
                x.w(s, x.r[s].wrapping_add(inc_a));
            }
            let (a, b) = if sign == "s" { (sx(a, w0), sx(b, 32)) } else { (a as i64, b as i64) };
            let product = (a as i128 * b as i128) as u64;
            let d = o.r("r");
            let acc = match op { "madd" => 1, "msub" => -1, _ => 0 };
            let result = if acc == 0 {
                product
            } else {
                let old = x.pair(d);
                let (r, carry) = if acc > 0 { old.overflowing_add(product) } else { (old.wrapping_sub(product), old >= product) };
                let f = (x.f & !F_C) | if carry { F_C } else { 0 };
                x.flags(f);
                r
            };
            // (the pair result wins over a coincident base update)
            x.wp(d, result);
        }
        // ==== m2.py: memory forms ====
        id::RMW_AND_INVR | id::RMW_SHL3_I | id::RMW_SHL3_R => {
            let addr = x.r[o.r("r")].wrapping_add(o.v("n") as u32);
            let old = bus.read32(addr);
            let new = if base == id::RMW_AND_INVR {
                old & !x.r[o.r("s")]
            } else {
                // (the count's low 8 bits, 32 and up as 32; signed saturating)
                let c = (if base == id::RMW_SHL3_I { o.v("m") as u32 } else { x.r[o.r("s")] } & 0xff).min(32);
                ((old as i32 as i64) << c).clamp(i32::MIN as i64, i32::MAX as i64) as u32
            };
            bus.write32(addr, new);
        }
        id::ST_H_HI_IDX | id::ST_H_HI_IDXS => {
            let i = x.r[o.r("s")];
            let addr = x.r[o.r("r")].wrapping_add(if base == id::ST_H_HI_IDXS { i << 1 } else { i });
            bus.write16(addr, (x.r[o.r("t")] >> 16) as u16);
        }
        // npc: the instruction's address + 4 rounded up to a word (a 6-byte `ld.w.npc` at 2 mod 4 reads
        // pc + 6; followup5-m-npc.jsonl)
        id::LD_W_NPC | id::LD_W_NPC_R => {
            let npc = x.cpu.pc.wrapping_add(7) & !3;
            let off = if base == id::LD_W_NPC { o.v("n") as u32 } else { x.r[o.r("s")] };
            let v = bus.read32(npc.wrapping_add(off));
            x.w(o.r("r"), v);
        }
        // ==== b2.py: branches on the flags, on r0's bit 0, and the 6-byte call (none touches the flags) ====
        _ if fid.starts_with("bc.") => {
            let f = x.f;
            let (n, z, c, v) = (f & F_N != 0, f & F_Z != 0, f & F_C != 0, f & F_V != 0);
            let taken = match &fid[3..] {
                "eq" => z, "ne" => !z, "cs" => c, "cc" => !c, "hi" => c && !z, "ls" => !c || z,
                "ge" => n == v, "lt" => n != v, "gt" => !z && n == v, _ => z || n != v,
            };
            if taken {
                return Some(Ok(Done::Jump(next_pc.wrapping_add(o.v("n") as u32))));
            }
        }
        // (r0's bit 0: `.c` taken when it is clear, `.s` when it is set)
        id::IFS2NS_C | id::IFS2NS_S => {
            if (x.r[0] & 1 != 0) == (base == id::IFS2NS_S) {
                return Some(Ok(Done::Jump(next_pc.wrapping_add(o.v("n") as u32))));
            }
        }
        id::CALL_XL => {
            x.cpu.rets = next_pc;
            return Some(Ok(Done::Jump(next_pc.wrapping_add(o.v("n") as u32))));
        }
        // ==== k2.py: special registers, register-mask pushes, bit fields by a register ====
        id::MOV_R_SR => {
            let v = sr_value(x.cpu, o.raw("s") as usize);
            x.w(o.r("r"), v);
        }
        id::PUSH_SRL | id::PUSH_SRMASK | id::PUSH_SRMASK_C => {
            let mask = o.raw(if base == id::PUSH_SRMASK_C { "n" } else { "m" });
            let vals: Vec<u32> = (0..16).filter(|i| mask >> i & 1 != 0).map(|i| sr_value(x.cpu, i)).collect();
            push_words(x.cpu, bus, &vals);
            // pushing psr clears N Z C V; pushing reti opens nesting, as `Fast::PushSregs`
            if mask >> SR_PSR & 1 != 0 {
                x.flags(0);
            }
            if mask >> SR_RETI & 1 != 0 {
                x.cpu.nest_ok = true;
                bus.irq_window();
            }
        }
        id::PUSH_RMASK_C | id::PUSH_RETS_RMASK_C => {
            let mask = o.raw("n");
            let mut vals: Vec<u32> = (0..16).filter(|i| mask >> i & 1 != 0).map(|i| x.r[i]).collect();
            if base == id::PUSH_RETS_RMASK_C {
                vals.push(x.cpu.rets);
            }
            push_words(x.cpu, bus, &vals);
        }
        // the field's position and width from a register: p = t[12:8], l = t[4:0]; N and Z
        id::INSERT_RRRIIRII | id::UEXTRA_RRRIIRII | id::SEXTRA_RRRIIRII => {
            let t = x.r[o.r("t")];
            let (p, l) = (t >> 8 & 31, t & 31);
            let src = x.r[o.r("s")];
            let d = o.r("r");
            let v = match base {
                id::INSERT_RRRIIRII => crate::bit_insert(x.r[d], src, p, l),
                id::UEXTRA_RRRIIRII => crate::bit_extract(src, p, l, false),
                _ => crate::bit_extract(src, p, l, true),
            };
            x.w(d, v);
            let f = nz(x.f, v as u64, 32);
            x.flags(f);
        }
        // ==== p.py: rotates through the carry, rounding pair shifts, signed-count shift, zcmp, halves ====
        id::ROR_RRR => {
            let v = x.r[o.r("s")].rotate_right(x.r[o.r("t")] & 31);
            x.w(o.r("r"), v);
            let f = nz(x.f, v as u64, 32);
            x.flags(f);
        }
        // the 33-bit (carry, rs) rotated right by c (0..32); N Z, C the bit rotated into it, V kept
        id::CYCC_RRI | id::CYCC_RRR => {
            let c = if base == id::CYCC_RRI { o.v("n") as u32 } else { (x.r[o.r("t")] & 0xff).min(32) };
            let (v, cy) = rorc(x.r[o.r("s")] as u64, (x.f >> 1 & 1) as u64, c, 33);
            x.w(o.r("r"), v as u32);
            let f = (x.f & F_V) | if cy != 0 { F_C } else { 0 } | nz(0, v, 32);
            x.flags(f);
        }
        // the 65-bit (carry, pair) rotated right by c (a count of 0 changes nothing, 64 and up as 64); only C
        id::CYCC_PI | id::CYCC_PR => {
            let c = if base == id::CYCC_PI { o.v("n") as u32 } else { x.r[o.r("s")] & 0xff };
            let d = o.r("r");
            // (a count of 0 still writes the pair, unchanged: as a pair's head it wins, cycc.pr.p)
            if c == 0 {
                let v = x.pair(d);
                x.wp(d, v);
            } else {
                let (v, cy) = rorc(x.pair(d), (x.f >> 1 & 1) as u64, c.min(64), 65);
                x.wp(d, v);
                let f = (x.f & !F_C) | if cy != 0 { F_C } else { 0 };
                x.flags(f);
            }
        }
        // a signed 6-bit count in rt's low bits: left for a positive one, right (zero fill) for a negative one
        id::XCHG_RRR => {
            let v = x.r[o.r("s")];
            let c = (x.r[o.r("t")] & 63) as i32;
            let c = if c >= 32 { c - 64 } else { c };
            let mut cy = x.f >> 1 & 1;
            let r = if c > 0 {
                cy = v >> (32 - c) & 1;
                v << c
            } else if c < 0 {
                cy = v >> (-c - 1) & 1;
                v.checked_shr((-c) as u32).unwrap_or(0)
            } else {
                v
            };
            x.w(o.r("r"), r);
            let f = (x.f & F_V) | cy << 1 | nz(0, r as u64, 32);
            x.flags(f);
        }
        // a pair shifted right by 0..64, rounded to the nearest (ties by the mode: up, to even, away from
        // zero, toward zero), saturated to 32 bits (unsigned for >>, signed for >>>); N Z, C the rounding bit
        id::SHIFTR_RPI_MODE | id::SHIFTR_RPR_MODE => {
            let imm = base == id::SHIFTR_RPI_MODE;
            let xx = x.pair(o.r("s"));
            let c = (if imm { o.v("m") as u32 } else { x.r[o.r("t")] & 0xff }).min(64);
            let mode = o.raw(if imm { "p" } else { "m" });
            let arith = o.raw("n") != 0;
            let xv: i128 = if arith { xx as i64 as i128 } else { xx as i128 };
            let mut q = xv >> c;
            let rem2 = 2 * (xv - (q << c));
            let unit = 1i128 << c;
            if c > 0 && rem2 > unit {
                q += 1;
            } else if c > 0 && rem2 == unit {
                q += [1, q & 1, (xv >= 0) as i128, (xv < 0) as i128][mode as usize & 3];
            }
            let r = if arith { q.clamp(i32::MIN as i128, i32::MAX as i128) as i64 as u32 } else { q.min(u32::MAX as i128) as u32 };
            let cy = if c > 0 { (xx >> (c - 1)) & 1 } else { 0 };
            x.w(o.r("r"), r);
            let f = (x.f & F_V) | (cy as u32) << 1 | nz(0, r as u64, 32);
            x.flags(f);
        }
        // each byte shifted right by 0..7 on its own; no flags
        id::LSR_RRI_VB => {
            let (v, n) = (x.r[o.r("s")], o.v("n") as u32);
            let r = (0..4).fold(0u32, |a, i| a | (((v >> (8 * i)) & 0xff) >> n) << (8 * i));
            x.w(o.r("r"), r);
        }
        // each halfword of the pair shifted right (sign fill) by 0..7, rounded (a tie up); no flags
        id::ASR_PPI_RND_VH => {
            let n = o.v("n") as u32;
            let (s, d) = (o.r("s"), o.r("r"));
            for k in 0..2 {
                let v = x.r[s + k];
                let mut r = 0u32;
                for i in 0..2 {
                    let h = ((v >> (16 * i)) & 0xffff) as u16 as i16 as i64;
                    let h = if n == 0 { h } else { (h + (1i64 << (n - 1))) >> n };
                    r |= ((h as u32) & 0xffff) << (16 * i);
                }
                x.w(d + k, r);
            }
        }
        // (this chip does not count bits: the pair form copies the low half of the pair's low register)
        id::POPCNT_R_PAIR => {
            let v = x.r[o.r("s")] & 0xffff;
            x.wh(o.r("r"), ".l", v);
        }
        // zcmp: 0, 1 or -1 by the sign; N and Z
        id::SGN_RR => {
            let v = x.r[o.r("s")];
            let r = if v == 0 { 0 } else if (v as i32) < 0 { u32::MAX } else { 1 };
            x.w(o.r("r"), r);
            let f = nz(x.f, r as u64, 32);
            x.flags(f);
        }
        id::MOV_HALF => {
            let s = x.r[o.r("s")];
            let src = if o.raw("m") != 0 { s >> 16 } else { s & 0xffff };
            x.wh(o.r("r"), if o.raw("n") != 0 { ".h" } else { ".l" }, src);
        }
        // ==== s.py: the float compare and the half-precision conversions ====
        id::FCMP_RR => {
            let f = crate::fcmp_flags(x.r[o.r("r")], x.r[o.r("s")]);
            x.flags(f);
        }
        // rD.l = the float rounded to binary16 (nearest even); a NaN keeps its sign and class, not its payload
        id::FTOF_HALF_RR => {
            let b = x.r[o.r("s")];
            let sign = ((b >> 16) & 0x8000) as u16;
            let h = if (b >> 23) & 0xff != 0xff {
                f32_to_f16(b)
            } else if b & 0x7f_ffff == 0 {
                sign | 0x7c00
            } else if b & 0x40_0000 != 0 {
                sign | 0x7e00
            } else {
                sign | 0x7c01
            };
            x.wh(o.r("r"), ".l", h as u32);
        }
        // rD = rS.l as a float (exact); a NaN as above
        id::FTOF_FULL_RR => {
            let h = x.r[o.r("s")] & 0xffff;
            let sign = (h >> 15) << 31;
            let v = if (h >> 10) & 0x1f != 0x1f {
                f16_to_f32(h as u16)
            } else if h & 0x3ff == 0 {
                sign | 0x7f80_0000
            } else if h & 0x200 != 0 {
                sign | 0x7fc0_0000
            } else {
                sign | 0x7f80_0001
            };
            x.w(o.r("r"), v);
        }
        // ==== v4.py: special-register writes and pops, the stack banks, returns, events ====
        // (the models describe what an FM-1 showed in its ROM's download context, not mode changes, interrupt
        // delivery or events: the returns jump to their register, the event and software-interrupt forms do
        // nothing visible, bkpt took its handler's return, popping pc)
        id::MOV_SR_R => {
            let v = x.r[o.r("r")];
            sr_write(x.cpu, o.raw("s") as usize, v);
        }
        id::LI_SR_U32 => sr_write(x.cpu, o.raw("s") as usize, o.v("n") as u32),
        id::POP_SRL | id::POP_SRMASK => {
            let mask = o.raw(if base == id::POP_SRL { "m" } else { "n" });
            let mut addr = x.cpu.sp;
            x.cpu.sp = addr.wrapping_add(4 * mask.count_ones());
            let mut jump = None;
            for n in 0..16usize {
                if mask >> n & 1 != 0 {
                    let v = bus.read32(addr);
                    if n == 15 {
                        jump = Some(v & crate::RETS_MASK);
                    } else {
                        sr_write(x.cpu, n, v);
                    }
                    addr = addr.wrapping_add(4);
                }
            }
            // (reti popped: back in an outer handler, as fm1-emu's own pops)
            if mask >> SR_RETI & 1 != 0 {
                x.cpu.nest_ok = false;
            }
            if let Some(t) = jump {
                return Some(Ok(Done::Jump(t)));
            }
        }
        // (uspn / sspn read as usp / ssp there)
        id::MOV_SP_USPN => x.cpu.sp = x.cpu.get_usp(),
        id::MOV_USPN_SP => {
            let sp = x.cpu.sp;
            x.cpu.set_usp(sp);
        }
        id::GOTOSS_XL => {
            x.cpu.rets = next_pc;
            return Some(Ok(Done::Jump(next_pc.wrapping_add(o.v("n") as u32))));
        }
        id::RTNS | id::RTE | id::RTX | id::RTHW | id::RTSS => {
            let name = match base { id::RTNS => "reti", id::RTE => "rete", id::RTSS => "rets", _ => "retx" };
            return Some(Ok(Done::Jump(crate::sreg_get_named(x.cpu, name))));
        }
        id::BKPT => {
            let t = bus.read32(x.cpu.sp);
            x.cpu.sp = x.cpu.sp.wrapping_add(4);
            return Some(Ok(Done::Jump(t)));
        }
        id::SEVL | id::SEVG | id::WFE | id::SWI => {}
        _ => return None,
    }
    Some(Ok(Done::Next))
}

// ---- dispatch --------------------------------------------------------------------------------------

/// Per form: the index of the form whose semantics it has (a pair head's base), when fm1-core runs it.
fn table() -> &'static [Option<usize>] {
    static T: OnceLock<Vec<Option<usize>>> = OnceLock::new();
    T.get_or_init(|| {
        FORMS
            .iter()
            .map(|f| {
                let base = pt::by_id(f.base_id())?;
                // (a twin must have every field of its base)
                if FORMS[base].fields.iter().any(|b| field_as(f, &FORMS[base], b.name).is_none()) {
                    return None;
                }
                // (each form once on a scratch core: the arms are the list of what is run)
                let mut cpu = Cpu::new(0);
                let mut bus = Scratch;
                let o = Ops { win: f.value, form: f, base: &FORMS[base] };
                exec(&mut cpu, &mut bus, base, &o, 0).map(|_| base)
            })
            .collect()
    })
}

/// The form fm1-core runs for a window: the table's first match, when it is one with semantics here.
pub(crate) fn decode(win: u64) -> Option<usize> {
    // (memoised on the 6-byte window, as fm1-isa's decoder: the scan goes over every form)
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<u64, Option<usize>>> = Default::default();
    }
    let win = win & 0xffff_ffff_ffff;
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&win).copied()) {
        return hit;
    }
    let k = pt::decode(win).filter(|&k| table()[k].is_some());
    CACHE.with(|c| c.borrow_mut().insert(win, k));
    k
}

/// Run form k (`decode`) from window win: None if it is not one fm1-core runs.
pub(crate) fn run<B: Bus>(cpu: &mut Cpu, bus: &mut B, k: usize, win: u64, next_pc: u32) -> Option<Result<Done, CoreError>> {
    let base = (*table().get(k)?)?;
    cpu.pi_wrote = [0; 16];
    exec(cpu, bus, base, &Ops { win, form: &FORMS[k], base: &FORMS[base] }, next_pc)
}

/// The micro-op form k is, where it is one (`fast_raw`): then the JITs compile it and the polling-loop
/// skip can look through it, as for fm1-isa's own classes. The 6-byte call.
pub(crate) fn fast(k: usize, win: u64, next_pc: u32) -> Option<crate::Fast> {
    let base = (*table().get(k)?)?;
    let o = Ops { win, form: &FORMS[k], base: &FORMS[base] };
    match base {
        id::CALL_XL => Some(crate::Fast::Call { target: next_pc.wrapping_add(o.v("n") as u32) }),
        _ => None,
    }
}

/// Does form k read the flags (for `flags_read_later`): the flag branches, the carry rotates and the
/// special-register reads and pushes.
pub(crate) fn reads_flags(k: usize) -> bool {
    let f = FORMS[k].base_id();
    f.starts_with("bc.") || f.starts_with("ifs2ns") || f.starts_with("cycc.") || f == "mov.r_sr"
        || f.starts_with("push.sr")
}

/// A bus that reads 0 and keeps nothing (`table`'s scratch runs).
struct Scratch;

impl Bus for Scratch {
    fn read8(&mut self, _: u32) -> u8 { 0 }
    fn read16(&mut self, _: u32) -> u16 { 0 }
    fn read32(&mut self, _: u32) -> u32 { 0 }
    fn write8(&mut self, _: u32, _: u8) {}
    fn write16(&mut self, _: u32, _: u16) {}
    fn write32(&mut self, _: u32, _: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes from 0, little-endian.
    struct FlatRam(Vec<u8>);

    impl FlatRam {
        fn new(mem: Vec<u8>) -> Self {
            FlatRam(mem)
        }
    }

    impl Bus for FlatRam {
        fn read8(&mut self, a: u32) -> u8 { self.0.get(a as usize).copied().unwrap_or(0) }
        fn read16(&mut self, a: u32) -> u16 { u16::from_le_bytes([self.read8(a), self.read8(a + 1)]) }
        fn read32(&mut self, a: u32) -> u32 { u32::from_le_bytes([0, 1, 2, 3].map(|k| self.read8(a + k))) }
        fn write8(&mut self, a: u32, v: u8) { if let Some(b) = self.0.get_mut(a as usize) { *b = v } }
        fn write16(&mut self, a: u32, v: u16) { for (k, b) in v.to_le_bytes().into_iter().enumerate() { self.write8(a + k as u32, b) } }
        fn write32(&mut self, a: u32, v: u32) { for (k, b) in v.to_le_bytes().into_iter().enumerate() { self.write8(a + k as u32, b) } }
    }

    /// One instruction at 0 on these registers and flags.
    fn run1(code: &[u8], regs: &[u32; 16], flags: u32) -> Cpu {
        let mut mem = vec![0u8; 0x100];
        mem[..code.len()].copy_from_slice(code);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        (cpu.regs, cpu.flags) = (*regs, flags);
        cpu.step(&mut ram).unwrap();
        cpu
    }

    #[test]
    fn add4_as_an_fm1_ran_it() {
        // pi32v2-isa hw/fm1-2026-10-24 (batch-1-003): `20 e4 04 00` r1_r0 = r1_r0 +|+|+|+ r0
        let mut r = [0u32; 16];
        r[..4].copy_from_slice(&[1225353999, 0, 2264391871, 4294967295]);
        let cpu = run1(&[0x20, 0xe4, 0x04, 0x00], &r, 13);
        assert_eq!((cpu.regs[0], cpu.regs[1], cpu.flags), (1232366366, 4784137, 13));
        assert_eq!(cpu.pc, 4);
    }

    #[test]
    fn the_forms_run_here() {
        // every form whose arm runs is decoded; a pair head runs as its base
        let run: Vec<&str> = FORMS.iter().zip(table()).filter(|(_, t)| t.is_some()).map(|(f, _)| f.id).collect();
        for id in ["add4.ppr", "add4.ppr.p", "mul2.prhrhrhrhe", "addsub.ppp.mode", "sum.acc.msat.rrr"] {
            assert!(run.contains(&id), "{id}");
        }
        assert!(!run.contains(&"nop"), "fm1-isa's own forms stay with it");
    }
}
