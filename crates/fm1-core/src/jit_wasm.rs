//! WebAssembly JIT for the blocks of micro-ops `Cpu::run_fast` interprets: the wasm32 build's
//! counterpart of jit.rs, with the same rules and the interpreter as the oracle (the two must
//! leave the same machine state; like the AArch64 JIT it leaves the add / subtract flags alone:
//! a producer whose flags a later instruction reads stays on the careful path).
//!
//! A hot block becomes a region: the block and the blocks reachable from it over fixed edges
//! (fall-through, branch, goto and call targets), compiled together as one function in a small
//! wasm module of its own (blocks average three or four instructions: one call per block cost
//! as much as the interpreter saved). The module imports the emulator's memory and function
//! table, and exports `run(cpu, ctx, limit, start) -> instructions`; any block of the region
//! can be the start, and inside it a `loop` + `br_table` dispatches from block to block. The host (JavaScript)
//! compiles it synchronously and puts `run` into the emulator's exported function table
//! (`fm1_jit_compile`, see web/jit.js); Rust then calls it through that table index, so running
//! compiled code involves no JavaScript at all. The generated code reaches back into Rust the
//! same way: the helpers' table indices are `call_indirect` targets.
//!
//! Timing is kept exact as in jit.rs: a block runs only when all its ticks fit before anything
//! is due; RAM / XIP accesses are inline (as are the register ops, the FPU's included: wasm's own
//! f32 instructions, see `fop_inline`), anything else goes through a helper that first brings
//! the clock to that instruction's tick, and when the next tick is due after it (a peripheral
//! write, an idle skip) the code leaves right after the instruction. Within a region every block
//! first checks that its instructions still fit the call's limit (as jit.rs's chained bodies
//! do); edges out of the region, computed jumps and returns go back to `run_fast`.

use crate::{Base, Bus, Cond, Cpu, Fast, JitMem, Op, RmwOp};
use std::mem::offset_of;
use std::sync::atomic::{AtomicU32, Ordering};

#[link(wasm_import_module = "env")]
extern "C" {
    /// Compile a module and install its `run` in the function table: the table index, 0 on failure.
    fn fm1_jit_compile(bytes: *const u8, len: usize) -> u32;
}

/// Compiles still allowed (the host refills it: compiling in the audio thread must not hold up a
/// quantum for long; a block that finds none left stays interpreted and tries again later).
pub static COMPILE_BUDGET: AtomicU32 = AtomicU32::new(u32::MAX);

/// Runs of a block before it is compiled.
const HOT: u32 = 32;

/// Most blocks / instructions in one region (Safari's top tier skips very large functions).
pub(crate) const REGION_BLOCKS: usize = 32;
pub(crate) const REGION_OPS: usize = 384;

#[derive(Clone, Copy)]
enum Entry {
    Cold(u32),
    /// table index of the region's function, this block's index in it, the region's number
    In(u32, u32, u32),
    No,
}

/// JIT state kept in the Cpu (per block id).
pub struct Jit {
    state: Vec<Entry>,
    /// Per region: its blocks over RAM, and the bus's count of code-word writes when they were
    /// last all found current (`Cpu::wasm_region_ok`); a region found stale is dropped.
    pub(crate) rinfo: Vec<(Box<[usize]>, u64)>,
    pub regions: u32,
    pub bytes: usize,
    /// compile helper calls that count themselves (`Cpu::profile` when the JIT was made)
    profile: bool,
}

impl std::fmt::Debug for Jit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Jit(wasm, {} regions, {} bytes)", self.regions, self.bytes)
    }
}

impl Jit {
    pub fn new(profile: bool) -> Option<Jit> {
        Some(Jit { state: Vec::new(), rinfo: Vec::new(), regions: 0, bytes: 0, profile })
    }

    fn at(&mut self, id: usize) -> &mut Entry {
        if self.state.len() <= id {
            self.state.resize(id + 1, Entry::Cold(0));
        }
        &mut self.state[id]
    }

    /// Block `id`'s compiled code (function, start index, region); else counts the run, and true
    /// says the block just became hot (form its region and `install` it, or `refuse`).
    pub(crate) fn lookup(&mut self, id: usize) -> (Option<(u32, u32, u32)>, bool) {
        match *self.at(id) {
            Entry::In(f, k, r) => (Some((f, k, r)), false),
            Entry::No => (None, false),
            Entry::Cold(c) if c + 1 < HOT => {
                *self.at(id) = Entry::Cold(c + 1);
                (None, false)
            }
            Entry::Cold(_) => (None, COMPILE_BUDGET.load(Ordering::Relaxed) > 0),
        }
    }

    /// Compile a region (blocks with their ids; the first is the hot one) and install it: every
    /// block not yet in a region gets this one. `ver`: the bus's count of code-word writes, its
    /// RAM blocks having just been found current. False: it could not be compiled.
    pub(crate) fn install<B: Bus>(&mut self, blocks: &[(usize, u32, Vec<crate::BOp>)], mem: &JitMem, ver: u64) -> bool {
        COMPILE_BUDGET.fetch_sub(1, Ordering::Relaxed);
        let m = compile::<B>(blocks, mem, self.profile);
        // SAFETY: a plain byte slice; the host copies it before returning
        let f = unsafe { fm1_jit_compile(m.as_ptr(), m.len()) };
        if f == 0 {
            return false;
        }
        self.regions += 1;
        self.bytes += m.len();
        let r = self.rinfo.len() as u32;
        let ram = blocks.iter().filter(|b| !(0x0200_0000..0x0400_0000).contains(&b.1)).map(|b| b.0).collect();
        self.rinfo.push((ram, ver));
        for (k, (id, ..)) in blocks.iter().enumerate() {
            if !matches!(*self.at(*id), Entry::In(..)) {
                *self.at(*id) = Entry::In(f, k as u32, r);
            }
        }
        true
    }

    /// Region `r` ran from RAM that has changed: its blocks are hot again, to be compiled anew.
    pub(crate) fn drop_region(&mut self, r: u32) {
        for e in self.state.iter_mut() {
            if matches!(*e, Entry::In(_, _, q) if q == r) {
                *e = Entry::Cold(0);
            }
        }
        self.rinfo[r as usize].0 = Box::new([]);
    }

    /// Block `id` stays interpreted.
    pub(crate) fn refuse(&mut self, id: usize) {
        *self.at(id) = Entry::No;
    }

    /// Forget every compiled block (the block table was rebuilt; the table slots stay in use).
    pub fn reset(&mut self) {
        self.state.clear();
        self.rinfo.clear();
    }
}

/// Per-call context the helpers use (field offsets are used by generated code).
#[repr(C)]
pub struct Ctx {
    cpu: *mut Cpu,
    bus: *mut (),
    /// ticks of instruction `idx` = idx + extra (0: the first one was ticked by the caller)
    extra: u32,
    /// ticks already applied to the bus
    synced: u32,
    /// the next tick is due: leave after this instruction
    brk: u32,
    /// left inside an if-else block's then-part: its skip (then end, else end, then start), for
    /// `run` to put in `Cpu::pred_skips` as the interpreter has it there (0: none)
    pred: [u32; 3],
}

/// Run compiled code: returns the instructions executed; cpu.pc is set. At most `cap`
/// instructions, as far as whole blocks go (the caller's budget, as the interpreter keeps it).
///
/// # Safety
/// `f` must come from `Jit::entry` for this bus type and memory map.
pub(crate) unsafe fn run<B: Bus>(f: u32, start: u32, cpu: &mut Cpu, bus: &mut B, extra: u32, cap: u32) -> u32 {
    // instructions this call may run: their ticks ((k - 1) + extra) must all fit tick_room
    let limit = (bus.tick_room() + 1).saturating_sub(extra as u64).min(1 << 30).min(cap as u64) as u32;
    let mut ctx = Ctx { cpu: cpu as *mut Cpu, bus: bus as *mut B as *mut (), extra, synced: 0, brk: 0, pred: [0; 3] };
    // SAFETY: on wasm32 a function pointer is its index in the function table, where the host
    // installed this block's `run` with exactly this signature
    let code: extern "C" fn(*mut Cpu, *mut Ctx, u32, u32) -> u32 = unsafe { std::mem::transmute(f as usize) };
    let k = code(cpu as *mut Cpu, &mut ctx, limit, start);
    if ctx.pred[0] != 0 {
        let (te, ee, ts) = (ctx.pred[0], ctx.pred[1], ctx.pred[2]);
        cpu.pred_skips.retain(|&(t, _, _)| t != te);
        cpu.pred_skips.push((te, ee, ts));
    }
    if k > 0 {
        // the ticks of the instructions run, less those the helpers applied already
        let need = (k - 1) + extra;
        if need > ctx.synced {
            bus.add_ticks((need - ctx.synced) as u64);
        }
    }
    k
}

