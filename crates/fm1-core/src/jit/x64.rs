//! x86-64 code for a block prefix: the AArch64 compiler's plan (jit.rs) op for op, with the
//! System V calling convention (Linux, and Windows too: jit.rs declares the entry and the helpers
//! `extern "sysv64"`).
//!
//! Register plan (callee-saved, so helpers may be called freely):
//!   rbx = &Cpu, rbp = &Ctx, r12 = RAM host base, r13 = XIP host base, r14d = instructions done
//!   in this call before the current block, r15 = code flags. Too few callee-saved registers
//!   for the rest: the limit and the two values kept across a helper call within one
//!   instruction live in the frame, [rsp], [rsp + 8], [rsp + 16]. Scratch: eax / ecx / edx /
//!   esi (the AArch64 code's w0 / w1 / w2 / w3), r10 / r11. Every block has the same frame, so a
//!   chained body runs in the frame of the function Rust called.
//!
//! Floats: add / sub / mul / div / compares in SSE, which gives what Rust's f32 gives on this
//! host (the interpreter); conversions to integers saturate as Rust's `as` does (cvttss2si alone
//! gives 0x8000_0000 for NaN and out of range); min / max through the interpreter's executor.

use super::{h_op, h_ram_ok, h_read, h_write, off_hold, off_pc, off_reg, off_rets, off_sp, off_stop_io, off_stop_pc, off_stop_rest, JitMem, RamGuard, DYN_BITS, OFF_BRK, OFF_SLOT, OFF_VERSION, UNCHECKED};
use crate::jit_x64::{m, mx, Alu, Asm, Cc, Fop, Label, Mem, Reg, Sh, RAX, RBP, RBX, RCX, RDI, RDX, RSI, RSP, R10, R11, R12, R13, R14, R15, R8, R9};
use crate::{Base, Bus, Cond, Fast, Op, RmwOp};

const CPU: Reg = RBX;
const CTX: Reg = RBP;
const RAM: Reg = R12;
const XIP: Reg = R13;
const DONE: Reg = R14;
const FLAGS: Reg = R15;
// frame slots
const LIMIT: i32 = 0;
const KEEP: i32 = 8;
const KEEP2: i32 = 16;
const FRAME: u8 = 24;

struct Gen<'a> {
    a: Asm,
    mem: &'a JitMem,
    exit: Label,
    h_read: u64,
    h_write: u64,
    h_op: u64,
    /// the Jit's dynamic-jump table (`DynEntry`)
    dyn_tab: u64,
    /// the runner's stop checks are compiled (`Jit::stops`)
    stops: bool,
}

fn icond(c: Cond) -> Cc {
    match c {
        Cond::Eq => Cc::E,
        Cond::Ne => Cc::Ne,
        Cond::LtU => Cc::B,
        Cond::GtU => Cc::A,
        Cond::LeU => Cc::Be,
        Cond::GeU => Cc::Ae,
        Cond::LtS => Cc::L,
        Cond::GtS => Cc::G,
        Cond::LeS => Cc::Le,
        Cond::GeS => Cc::Ge,
        _ => unreachable!("a float condition"),
    }
}