// ------------------------------------------------------------------ helpers called from generated code

unsafe fn sync<B: Bus>(ctx: &mut Ctx, bus: &mut B, idx: u32) {
    let need = idx + ctx.extra;
    if need > ctx.synced {
        bus.add_ticks((need - ctx.synced) as u64);
        ctx.synced = need;
    }
}

extern "C" fn h_read<B: Bus>(ctx: *mut Ctx, addr: u32, idx: u32, w: u32) -> u32 {
    // SAFETY: ctx is the live context `run` passed in; the bus pointer is the caller's &mut B.
    let ctx = unsafe { &mut *ctx };
    let bus = unsafe { &mut *(ctx.bus as *mut B) };
    unsafe { sync(ctx, bus, idx) };
    let v = match w {
        1 => bus.read8(addr) as u32,
        2 => bus.read16(addr) as u32,
        _ => bus.read32(addr),
    };
    if bus.next_tick_due() {
        ctx.brk = 1;
    }
    v
}

extern "C" fn h_write<B: Bus>(ctx: *mut Ctx, addr: u32, v: u32, idx: u32, w: u32) {
    // SAFETY: as h_read.
    let ctx = unsafe { &mut *ctx };
    let bus = unsafe { &mut *(ctx.bus as *mut B) };
    unsafe { sync(ctx, bus, idx) };
    let code = bus.code_writes();
    match w {
        1 => bus.write8(addr, v as u8),
        2 => bus.write16(addr, v as u16),
        _ => bus.write32(addr, v),
    }
    // (a write to a word of RAM code: this region may include what it changed, so leave; the
    // next entry checks the region against memory)
    if bus.next_tick_due() || bus.code_writes() != code {
        ctx.brk = 1;
    }
}

/// Any op through the interpreter's own executor: bit 32 = jumped (target in the low word).
// PROF: count the call (`Cpu::jit_helpers`); chosen when compiling, so code compiled without
// profiling pays nothing for it (an unconditional call cost 10-12% on AMB / TRIBE)
extern "C" fn h_op<B: Bus, const PROF: bool>(ctx: *mut Ctx, op: *const Fast, idx: u32, next_pc: u32) -> u64 {
    // SAFETY: as h_read; `op` points at a leaked, immutable Fast.
    let ctx = unsafe { &mut *ctx };
    let bus = unsafe { &mut *(ctx.bus as *mut B) };
    let cpu = unsafe { &mut *ctx.cpu };
    unsafe { sync(ctx, bus, idx) };
    if PROF {
        cpu.profile_helper(unsafe { &*op });
    }
    let j = cpu.exec_fast(bus, unsafe { *op }, next_pc);
    if bus.next_tick_due() {
        ctx.brk = 1;
    }
    match j {
        Some(t) => (1u64 << 32) | t as u64,
        None => 0,
    }
}

// ------------------------------------------------------------------ wasm encoding

fn uleb(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn sleb(out: &mut Vec<u8>, mut v: i64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if (v == 0 && b & 0x40 == 0) || (v == -1 && b & 0x40 != 0) {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

// opcodes
const BLOCK: u8 = 0x02;
const LOOP: u8 = 0x03;
const IF: u8 = 0x04;
const ELSE: u8 = 0x05;
const END: u8 = 0x0b;
const BR: u8 = 0x0c;
const BR_TABLE: u8 = 0x0e;
const RETURN: u8 = 0x0f;
const CALL_INDIRECT: u8 = 0x11;
const LOCAL_GET: u8 = 0x20;
const LOCAL_SET: u8 = 0x21;
const LOCAL_TEE: u8 = 0x22;
const I32_LOAD: u8 = 0x28;
const I32_LOAD8_S: u8 = 0x2c;
const I32_LOAD8_U: u8 = 0x2d;
const I32_LOAD16_S: u8 = 0x2e;
const I32_LOAD16_U: u8 = 0x2f;
const I32_STORE: u8 = 0x36;
const I32_STORE8: u8 = 0x3a;
const I32_STORE16: u8 = 0x3b;
const I32_CONST: u8 = 0x41;
const I32_EQZ: u8 = 0x45;
const I32_EQ: u8 = 0x46;
const I32_NE: u8 = 0x47;
const I32_LT_S: u8 = 0x48;
const I32_LT_U: u8 = 0x49;
const I32_GT_S: u8 = 0x4a;
const I32_GT_U: u8 = 0x4b;
const I32_LE_S: u8 = 0x4c;
const I32_LE_U: u8 = 0x4d;
const I32_GE_S: u8 = 0x4e;
const I32_GE_U: u8 = 0x4f;
const F32_EQ: u8 = 0x5b;
const F32_NE: u8 = 0x5c;
const F32_LT: u8 = 0x5d;
const F32_GT: u8 = 0x5e;
const F32_LE: u8 = 0x5f;
const F32_GE: u8 = 0x60;
const I32_ADD: u8 = 0x6a;
const I32_SUB: u8 = 0x6b;
const I32_MUL: u8 = 0x6c;
const I32_AND: u8 = 0x71;
const I32_OR: u8 = 0x72;
const I32_XOR: u8 = 0x73;
const I32_SHL: u8 = 0x74;
const I32_SHR_S: u8 = 0x75;
const I32_SHR_U: u8 = 0x76;
const I32_WRAP_I64: u8 = 0xa7;
const I64_SHR_U: u8 = 0x88;
const I64_CONST: u8 = 0x42;
const I64_EQZ: u8 = 0x50;
const F32_REINTERPRET_I32: u8 = 0xbe;
const I32_REINTERPRET_F32: u8 = 0xbc;
const SELECT: u8 = 0x1b;
const F32_CEIL: u8 = 0x8d;
const F32_FLOOR: u8 = 0x8e;
const F32_TRUNC: u8 = 0x8f;
const F32_NEAREST: u8 = 0x90;
const F32_ADD: u8 = 0x92;
const F32_SUB: u8 = 0x93;
const F32_MUL: u8 = 0x94;
const F32_DIV: u8 = 0x95;
const F32_CONVERT_I32_S: u8 = 0xb2;
const F32_CONVERT_I32_U: u8 = 0xb3;
/// the `0xfc` prefix of the saturating conversions: i32.trunc_sat_f32_s is 0, i32.trunc_sat_f32_u 1
const FC_PREFIX: u8 = 0xfc;
const I32_EXTEND8_S: u8 = 0xc0;
const I32_EXTEND16_S: u8 = 0xc1;
const VOID: u8 = 0x40;
const T_I32: u8 = 0x7f;
const T_I64: u8 = 0x7e;
const T_F32: u8 = 0x7d;

// function types of the module: run, and the helpers as Rust exports them
const TY_RUN: u32 = 0;
const TY_READ: u32 = 1;
const TY_WRITE: u32 = 2;
const TY_OP: u32 = 3;

// locals: params cpu, ctx, limit, start; then i32 temporaries, the instructions done in this call
// before the current block, the block to run next, one i64, and two f32 (the FPU's fmin / fmax)
const L_CPU: u32 = 0;
const L_CTX: u32 = 1;
const L_LIMIT: u32 = 2;
const L_START: u32 = 3;
const L_A: u32 = 4;
const L_B: u32 = 5;
const L_ADDR: u32 = 6;
const L_VAL: u32 = 7;
const L_OFF: u32 = 8;
const L_DONE: u32 = 9;
const L_CUR: u32 = 10;
const L_OPRES: u32 = 11;
const L_FA: u32 = 12;
const L_FB: u32 = 13;

fn off_reg(r: u8) -> u32 {
    (offset_of!(Cpu, regs) + 4 * (r & 15) as usize) as u32
}
fn off_sp() -> u32 {
    offset_of!(Cpu, sp) as u32
}
fn off_rets() -> u32 {
    offset_of!(Cpu, rets) as u32
}
fn off_pc() -> u32 {
    offset_of!(Cpu, pc) as u32
}
fn off_hold() -> u32 {
    offset_of!(Cpu, hold_irq) as u32
}
const OFF_BRK: u32 = offset_of!(Ctx, brk) as u32;
const OFF_PRED: u32 = offset_of!(Ctx, pred) as u32;

struct Gen<'a> {
    c: Vec<u8>,
    mem: &'a JitMem,
    h_read: u32,
    h_write: u32,
    h_op: u32,
    /// the region's blocks by start pc -> index
    index: std::collections::HashMap<u32, u32>,
    /// the same as a table in memory for jumps to a pc known only at run time (`jump_local`):
    /// its address and mask; (pc, index) pairs at ((pc >> 1) & mask) * 8, collision-free
    table: Option<(u32, u32)>,
    /// emitting an if-else block's then-part: its skip (then end, else end, then start)
    pred: Option<(u32, u32, u32)>,
    /// the block being emitted, the region's size, and the `if`s open inside the block's code
    cur: u32,
    n: u32,
    nest: u32,
}

impl Gen<'_> {
    fn op(&mut self, b: u8) {
        self.c.push(b);
    }
    fn get(&mut self, l: u32) {
        self.c.push(LOCAL_GET);
        uleb(&mut self.c, l as u64);
    }
    fn set(&mut self, l: u32) {
        self.c.push(LOCAL_SET);
        uleb(&mut self.c, l as u64);
    }
    fn tee(&mut self, l: u32) {
        self.c.push(LOCAL_TEE);
        uleb(&mut self.c, l as u64);
    }
    fn i32c(&mut self, v: u32) {
        self.c.push(I32_CONST);
        sleb(&mut self.c, v as i32 as i64);
    }
    fn mem_op(&mut self, op: u8, align: u32, off: u32) {
        self.c.push(op);
        uleb(&mut self.c, align as u64);
        uleb(&mut self.c, off as u64);
    }
    /// push the guest register r
    fn ld_reg(&mut self, r: u8) {
        self.get(L_CPU);
        self.mem_op(I32_LOAD, 2, off_reg(r));
    }
    /// guest register d = local l
    fn st_reg_from(&mut self, d: u8, l: u32) {
        self.get(L_CPU);
        self.get(l);
        self.mem_op(I32_STORE, 2, off_reg(d));
    }
    fn ld_field(&mut self, off: u32) {
        self.get(L_CPU);
        self.mem_op(I32_LOAD, 2, off);
    }
    fn st_field_from(&mut self, off: u32, l: u32) {
        self.get(L_CPU);
        self.get(l);
        self.mem_op(I32_STORE, 2, off);
    }
    /// push done + count
    fn done_plus(&mut self, count: u32) {
        self.get(L_DONE);
        if count != 0 {
            self.i32c(count);
            self.op(I32_ADD);
        }
    }
    /// leave with pc = imm, `count` instructions of this block done
    fn exit_imm(&mut self, pc: u32, count: u32) {
        self.get(L_CPU);
        self.i32c(pc);
        self.mem_op(I32_STORE, 2, off_pc());
        self.done_plus(count);
        self.op(RETURN);
    }
    /// leave with pc = local l
    fn exit_local(&mut self, l: u32, count: u32) {
        self.st_field_from(off_pc(), l);
        self.done_plus(count);
        self.op(RETURN);
    }
    /// continue at the pc in local l (a return, a pc popped, a table branch, a helper's jump):
    /// the region's block there, found in its table, else back to Rust. (Every such jump used
    /// to leave: X0X went back to the dispatcher every 15 instructions.)
    fn jump_local(&mut self, l: u32, count: u32) {
        if let Some((table, mask)) = self.table {
            // L_OFF = the entry's offset; its pc there: continue at its block
            self.get(l);
            self.i32c(1);
            self.op(I32_SHR_U);
            self.i32c(mask);
            self.op(I32_AND);
            self.i32c(3);
            self.op(I32_SHL);
            self.set(L_OFF);
            self.get(L_OFF);
            self.mem_op(I32_LOAD, 2, table);
            self.get(l);
            self.op(I32_EQ);
            self.if_();
            self.get(L_OFF);
            self.mem_op(I32_LOAD, 2, table + 4);
            self.set(L_CUR);
            self.done_plus(count);
            self.set(L_DONE);
            // to the dispatch loop, as `goto`
            self.op(BR);
            uleb(&mut self.c, (self.n - 1 - self.cur + self.nest) as u64);
            self.end();
        }
        self.exit_local(l, count);
    }
    /// continue at a fixed pc, `count` instructions of this block done: the region's block there,
    /// else back to Rust. `last`: at the very end of the block's code (the next block's code
    /// follows, so going there is falling through)
    fn goto(&mut self, pc: u32, count: u32, last: bool) {
        let Some(&k) = self.index.get(&pc) else {
            self.exit_imm(pc, count);
            return;
        };
        self.done_plus(count);
        self.set(L_DONE);
        if last && self.nest == 0 && k == self.cur + 1 {
            return;
        }
        self.i32c(k);
        self.set(L_CUR);
        // to the dispatch loop: past the blocks still open after this one's code, and the ifs
        self.op(BR);
        uleb(&mut self.c, (self.n - 1 - self.cur + self.nest) as u64);
    }
    fn if_(&mut self) {
        self.op(IF);
        self.op(VOID);
        self.nest += 1;
    }
    fn end(&mut self) {
        self.op(END);
        self.nest -= 1;
    }
    /// push the helpers' instruction index of op i of this block
    fn idx(&mut self, i: u32) {
        self.done_plus(i);
    }
    fn call_ind(&mut self, ty: u32, f: u32) {
        self.i32c(f);
        self.op(CALL_INDIRECT);
        uleb(&mut self.c, ty as u64);
        self.op(0); // table 0
    }
    /// After an op that may have called the bus: leave if the next tick is due (and, between a
    /// glued pair's two instructions, keep interrupts back until the second has run).
    fn check_brk(&mut self, next_pc: u32, count: u32, hold: bool) {
        self.get(L_CTX);
        self.mem_op(I32_LOAD, 2, OFF_BRK);
        self.if_();
        if hold {
            self.get(L_CPU);
            self.i32c(1);
            self.mem_op(I32_STORE, 2, off_hold());
        }
        match self.pred {
            // inside an if-else block's then-part: at its end the skip is taken (as `finish`),
            // before it the skip goes with the exit (`run` puts it in pred_skips)
            Some((te, ee, _)) if next_pc == te => self.exit_imm(ee, count),
            Some((te, ee, ts)) => {
                self.get(L_CTX);
                self.i32c(te);
                self.mem_op(I32_STORE, 2, OFF_PRED);
                self.get(L_CTX);
                self.i32c(ee);
                self.mem_op(I32_STORE, 2, OFF_PRED + 4);
                self.get(L_CTX);
                self.i32c(ts);
                self.mem_op(I32_STORE, 2, OFF_PRED + 8);
                self.exit_imm(next_pc, count);
            }
            None => self.exit_imm(next_pc, count),
        }
        self.end();
    }

    /// push alu(op, local a, local b) (wasm shifts take the count mod 32, as `alu`)
    fn alu(&mut self, op: Op) {
        match op {
            Op::RSub => {
                self.get(L_B);
                self.get(L_A);
                self.op(I32_SUB);
                return;
            }
            Op::AndNot => {
                self.get(L_A);
                self.get(L_B);
                self.i32c(u32::MAX);
                self.op(I32_XOR);
                self.op(I32_AND);
                return;
            }
            _ => {}
        }
        self.get(L_A);
        self.get(L_B);
        self.op(match op {
            Op::Add => I32_ADD,
            Op::Sub => I32_SUB,
            Op::And => I32_AND,
            Op::Or => I32_OR,
            Op::Xor => I32_XOR,
            Op::Mul => I32_MUL,
            Op::Shl => I32_SHL,
            Op::Shr => I32_SHR_U,
            Op::Sar => I32_SHR_S,
            Op::RSub | Op::AndNot => unreachable!(),
        });
    }
    /// local b = an A4 / BitBr right-hand side: rk 0 register, 1 immediate, 2 `1 << reg`
    fn rhs(&mut self, rk: u8, rv: u32) {
        match rk {
            0 => self.ld_reg(rv as u8),
            1 => self.i32c(rv),
            _ => {
                self.i32c(1);
                self.ld_reg(rv as u8);
                self.op(I32_SHL);
            }
        }
        self.set(L_B);
    }
    /// push the compare of locals a and b
    fn cmp(&mut self, c: Cond) {
        let float = c.is_float();
        self.get(L_A);
        if float {
            self.op(F32_REINTERPRET_I32);
        }
        self.get(L_B);
        if float {
            self.op(F32_REINTERPRET_I32);
        }
        let (op, negate) = match c {
            Cond::Eq => (I32_EQ, false),
            Cond::Ne => (I32_NE, false),
            Cond::LtU => (I32_LT_U, false),
            Cond::GtU => (I32_GT_U, false),
            Cond::LeU => (I32_LE_U, false),
            Cond::GeU => (I32_GE_U, false),
            Cond::LtS => (I32_LT_S, false),
            Cond::GtS => (I32_GT_S, false),
            Cond::LeS => (I32_LE_S, false),
            Cond::GeS => (I32_GE_S, false),
            // as `fcond`: the unordered forms are the negations
            Cond::FEq => (F32_EQ, false),
            Cond::FUne => (F32_NE, false),
            Cond::FLt => (F32_LT, false),
            Cond::FUge => (F32_LT, true),
            Cond::FLe => (F32_LE, false),
            Cond::FUgt => (F32_LE, true),
            Cond::FGe => (F32_GE, false),
            Cond::FUlt => (F32_GE, true),
            Cond::FGt => (F32_GT, false),
            Cond::FUle => (F32_GT, true),
        };
        self.op(op);
        if negate {
            self.op(I32_EQZ);
        }
    }

    /// local val = load(w, signed) at guest address local addr: inline RAM / XIP (the Soc's fast
    /// paths: the word or halfword must lie inside), else the bus.
    fn load(&mut self, w: u8, signed: bool, idx: u32) {
        let mem = *self.mem;
        let ld = match (w, signed) {
            (4, _) => I32_LOAD,
            (2, false) => I32_LOAD16_U,
            (2, true) => I32_LOAD16_S,
            (1, false) => I32_LOAD8_U,
            _ => I32_LOAD8_S,
        };
        let align = match w { 4 => 2, 2 => 1, _ => 0 };
        for (gbase, len, host) in [(mem.ram_base, mem.ram_len, mem.ram as u32), (mem.xip_base, mem.xip_len, mem.xip as u32)] {
            self.get(L_ADDR);
            self.i32c(gbase);
            self.op(I32_SUB);
            if w == 2 {
                self.i32c(!1u32); // a halfword is the one at addr & !1 (Soc read16)
                self.op(I32_AND);
            }
            self.tee(L_OFF);
            self.i32c(len.saturating_sub(w as u32));
            self.op(I32_LE_U);
            self.if_();
            self.get(L_OFF);
            self.mem_op(ld, align, host);
            self.set(L_VAL);
            self.op(ELSE);
        }
        self.get(L_CTX);
        self.get(L_ADDR);
        self.idx(idx);
        self.i32c(w as u32);
        self.call_ind(TY_READ, self.h_read);
        match (w, signed) {
            (2, true) => self.op(I32_EXTEND16_S),
            (1, true) => self.op(I32_EXTEND8_S),
            _ => {}
        }
        self.set(L_VAL);
        self.end();
        self.end();
    }

    /// store(w) of local val at guest address local addr: inline RAM, else the bus (and so is a
    /// store to a word of RAM code: the bus counts it, see `Bus::code_writes`).
    fn store(&mut self, w: u8, idx: u32) {
        let mem = *self.mem;
        self.get(L_ADDR);
        self.i32c(mem.ram_base);
        self.op(I32_SUB);
        self.tee(L_OFF);
        self.i32c(mem.ram_len.saturating_sub(w as u32));
        self.op(I32_LE_U);
        if w == 2 {
            // an odd halfword goes the Soc's merging way
            self.get(L_ADDR);
            self.i32c(1);
            self.op(I32_AND);
            self.op(I32_EQZ);
            self.op(I32_AND);
        }
        self.if_();
        self.get(L_OFF);
        self.i32c(2);
        self.op(I32_SHR_U);
        self.mem_op(I32_LOAD8_U, 0, mem.ram_code as u32);
        self.op(I32_EQZ);
        self.if_();
        self.get(L_OFF);
        self.get(L_VAL);
        match w {
            4 => self.mem_op(I32_STORE, 2, mem.ram as u32),
            2 => self.mem_op(I32_STORE16, 1, mem.ram as u32),
            _ => self.mem_op(I32_STORE8, 0, mem.ram as u32),
        }
        self.op(ELSE);
        self.store_bus(w, idx);
        self.end();
        self.op(ELSE);
        self.store_bus(w, idx);
        self.end();
    }

    /// the bus's write of local val at local addr
    fn store_bus(&mut self, w: u8, idx: u32) {
        self.get(L_CTX);
        self.get(L_ADDR);
        self.get(L_VAL);
        self.idx(idx);
        self.i32c(w as u32);
        self.call_ind(TY_WRITE, self.h_write);
    }

    /// local addr = base + off
    fn addr(&mut self, b: Base, off: u32) {
        match b {
            Base::Reg(r) => self.ld_reg(r),
            Base::Sp => self.ld_field(off_sp()),
        }
        if off != 0 {
            self.i32c(off);
            self.op(I32_ADD);
        }
        self.set(L_ADDR);
    }
}

/// The module for a region: blocks (id, start pc, plain micro-ops: none careful), the hot one
/// first.
fn compile<B: Bus>(blocks: &[(usize, u32, Vec<crate::BOp>)], mem: &JitMem, profile: bool) -> Vec<u8> {
    let n = blocks.len() as u32;
    let mut g = Gen {
        c: Vec::with_capacity(64 * blocks.iter().map(|b| b.2.len()).sum::<usize>()),
        mem,
        h_read: h_read::<B> as usize as u32,
        h_write: h_write::<B> as usize as u32,
        h_op: if profile { h_op::<B, true> as usize as u32 } else { h_op::<B, false> as usize as u32 },
        index: blocks.iter().enumerate().map(|(k, b)| (b.1, k as u32)).collect(),
        table: jump_table(blocks),
        pred: None,
        cur: 0,
        n,
        nest: 0,
    };
    // loop { block_{n-1} { .. block_0 { br_table cur } code_0 } code_1 .. } code_{n-1} }: a br
    // to depth k from the innermost block lands on code_k; code_k reaches the loop through the
    // n - 1 - k blocks still open
    g.get(L_START);
    g.set(L_CUR);
    g.op(LOOP);
    g.op(VOID);
    for _ in 0..n {
        g.op(BLOCK);
        g.op(VOID);
    }
    g.get(L_CUR);
    g.op(BR_TABLE);
    uleb(&mut g.c, n as u64);
    for k in 0..n {
        uleb(&mut g.c, k as u64);
    }
    uleb(&mut g.c, (n - 1) as u64);
    for (k, (_, pc0, ops)) in blocks.iter().enumerate() {
        g.op(END);
        g.cur = k as u32;
        g.nest = 0;
        block::<B>(&mut g, *pc0, ops);
    }
    g.op(END); // loop
    g.op(0x00); // unreachable: every block's code leaves or branches
    g.op(END);
    module(&g.c)
}

/// The code of one block of a region.
fn block<B: Bus>(g: &mut Gen<'_>, pc0: u32, ops: &[crate::BOp]) {
    // the whole block must fit the call's limit, else back to Rust before running any of it
    g.done_plus(ops.len() as u32);
    g.get(L_LIMIT);
    g.op(I32_GT_U);
    g.if_();
    g.exit_imm(pc0, 0);
    g.end();

    let mut pc = pc0;
    let mut ended = false;
    for (i, op) in ops.iter().enumerate() {
        let idx = i as u32;
        let next_pc = pc + op.len as u32;
        let count = idx + 1;
        let mut bus_used = false;
        match op.fast {
            Fast::Nop => {}
            Fast::MovI { d, v } => {
                g.get(L_CPU);
                g.i32c(v);
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::Mov { d, s } => {
                g.get(L_CPU);
                g.ld_reg(s);
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::AddRI { d, s, v } => {
                g.get(L_CPU);
                g.ld_reg(s);
                g.i32c(v);
                g.op(I32_ADD);
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::ShlI { d, s, n } | Fast::SarI { d, s, n } => {
                g.get(L_CPU);
                g.ld_reg(s);
                g.i32c(n as u32 & 31);
                g.op(if matches!(op.fast, Fast::ShlI { .. }) { I32_SHL } else { I32_SHR_S });
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::Alu3 { d, a, b, op } => {
                g.ld_reg(a);
                g.set(L_A);
                g.ld_reg(b);
                g.set(L_B);
                g.get(L_CPU);
                g.alu(op);
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::Alu3I { d, a, v, op } => {
                g.ld_reg(a);
                g.set(L_A);
                g.i32c(v);
                g.set(L_B);
                g.get(L_CPU);
                g.alu(op);
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::Alu2 { d, b, op } => {
                g.ld_reg(d);
                g.set(L_A);
                g.ld_reg(b);
                g.set(L_B);
                g.get(L_CPU);
                g.alu(op);
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::Alu2I { d, v, op } => {
                g.ld_reg(d);
                g.set(L_A);
                g.i32c(v);
                g.set(L_B);
                g.get(L_CPU);
                g.alu(op);
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::A4 { d, s, op, rk, rv } => {
                g.rhs(rk, rv);
                g.ld_reg(s);
                g.set(L_A);
                g.get(L_CPU);
                g.alu(op);
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::MemA4 { b, off, op, rk, rv } => {
                // [R + off] = [R + off] op rhs: one read and one write, as the interpreter
                g.rhs(rk, rv);
                g.addr(Base::Reg(b), off as u32);
                g.load(4, false, idx);
                g.get(L_VAL);
                g.set(L_A);
                g.alu(op);
                g.set(L_VAL);
                g.store(4, idx);
                bus_used = true;
            }
            Fast::BitExt { d, s, p, l, signed } => {
                let (p, l) = (p as u32 & 31, l as u32 & 31);
                g.get(L_CPU);
                g.ld_reg(s);
                g.i32c(p);
                g.op(I32_SHR_U);
                if signed {
                    if l != 0 {
                        g.i32c(32 - l);
                        g.op(I32_SHL);
                        g.i32c(32 - l);
                        g.op(I32_SHR_S);
                    }
                } else {
                    g.i32c(if l == 0 { u32::MAX } else { (1u32 << l).wrapping_sub(1) });
                    g.op(I32_AND);
                }
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::BitIns { d, s, p, l } => {
                let (p, l) = (p as u32 & 31, l as u32 & 31);
                let mask = if l == 0 { u32::MAX } else { (1u32 << l).wrapping_sub(1) };
                g.get(L_CPU);
                g.ld_reg(d);
                g.i32c(!(mask << p));
                g.op(I32_AND);
                g.ld_reg(s);
                g.i32c(mask);
                g.op(I32_AND);
                g.i32c(p);
                g.op(I32_SHL);
                g.op(I32_OR);
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::SetIe { on: false } => {
                // `cli` (`sti` goes through the helper: it opens the bus's interrupt window)
                g.get(L_CPU);
                g.i32c(0);
                g.mem_op(I32_STORE8, 0, offset_of!(Cpu, ie) as u32);
            }
            Fast::Mov2 { dh, dl, sh, sl } => {
                // both read before either is written
                g.ld_reg(sh);
                g.set(L_A);
                g.ld_reg(sl);
                g.set(L_B);
                g.st_reg_from(dh, L_A);
                g.st_reg_from(dl, L_B);
            }
            Fast::Mov2I { dh, dl, v } => {
                g.get(L_CPU);
                g.i32c(v);
                g.mem_op(I32_STORE, 2, off_reg(dl));
                g.get(L_CPU);
                g.i32c(((v as i32) >> 31) as u32);
                g.mem_op(I32_STORE, 2, off_reg(dh));
            }
            Fast::Ext { d, s, part, signed } => {
                g.get(L_CPU);
                g.ld_reg(s);
                match (part, signed) {
                    (0, false) => {
                        g.i32c(0xff);
                        g.op(I32_AND);
                    }
                    (0, true) => g.op(I32_EXTEND8_S),
                    (1, false) => {
                        g.i32c(0xffff);
                        g.op(I32_AND);
                    }
                    (1, true) => g.op(I32_EXTEND16_S),
                    (_, false) => {
                        g.i32c(16);
                        g.op(I32_SHR_U);
                    }
                    (_, true) => {
                        g.i32c(16);
                        g.op(I32_SHR_S);
                    }
                }
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::MinMax { d, a, b, kind } => {
                g.ld_reg(a);
                g.set(L_A);
                g.ld_reg(b);
                g.set(L_B);
                g.get(L_CPU);
                g.get(L_A);
                g.get(L_B);
                g.get(L_A);
                g.get(L_B);
                // select(a, b, a OP b): smin a <s b, smax a >s b, umin a <u b, umax a >u b
                g.op(match kind { 0 => I32_LT_S, 1 => I32_GT_S, 2 => I32_LT_U, _ => I32_GT_U });
                g.op(SELECT);
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            // ---- the FPU (fm1-isa's fpu.rs, the interpreter's `fpu`), as wasm's own f32 ops,
            // which are the IEEE single precision Rust's are in this build: + - * / as they
            // are (NaN payloads included: the engine's own instructions make them for both);
            // fmin / fmax as LLVM expands Rust's `f32::min` / `max` for wasm (a NaN
            // operand yields the other one, equal operands yield b; wasm's f32.min would
            // propagate the NaN); the conversions as the `as` casts: trunc_sat saturates and
            // sends a NaN to 0, the rounding done first by nearest / ceil / floor (trunc_sat
            // truncates by itself). The multiply-accumulates 7 / 8 stay with the executor, as in
            // jit.rs. (`fop_selftest` checks each against the executor in the engine at hand.)
            Fast::FOp { d, a, b, op } if fop_inline(op) => {
                let k = op >> 4;
                let to_int = op & 0xf == 0xf && k < 8;
                g.get(L_CPU);
                match op {
                    0..=3 => {
                        // (+ and * take b first: LLVM commuted them in the interpreter's build,
                        // and when both operands are NaNs the engine passes the first one's on;
                        // `fop_selftest` tells when a build changes that)
                        let (first, second) = if op & 1 == 0 { (b, a) } else { (a, b) };
                        g.ld_reg(first);
                        g.op(F32_REINTERPRET_I32);
                        g.ld_reg(second);
                        g.op(F32_REINTERPRET_I32);
                        g.op(match op { 0 => F32_ADD, 1 => F32_SUB, 2 => F32_MUL, _ => F32_DIV });
                    }
                    5 | 6 => {
                        // a' = a != a ? b : a; b' = b != b ? a' : b; a' < b' ? a' : b' (max: >)
                        g.ld_reg(a);
                        g.op(F32_REINTERPRET_I32);
                        g.set(L_FA);
                        g.ld_reg(b);
                        g.op(F32_REINTERPRET_I32);
                        g.set(L_FB);
                        g.get(L_FB);
                        g.get(L_FA);
                        g.get(L_FA);
                        g.get(L_FA);
                        g.op(F32_NE);
                        g.op(SELECT);
                        g.tee(L_FA);
                        g.get(L_FA);
                        g.get(L_FB);
                        g.get(L_FB);
                        g.get(L_FB);
                        g.op(F32_NE);
                        g.op(SELECT);
                        g.tee(L_FB);
                        g.get(L_FA);
                        g.get(L_FB);
                        g.op(if op == 5 { F32_LT } else { F32_GT });
                        g.op(SELECT);
                    }
                    0x8f | 0x9f => {
                        g.ld_reg(a);
                        g.op(if op == 0x8f { F32_CONVERT_I32_S } else { F32_CONVERT_I32_U });
                    }
                    _ => {
                        g.ld_reg(a);
                        g.op(F32_REINTERPRET_I32);
                        match k & 3 {
                            0 => g.op(F32_NEAREST),
                            1 if !to_int => g.op(F32_TRUNC),
                            1 => {}
                            2 => g.op(F32_CEIL),
                            _ => g.op(F32_FLOOR),
                        }
                        if to_int {
                            g.op(FC_PREFIX);
                            g.op(if k < 4 { 0 } else { 1 });
                        }
                    }
                }
                if !to_int {
                    g.op(I32_REINTERPRET_F32);
                }
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::SpAdd { v } => {
                g.get(L_CPU);
                g.ld_field(off_sp());
                g.i32c(v);
                g.op(I32_ADD);
                g.mem_op(I32_STORE, 2, off_sp());
            }
            Fast::SpFromR { s } => {
                g.get(L_CPU);
                g.ld_reg(s);
                g.mem_op(I32_STORE, 2, off_sp());
            }
            Fast::RFromSp { d, v } => {
                g.get(L_CPU);
                g.ld_field(off_sp());
                g.i32c(v);
                g.op(I32_ADD);
                g.mem_op(I32_STORE, 2, off_reg(d));
            }
            Fast::Ld { d, b, off, w, signed } => {
                g.addr(b, off);
                g.load(w, signed, idx);
                g.st_reg_from(d, L_VAL);
                bus_used = true;
            }
            Fast::LdW { d, b, off } => {
                g.addr(Base::Reg(b), off);
                g.load(4, false, idx);
                g.st_reg_from(d, L_VAL);
                bus_used = true;
            }
            Fast::LdWSp { d, off } => {
                g.addr(Base::Sp, off);
                g.load(4, false, idx);
                g.st_reg_from(d, L_VAL);
                bus_used = true;
            }
            Fast::LdBu { d, b, off } => {
                g.addr(Base::Reg(b), off);
                g.load(1, false, idx);
                g.st_reg_from(d, L_VAL);
                bus_used = true;
            }
            Fast::St { s, b, off, w } => {
                g.addr(b, off);
                g.ld_reg(s);
                g.set(L_VAL);
                g.store(w, idx);
                bus_used = true;
            }
            Fast::StW { s, b, off } => {
                g.addr(Base::Reg(b), off);
                g.ld_reg(s);
                g.set(L_VAL);
                g.store(4, idx);
                bus_used = true;
            }
            Fast::StWSp { s, off } => {
                g.addr(Base::Sp, off);
                g.ld_reg(s);
                g.set(L_VAL);
                g.store(4, idx);
                bus_used = true;
            }
            Fast::LdIdx { d, b, i, sh, w, signed } => {
                g.ld_reg(b);
                g.ld_reg(i);
                if sh != 0 {
                    g.i32c(sh as u32);
                    g.op(I32_SHL);
                }
                g.op(I32_ADD);
                g.set(L_ADDR);
                g.load(w, signed, idx);
                g.st_reg_from(d, L_VAL);
                bus_used = true;
            }
            Fast::StIdx { s, b, i, sh, w } => {
                g.ld_reg(b);
                g.ld_reg(i);
                if sh != 0 {
                    g.i32c(sh as u32);
                    g.op(I32_SHL);
                }
                g.op(I32_ADD);
                g.set(L_ADDR);
                g.ld_reg(s);
                g.set(L_VAL);
                g.store(w, idx);
                bus_used = true;
            }
            Fast::LdPost { d, b, w, signed, inc } => {
                // d = [b]; then b = b + inc (b read after the load, as exec_fast)
                g.ld_reg(b);
                g.set(L_ADDR);
                g.load(w, signed, idx);
                g.st_reg_from(d, L_VAL);
                g.get(L_CPU);
                g.ld_reg(b);
                g.i32c(inc);
                g.op(I32_ADD);
                g.mem_op(I32_STORE, 2, off_reg(b));
                bus_used = true;
            }
            Fast::StPost { s, b, w, inc } => {
                g.ld_reg(b);
                g.set(L_ADDR);
                g.ld_reg(s);
                g.set(L_VAL);
                g.store(w, idx);
                g.get(L_CPU);
                g.ld_reg(b);
                g.i32c(inc);
                g.op(I32_ADD);
                g.mem_op(I32_STORE, 2, off_reg(b));
                bus_used = true;
            }
            Fast::Rmw { b, w, op: rop, off, v } => {
                g.addr(Base::Reg(b), off);
                if !matches!(rop, RmwOp::Mov) {
                    g.load(w, false, idx);
                    g.get(L_VAL);
                    g.i32c(v);
                    g.op(match rop {
                        RmwOp::Or => I32_OR,
                        RmwOp::And => I32_AND,
                        RmwOp::Xor => I32_XOR,
                        RmwOp::Add => I32_ADD,
                        RmwOp::Sub => I32_SUB,
                        RmwOp::Mov => unreachable!(),
                    });
                } else {
                    g.i32c(v);
                }
                g.set(L_VAL);
                g.store(w, idx);
                bus_used = true;
            }
            Fast::PushRet => {
                // sp -= 4; [sp] = rets
                g.ld_field(off_sp());
                g.i32c(4);
                g.op(I32_SUB);
                g.set(L_ADDR);
                g.st_field_from(off_sp(), L_ADDR);
                g.ld_field(off_rets());
                g.set(L_VAL);
                g.store(4, idx);
                bus_used = true;
            }
            Fast::PopRet => {
                // pc = [sp]; sp += 4
                g.ld_field(off_sp());
                g.set(L_ADDR);
                g.load(4, false, idx);
                g.get(L_CPU);
                g.get(L_ADDR);
                g.i32c(4);
                g.op(I32_ADD);
                g.mem_op(I32_STORE, 2, off_sp());
                g.jump_local(L_VAL, count);
                ended = true;
            }
            Fast::PushRets { n } => {
                // a function's prologue, as the interpreter: rets, then r_top .. r_bot, each at
                // sp -= 4 (a helper call between them sees the same sp)
                let n = n as i32;
                let (top, bot) = (n.max(3), n.min(4));
                for r in std::iter::once(-1i32).chain((bot..=top).rev()) {
                    g.ld_field(off_sp());
                    g.i32c(4);
                    g.op(I32_SUB);
                    g.set(L_ADDR);
                    g.st_field_from(off_sp(), L_ADDR);
                    if r < 0 {
                        g.ld_field(off_rets());
                    } else {
                        g.ld_reg(r as u8);
                    }
                    g.set(L_VAL);
                    g.store(4, idx);
                }
                bus_used = true;
            }
            Fast::PopPc { n } => {
                // its epilogue: r_bot .. r_top = [sp++] each, then pc = [sp++]
                let n = n as i32;
                let (top, bot) = (n.max(3), n.min(4));
                for r in bot..=top {
                    g.ld_field(off_sp());
                    g.set(L_ADDR);
                    g.load(4, false, idx);
                    g.st_reg_from(r as u8, L_VAL);
                    g.get(L_CPU);
                    g.get(L_ADDR);
                    g.i32c(4);
                    g.op(I32_ADD);
                    g.mem_op(I32_STORE, 2, off_sp());
                }
                g.ld_field(off_sp());
                g.set(L_ADDR);
                g.load(4, false, idx);
                g.get(L_CPU);
                g.get(L_ADDR);
                g.i32c(4);
                g.op(I32_ADD);
                g.mem_op(I32_STORE, 2, off_sp());
                g.jump_local(L_VAL, count);
                ended = true;
            }
            Fast::BrI { a, v, cond, target } => {
                g.ld_reg(a);
                g.set(L_A);
                g.i32c(v);
                g.set(L_B);
                g.cmp(cond);
                g.if_();
                g.goto(target, count, false);
                g.end();
            }
            Fast::BrIEq { a, v, target } | Fast::BrINe { a, v, target } => {
                g.ld_reg(a);
                g.i32c(v);
                g.op(if matches!(op.fast, Fast::BrIEq { .. }) { I32_EQ } else { I32_NE });
                g.if_();
                g.goto(target, count, false);
                g.end();
            }
            Fast::BrR { a, b, cond, target } => {
                g.ld_reg(a);
                g.set(L_A);
                g.ld_reg(b);
                g.set(L_B);
                g.cmp(cond);
                g.if_();
                g.goto(target, count, false);
                g.end();
            }
            Fast::BitBr { r, mk, ne, mv, target } => {
                g.rhs(mk, mv);
                g.ld_reg(r);
                g.get(L_B);
                g.op(I32_AND);
                if !ne {
                    g.op(I32_EQZ);
                }
                g.if_();
                g.goto(target, count, false);
                g.end();
            }
            Fast::DecBr { r, cond, v, target } => {
                g.ld_reg(r);
                g.i32c(1);
                g.op(I32_SUB);
                g.set(L_A);
                g.st_reg_from(r, L_A);
                g.i32c(v);
                g.set(L_B);
                g.cmp(cond);
                g.if_();
                g.goto(target, count, false);
                g.end();
            }
            Fast::IfBlk { a, rk, fam, alt, has_else: true, rv, then_d, else_d } if fam < 8 && g.pred.is_none() => {
                // the region brought its then-part along (`Cpu::wasm_region`): the rest of this
                // block's ops. Not taken: on at the else part. Taken: the then-part, then past
                // the else part (the interpreter's pred_skips, kept by check_brk on an exit)
                let then_end = next_pc.wrapping_add(then_d as u32);
                let else_end = then_end.wrapping_add(else_d as u32);
                g.rhs(rk, rv);
                g.ld_reg(a);
                g.set(L_A);
                if fam == 2 {
                    g.get(L_A);
                    g.get(L_B);
                    g.op(I32_AND);
                    g.i32c(0);
                    g.op(I32_NE);
                    g.i32c(alt as u32);
                    g.op(I32_EQ);
                } else {
                    g.get(L_A);
                    g.get(L_B);
                    g.op(match fam { 0 => I32_EQ, 1 => I32_GE_U, 4 => I32_GT_U, 5 => I32_GE_S, _ => I32_GT_S });
                    if alt {
                        g.op(I32_EQZ);
                    }
                }
                g.op(I32_EQZ);
                g.if_();
                g.goto(then_end, count, false);
                g.end();
                if i + 1 == ops.len() {
                    // an empty then-part
                    g.goto(else_end, count, true);
                    ended = true;
                    break;
                }
                g.pred = Some((then_end, else_end, next_pc));
                pc = next_pc;
                continue;
            }
            Fast::IfBlk { a, rk, fam, alt, has_else: false, rv, then_d, .. } if fam < 8 => {
                // taken: the then-part follows; not taken: skip it
                g.rhs(rk, rv);
                g.ld_reg(a);
                g.set(L_A);
                if fam == 2 {
                    g.get(L_A);
                    g.get(L_B);
                    g.op(I32_AND);
                    g.i32c(0);
                    g.op(I32_NE);
                    g.i32c(alt as u32);
                    g.op(I32_EQ);
                } else {
                    g.get(L_A);
                    g.get(L_B);
                    g.op(match fam { 0 => I32_EQ, 1 => I32_GE_U, 4 => I32_GT_U, 5 => I32_GE_S, _ => I32_GT_S });
                    if alt {
                        g.op(I32_EQZ);
                    }
                }
                g.op(I32_EQZ);
                g.if_();
                g.goto(next_pc.wrapping_add(then_d as u32), count, false);
                g.end();
            }
            Fast::Goto { target } => {
                g.goto(target, count, true);
                ended = true;
            }
            Fast::Call { target } => {
                g.get(L_CPU);
                g.i32c(next_pc);
                g.mem_op(I32_STORE, 2, off_rets());
                g.goto(target, count, true);
                ended = true;
            }
            Fast::PopRets => {
                g.ld_field(off_sp()); g.set(L_ADDR);
                g.load(4, false, idx);
                g.get(L_CPU); g.get(L_VAL); g.mem_op(I32_STORE, 2, off_rets());
                g.get(L_CPU); g.get(L_ADDR); g.i32c(4); g.op(I32_ADD);
                g.mem_op(I32_STORE, 2, off_sp());
                bus_used = true;
            }
            Fast::Tb { r, half } => {
                g.ld_reg(r); g.i32c(next_pc); g.op(I32_ADD); g.set(L_ADDR);
                g.load(if half { 2 } else { 1 }, false, idx);
                g.get(L_VAL); g.i32c(1); g.op(I32_SHL); g.i32c(next_pc); g.op(I32_ADD);
                g.set(L_VAL); g.jump_local(L_VAL, count);
                ended = true;
            }
            Fast::Rts => {
                g.ld_field(off_rets());
                g.set(L_VAL);
                g.jump_local(L_VAL, count);
                ended = true;
            }
            // ---- everything else: the interpreter's executor for this one op
            other => {
                let p: &'static Fast = Box::leak(Box::new(other));
                g.get(L_CTX);
                g.i32c(p as *const Fast as u32);
                g.idx(idx);
                g.i32c(next_pc);
                g.call_ind(TY_OP, g.h_op);
                g.c.push(LOCAL_TEE);
                uleb(&mut g.c, L_OPRES as u64);
                g.c.push(I64_CONST);
                sleb(&mut g.c, 32);
                g.op(I64_SHR_U);
                g.op(I64_EQZ);
                g.op(I32_EQZ);
                g.if_();
                g.get(L_OPRES);
                g.op(I32_WRAP_I64);
                g.set(L_VAL);
                g.jump_local(L_VAL, count);
                g.end();
                bus_used = true;
            }
        }
        if ended {
            break;
        }
        if bus_used && !op.fast.may_jump() {
            g.check_brk(next_pc, count, op.glue);
        }
        pc = next_pc;
        if let Some((te, ee, _)) = g.pred.filter(|&(te, _, _)| next_pc == te) {
            // the then-part's end: past the else part
            let _ = te;
            g.pred = None;
            g.goto(ee, count, true);
            ended = true;
            break;
        }
        if op.fast.may_jump() {
            // (the block's last op): it fell through
            g.goto(next_pc, count, true);
            ended = true;
            break;
        }
    }
    if !ended {
        // fell off the end of the prefix
        g.goto(pc, ops.len() as u32, true);
    }
}

/// The FPU ops `block` computes inline: the binary + - * / fmin fmax, the conversions ftoi /
/// ftou in every rounding, itof, utof, and ftof in every rounding (fm1-isa's fpu.rs). Not the
/// multiply-accumulates 7 / 8 (the executor, as jit.rs), nor an op the table does not decode.
fn fop_inline(op: u8) -> bool {
    matches!(op, 0..=3 | 5 | 6 | 0x0f | 0x1f | 0x2f | 0x3f | 0x4f | 0x5f | 0x6f | 0x7f | 0x8f | 0x9f | 0xcf | 0xdf | 0xef | 0xff)
}

static mut SELFTEST_DETAIL: String = String::new();

/// The mismatches `fop_selftest` found, the first of each op and register choice, one per line
/// (empty: none).
#[allow(static_mut_refs)]
pub fn fop_selftest_detail() -> &'static str {
    unsafe { SELFTEST_DETAIL.as_str() }
}

/// Every FPU op the generated code computes inline, against the interpreter in the engine at
/// hand (web/jit.test.mjs calls it through `fm1_jit_selftest`): each op alone in a block, on
/// every pair of the special values (NaNs with payloads, quiet and signalling, both zeros,
/// denormals, infinities, the integers' bounds, halves) and on random words, must leave the
/// registers and pc the executor leaves, and so must the interpreter proper (`run_fast` on the
/// instruction word); into a fresh register, into a source register, with both sources the same
/// register, and into the second source. The mismatches (`fop_selftest_detail` describes the
/// first of each op and register choice); a module the host could not compile counts as one.
#[allow(static_mut_refs)]
pub fn fop_selftest() -> u32 {
    /// 16 bytes of RAM at 0x0200_0000 holding the instruction under test (the interpreter
    /// decodes and runs it from there: `run_fast`), nothing else
    struct NoBus([u8; 16]);
    impl Bus for NoBus {
        fn read8(&mut self, a: u32) -> u8 { self.0.get(a.wrapping_sub(0x0200_0000) as usize).copied().unwrap_or(0) }
        fn read16(&mut self, a: u32) -> u16 { self.read8(a) as u16 | (self.read8(a + 1) as u16) << 8 }
        fn read32(&mut self, a: u32) -> u32 { self.read16(a) as u32 | (self.read16(a + 2) as u32) << 16 }
        fn write8(&mut self, _: u32, _: u8) {}
        fn write16(&mut self, _: u32, _: u16) {}
        fn write32(&mut self, _: u32, _: u32) {}
        fn tick_room(&self) -> u64 { 1 << 40 }
    }
    let mem = JitMem { ram: std::ptr::null_mut(), ram_base: 0, ram_len: 0, ram_code: std::ptr::null(), xip: std::ptr::null(), xip_base: 0, xip_len: 0 };
    let specials: Vec<u32> = [0.0f32, -0.0, 1.0, -1.0, 0.5, -0.5, 1.5, -1.5, 2.5, -2.5, 0.49999997, 3.25, -7.75, 1e-3, 65536.25,
        8388607.5, 16777216.0, 123456.7, 2147483520.0, 2147483648.0, -2147483648.0, -2147483904.0, 4294967040.0, 4294967296.0,
        3e9, -3e9, 1e-40, -1e-40, f32::MIN_POSITIVE, f32::MAX, f32::MIN, f32::INFINITY, f32::NEG_INFINITY, f32::NAN, -f32::NAN]
        .iter().map(|f| f.to_bits())
        .chain([1u32, 0x8000_0001, 0x007f_ffff, 0x7f80_0001, 0xff80_0001, 0x7fc1_2345, 0xffc0_0001, 0x7fff_ffff, 0xffff_ffff])
        .collect();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = move || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed as u32 };
    let n = specials.len();
    let (mut fails, mut bus) = (0u32, NoBus([0; 16]));
    unsafe { SELFTEST_DETAIL.clear(); }
    // (per op and register choice: the mismatches, the first described)
    let mut seen: Vec<(u8, u8, u8, u8)> = Vec::new();
    let mut fail = |op: u8, d: u8, a: u8, b: u8, x: u32, y: u32, msg: String| {
        fails += 1;
        if !seen.contains(&(op, d, a, b)) {
            seen.push((op, d, a, b));
            unsafe { SELFTEST_DETAIL += &format!("op {op:02x} (d {d} a {a} b {b}) a {x:08x} b {y:08x}: {msg}\n"); }
        }
    };
    for op in (0..=0xffu8).filter(|&op| fop_inline(op)) {
        for (d, a, b) in [(0u8, 1u8, 2u8), (1, 1, 2), (0, 1, 1), (2, 1, 2)] {
            let fast = Fast::FOp { d, a, b, op };
            let bop = crate::BOp { fast, len: 4, careful: false, pair_head: false, glue: false };
            // the instruction word (fm1-isa's fpu.rs): `3f e5 b2 b3`, binary b2 = a << 4 | op,
            // b3 = d << 4 | b; unary b2 = op, b3 = d << 4 | a
            let (b2, b3) = if op <= 6 { (a << 4 | op, d << 4 | b) } else { (op, d << 4 | a) };
            bus.0 = [0; 16];
            bus.0[..4].copy_from_slice(&[0x3f, 0xe5, b2, b3]);
            let m = compile::<NoBus>(&[(0, 0x0200_0000, vec![bop])], &mem, false);
            // SAFETY: a plain byte slice; the host copies it before returning
            let f = unsafe { fm1_jit_compile(m.as_ptr(), m.len()) };
            if f == 0 {
                fail(op, d, a, b, 0, 0, "the host could not compile the module".to_owned());
                continue;
            }
            let binary = op <= 6;
            let cases = if binary { n * n } else { n } + 256;
            for i in 0..cases {
                let (x, y) = if i < n * n && binary { (specials[i / n], specials[i % n]) }
                    else if i < n { (specials[i], rnd()) }
                    else { (rnd(), rnd()) };
                let mut cpu = Cpu::new(0x0200_0000);
                for r in 0..16 { cpu.regs[r] = rnd(); }
                cpu.regs[1] = x;
                cpu.regs[2] = y;
                let init = cpu.regs;
                // the oracle: the executor as the helper calls it; and, for the first register
                // choice, the interpreter proper, which decodes the word from the bus and runs
                // it in `run_fast`'s loop (its own copy of the float code: the two must agree
                // with each other too, NaN payloads included)
                let mut want = Cpu::new(0x0200_0000);
                want.regs = cpu.regs;
                want.exec_fast(&mut bus, fast, 0x0200_0004);
                want.pc = 0x0200_0004;
                // SAFETY: `f` is the module just installed, built for NoBus
                let k = unsafe { run::<NoBus>(f, 0, &mut cpu, &mut bus, 0, 1 << 20) };
                let r = (d & 15) as usize;
                if k != 1 || cpu.regs != want.regs || cpu.pc != want.pc {
                    fail(op, d, a, b, x, y, format!("ran {k}, r{r} {:08x}, the executor {:08x}", cpu.regs[r], want.regs[r]));
                    continue;
                }
                if (d, a, b) != (0, 1, 2) {
                    continue;
                }
                let mut want2 = Cpu::new(0x0200_0000);
                want2.regs = init;
                let ran = want2.run_fast(&mut bus, 1).ok().map(|r| r.0);
                if ran != Some(1) || want2.regs != want.regs || want2.pc != want.pc {
                    fail(op, d, a, b, x, y, format!("the interpreter ran {ran:?}, r{r} {:08x} at pc {:08x}, the executor {:08x}", want2.regs[r], want2.pc, want.regs[r]));
                }
            }
        }
    }
    fails
}

/// A region's blocks as a table for `Gen::jump_local`: the smallest power-of-two size (at least
/// twice the blocks) where (pc >> 1) & mask sets them all apart. Free slots hold pc 1 (no
/// block starts at an odd pc). Lives as long as the program (as the regions' code does).
fn jump_table(blocks: &[(usize, u32, Vec<crate::BOp>)]) -> Option<(u32, u32)> {
    let mut size = (2 * blocks.len()).next_power_of_two().max(8);
    while size <= 4096 {
        let mask = size as u32 - 1;
        let mut t = vec![1u32; 2 * size];
        let fits = blocks.iter().enumerate().all(|(k, b)| {
            let h = ((b.1 >> 1) & mask) as usize;
            let free = t[2 * h] == 1;
            t[2 * h] = b.1;
            t[2 * h + 1] = k as u32;
            free
        });
        if fits {
            let t: &'static [u32] = Box::leak(t.into_boxed_slice());
            return Some((t.as_ptr() as usize as u32, mask));
        }
        size *= 2;
    }
    None
}

/// The whole module around one function body.
fn module(body: &[u8]) -> Vec<u8> {
    let mut m = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
    let section = |m: &mut Vec<u8>, id: u8, content: &[u8]| {
        m.push(id);
        uleb(m, content.len() as u64);
        m.extend_from_slice(content);
    };
    let name = |v: &mut Vec<u8>, s: &str| {
        uleb(v, s.len() as u64);
        v.extend_from_slice(s.as_bytes());
    };
    // types: run(i32 x4) -> i32; read(i32 x4) -> i32; write(i32 x5); op(i32 x4) -> i64
    let types: [(&[u8], &[u8]); 4] = [(&[T_I32; 4], &[T_I32]), (&[T_I32; 4], &[T_I32]), (&[T_I32; 5], &[]), (&[T_I32; 4], &[T_I64])];
    let mut t = Vec::new();
    uleb(&mut t, types.len() as u64);
    for (p, r) in types {
        t.push(0x60);
        uleb(&mut t, p.len() as u64);
        t.extend_from_slice(p);
        uleb(&mut t, r.len() as u64);
        t.extend_from_slice(r);
    }
    section(&mut m, 1, &t);
    // imports: env.memory, env.table
    let mut im = Vec::new();
    uleb(&mut im, 2);
    name(&mut im, "env");
    name(&mut im, "memory");
    im.extend_from_slice(&[0x02, 0x00, 0x00]);
    name(&mut im, "env");
    name(&mut im, "table");
    im.extend_from_slice(&[0x01, 0x70, 0x00, 0x00]);
    section(&mut m, 2, &im);
    section(&mut m, 3, &[1, TY_RUN as u8]);
    let mut ex = Vec::new();
    uleb(&mut ex, 1);
    name(&mut ex, "run");
    ex.extend_from_slice(&[0x00, 0x00]);
    section(&mut m, 7, &ex);
    // code: locals a, b, addr, val, off, done, cur (i32), the op result (i64), fa, fb (f32)
    let mut f = Vec::new();
    f.extend_from_slice(&[3, 7, T_I32, 1, T_I64, 2, T_F32]);
    f.extend_from_slice(body);
    let mut code = Vec::new();
    uleb(&mut code, 1);
    uleb(&mut code, f.len() as u64);
    code.extend_from_slice(&f);
    section(&mut m, 10, &code);
    m
}