impl Gen<'_> {
    fn ld_reg(&mut self, d: Reg, r: u8) {
        self.a.ld(d, m(CPU, off_reg(r) as i32));
    }
    fn st_reg(&mut self, s: Reg, r: u8) {
        self.a.st(m(CPU, off_reg(r) as i32), s);
    }
    fn ld_sp(&mut self, d: Reg) {
        self.a.ld(d, m(CPU, off_sp() as i32));
    }
    fn st_sp(&mut self, s: Reg) {
        self.a.st(m(CPU, off_sp() as i32), s);
    }
    /// r += imm (any 32-bit immediate)
    fn add_imm(&mut self, r: Reg, imm: u32) {
        if imm != 0 {
            self.a.alu_ri(Alu::Add, r, imm);
        }
    }
    /// eax = op(eax, ecx) (ecx may be clobbered)
    fn alu(&mut self, op: Op) {
        let a = &mut self.a;
        match op {
            Op::Add => a.alu_rr(Alu::Add, RAX, RCX),
            Op::Sub => a.alu_rr(Alu::Sub, RAX, RCX),
            Op::RSub => {
                a.alu_rr(Alu::Sub, RCX, RAX);
                a.mov_rr(RAX, RCX);
            }
            Op::And => a.alu_rr(Alu::And, RAX, RCX),
            Op::AndNot => {
                a.not(RCX);
                a.alu_rr(Alu::And, RAX, RCX);
            }
            Op::Or => a.alu_rr(Alu::Or, RAX, RCX),
            Op::Xor => a.alu_rr(Alu::Xor, RAX, RCX),
            Op::Mul => a.imul(RAX, RCX),
            // (x86 takes a 32-bit shift count mod 32, as the interpreter does)
            Op::Shl => a.sh_cl(Sh::Shl, RAX),
            Op::Shr => a.sh_cl(Sh::Shr, RAX),
            Op::Sar => a.sh_cl(Sh::Sar, RAX),
        }
    }
    /// ecx = an A4/BitBr right-hand side: rk 0 register, 1 immediate, 2 `1 << reg` (edx clobbered)
    fn rhs(&mut self, rk: u8, rv: u32) {
        match rk {
            0 => self.ld_reg(RCX, rv as u8),
            1 => self.a.mov_ri(RCX, rv),
            _ => {
                self.ld_reg(RCX, rv as u8);
                self.a.mov_ri(RDX, 1);
                self.a.sh_cl(Sh::Shl, RDX);
                self.a.mov_rr(RCX, RDX);
            }
        }
    }
    fn call(&mut self, f: u64) {
        self.a.mov_ri64(R11, f);
        self.a.call_r(R11);
    }
    /// eax = DONE + count, then leave
    fn leave(&mut self, count: u32) {
        self.a.lea(RAX, m(DONE, count as i32));
        self.a.jmp(self.exit);
    }
    /// Leave with pc = imm, `count` instructions of this block done.
    fn exit_imm(&mut self, pc: u32, count: u32) {
        self.a.st_imm(m(CPU, off_pc() as i32), pc);
        self.leave(count);
    }
    /// Leave with pc = reg; or, as the AArch64 code, into the target's compiled body when the
    /// dynamic-jump table has it and nothing is due or held
    fn exit_reg(&mut self, reg: Reg, count: u32) {
        debug_assert!(![RCX, R10, R11].contains(&reg));
        let out = self.a.label();
        self.a.cmp_m_i8(m(CTX, OFF_BRK as i32), 0);
        self.a.jcc(Cc::Ne, out);
        self.a.cmp_m_i8(m(CPU, off_hold() as i32), 0);
        self.a.jcc(Cc::Ne, out);
        self.a.mov_rr(RCX, reg);
        self.a.sh_i(Sh::Shr, RCX, 1);
        self.a.alu_ri(Alu::And, RCX, (1 << DYN_BITS) - 1);
        self.a.sh_i(Sh::Shl, RCX, 4);
        self.a.mov_ri64(R10, self.dyn_tab);
        self.a.cmp_rm(reg, mx(R10, RCX));
        self.a.jcc(Cc::Ne, out);
        self.a.ld64(R11, Mem { base: R10, index: Some((RCX, 1)), disp: 8 });
        self.add_imm(DONE, count);
        self.a.jmp_r(R11);
        self.a.bind(out);
        self.a.st(m(CPU, off_pc() as i32), reg);
        self.leave(count);
    }
    /// Continue at a fixed pc: straight into its compiled body when the slot holds one,
    /// else back to Rust, which fills the slot once that block is compiled.
    fn chain(&mut self, pc: u32, count: u32) {
        let slot: &'static mut u64 = Box::leak(Box::new(0u64));
        let back = self.a.label();
        self.add_imm(DONE, count);
        self.a.mov_ri64(R10, slot as *mut u64 as u64);
        self.a.ld64(R11, m(R10, 0));
        self.a.test_rr64(R11, R11);
        self.a.jcc(Cc::E, back);
        self.a.jmp_r(R11);
        self.a.bind(back);
        self.a.st_imm(m(CPU, off_pc() as i32), pc);
        self.a.st64(m(CTX, OFF_SLOT as i32), R10);
        self.a.mov_rr(RAX, DONE);
        self.a.jmp(self.exit);
    }
    /// The helpers' instruction index of op `i` of this block: r = DONE + i.
    fn idx(&mut self, r: Reg, i: u32) {
        self.a.lea(r, m(DONE, i as i32));
    }

    /// eax = load(width, signed) at guest address eax. Inline RAM / XIP, else the bus.
    fn load(&mut self, w: u8, signed: bool, idx: u32) {
        let (xip, slow, done) = (self.a.label(), self.a.label(), self.a.label());
        let mem = *self.mem;
        for (base_reg, gbase, len, miss) in [(RAM, mem.ram_base, mem.ram_len, xip), (XIP, mem.xip_base, mem.xip_len, slow)] {
            if base_reg == XIP {
                self.a.bind(xip);
            }
            // off = addr - base; the word holding it must lie inside (as the Soc's fast paths)
            self.a.mov_rr(RSI, RAX);
            self.a.alu_ri(Alu::Sub, RSI, gbase);
            if w == 2 {
                self.a.alu_ri(Alu::And, RSI, !1u32); // a halfword is the one at addr & !1 (Soc read16)
            }
            self.a.alu_ri(Alu::Cmp, RSI, len.saturating_sub(w as u32));
            self.a.jcc(Cc::A, miss);
            // (esi was written as 32 bits: rsi is the zero-extended offset)
            match (w, signed) {
                (4, _) => self.a.ld(RAX, mx(base_reg, RSI)),
                (2, false) => self.a.ldzx16(RAX, mx(base_reg, RSI)),
                (2, true) => self.a.ldsx16(RAX, mx(base_reg, RSI)),
                (1, false) => self.a.ldzx8(RAX, mx(base_reg, RSI)),
                _ => self.a.ldsx8(RAX, mx(base_reg, RSI)),
            }
            self.a.jmp(done);
        }
        self.a.bind(slow);
        self.a.mov_rr(RSI, RAX);
        self.a.mov_rr64(RDI, CTX);
        self.idx(RDX, idx);
        self.a.mov_ri(RCX, w as u32);
        self.call(self.h_read);
        match (w, signed) {
            (2, true) => self.a.movsx16(RAX, RAX),
            (1, true) => self.a.movsx8(RAX, RAX),
            _ => {}
        }
        self.a.bind(done);
    }

    /// store(width) of ecx at guest address eax. Inline RAM, else the bus.
    fn store(&mut self, w: u8, idx: u32) {
        let (slow, done) = (self.a.label(), self.a.label());
        let mem = *self.mem;
        if w == 2 {
            self.a.test_ri(RAX, 1); // an odd halfword goes the Soc's merging way
            self.a.jcc(Cc::Ne, slow);
        }
        self.a.mov_rr(RSI, RAX);
        self.a.alu_ri(Alu::Sub, RSI, mem.ram_base);
        self.a.alu_ri(Alu::Cmp, RSI, mem.ram_len.saturating_sub(w as u32));
        self.a.jcc(Cc::A, slow);
        if !mem.ram_code.is_null() {
            self.a.mov_rr(RDX, RSI);
            self.a.sh_i(Sh::Shr, RDX, 2);
            self.a.cmp_m8_i8(mx(FLAGS, RDX), 0);
            self.a.jcc(Cc::Ne, slow);
        }
        match w {
            4 => self.a.st(mx(RAM, RSI), RCX),
            2 => self.a.st16(mx(RAM, RSI), RCX),
            _ => self.a.st8(mx(RAM, RSI), RCX),
        }
        self.a.jmp(done);
        self.a.bind(slow);
        self.a.mov_rr(RDX, RCX);
        self.a.mov_rr(RSI, RAX);
        self.a.mov_rr64(RDI, CTX);
        self.idx(RCX, idx);
        self.a.mov_ri(R8, w as u32);
        self.call(self.h_write);
        self.a.bind(done);
    }

    /// Jump to `taken` when cond(eax, ecx) holds: integer, or the two as floats.
    fn branch(&mut self, cond: Cond, taken: Label) {
        if !cond.is_float() {
            self.a.alu_rr(Alu::Cmp, RAX, RCX);
            self.a.jcc(icond(cond), taken);
            return;
        }
        self.a.movd_xr(0, RAX);
        self.a.movd_xr(1, RCX);
        // ucomiss: unordered ZF PF CF all set, less CF, equal ZF, greater none
        let (x, y) = (0, 1);
        match cond {
            Cond::FEq => {
                let skip = self.a.label();
                self.a.ucomiss(x, y);
                self.a.jcc(Cc::P, skip);
                self.a.jcc(Cc::E, taken);
                self.a.bind(skip);
            }
            Cond::FUne => {
                self.a.ucomiss(x, y);
                self.a.jcc(Cc::P, taken);
                self.a.jcc(Cc::Ne, taken);
            }
            // x < y (ordered) is y > x; x <= y is y >= x
            Cond::FLt => { self.a.ucomiss(y, x); self.a.jcc(Cc::A, taken); }
            Cond::FUge => { self.a.ucomiss(y, x); self.a.jcc(Cc::Be, taken); }
            Cond::FLe => { self.a.ucomiss(y, x); self.a.jcc(Cc::Ae, taken); }
            Cond::FUgt => { self.a.ucomiss(y, x); self.a.jcc(Cc::B, taken); }
            Cond::FGe => { self.a.ucomiss(x, y); self.a.jcc(Cc::Ae, taken); }
            Cond::FUlt => { self.a.ucomiss(x, y); self.a.jcc(Cc::B, taken); }
            Cond::FGt => { self.a.ucomiss(x, y); self.a.jcc(Cc::A, taken); }
            _ => { self.a.ucomiss(x, y); self.a.jcc(Cc::Be, taken); } // FUle
        }
    }

    /// eax = the f32 in eax converted to i32 (signed) or u32, saturating as Rust's `as`.
    fn f2i(&mut self, signed: bool) {
        let (done, zero, max) = (self.a.label(), self.a.label(), self.a.label());
        self.a.movd_xr(0, RAX);
        if signed {
            // 0x8000_0000: out of range or NaN, or exactly -2^31
            self.a.cvttss2si(RAX, 0, false);
            self.a.alu_ri(Alu::Cmp, RAX, 0x8000_0000);
            self.a.jcc(Cc::Ne, done);
            self.a.ucomiss(0, 0);
            self.a.jcc(Cc::P, zero);
            self.a.movd_rx(RCX, 0);
            self.a.test_rr(RCX, RCX);
            self.a.jcc(Cc::S, done); // negative: i32::MIN
            self.a.mov_ri(RAX, 0x7fff_ffff);
            self.a.jmp(done);
        } else {
            // 64-bit: exact for 0 .. 2^63; NaN and out of range give a negative result
            self.a.cvttss2si(RAX, 0, true);
            self.a.test_rr64(RAX, RAX);
            let neg = self.a.label();
            self.a.jcc(Cc::S, neg);
            self.a.mov_rr64(RCX, RAX);
            self.a.sh_i64(Sh::Shr, RCX, 32);
            self.a.jcc(Cc::Ne, max);
            self.a.jmp(done);
            self.a.bind(neg);
            // NaN or below zero: 0; 2^63 and up: u32::MAX
            self.a.ucomiss(0, 0);
            self.a.jcc(Cc::P, zero);
            self.a.movd_rx(RCX, 0);
            self.a.test_rr(RCX, RCX);
            self.a.jcc(Cc::S, zero);
            self.a.bind(max);
            self.a.mov_ri(RAX, u32::MAX);
            self.a.jmp(done);
        }
        self.a.bind(zero);
        self.a.mov_ri(RAX, 0);
        self.a.bind(done);
    }

    /// After an instruction that may have called the bus: leave if the next tick is due (or
    /// code was written), `rest` instructions before the block's end (`Cpu::stop_rest`).
    fn check_brk(&mut self, next_pc: u32, count: u32, hold: bool, rest: u32) {
        let go = self.a.label();
        self.a.cmp_m_i8(m(CTX, OFF_BRK as i32), 0);
        self.a.jcc(Cc::E, go);
        if hold {
            self.a.st_imm(m(CPU, off_hold() as i32), 1);
        }
        if self.stops && rest > 0 {
            self.a.st_imm(m(CPU, off_stop_rest() as i32), rest);
        }
        self.exit_imm(next_pc, count);
        self.a.bind(go);
    }
}

/// Native code for a block prefix of plain micro-ops (none careful), and its body's offset.
pub(super) fn compile<B: Bus>(ops: &[crate::BOp], pc0: u32, mem: &JitMem, guards: &mut Vec<Box<RamGuard>>, profile: bool, dyn_tab: u64, stops: Option<bool>) -> Option<(Vec<u8>, usize)> {
    let mut a = Asm::new();
    let exit = a.label();
    let mut g = Gen {
        a,
        mem,
        exit,
        h_read: h_read::<B> as *const () as u64,
        h_write: h_write::<B> as *const () as u64,
        h_op: if profile { h_op::<B, true> as *const () as u64 } else { h_op::<B, false> as *const () as u64 },
        dyn_tab,
        stops: stops.is_some(),
    };
    // prologue (entry from Rust: rdi cpu, rsi ctx, rdx ram, rcx xip, r8d limit, r9 code flags);
    // chained blocks enter at the body. Six pushes and the frame keep rsp 16-byte aligned for
    // the helper calls.
    for r in [RBX, RBP, R12, R13, R14, R15] {
        g.a.push(r);
    }
    g.a.alu_ri64(Alu::Sub, RSP, FRAME as i8);
    g.a.mov_rr64(CPU, RDI);
    g.a.mov_rr64(CTX, RSI);
    g.a.mov_rr64(RAM, RDX);
    g.a.mov_rr64(XIP, RCX);
    g.a.st(m(RSP, LIMIT), R8);
    g.a.mov_rr64(FLAGS, R9);
    g.a.mov_ri(DONE, 0);
    let body = g.a.here();
    // (a runner's stretch, as the AArch64 code)
    if let Some(whole) = stops {
        let (go, stop) = (g.a.label(), g.a.label());
        g.a.test_rr(DONE, DONE);
        g.a.jcc(Cc::E, go);
        g.a.ld(R11, m(CPU, off_stop_pc() as i32));
        g.a.test_rr(R11, R11);
        g.a.jcc(Cc::E, go);
        if whole {
            let bytes: u32 = ops.iter().map(|o| o.len as u32).sum();
            g.a.alu_ri(Alu::Sub, R11, pc0);
            g.a.alu_ri(Alu::Cmp, R11, bytes);
            g.a.jcc(Cc::B, stop);
            g.a.cmp_m_i8(m(CPU, off_stop_io() as i32), 0);
            g.a.jcc(Cc::Ne, stop);
            g.a.lea(R11, m(DONE, 32));
            g.a.cmp_rm(R11, m(RSP, LIMIT));
            g.a.jcc(Cc::B, go);
        }
        g.a.bind(stop);
        g.exit_imm(pc0, 0);
        g.a.bind(go);
    }
    // the whole prefix must fit the call's limit, else back to Rust before running any of it
    {
        let fits = g.a.label();
        g.a.lea(R11, m(DONE, ops.len() as i32));
        g.a.cmp_rm(R11, m(RSP, LIMIT));
        g.a.jcc(Cc::Be, fits);
        g.exit_imm(pc0, 0);
        g.a.bind(fits);
    }

    if !(0x0200_0000..0x0400_0000).contains(&pc0) {
        let off = pc0.wrapping_sub(mem.ram_base) as usize;
        let len = ops.iter().map(|op| op.len as usize).sum::<usize>() + 6;
        let len = len.min((mem.ram_len as usize).checked_sub(off)?);
        // SAFETY: jit_mem provides a live RAM allocation, and the range is checked above.
        let bytes = unsafe { std::slice::from_raw_parts(mem.ram.add(off), len) }.into();
        guards.push(Box::new(RamGuard { pc: pc0, bytes, version: UNCHECKED }));
        let guard = guards.last_mut().unwrap().as_mut();
        let valid = g.a.label();
        // (the AArch64 code's inline check: current at this call's count, brk clear)
        let slow = g.a.label();
        g.a.cmp_m_i8(m(CTX, OFF_BRK as i32), 0);
        g.a.jcc(Cc::Ne, slow);
        g.a.ld64(RAX, m(CTX, OFF_VERSION as i32));
        g.a.mov_ri64(RCX, guard as *mut RamGuard as u64);
        g.a.cmp_rm64(RAX, m(RCX, 0));
        g.a.jcc(Cc::E, valid);
        g.a.bind(slow);
        g.a.mov_rr64(RDI, CTX);
        g.a.mov_ri64(RSI, guard as *mut RamGuard as u64);
        g.call(h_ram_ok::<B> as *const () as u64);
        g.a.test_rr(RAX, RAX);
        g.a.jcc(Cc::Ne, valid);
        g.exit_imm(pc0, 0);
        g.a.bind(valid);
    }

    let mut pc = pc0;
    for (i, op) in ops.iter().enumerate() {
        let idx = i as u32;
        let next_pc = pc + op.len as u32;
        let count = idx + 1;
        let mut bus_used = false;
        match op.fast {
            Fast::Nop => {}
            Fast::MovI { d, v } => {
                g.a.st_imm(m(CPU, off_reg(d) as i32), v);
            }
            Fast::Mov { d, s } => {
                g.ld_reg(RAX, s);
                g.st_reg(RAX, d);
            }
            Fast::AddRI { d, s, v } => {
                g.ld_reg(RAX, s);
                g.add_imm(RAX, v);
                g.st_reg(RAX, d);
            }
            Fast::ShlI { d, s, n } => {
                g.ld_reg(RAX, s);
                if n & 31 != 0 {
                    g.a.sh_i(Sh::Shl, RAX, n & 31);
                }
                g.st_reg(RAX, d);
            }
            Fast::SarI { d, s, n } => {
                g.ld_reg(RAX, s);
                if n & 31 != 0 {
                    g.a.sh_i(Sh::Sar, RAX, n & 31);
                }
                g.st_reg(RAX, d);
            }
            Fast::Alu3 { d, a, b, op } => {
                g.ld_reg(RAX, a);
                g.ld_reg(RCX, b);
                g.alu(op);
                g.st_reg(RAX, d);
            }
            Fast::Alu3I { d, a, v, op } => {
                g.ld_reg(RAX, a);
                g.a.mov_ri(RCX, v);
                g.alu(op);
                g.st_reg(RAX, d);
            }
            Fast::Alu2 { d, b, op } => {
                g.ld_reg(RAX, d);
                g.ld_reg(RCX, b);
                g.alu(op);
                g.st_reg(RAX, d);
            }
            Fast::Alu2I { d, v, op } => {
                g.ld_reg(RAX, d);
                g.a.mov_ri(RCX, v);
                g.alu(op);
                g.st_reg(RAX, d);
            }
            Fast::A4 { d, s, op, rk, rv } => {
                g.rhs(rk, rv);
                g.ld_reg(RAX, s);
                g.alu(op);
                g.st_reg(RAX, d);
            }
            Fast::Ext { d, s, part, signed } => {
                g.ld_reg(RAX, s);
                match (part, signed) {
                    (0, false) => g.a.movzx8(RAX, RAX),
                    (0, true) => g.a.movsx8(RAX, RAX),
                    (1, false) => g.a.movzx16(RAX, RAX),
                    (1, true) => g.a.movsx16(RAX, RAX),
                    (_, false) => g.a.sh_i(Sh::Shr, RAX, 16),
                    (_, true) => g.a.sh_i(Sh::Sar, RAX, 16),
                }
                g.st_reg(RAX, d);
            }
            Fast::MinMax { d, a, b, kind } => {
                g.ld_reg(RAX, a);
                g.ld_reg(RCX, b);
                g.a.alu_rr(Alu::Cmp, RAX, RCX);
                // b where a is not the one kept
                let c = match kind { 0 => Cc::Ge, 1 => Cc::Le, 2 => Cc::Ae, _ => Cc::Be };
                g.a.cmov(c, RAX, RCX);
                g.st_reg(RAX, d);
            }
            // ---- branches (always a block's last op)
            Fast::BrI { a, v, cond, target } => {
                let t = g.a.label();
                g.ld_reg(RAX, a);
                g.a.mov_ri(RCX, v);
                g.branch(cond, t);
                g.chain(next_pc, count);
                g.a.bind(t);
                g.chain(target, count);
            }
            Fast::BrIEq { a, v, target } | Fast::BrINe { a, v, target } => {
                let t = g.a.label();
                g.ld_reg(RAX, a);
                g.a.mov_ri(RCX, v);
                g.branch(if matches!(op.fast, Fast::BrIEq { .. }) { Cond::Eq } else { Cond::Ne }, t);
                g.chain(next_pc, count);
                g.a.bind(t);
                g.chain(target, count);
            }
            Fast::BrR { a, b, cond, target } => {
                let t = g.a.label();
                g.ld_reg(RAX, a);
                g.ld_reg(RCX, b);
                g.branch(cond, t);
                g.chain(next_pc, count);
                g.a.bind(t);
                g.chain(target, count);
            }
            Fast::BitBr { r, mk, ne, mv, target } => {
                let t = g.a.label();
                g.rhs(mk, mv);
                g.ld_reg(RAX, r);
                g.a.test_rr(RAX, RCX);
                g.a.jcc(if ne { Cc::Ne } else { Cc::E }, t);
                g.chain(next_pc, count);
                g.a.bind(t);
                g.chain(target, count);
            }
            Fast::DecBr { r, cond, v, target } => {
                let t = g.a.label();
                g.ld_reg(RAX, r);
                g.a.alu_ri(Alu::Sub, RAX, 1);
                g.st_reg(RAX, r);
                g.a.mov_ri(RCX, v);
                g.branch(cond, t);
                g.chain(next_pc, count);
                g.a.bind(t);
                g.chain(target, count);
            }
            Fast::Goto { target } => g.chain(target, count),
            Fast::Call { target } => {
                g.a.st_imm(m(CPU, off_rets() as i32), next_pc);
                g.chain(target, count);
            }
            Fast::IfBlk { a, rk, fam, alt, has_else: false, rv, then_d, .. } => {
                // without an else part a predicated block is a conditional jump over its then part
                let taken = g.a.label();
                g.rhs(rk, rv);
                g.ld_reg(RAX, a);
                if fam == 2 {
                    g.a.test_rr(RAX, RCX);
                    g.a.jcc(if alt { Cc::Ne } else { Cc::E }, taken);
                } else {
                    g.branch(crate::cond_of_fam(fam as u64, alt), taken);
                }
                g.chain(next_pc.wrapping_add(then_d as u32), count);
                g.a.bind(taken);
                g.chain(next_pc, count);
            }
            // (the multiply-accumulates 7 / 8 read d too, min / max are Rust's: through the
            // interpreter's executor)
            Fast::FOp { d, a, b, op } if !matches!(op, 5..=8) => {
                g.ld_reg(RAX, a);
                match op {
                    0..=3 => {
                        g.ld_reg(RCX, b);
                        g.a.movd_xr(0, RAX);
                        g.a.movd_xr(1, RCX);
                        let f = match op { 0 => Fop::Add, 1 => Fop::Sub, 2 => Fop::Mul, _ => Fop::Div };
                        g.a.fop(f, 0, 1);
                        g.a.movd_rx(RAX, 0);
                    }
                    0x1f => g.f2i(true),
                    0x5f => g.f2i(false),
                    0x8f => {
                        g.a.cvtsi2ss(0, RAX, false);
                        g.a.movd_rx(RAX, 0);
                    }
                    _ => {
                        // u32 -> f32: the zero-extended rax as a signed 64-bit integer
                        g.a.cvtsi2ss(0, RAX, true);
                        g.a.movd_rx(RAX, 0);
                    }
                }
                g.st_reg(RAX, d);
            }
            Fast::Rts => {
                g.a.ld(RAX, m(CPU, off_rets() as i32));
                g.exit_reg(RAX, count);
            }
            Fast::GotoR { r } => {
                g.ld_reg(RAX, r);
                g.exit_reg(RAX, count);
            }
            Fast::CallR { r } => {
                g.ld_reg(RAX, r);
                g.a.st_imm(m(CPU, off_rets() as i32), next_pc);
                g.exit_reg(RAX, count);
            }
            // ---- memory
            Fast::Ld { d, b, off, w, signed } => {
                match b {
                    Base::Reg(r) => g.ld_reg(RAX, r),
                    Base::Sp => g.ld_sp(RAX),
                }
                g.add_imm(RAX, off);
                g.load(w, signed, idx);
                g.st_reg(RAX, d);
                bus_used = true;
            }
            Fast::LdW { d, b, off } => {
                g.ld_reg(RAX, b);
                g.add_imm(RAX, off);
                g.load(4, false, idx);
                g.st_reg(RAX, d);
                bus_used = true;
            }
            Fast::LdWSp { d, off } => {
                g.ld_sp(RAX);
                g.add_imm(RAX, off);
                g.load(4, false, idx);
                g.st_reg(RAX, d);
                bus_used = true;
            }
            Fast::LdBu { d, b, off } => {
                g.ld_reg(RAX, b);
                g.add_imm(RAX, off);
                g.load(1, false, idx);
                g.st_reg(RAX, d);
                bus_used = true;
            }
            Fast::St { s, b, off, w } => {
                match b {
                    Base::Reg(r) => g.ld_reg(RAX, r),
                    Base::Sp => g.ld_sp(RAX),
                }
                g.add_imm(RAX, off);
                g.ld_reg(RCX, s);
                g.store(w, idx);
                bus_used = true;
            }
            Fast::StW { s, b, off } => {
                g.ld_reg(RAX, b);
                g.add_imm(RAX, off);
                g.ld_reg(RCX, s);
                g.store(4, idx);
                bus_used = true;
            }
            Fast::StWSp { s, off } => {
                g.ld_sp(RAX);
                g.add_imm(RAX, off);
                g.ld_reg(RCX, s);
                g.store(4, idx);
                bus_used = true;
            }
            Fast::LdIdx { d, b, i, sh, w, signed } => {
                g.ld_reg(RAX, b);
                g.ld_reg(RCX, i);
                if sh & 31 != 0 {
                    g.a.sh_i(Sh::Shl, RCX, sh & 31);
                }
                g.a.alu_rr(Alu::Add, RAX, RCX);
                g.load(w, signed, idx);
                g.st_reg(RAX, d);
                bus_used = true;
            }
            Fast::StIdx { s, b, i, sh, w } => {
                g.ld_reg(RAX, b);
                g.ld_reg(RCX, i);
                if sh & 31 != 0 {
                    g.a.sh_i(Sh::Shl, RCX, sh & 31);
                }
                g.a.alu_rr(Alu::Add, RAX, RCX);
                g.ld_reg(RCX, s);
                g.store(w, idx);
                bus_used = true;
            }
            Fast::LdPost { d, b, w, signed, inc } => {
                g.ld_reg(RAX, b);
                g.load(w, signed, idx);
                g.st_reg(RAX, d);
                g.ld_reg(RAX, b); // (after the load wrote d, as the interpreter)
                g.add_imm(RAX, inc);
                g.st_reg(RAX, b);
                bus_used = true;
            }
            Fast::StPost { s, b, w, inc } => {
                g.ld_reg(RAX, b);
                g.ld_reg(RCX, s);
                g.store(w, idx);
                g.ld_reg(RAX, b);
                g.add_imm(RAX, inc);
                g.st_reg(RAX, b);
                bus_used = true;
            }
            // (i 16: the immediate increment; else register i's)
            Fast::LdPre { d, b, i, w, signed, inc } => {
                g.ld_reg(RAX, b);
                if i < 16 {
                    g.ld_reg(RCX, i);
                    g.a.alu_rr(Alu::Add, RAX, RCX);
                } else {
                    g.add_imm(RAX, inc);
                }
                g.st_reg(RAX, b);
                g.load(w, signed, idx);
                g.st_reg(RAX, d);
                bus_used = true;
            }
            Fast::StPre { s, b, i, w, inc } => {
                g.ld_reg(RAX, b);
                if i < 16 {
                    g.ld_reg(RCX, i);
                    g.a.alu_rr(Alu::Add, RAX, RCX);
                } else {
                    g.add_imm(RAX, inc);
                }
                g.st_reg(RAX, b);
                g.ld_reg(RCX, s);
                g.store(w, idx);
                bus_used = true;
            }
            Fast::LdPostR { d, b, i, w, signed } => {
                // (the increment as it was before the load wrote d, kept across a helper call)
                g.ld_reg(RCX, i);
                g.a.st(m(RSP, KEEP), RCX);
                g.ld_reg(RAX, b);
                g.load(w, signed, idx);
                g.st_reg(RAX, d);
                g.ld_reg(RAX, b);
                g.a.ld(RCX, m(RSP, KEEP));
                g.a.alu_rr(Alu::Add, RAX, RCX);
                g.st_reg(RAX, b);
                bus_used = true;
            }
            Fast::StPostR { s, b, i, w } => {
                g.ld_reg(RAX, b);
                g.ld_reg(RCX, s);
                g.store(w, idx);
                g.ld_reg(RAX, b);
                g.ld_reg(RCX, i);
                g.a.alu_rr(Alu::Add, RAX, RCX);
                g.st_reg(RAX, b);
                bus_used = true;
            }
            Fast::Rmw { b, w, op: rop, off, v } => {
                g.ld_reg(RAX, b);
                g.add_imm(RAX, off);
                g.a.st(m(RSP, KEEP), RAX);
                if let RmwOp::Mov = rop {
                    g.a.mov_ri(RCX, v);
                } else {
                    g.load(w, false, idx);
                    g.a.mov_rr(RCX, RAX);
                    let o = match rop {
                        RmwOp::Or => Alu::Or,
                        RmwOp::And => Alu::And,
                        RmwOp::Xor => Alu::Xor,
                        RmwOp::Add => Alu::Add,
                        RmwOp::Sub => Alu::Sub,
                        RmwOp::Mov => unreachable!(),
                    };
                    g.a.alu_ri(o, RCX, v);
                }
                g.a.ld(RAX, m(RSP, KEEP));
                g.store(w, idx);
                bus_used = true;
            }
            Fast::MemA4 { b, off, op: aop, rk, rv } => {
                g.rhs(rk, rv);
                g.a.st(m(RSP, KEEP2), RCX);
                g.ld_reg(RAX, b);
                g.add_imm(RAX, off as u32);
                g.a.st(m(RSP, KEEP), RAX);
                g.load(4, false, idx);
                g.a.ld(RCX, m(RSP, KEEP2));
                g.alu(aop);
                g.a.mov_rr(RCX, RAX);
                g.a.ld(RAX, m(RSP, KEEP));
                g.store(4, idx);
                bus_used = true;
            }
            Fast::SpAdd { v } => {
                g.ld_sp(RAX);
                g.add_imm(RAX, v);
                g.st_sp(RAX);
            }
            Fast::RFromSp { d, v } => {
                g.ld_sp(RAX);
                g.add_imm(RAX, v);
                g.st_reg(RAX, d);
            }
            Fast::Mov2 { dh, dl, sh, sl } => {
                g.ld_reg(RAX, sh);
                g.ld_reg(RCX, sl);
                g.st_reg(RAX, dh);
                g.st_reg(RCX, dl);
            }
            Fast::Mov2I { dh, dl, v } => {
                g.a.st_imm(m(CPU, off_reg(dl) as i32), v);
                g.a.st_imm(m(CPU, off_reg(dh) as i32), ((v as i32) >> 31) as u32);
            }
            Fast::BitExt { d, s, p, l, signed } => {
                let (p, l) = (p & 31, l & 31);
                g.ld_reg(RAX, s);
                if p != 0 {
                    g.a.sh_i(Sh::Shr, RAX, p);
                }
                if l != 0 {
                    if signed {
                        g.a.sh_i(Sh::Shl, RAX, 32 - l);
                        g.a.sh_i(Sh::Sar, RAX, 32 - l);
                    } else {
                        g.a.alu_ri(Alu::And, RAX, (1u32 << l) - 1);
                    }
                }
                g.st_reg(RAX, d);
            }
            Fast::BitIns { d, s, p, l } => {
                let (p, l) = (p as u32 & 31, l as u32 & 31);
                let mask = if l == 0 { u32::MAX } else { (1u32 << l) - 1 };
                g.ld_reg(RAX, s);
                g.ld_reg(RCX, d);
                g.a.alu_ri(Alu::And, RAX, mask);
                if p != 0 {
                    g.a.sh_i(Sh::Shl, RAX, p as u8);
                }
                g.a.alu_ri(Alu::And, RCX, !(mask << p));
                g.a.alu_rr(Alu::Or, RAX, RCX);
                g.st_reg(RAX, d);
            }
            Fast::PushRets { n } => {
                let (top, bot) = ((n as i32).max(3), (n as i32).min(4));
                for r in std::iter::once(-1).chain((bot..=top).rev()) {
                    g.ld_sp(RAX);
                    g.a.alu_ri(Alu::Sub, RAX, 4);
                    g.st_sp(RAX);
                    if r < 0 {
                        g.a.ld(RCX, m(CPU, off_rets() as i32));
                    } else {
                        g.ld_reg(RCX, r as u8);
                    }
                    g.store(4, idx);
                }
                bus_used = true;
            }
            Fast::PopPc { n } => {
                let (top, bot) = ((n as i32).max(3), (n as i32).min(4));
                for r in bot..=top {
                    g.ld_sp(RAX);
                    g.load(4, false, idx);
                    g.st_reg(RAX, r as u8);
                    g.ld_sp(RAX);
                    g.a.alu_ri(Alu::Add, RAX, 4);
                    g.st_sp(RAX);
                }
                g.ld_sp(RAX);
                g.load(4, false, idx);
                g.ld_sp(RDX);
                g.a.alu_ri(Alu::Add, RDX, 4);
                g.st_sp(RDX);
                g.exit_reg(RAX, count);
            }
            Fast::PopRets => {
                g.ld_sp(RAX);
                g.load(4, false, idx);
                g.a.st(m(CPU, off_rets() as i32), RAX);
                g.ld_sp(RAX);
                g.a.alu_ri(Alu::Add, RAX, 4);
                g.st_sp(RAX);
                bus_used = true;
            }
            Fast::Tb { r, half } => {
                g.ld_reg(RAX, r);
                g.add_imm(RAX, next_pc);
                g.load(if half { 2 } else { 1 }, false, idx);
                g.a.sh_i(Sh::Shl, RAX, 1);
                g.add_imm(RAX, next_pc);
                g.exit_reg(RAX, count);
            }
            Fast::PushRet => {
                g.ld_sp(RAX);
                g.a.alu_ri(Alu::Sub, RAX, 4);
                g.st_sp(RAX);
                g.a.ld(RCX, m(CPU, off_rets() as i32));
                g.store(4, idx);
                bus_used = true;
            }
            Fast::PopRet => {
                g.ld_sp(RAX);
                g.load(4, false, idx);
                g.ld_sp(RDX);
                g.a.alu_ri(Alu::Add, RDX, 4);
                g.st_sp(RDX);
                g.exit_reg(RAX, count);
            }
            // ---- everything else: the interpreter's executor for this one op
            other => {
                let p: &'static Fast = Box::leak(Box::new(other));
                g.a.mov_rr64(RDI, CTX);
                g.a.mov_ri64(RSI, p as *const Fast as u64);
                g.idx(RDX, idx);
                g.a.mov_ri(RCX, next_pc);
                g.call(g.h_op);
                let fall = g.a.label();
                g.a.bt64(RAX, 32); // bit 32: jumped (target in eax)
                g.a.jcc(Cc::Ae, fall);
                g.exit_reg(RAX, count);
                g.a.bind(fall);
                bus_used = true;
                if other.may_jump() {
                    // a branch through the interpreter (the float compares) that fell through
                    g.chain(next_pc, count);
                }
            }
        }
        if bus_used && !op.fast.may_jump() {
            // leaving between a glued pair's two instructions: interrupts wait for the second
            g.check_brk(next_pc, count, op.glue, ops.len() as u32 - count);
        }
        pc = next_pc;
        if op.fast.may_jump() {
            break; // (the block's last op)
        }
    }
    // fell off the end of the prefix
    let n = ops.len() as u32;
    if !ops.last().map_or(false, |o| o.fast.may_jump()) {
        g.chain(pc, n);
    }
    g.a.bind(exit);
    g.a.alu_ri64(Alu::Add, RSP, FRAME as i8);
    for r in [R15, R14, R13, R12, RBP, RBX] {
        g.a.pop(r);
    }
    g.a.ret();
    Some((g.a.finish(), body))
}
