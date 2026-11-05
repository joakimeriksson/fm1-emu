//! Native JIT for the blocks of micro-ops `Cpu::run_fast` interprets (after esp32sim's
//! xtensa-lx7 JIT): AArch64 code (here), x86-64 code (jit/x64.rs, the same plan op for op). A compiled block is the block's plain prefix (up to the first op that needs
//! the careful path) as native code, with the interpreter as the oracle: the two must leave
//! bit-identical machine state.
//!
//! Timing is kept exact the same way the interpreter does it. A block runs as native code only
//! when no tick inside it can be due (`Bus::tick_room`). Before any access that leaves the
//! inline RAM / XIP fast paths, the helper brings the clock to that instruction's tick; after
//! it, if the next tick would be due (a peripheral write, an idle skip), the block exits right
//! after the instruction, where the interpreter would have stopped as well.
//!
//! Blocks chain: an exit to a fixed pc jumps through a slot straight into the next block's
//! compiled body once that block is compiled (Rust fills the slot the first time the exit
//! returns there). An exit to a computed pc (a return, `call R` / `goto R`, a table jump, a pop
//! into pc) looks the pc up in a direct-mapped table of compiled bodies (`Jit::itab`) and jumps
//! there when it is found (a body goes in when it compiles, and again when an exit to it came
//! back to Rust after another took its entry). Every body first checks that its instructions still fit the call's
//! tick limit, so the chain never runs past a due tick; the helpers sync the clock with the
//! running instruction count, so timing stays exact across the chain.
//!
//! AArch64 register plan (AAPCS64 callee-saved, so helpers may be called freely):
//!   x19 = &Cpu, x20 = &Ctx, x21 = RAM host base, x22 = XIP host base,
//!   w23 / w24 = values kept across a helper call within one instruction, x25 = code flags,
//!   w26 = instructions done in this call before the current block, w27 = the limit
//!   (instructions this call may run). Guest registers, sp and rets stay in the Cpu struct;
//!   pc is stored at exit.

#[cfg(target_arch = "aarch64")]
use crate::jit_a64::{Asm, Cond as AC, Label, Reg, SP, ZR};
#[cfg(target_arch = "aarch64")]
use crate::{Base, Cond, Op, RmwOp};
use crate::{Bus, Cpu, Fast};
#[cfg(not(windows))]
use std::ffi::c_int;
use std::ffi::c_void;

#[cfg(target_arch = "x86_64")]
mod x64;
#[cfg(target_arch = "x86_64")]
use x64::compile;

/// Host view of the guest memory the inline paths read and write directly.
#[derive(Clone, Copy, Debug)]
pub struct JitMem {
    pub ram: *mut u8,
    pub ram_base: u32,
    pub ram_len: u32,
    /// RAM code-word flags: stores to watched words must pass through the bus.
    pub ram_code: *const u8,
    pub xip: *const u8,
    pub xip_base: u32,
    pub xip_len: u32,
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
    /// `Bus::irq_pending` as last seen: 0 no, 1 yes, 2 not known (at entry, and after any
    /// helper: only the bus changes it). A swapped pair stops after itself while one is pending,
    /// as the careful path does after a pair (`h_pair`). (After brk: one 64-bit load sees both.)
    irq: u32,
    /// the chain slot of the exit taken back to Rust (null: not a chainable exit)
    slot: *mut u64,
    /// the bus's count of code-word writes at entry, as RAM guards compare it (`RamGuard::ver`;
    /// u64::MAX - 1: the bus keeps none). It stays the count while compiled code runs: a write
    /// to a code word sets brk, and the code leaves right after that instruction.
    ver: u64,
    /// it left after a swapped pair with an interrupt pending: `run_fast` stops there too
    stop: u32,
    /// inside a then-part or rep loop: a pair does not stop for an interrupt (`h_pair`)
    inside: u32,
    /// it left inside an if-else block's then-part: its (end, else part's end, start), for
    /// `Cpu::pred_skips` (0: not)
    pred: [u32; 3],
    code_version: Option<u64>,
}
const OFF_BRK: u32 = 24;
const OFF_SLOT: u32 = 32;
const OFF_VER: u32 = 40;
const OFF_PRED: u32 = 56;
const _: () = assert!(std::mem::offset_of!(Ctx, pred) == OFF_PRED as usize);
const _: () = assert!(std::mem::offset_of!(Ctx, ver) == OFF_VER as usize);
const _: () = assert!(std::mem::offset_of!(Ctx, brk) == OFF_BRK as usize);
const _: () = assert!(std::mem::offset_of!(Ctx, irq) == OFF_BRK as usize + 4);
const _: () = assert!(std::mem::offset_of!(Ctx, slot) == OFF_SLOT as usize);

// ------------------------------------------------------------------ executable memory
// macOS (Apple silicon): a MAP_JIT mapping, writable or executable per thread, toggled around a
// write. Linux and Android (the same kernel interface): a plain read-write-execute mapping (a
// hardened kernel that refuses one, SELinux's execmem, leaves the interpreter running: `new`
// gives None). That gives up W^X there: a stray write into the code cache would be executable
// (an mprotect toggle around each write would keep it, at a system call per compiled block).
// Both flush the instruction cache for what was written on AArch64. (Code is only appended, so a
// missing flush would seldom show: the tests cannot prove it. Tested on Apple silicon's cores and
// a Tensor G2's (Cortex-X1, A78, A55), which need the instruction side, CTR_EL0.DIC = 0, but not
// the data side, IDC = 1; older Cortex-A cores such as a Raspberry Pi 4's need both.) x86-64
// keeps instruction fetch coherent with stores itself: nothing to flush. Windows: VirtualAlloc's
// read-write-execute pages (after luginf's fork, issue #3), W^X given up as on Linux.
#[cfg(not(windows))]
extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
}
#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn VirtualAlloc(addr: *mut c_void, len: usize, alloc_type: u32, protect: u32) -> *mut c_void;
}
#[cfg(windows)]
const MEM_COMMIT_RESERVE: u32 = 0x1000 | 0x2000;
#[cfg(windows)]
const PAGE_EXECUTE_READWRITE: u32 = 0x40;
#[cfg(target_os = "macos")]
extern "C" {
    fn pthread_jit_write_protect_np(enabled: c_int);
    fn sys_icache_invalidate(start: *mut c_void, len: usize);
}
#[cfg(target_os = "macos")]
const MAP_FLAGS: c_int = 0x0002 | 0x1000 | 0x0800; // PRIVATE | ANON | JIT
#[cfg(any(target_os = "linux", target_os = "android"))]
const MAP_FLAGS: c_int = 0x02 | 0x20; // PRIVATE | ANONYMOUS

/// Make freshly written code visible to instruction fetch (what `__clear_cache` does on
/// AArch64): clean the data cache lines to the point of unification, invalidate the instruction
/// cache lines, then synchronise. Line sizes from CTR_EL0.
#[cfg(all(any(target_os = "linux", target_os = "android"), target_arch = "aarch64"))]
unsafe fn flush_icache(start: *const u8, len: usize) {
    let ctr: u64;
    core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack));
    let dline = 4usize << ((ctr >> 16) & 0xf);
    let iline = 4usize << (ctr & 0xf);
    let (a, end) = (start as usize, start as usize + len);
    let mut p = a & !(dline - 1);
    while p < end {
        core::arch::asm!("dc cvau, {}", in(reg) p, options(nostack));
        p += dline;
    }
    core::arch::asm!("dsb ish", options(nostack));
    let mut p = a & !(iline - 1);
    while p < end {
        core::arch::asm!("ic ivau, {}", in(reg) p, options(nostack));
        p += iline;
    }
    core::arch::asm!("dsb ish", "isb", options(nostack));
}

pub struct CodeCache {
    base: *mut u8,
    size: usize,
    used: usize,
}

impl CodeCache {
    pub fn new(size: usize) -> Option<CodeCache> {
        // SAFETY: anonymous mapping chosen by the OS; failure is checked.
        #[cfg(not(windows))]
        let p = unsafe { mmap(std::ptr::null_mut(), size, 7, MAP_FLAGS, -1, 0) };
        // SAFETY: fresh pages chosen by the OS; failure (null) is checked.
        #[cfg(windows)]
        let p = unsafe { VirtualAlloc(std::ptr::null_mut(), size, MEM_COMMIT_RESERVE, PAGE_EXECUTE_READWRITE) };
        if p as isize == -1 || p.is_null() {
            return None;
        }
        Some(CodeCache { base: p as *mut u8, size, used: 0 })
    }

    fn write(&mut self, code: &[u8]) -> Option<*const u8> {
        let bytes = code.len();
        if self.used + bytes > self.size {
            return None;
        }
        // SAFETY: the range is inside the owned mapping; write protection is lifted only around
        // the copy, and the instruction cache is invalidated for exactly that range.
        unsafe {
            let dst = self.base.add(self.used);
            #[cfg(target_os = "macos")]
            {
                pthread_jit_write_protect_np(0);
                std::ptr::copy_nonoverlapping(code.as_ptr(), dst, bytes);
                pthread_jit_write_protect_np(1);
                sys_icache_invalidate(dst as *mut c_void, bytes);
            }
            #[cfg(any(target_os = "linux", target_os = "android"))]
            {
                std::ptr::copy_nonoverlapping(code.as_ptr(), dst, bytes);
                #[cfg(target_arch = "aarch64")]
                flush_icache(dst, bytes);
            }
            #[cfg(windows)]
            std::ptr::copy_nonoverlapping(code.as_ptr(), dst, bytes);
            self.used += bytes;
            Some(dst as *const u8)
        }
    }
}

/// JIT state kept in the Cpu.
pub struct Jit {
    cache: CodeCache,
    /// per block: runs counted until hot, then the compiled entry (or a failure)
    state: Vec<Entry>,
    // Boxes keep addresses embedded in generated code stable until cache reset.
    guards: Vec<Box<RamGuard>>,
    /// compile helper calls that count themselves (`Cpu::profile` when the JIT was made)
    profile: bool,
    /// computed jumps' targets: pc -> compiled body, direct-mapped by pc (`Gen::ind`); a body
    /// is entered from it as from a chain slot (it checks the limit and its RAM snapshot)
    itab: Box<[ITab]>,
    /// compile the checks for `Cpu::stop_pc` / `stop_io` at each body's entry (`Cpu::stop_checks`)
    pub(crate) stops: bool,
}

/// An `itab` entry (generated code reads it: pc at 0, body at 8).
#[repr(C)]
#[derive(Clone, Copy)]
struct ITab {
    pc: u32,
    _pad: u32,
    body: u64,
}
const ITAB_BITS: u32 = 14;
const _: () = assert!(std::mem::size_of::<ITab>() == 16);

/// Empty entry i: its tag is a pc of another entry, so no computed target matches it (a tag no
/// instruction starts at, such as all ones, still matches a guest that jumps there: through an
/// erased-flash pointer or a trashed rets, into body 0).
fn itab_empty(i: usize) -> ITab {
    ITab { pc: ((i as u32) ^ 1) << 1, _pad: 0, body: 0 }
}

/// The `itab` entry of pc (generated code computes the same: `Gen::ind`).
fn itab_slot(pc: u32) -> usize {
    ((pc >> 1) & ((1 << ITAB_BITS) - 1)) as usize
}

#[derive(Clone, Copy)]
enum Entry {
    Cold(u32),
    /// the prefix, and the one to run inside a then-part or rep loop (`Prefix::inside_ok`; when
    /// the prefix takes in an if-else block, the ops before it, compiled on their own)
    Code(Prefix, Option<Prefix>),
    No,
}

/// A block's compiled prefix.
#[derive(Clone, Copy)]
pub(crate) struct Prefix {
    /// the function Rust calls (`run`)
    pub code: *const u8,
    /// the chain target
    body: *const u8,
    /// instructions in it (a swapped pair is two)
    pub insns: u32,
    /// the pc after its last instruction
    pub end: u32,
    /// it may run inside a then-part or rep loop (`Cpu::prefix_inside`): no glued pair whose
    /// head may use the bus (compiled code could leave between the two), no if-else block
    inside_ok: bool,
    /// it is its block whole, no then-part in tow: a runner's stretch may run on from it
    /// (`Cpu::stop_pc`)
    pub whole: bool,
}

// SAFETY: the code cache and the compiled entries are owned by one Cpu and only used by the
// thread that runs it; moving the Cpu (with them) to another thread is fine.
unsafe impl Send for Jit {}

impl std::fmt::Debug for Jit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Jit({} bytes, {} blocks)", self.cache.used, self.state.len())
    }
}

/// Runs of a block before it is compiled.
const HOT: u32 = 32;

impl Jit {
    pub fn new(profile: bool) -> Option<Jit> {
        let itab = (0..1 << ITAB_BITS).map(itab_empty).collect();
        Some(Jit { cache: CodeCache::new(64 << 20)?, state: Vec::new(), guards: Vec::new(), profile, itab, stops: false })
    }

    /// Will `entry` compile block `id` on its next call? (Then `Cpu::then_of` looks for the
    /// then-part of an if-else block that ends it.)
    pub(crate) fn compiles_next(&self, id: usize) -> bool {
        match self.state.get(id) {
            None => HOT <= 1,
            Some(Entry::Cold(c)) => c + 1 >= HOT,
            _ => false,
        }
    }

    /// The compiled code of block `id`, compiling it once it is hot. `swaps`: `Cpu::swaps`, the
    /// pairs of its `Fast::Swap` ops. `then`: the block ends in an if-else block and this is its
    /// then-part (plain ops, `Cpu::then_part`): the prefix may take both in. `table`: a body over
    /// RAM may go in the computed jumps' table (what `Cpu::jit_body_at` asks of a chain target:
    /// the bus counts code writes and compiled stores see the code flags).
    /// `inside`: the prefix to run inside a then-part or rep loop (`Prefix::inside_ok`), if any.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn entry<B: Bus>(&mut self, id: usize, ops: &[crate::BOp], swaps: &[(Fast, Fast, u8)], then: Option<&[crate::BOp]>, pc0: u32, mem: &JitMem, table: bool, inside: bool) -> Option<Prefix> {
        if self.state.len() <= id {
            self.state.resize(id + 1, Entry::Cold(0));
        }
        match self.state[id] {
            Entry::Code(p, q) => if inside { q } else { Some(p) },
            Entry::No => None,
            Entry::Cold(c) if c + 1 < HOT => {
                self.state[id] = Entry::Cold(c + 1);
                None
            }
            Entry::Cold(_) => {
                // the plain prefix; a swapped pair joins it unless its second instruction jumps
                let plen = ops
                    .iter()
                    .take_while(|o| match o.fast {
                        Fast::Swap { idx } => !swaps[idx as usize].1.may_jump(),
                        _ => !o.careful,
                    })
                    .count();
                // an if-else block ending it (careful: interpreted, else) with its then-part
                let then = then.filter(|_| plen + 1 == ops.len() && matches!(ops[plen].fast, Fast::IfBlk { has_else: true, .. }) && !ops[plen].pair_head);
                let with_if = then.is_some();
                let plen = plen + with_if as usize;
                let whole = plen == ops.len() && !with_if;
                let e = match self.compile_prefix::<B>(&ops[..plen], swaps, then.unwrap_or(&[]), pc0, mem, table, true, whole) {
                    None => Entry::No,
                    Some(p) if !with_if => Entry::Code(p, Some(p).filter(|p| p.inside_ok)),
                    // (the ops before the if-else block, for runs inside a construct)
                    Some(p) => Entry::Code(p, self.compile_prefix::<B>(&ops[..plen - 1], swaps, &[], pc0, mem, false, false, false).filter(|q| q.inside_ok)),
                };
                self.state[id] = e;
                match e {
                    Entry::Code(p, q) => if inside { q } else { Some(p) },
                    _ => None,
                }
            }
        }
    }

    /// Compile `ops` (and an if-else block's then-part, `then`) starting at pc0. `table`: a body
    /// over RAM may go in the computed jumps' table; `listed`: it goes there at all (the block's
    /// own prefix: a pc has one body there).
    #[allow(clippy::too_many_arguments)]
    fn compile_prefix<B: Bus>(&mut self, ops: &[crate::BOp], swaps: &[(Fast, Fast, u8)], then: &[crate::BOp], pc0: u32, mem: &JitMem, table: bool, listed: bool, whole: bool) -> Option<Prefix> {
        if ops.is_empty() {
            return None;
        }
        let itab = self.itab.as_ptr() as u64;
        let stops = self.stops.then_some(whole);
        let (code, body) = compile::<B>(ops, swaps, then, pc0, mem, &mut self.guards, self.profile, itab, stops)?;
        let p = self.cache.write(&code)?;
        // SAFETY: body is a byte offset inside the code just written
        let body = unsafe { p.add(body) };
        if listed && (table || (0x0200_0000..0x0400_0000).contains(&pc0)) {
            self.itab[itab_slot(pc0)] = ITab { pc: pc0, _pad: 0, body: body as u64 };
        }
        Some(Prefix {
            code: p,
            body,
            insns: insns(ops) + then.len() as u32,
            end: pc0 + ops.iter().map(|o| o.len as u32).sum::<u32>(),
            inside_ok: !ops.iter().any(|o| (o.glue && !o.fast.reg_only()) || (o.careful && !matches!(o.fast, Fast::Swap { .. }))),
            whole,
        })
    }

    /// List `body`, compiled at pc, in the computed jumps' table again: a later body on the same
    /// entry took its place, and an exit from compiled code just landed on it (the table is
    /// filled when a body compiles; without this a hot return target, once evicted by a cold
    /// one, went back to Rust for good).
    pub(crate) fn relist(&mut self, pc: u32, body: *const u8) {
        self.itab[itab_slot(pc)] = ITab { pc, _pad: 0, body: body as u64 };
    }

    /// The chain target of block `id`, if it is compiled.
    pub(crate) fn body(&self, id: usize) -> Option<*const u8> {
        match self.state.get(id) {
            Some(Entry::Code(p, _)) => Some(p.body),
            _ => None,
        }
    }

    /// Forget every compiled block (the block table was rebuilt).
    pub fn reset(&mut self) {
        self.state.clear();
        self.cache.used = 0;
        self.guards.clear();
        self.itab.iter_mut().enumerate().for_each(|(i, e)| *e = itab_empty(i));
    }
}

/// Link a chain exit (`Ctx::slot`, as `run` returned it) to `body`, the compiled block it leads
/// to: the exit then goes straight in. AArch64: the slot is the exit's own branch instruction,
/// patched into `b body` (the code cache is one 64 MB mapping, within a `b`'s reach); x86-64:
/// a word the exit's code loads and jumps through.
///
/// # Safety
/// `slot` must be a chain exit of code in this JIT's cache (non-null), `body` a compiled body.
pub(crate) unsafe fn link(slot: *mut u64, body: *const u8) {
    #[cfg(target_arch = "aarch64")]
    {
        let site = slot as *mut u32;
        let off = (body as isize - site as isize) / 4;
        assert!((-(1 << 25)..(1 << 25)).contains(&off), "chain target out of a branch's reach");
        let insn = 0x1400_0000u32 | (off as u32) & 0x03ff_ffff;
        #[cfg(target_os = "macos")]
        {
            pthread_jit_write_protect_np(0);
            site.write_volatile(insn);
            pthread_jit_write_protect_np(1);
            sys_icache_invalidate(site as *mut c_void, 4);
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            site.write_volatile(insn);
            flush_icache(site as *const u8, 4);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
        site.write_volatile(insn); // (as CodeCache::write: no flush on the other systems)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        *slot = body as u64;
    }
}

/// Run compiled code: returns the instructions executed, the chain slot of the exit taken (or
/// null), and whether it stopped after a swapped pair with an interrupt pending (where
/// `run_fast` stops too); cpu.pc is set. `inside`: inside a then-part or rep loop (the careful
/// path does not stop after a pair there, so neither does compiled code).
///
/// # Safety
/// `code` must come from `Jit::entry` for this bus type and the memory `mem` describes.
pub(crate) unsafe fn run<B: Bus>(code: *const u8, cpu: &mut Cpu, bus: &mut B, mem: &JitMem, extra: u32, cap: u32, inside: bool) -> (u32, *mut u64, bool) {
    // instructions this call may run: their ticks ((k - 1) + extra) must all fit tick_room
    let limit = (bus.tick_room() + 1).saturating_sub(extra as u64).min(1 << 30).min(cap as u64) as u32;
    let mut ctx = Ctx {
        cpu: cpu as *mut Cpu,
        bus: bus as *mut B as *mut (),
        extra,
        synced: 0,
        brk: 0,
        irq: 2,
        slot: std::ptr::null_mut(),
        ver: bus.code_writes().unwrap_or(u64::MAX - 1),
        stop: 0,
        inside: inside as u32,
        pred: [0; 3],
        code_version: bus.code_writes(),
    };
    let f: CodeFn = unsafe { std::mem::transmute(code) };
    let k = f(cpu as *mut Cpu, &mut ctx, mem.ram, mem.xip, limit, mem.ram_code);
    if ctx.pred[0] != 0 {
        // (as the if-else op put it there)
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
    (k, ctx.slot, ctx.stop != 0)
}

// ------------------------------------------------------------------ helpers called from generated code

// The generated code's calling convention, for its entry and the helpers it calls: the host's C
// one, except on x86-64, where it is System V everywhere (on Windows too, so jit/x64.rs has one
// register plan; Rust saves what Win64 would want kept around the call into it).
#[cfg(target_arch = "x86_64")]
type CodeFn = extern "sysv64" fn(*mut Cpu, *mut Ctx, *mut u8, *const u8, u32, *const u8) -> u32;
#[cfg(not(target_arch = "x86_64"))]
type CodeFn = extern "C" fn(*mut Cpu, *mut Ctx, *mut u8, *const u8, u32, *const u8) -> u32;
macro_rules! code_abi {
    ($(#[$m:meta])* fn $($rest:tt)*) => {
        #[cfg(target_arch = "x86_64")]
        $(#[$m])* extern "sysv64" fn $($rest)*
        #[cfg(not(target_arch = "x86_64"))]
        $(#[$m])* extern "C" fn $($rest)*
    };
}

unsafe fn sync<B: Bus>(ctx: &mut Ctx, bus: &mut B, idx: u32) {
    let need = idx + ctx.extra;
    if need > ctx.synced {
        bus.add_ticks((need - ctx.synced) as u64);
        ctx.synced = need;
    }
}

code_abi! {
fn h_read<B: Bus>(ctx: *mut Ctx, addr: u32, idx: u32, w: u32) -> u32 {
    // SAFETY: ctx is the live context `run` passed in; the bus pointer is the caller's &mut B.
    let ctx = unsafe { &mut *ctx };
    let bus = unsafe { &mut *(ctx.bus as *mut B) };
    unsafe { sync(ctx, bus, idx) };
    stop_io(ctx);
    let v = match w {
        1 => bus.read8(addr) as u32,
        2 => bus.read16(addr) as u32,
        _ => bus.read32(addr),
    };
    if bus.next_tick_due() || bus.code_writes() != ctx.code_version {
        ctx.brk = 1;
    }
    ctx.irq = 2;
    v
}
}

code_abi! {
fn h_write<B: Bus>(ctx: *mut Ctx, addr: u32, v: u32, idx: u32, w: u32) {
    // SAFETY: as h_read.
    let ctx = unsafe { &mut *ctx };
    let bus = unsafe { &mut *(ctx.bus as *mut B) };
    unsafe { sync(ctx, bus, idx) };
    stop_io(ctx);
    match w {
        1 => bus.write8(addr, v as u8),
        2 => bus.write16(addr, v as u16),
        _ => bus.write32(addr, v),
    }
    if bus.next_tick_due() || bus.code_writes() != ctx.code_version {
        ctx.brk = 1;
    }
    ctx.irq = 2;
}
}

// PROF: count the call (`Cpu::jit_helpers`); chosen when compiling, so code compiled without
// profiling pays nothing for it
code_abi! {
/// Any op through the interpreter's own executor: bit 32 = jumped (target in the low word).
fn h_op<B: Bus, const PROF: bool>(ctx: *mut Ctx, op: *const Fast, idx: u32, next_pc: u32) -> u64 {
    // SAFETY: as h_read; `op` points at a leaked, immutable Fast.
    let ctx = unsafe { &mut *ctx };
    let bus = unsafe { &mut *(ctx.bus as *mut B) };
    let cpu = unsafe { &mut *ctx.cpu };
    unsafe { sync(ctx, bus, idx) };
    if cpu.stop_pc != 0 {
        cpu.stop_io = 1;
    }
    if PROF {
        cpu.profile_helper(unsafe { &*op });
    }
    let j = cpu.exec_fast(bus, unsafe { *op }, next_pc);
    if bus.next_tick_due() || bus.code_writes() != ctx.code_version {
        ctx.brk = 1;
    }
    ctx.irq = 2;
    match j {
        Some(t) => (1u64 << 32) | t as u64,
        None => 0,
    }
}
}

code_abi! {
/// `h_op` for a `Fast::quiet` op (`cli`, `R = icfg`): CPU state only, so as for an op compiled
/// inline, no ticks to sync first and nothing due after (it cannot change either).
fn h_op_quiet<const PROF: bool>(ctx: *mut Ctx, op: *const Fast, _idx: u32, _next_pc: u32) -> u64 {
    // SAFETY: as h_op.
    let ctx = unsafe { &mut *ctx };
    let cpu = unsafe { &mut *ctx.cpu };
    if PROF {
        cpu.profile_helper(unsafe { &*op });
    }
    cpu.exec_quiet(unsafe { *op });
    0
}
}

code_abi! {
/// `sti` (`Fast::SetIe { on: true }`): `h_op`'s work for it without the executor's dispatch. It
/// opens the bus's interrupt window, so the ticks are synced first and brk checked after.
fn h_sti<B: Bus>(ctx: *mut Ctx, idx: u32) {
    // SAFETY: as h_op.
    let ctx = unsafe { &mut *ctx };
    let bus = unsafe { &mut *(ctx.bus as *mut B) };
    let cpu = unsafe { &mut *ctx.cpu };
    unsafe { sync(ctx, bus, idx) };
    if cpu.stop_pc != 0 {
        cpu.stop_io = 1;
    }
    cpu.profile_helper(&Fast::SetIe { on: true });
    cpu.ie = true;
    bus.irq_window();
    if bus.next_tick_due() || bus.code_writes() != ctx.code_version {
        ctx.brk = 1;
    }
    ctx.irq = 2;
}
}

code_abi! {
/// After a swapped pair whose brk or `Ctx::irq` is set: leave (1) if the next tick is due (or
/// code was written) or an interrupt is pending; the latter also stops `run_fast` there.
fn h_pair<B: Bus>(ctx: *mut Ctx) -> u32 {
    // SAFETY: as h_read.
    let ctx = unsafe { &mut *ctx };
    let bus = unsafe { &mut *(ctx.bus as *mut B) };
    if ctx.inside != 0 {
        // (the construct goes on after the pair: interrupts wait)
        ctx.irq = 0;
        return ctx.brk;
    }
    let p = bus.irq_pending();
    ctx.irq = p as u32;
    ctx.stop = p as u32;
    (ctx.brk != 0 || p) as u32
}
}

/// Instructions in a compiled prefix (a swapped pair is two).
fn insns(ops: &[crate::BOp]) -> u32 {
    ops.iter().map(|o| if matches!(o.fast, Fast::Swap { .. }) { 2 } else { 1 }).sum()
}

/// The helper compiled code calls for an op it has no code of its own for: `h_op`, or for a
/// quiet one `h_op_quiet` (the same arguments and result).
fn op_helper<B: Bus>(op: &Fast, profile: bool) -> u64 {
    match (op.quiet(), profile) {
        (true, true) => h_op_quiet::<true> as *const () as u64,
        (true, false) => h_op_quiet::<false> as *const () as u64,
        (false, true) => h_op::<B, true> as *const () as u64,
        (false, false) => h_op::<B, false> as *const () as u64,
    }
}

// Each RAM body owns an immutable snapshot. Even an old incoming chain remains safe
// after its block has been rebuilt: it must validate the code it actually executes.
#[repr(C)]
struct RamGuard {
    /// the bus's count of code-word writes when the snapshot was last found current (u64::MAX:
    /// never, or the bus keeps none): while `Ctx::ver` is the same, compiled code enters
    /// without calling `h_ram_ok` (generated code reads it: offset 0)
    ver: u64,
    pc: u32,
    bytes: Box<[u8]>,
}
const _: () = assert!(std::mem::offset_of!(RamGuard, ver) == 0);

impl RamGuard {
    fn new(pc: u32, bytes: Box<[u8]>) -> RamGuard {
        RamGuard { ver: u64::MAX, pc, bytes }
    }
}

code_abi! {
/// A RAM body's guard when its count is not `Ctx::ver`: is the snapshot what memory holds?
fn h_ram_ok<B: Bus>(ctx: *mut Ctx, guard: *mut RamGuard) -> u32 {
    let ctx = unsafe { &mut *ctx };
    let bus = unsafe { &mut *(ctx.bus as *mut B) };
    let guard = unsafe { &mut *guard };
    let version = bus.code_writes();
    let valid = crate::ram_code(bus, guard.pc)
        .is_some_and(|now| now.len() >= guard.bytes.len() && now[..guard.bytes.len()] == *guard.bytes);
    if valid {
        guard.ver = version.unwrap_or(u64::MAX);
    }
    valid as u32
}
}

// ------------------------------------------------------------------ code generation (AArch64)

#[cfg(target_arch = "aarch64")]
const CPU: Reg = 19;
#[cfg(target_arch = "aarch64")]
const CTX: Reg = 20;
#[cfg(target_arch = "aarch64")]
const RAM: Reg = 21;
#[cfg(target_arch = "aarch64")]
const XIP: Reg = 22;
#[cfg(target_arch = "aarch64")]
const KEEP: Reg = 23;
#[cfg(target_arch = "aarch64")]
const KEEP2: Reg = 24;
#[cfg(target_arch = "aarch64")]
const FLAGS: Reg = 25;
#[cfg(target_arch = "aarch64")]
const DONE: Reg = 26;
#[cfg(target_arch = "aarch64")]
const LIMIT: Reg = 27;
#[cfg(target_arch = "aarch64")]
const FRAME: i32 = 96;

#[cfg(target_arch = "aarch64")]
struct Gen<'a> {
    a: Asm,
    mem: &'a JitMem,
    exit: Label,
    h_read: u64,
    h_write: u64,
    h_pair: u64,
    /// `Jit::itab`'s address
    itab: u64,
    /// the runner's stop checks are compiled (`Jit::stops`)
    stops: bool,
    rc: RegCache,
}

/// Guest registers kept in host registers within a block. `ld_reg` loads one into its host
/// register at its first use and `st_reg` only marks it changed; whatever leaves the block's
/// straight line writes the changed ones back first: an exit or a chain (`flush`), and a helper
/// call, which reloads them afterwards (`call`: the helper may read or set any of them, and the
/// pool is caller-saved). An op's lowering branches only to leave, so every path through a block
/// sees the same assignment. (Each operand was a load and each result a store, and audio code
/// feeds one op's result straight into the next: the store-to-load wait was most of its time.)
#[cfg(target_arch = "aarch64")]
const POOL: [Reg; 8] = [5, 6, 7, 8, 14, 15, 16, 17];
#[cfg(target_arch = "aarch64")]
#[derive(Default)]
struct RegCache {
    /// the guest register each pool register holds
    guest: [Option<u8>; 8],
    /// changed since it was loaded (memory is behind)
    dirty: [bool; 8],
    /// last use, for choosing which to give up when all are taken
    used: [u32; 8],
    clock: u32,
}

fn off_reg(r: u8) -> u32 {
    (std::mem::offset_of!(Cpu, regs) + 4 * (r & 15) as usize) as u32
}
fn off_sp() -> u32 {
    std::mem::offset_of!(Cpu, sp) as u32
}
fn off_rets() -> u32 {
    std::mem::offset_of!(Cpu, rets) as u32
}
/// A runner's stop is set (`Cpu::stop_pc`): this access may reach a peripheral.
fn stop_io(ctx: &mut Ctx) {
    // SAFETY: ctx.cpu is the live Cpu `run` was given.
    let cpu = unsafe { &mut *ctx.cpu };
    if cpu.stop_pc != 0 {
        cpu.stop_io = 1;
    }
}
#[cfg(target_arch = "aarch64")]
fn off_ie() -> u32 {
    std::mem::offset_of!(Cpu, ie) as u32
}
fn off_stop_pc() -> u32 {
    std::mem::offset_of!(Cpu, stop_pc) as u32
}
fn off_stop_io() -> u32 {
    std::mem::offset_of!(Cpu, stop_io) as u32
}
fn off_stop_rest() -> u32 {
    std::mem::offset_of!(Cpu, stop_rest) as u32
}
fn off_hold() -> u32 {
    std::mem::offset_of!(Cpu, hold_irq) as u32
}
fn off_pc() -> u32 {
    std::mem::offset_of!(Cpu, pc) as u32
}

#[cfg(target_arch = "aarch64")]
fn acond(c: Cond) -> AC {
    match c {
        Cond::Eq => AC::Eq,
        Cond::Ne => AC::Ne,
        Cond::LtU => AC::Lo,
        Cond::GtU => AC::Hi,
        Cond::LeU => AC::Ls,
        Cond::GeU => AC::Hs,
        Cond::LtS => AC::Lt,
        Cond::GtS => AC::Gt,
        Cond::LeS => AC::Le,
        Cond::GeS => AC::Ge,
        // after fcmp: less N, equal ZC, greater C, unordered CV
        Cond::FEq => AC::Eq,
        Cond::FUne => AC::Ne,
        Cond::FLt => AC::Mi,
        Cond::FUge => AC::Pl,
        Cond::FLe => AC::Ls,
        Cond::FUgt => AC::Hi,
        Cond::FGe => AC::Ge,
        Cond::FUlt => AC::Lt,
        Cond::FGt => AC::Gt,
        Cond::FUle => AC::Le,
    }
}

#[cfg(target_arch = "aarch64")]
impl Gen<'_> {
    /// flags for a compare of w(x) with w(y): integer, or as two floats
    fn cmp(&mut self, float: bool, x: Reg, y: Reg) {
        if float {
            self.a.fmov_s_w(0, x);
            self.a.fmov_s_w(1, y);
            self.a.fcmp(0, 1);
        } else {
            self.a.cmp(x, y);
        }
    }
    /// w = guest register r (see `RegCache`)
    fn ld_reg(&mut self, w: Reg, r: u8) {
        let r = r & 15;
        let i = match self.slot(r) {
            Some(i) => i,
            None => {
                let i = self.take_slot(r);
                self.a.ldr(POOL[i], CPU, off_reg(r));
                i
            }
        };
        self.a.mov(w, POOL[i]);
    }
    /// guest register r = w (see `RegCache`)
    fn st_reg(&mut self, w: Reg, r: u8) {
        let r = r & 15;
        let i = match self.slot(r) {
            Some(i) => i,
            None => self.take_slot(r),
        };
        self.a.mov(POOL[i], w);
        self.rc.dirty[i] = true;
    }
    /// The pool register holding r, if one does (counted as a use).
    fn slot(&mut self, r: u8) -> Option<usize> {
        let i = self.rc.guest.iter().position(|&g| g == Some(r))?;
        self.rc.clock += 1;
        self.rc.used[i] = self.rc.clock;
        Some(i)
    }
    /// A pool register for r: a free one, else the least recently used, written back if changed.
    fn take_slot(&mut self, r: u8) -> usize {
        let i = match self.rc.guest.iter().position(|g| g.is_none()) {
            Some(i) => i,
            None => {
                let i = (0..POOL.len()).min_by_key(|&i| self.rc.used[i]).unwrap();
                if self.rc.dirty[i] {
                    self.a.str(POOL[i], CPU, off_reg(self.rc.guest[i].unwrap()));
                }
                i
            }
        };
        self.rc.clock += 1;
        self.rc.guest[i] = Some(r);
        self.rc.dirty[i] = false;
        self.rc.used[i] = self.rc.clock;
        i
    }
    /// Write the changed guest registers back. The assignment stays as it is: after a side exit
    /// the straight line goes on with them still in their host registers.
    fn flush(&mut self) {
        for i in 0..POOL.len() {
            if let (Some(r), true) = (self.rc.guest[i], self.rc.dirty[i]) {
                self.a.str(POOL[i], CPU, off_reg(r));
            }
        }
    }
    /// Load the held guest registers again (after a call: the pool is caller-saved).
    fn reload(&mut self) {
        for i in 0..POOL.len() {
            if let Some(r) = self.rc.guest[i] {
                self.a.ldr(POOL[i], CPU, off_reg(r));
            }
        }
    }
    /// w(dst) = w(src) + imm (any 32-bit immediate)
    fn add_imm(&mut self, dst: Reg, src: Reg, imm: u32) {
        if imm == 0 {
            if dst != src {
                self.a.mov(dst, src);
            }
        } else if imm < 4096 {
            self.a.add_imm(dst, src, imm);
        } else if imm.wrapping_neg() < 4096 {
            self.a.sub_imm(dst, src, imm.wrapping_neg());
        } else {
            self.a.mov32(9, imm);
            self.a.add(dst, src, 9);
        }
    }
    /// w0 = op(w0, w1)
    fn alu(&mut self, op: Op) {
        let a = &mut self.a;
        match op {
            Op::Add => a.add(0, 0, 1),
            Op::Sub => a.sub(0, 0, 1),
            Op::RSub => a.sub(0, 1, 0),
            Op::And => a.and(0, 0, 1),
            Op::AndNot => a.bic(0, 0, 1),
            Op::Or => a.orr(0, 0, 1),
            Op::Xor => a.eor(0, 0, 1),
            Op::Mul => a.mul(0, 0, 1),
            Op::Shl => a.lslv(0, 0, 1),
            Op::Shr => a.lsrv(0, 0, 1),
            Op::Sar => a.asrv(0, 0, 1),
        }
    }
    /// w1 = an A4/BitBr right-hand side: rk 0 register, 1 immediate, 2 `1 << reg`
    fn rhs(&mut self, rk: u8, rv: u32) {
        match rk {
            0 => self.ld_reg(1, rv as u8),
            1 => self.a.mov32(1, rv),
            _ => {
                self.ld_reg(2, rv as u8);
                self.a.movz(1, 1, 0);
                self.a.lslv(1, 1, 2);
            }
        }
    }
    /// Call a helper with the guest registers in memory, where it reads and sets them.
    fn call(&mut self, f: u64) {
        self.flush();
        self.a.ldr_lit_x(9, f);
        self.a.blr(9);
        self.reload();
    }
    /// Leave with pc = imm, `count` instructions of this block done.
    fn exit_imm(&mut self, pc: u32, count: u32) {
        self.flush();
        self.a.mov32(9, pc);
        self.a.str(9, CPU, off_pc());
        self.a.add_imm(0, DONE, count);
        self.a.b(self.exit);
    }
    /// Continue at a fixed pc: straight into its compiled body when the slot holds one,
    /// else back to Rust, which fills the slot once that block is compiled.
    /// (AArch64: the "slot" is the branch itself, `b` to the exit code right after it until
    /// `link` patches it into `b body`: one direct branch instead of a slot load and a branch
    /// through a register, on every block's way out)
    fn chain(&mut self, pc: u32, count: u32) {
        self.flush();
        let (site, back) = (self.a.label(), self.a.label());
        self.a.add_imm(DONE, DONE, count);
        self.a.bind(site);
        self.a.b(back);
        self.a.bind(back);
        self.a.mov32(9, pc);
        self.a.str(9, CPU, off_pc());
        self.a.adr(10, site);
        self.a.str_x(10, CTX, OFF_SLOT);
        self.a.mov(0, DONE);
        self.a.b(self.exit);
    }
    /// Continue at pc = w(reg), a computed target: straight into its compiled body when the
    /// table holds one for that pc, else back to Rust.
    fn ind(&mut self, reg: Reg, count: u32) {
        self.ind_brk(reg, count, false);
    }
    /// `ind` after an instruction that may have used the bus (`brk`): first leave if the next
    /// tick is due (or a code word was written), as `check_brk` does.
    fn ind_brk(&mut self, reg: Reg, count: u32, brk: bool) {
        // (either way out: the target's body, or Rust, finds the registers in memory)
        self.flush();
        let back = self.a.label();
        self.a.mov(12, reg);
        self.a.add_imm(DONE, DONE, count);
        if brk {
            self.a.ldr(9, CTX, OFF_BRK);
            self.a.cbnz(9, back);
        }
        self.a.ubfx(10, 12, 1, ITAB_BITS);
        self.a.ldr_lit_x(11, self.itab);
        self.a.add_x_lsl(11, 11, 10, 4);
        self.a.ldr(13, 11, 0);
        self.a.cmp(13, 12);
        self.a.b_cond(AC::Ne, back);
        self.a.ldr_x(9, 11, 8);
        self.a.br(9);
        self.a.bind(back);
        self.a.str(12, CPU, off_pc());
        self.a.mov(0, DONE);
        self.a.b(self.exit);
    }
    /// The helpers' instruction index of op `i` of this block: w2 = DONE + i.
    fn idx_w2(&mut self, i: u32) {
        self.a.add_imm(2, DONE, i);
    }

    /// w0 = load(width, signed) at guest address w0. Inline RAM / XIP, else the bus.
    fn load(&mut self, w: u8, signed: bool, idx: u32) {
        let (xip, slow, done) = (self.a.label(), self.a.label(), self.a.label());
        let mem = *self.mem;
        for (base_reg, gbase, len, miss) in [(RAM, mem.ram_base, mem.ram_len, xip), (XIP, mem.xip_base, mem.xip_len, slow)] {
            if base_reg == XIP {
                self.a.bind(xip);
            }
            // off = addr - base; the word holding it must lie inside (as the Soc's fast paths)
            self.a.mov32(2, gbase);
            self.a.sub(3, 0, 2);
            if w == 2 {
                self.a.mov32(2, !1u32);
                self.a.and(3, 3, 2); // a halfword is the one at addr & !1 (Soc read16)
            }
            self.a.mov32(2, len.saturating_sub(w as u32));
            self.a.cmp(3, 2);
            self.a.b_cond(AC::Hi, miss);
            match (w, signed) {
                (4, _) => self.a.ldr_u(0, base_reg, 3),
                (2, false) => self.a.ldrh_u(0, base_reg, 3),
                (2, true) => self.a.ldrsh_u(0, base_reg, 3),
                (1, false) => self.a.ldrb_u(0, base_reg, 3),
                _ => self.a.ldrsb_u(0, base_reg, 3),
            }
            self.a.b(done);
        }
        self.a.bind(slow);
        self.a.mov(1, 0);
        self.a.mov_x(0, CTX);
        self.idx_w2(idx);
        self.a.mov32(3, w as u32);
        self.call(self.h_read);
        match (w, signed) {
            (2, true) => self.a.sxth(0, 0),
            (1, true) => self.a.sbfx(0, 0, 0, 8),
            _ => {}
        }
        self.a.bind(done);
    }

    /// store(width) of w1 at guest address w0. Inline RAM, else the bus.
    fn store(&mut self, w: u8, idx: u32) {
        let (slow, done) = (self.a.label(), self.a.label());
        let mem = *self.mem;
        if w == 2 {
            self.a.tbnz(0, 0, slow); // an odd halfword goes the Soc's merging way
        }
        self.a.mov32(2, mem.ram_base);
        self.a.sub(3, 0, 2);
        self.a.mov32(2, mem.ram_len.saturating_sub(w as u32));
        self.a.cmp(3, 2);
        self.a.b_cond(AC::Hi, slow);
        if !mem.ram_code.is_null() {
            self.a.lsr_imm(2, 3, 2);
            self.a.ldrb_u(2, FLAGS, 2);
            self.a.cbnz(2, slow);
        }
        match w {
            4 => self.a.str_u(1, RAM, 3),
            2 => self.a.strh_u(1, RAM, 3),
            _ => self.a.strb_u(1, RAM, 3),
        }
        self.a.b(done);
        self.a.bind(slow);
        self.a.mov(2, 1);
        self.a.mov(1, 0);
        self.a.mov_x(0, CTX);
        self.a.add_imm(3, DONE, idx);
        self.a.mov32(4, w as u32);
        self.call(self.h_write);
        self.a.bind(done);
    }

    /// After an instruction that may have called the bus: leave if the next tick is due (or
    /// code was written), `rest` instructions before the block's end (`Cpu::stop_rest`).
    fn check_brk_hold(&mut self, next_pc: u32, count: u32, rest: u32) {
        let go = self.a.label();
        self.a.ldr(9, CTX, OFF_BRK);
        self.a.cbz(9, go);
        self.a.mov32(9, 1);
        self.a.str(9, CPU, off_hold());
        self.rest(rest);
        self.exit_imm(next_pc, count);
        self.a.bind(go);
    }
    fn check_brk(&mut self, next_pc: u32, count: u32, rest: u32) {
        let go = self.a.label();
        self.a.ldr(9, CTX, OFF_BRK);
        self.a.cbz(9, go);
        self.rest(rest);
        self.exit_imm(next_pc, count);
        self.a.bind(go);
    }
    /// Leaving `rest` instructions before the block's end: a runner's stretch learns where the
    /// block's call ends (`Cpu::stop_rest`).
    fn rest(&mut self, rest: u32) {
        if self.stops && rest > 0 {
            self.a.mov32(9, rest);
            self.a.str(9, CPU, off_stop_rest());
        }
    }
    /// Inside an if-else block's then-part, after an op that may have used the bus: leave if
    /// the next tick is due, with the skip (te, ee, ts) for `Cpu::pred_skips` (`Ctx::pred`).
    fn check_brk_pred(&mut self, next_pc: u32, count: u32, te: u32, ee: u32, ts: u32) {
        let go = self.a.label();
        self.a.ldr(9, CTX, OFF_BRK);
        self.a.cbz(9, go);
        self.a.mov32(9, te);
        self.a.str(9, CTX, OFF_PRED);
        self.a.mov32(9, ee);
        self.a.str(9, CTX, OFF_PRED + 4);
        self.a.mov32(9, ts);
        self.a.str(9, CTX, OFF_PRED + 8);
        self.exit_imm(next_pc, count);
        self.a.bind(go);
    }
    /// Jump to `taken` when a predicated block's condition holds (`exec_fast`'s IfBlk).
    fn if_jump(&mut self, a: u8, rk: u8, fam: u8, alt: bool, rv: u32, taken: Label) {
        self.rhs(rk, rv);
        self.ld_reg(0, a);
        let c = if fam == 2 {
            self.a.tst(0, 1);
            if alt { AC::Ne } else { AC::Eq }
        } else {
            self.cmp(fam >= 8, 0, 1);
            acond(crate::cond_of_fam(fam as u64, alt))
        };
        self.a.b_cond(c, taken);
    }
    /// After a swapped pair (or an if-else block's then-part): leave if the next tick is due
    /// (or a code word was written), or an interrupt is pending (then `run_fast` stops:
    /// `Ctx::stop`). Nothing to call while neither brk is set nor `Ctx::irq` (none pending,
    /// known since the last helper).
    /// (`rest`: instructions left in the block, as `check_brk`)
    fn check_pair(&mut self, next_pc: u32, count: u32, rest: u32) {
        let go = self.a.label();
        self.a.ldr_x(9, CTX, OFF_BRK); // brk | irq << 32
        self.a.cbz_x(9, go);
        self.a.mov_x(0, CTX);
        self.call(self.h_pair);
        self.a.cbz(0, go);
        self.rest(rest);
        self.exit_imm(next_pc, count);
        self.a.bind(go);
    }
}

/// Native code for one plain micro-op, instruction `idx` of its block (the helpers' tick index),
/// `next_pc` after it; true: it may have used the bus.
#[cfg(target_arch = "aarch64")]
fn emit_op<B: Bus>(g: &mut Gen, fast: Fast, idx: u32, next_pc: u32, profile: bool) -> bool {
    let count = idx + 1;
    let mut bus_used = false;
    match fast {
        Fast::Nop => {}
        Fast::MovI { d, v } => {
            g.a.mov32(0, v);
            g.st_reg(0, d);
        }
        Fast::Mov { d, s } => {
            g.ld_reg(0, s);
            g.st_reg(0, d);
        }
        Fast::AddRI { d, s, v } => {
            g.ld_reg(0, s);
            g.add_imm(0, 0, v);
            g.st_reg(0, d);
        }
        Fast::ShlI { d, s, n } => {
            g.ld_reg(0, s);
            g.a.lsl_imm(0, 0, n as u32 & 31);
            g.st_reg(0, d);
        }
        Fast::SarI { d, s, n } => {
            g.ld_reg(0, s);
            g.a.asr_imm(0, 0, n as u32 & 31);
            g.st_reg(0, d);
        }
        Fast::Alu3 { d, a, b, op } => {
            g.ld_reg(0, a);
            g.ld_reg(1, b);
            g.alu(op);
            g.st_reg(0, d);
        }
        Fast::Alu3I { d, a, v, op } => {
            g.ld_reg(0, a);
            g.a.mov32(1, v);
            g.alu(op);
            g.st_reg(0, d);
        }
        Fast::Alu2 { d, b, op } => {
            g.ld_reg(0, d);
            g.ld_reg(1, b);
            g.alu(op);
            g.st_reg(0, d);
        }
        Fast::Alu2I { d, v, op } => {
            g.ld_reg(0, d);
            g.a.mov32(1, v);
            g.alu(op);
            g.st_reg(0, d);
        }
        Fast::A4 { d, s, op, rk, rv } => {
            g.rhs(rk, rv);
            g.ld_reg(0, s);
            g.alu(op);
            g.st_reg(0, d);
        }
        Fast::Ext { d, s, part, signed } => {
            g.ld_reg(0, s);
            match (part, signed) {
                (0, false) => g.a.ubfx(0, 0, 0, 8),
                (0, true) => g.a.sbfx(0, 0, 0, 8),
                (1, false) => g.a.ubfx(0, 0, 0, 16),
                (1, true) => g.a.sbfx(0, 0, 0, 16),
                (_, false) => g.a.ubfx(0, 0, 16, 16),
                (_, true) => g.a.sbfx(0, 0, 16, 16),
            }
            g.st_reg(0, d);
        }
        Fast::MinMax { d, a, b, kind } => {
            g.ld_reg(0, a);
            g.ld_reg(1, b);
            g.a.cmp(0, 1);
            let c = match kind { 0 => AC::Lt, 1 => AC::Gt, 2 => AC::Lo, _ => AC::Hi };
            g.a.csel(0, 0, 1, c);
            g.st_reg(0, d);
        }
        // ---- branches (always a block's last op)
        Fast::BrI { a, v, cond, target } => {
            let t = g.a.label();
            g.ld_reg(0, a);
            g.a.mov32(1, v);
            g.a.cmp(0, 1);
            g.a.b_cond(acond(cond), t);
            g.chain(next_pc, count);
            g.a.bind(t);
            g.chain(target, count);
        }
        Fast::BrIEq { a, v, target } | Fast::BrINe { a, v, target } => {
            let t = g.a.label();
            g.ld_reg(0, a);
            g.a.mov32(1, v);
            g.a.cmp(0, 1);
            g.a.b_cond(if matches!(fast, Fast::BrIEq { .. }) { AC::Eq } else { AC::Ne }, t);
            g.chain(next_pc, count);
            g.a.bind(t);
            g.chain(target, count);
        }
        Fast::BrR { a, b, cond, target } => {
            let t = g.a.label();
            g.ld_reg(0, a);
            g.ld_reg(1, b);
            g.cmp(cond.is_float(), 0, 1);
            g.a.b_cond(acond(cond), t);
            g.chain(next_pc, count);
            g.a.bind(t);
            g.chain(target, count);
        }
        Fast::BitBr { r, mk, ne, mv, target } => {
            let t = g.a.label();
            g.rhs(mk, mv);
            g.ld_reg(0, r);
            g.a.tst(0, 1);
            g.a.b_cond(if ne { AC::Ne } else { AC::Eq }, t);
            g.chain(next_pc, count);
            g.a.bind(t);
            g.chain(target, count);
        }
        Fast::DecBr { r, cond, v, target } => {
            let t = g.a.label();
            g.ld_reg(0, r);
            g.a.sub_imm(0, 0, 1);
            g.st_reg(0, r);
            g.a.mov32(1, v);
            g.a.cmp(0, 1);
            g.a.b_cond(acond(cond), t);
            g.chain(next_pc, count);
            g.a.bind(t);
            g.chain(target, count);
        }
        Fast::Goto { target } => g.chain(target, count),
        Fast::Call { target } => {
            g.a.mov32(0, next_pc);
            g.a.str(0, CPU, off_rets());
            g.chain(target, count);
        }
        Fast::IfBlk { a, rk, fam, alt, has_else: false, rv, then_d, .. } => {
            // without an else part a predicated block is a conditional jump over its then part
            let taken = g.a.label();
            g.if_jump(a, rk, fam, alt, rv, taken);
            g.chain(next_pc.wrapping_add(then_d as u32), count);
            g.a.bind(taken);
            g.chain(next_pc, count);
        }
        // (the multiply-accumulates 7 / 8 read d too, and the rounding conversions other than
        // trunc have no single A64 instruction here: through the interpreter's executor)
        Fast::FOp { d, a, b, op } if matches!(op, 0..=6 | 0x1f | 0x5f | 0x8f | 0x9f) => {
            g.ld_reg(0, a);
            match op {
                0x1f | 0x5f => {
                    g.a.fmov_s_w(0, 0);
                    if op == 0x1f { g.a.fcvtzs(0, 0) } else { g.a.fcvtzu(0, 0) }
                }
                0x8f | 0x9f => {
                    if op == 0x8f { g.a.scvtf(0, 0) } else { g.a.ucvtf(0, 0) }
                    g.a.fmov_w_s(0, 0);
                }
                _ => {
                    g.ld_reg(1, b);
                    g.a.fmov_s_w(0, 0);
                    g.a.fmov_s_w(1, 1);
                    let opc = match op { 0 => 0x1e20_2800, 1 => 0x1e20_3800, 2 => 0x1e20_0800, 3 => 0x1e20_1800, 5 => 0x1e20_7800, _ => 0x1e20_6800 };
                    g.a.fop(opc, 0, 0, 1);
                    g.a.fmov_w_s(0, 0);
                }
            }
            g.st_reg(0, d);
        }
        Fast::Rts => {
            g.a.ldr(9, CPU, off_rets());
            g.ind(9, count);
        }
        Fast::CallR { r } => {
            g.a.mov32(0, next_pc);
            g.a.str(0, CPU, off_rets());
            g.ld_reg(9, r);
            g.ind(9, count);
        }
        Fast::GotoR { r } => {
            g.ld_reg(9, r);
            g.ind(9, count);
        }
        // ---- memory
        Fast::Ld { d, b, off, w, signed } => {
            match b {
                Base::Reg(r) => g.ld_reg(0, r),
                Base::Sp => g.a.ldr(0, CPU, off_sp()),
            }
            g.add_imm(0, 0, off);
            g.load(w, signed, idx);
            g.st_reg(0, d);
            bus_used = true;
        }
        Fast::LdW { d, b, off } => {
            g.ld_reg(0, b);
            g.add_imm(0, 0, off);
            g.load(4, false, idx);
            g.st_reg(0, d);
            bus_used = true;
        }
        Fast::LdWSp { d, off } => {
            g.a.ldr(0, CPU, off_sp());
            g.add_imm(0, 0, off);
            g.load(4, false, idx);
            g.st_reg(0, d);
            bus_used = true;
        }
        Fast::LdBu { d, b, off } => {
            g.ld_reg(0, b);
            g.add_imm(0, 0, off);
            g.load(1, false, idx);
            g.st_reg(0, d);
            bus_used = true;
        }
        Fast::St { s, b, off, w } => {
            match b {
                Base::Reg(r) => g.ld_reg(0, r),
                Base::Sp => g.a.ldr(0, CPU, off_sp()),
            }
            g.add_imm(0, 0, off);
            g.ld_reg(1, s);
            g.store(w, idx);
            bus_used = true;
        }
        Fast::StW { s, b, off } => {
            g.ld_reg(0, b);
            g.add_imm(0, 0, off);
            g.ld_reg(1, s);
            g.store(4, idx);
            bus_used = true;
        }
        Fast::StWSp { s, off } => {
            g.a.ldr(0, CPU, off_sp());
            g.add_imm(0, 0, off);
            g.ld_reg(1, s);
            g.store(4, idx);
            bus_used = true;
        }
        Fast::LdIdx { d, b, i, sh, w, signed } => {
            g.ld_reg(0, b);
            g.ld_reg(1, i);
            g.a.add_lsl(0, 0, 1, sh as u32 & 31);
            g.load(w, signed, idx);
            g.st_reg(0, d);
            bus_used = true;
        }
        Fast::StIdx { s, b, i, sh, w } => {
            g.ld_reg(0, b);
            g.ld_reg(1, i);
            g.a.add_lsl(0, 0, 1, sh as u32 & 31);
            g.ld_reg(1, s);
            g.store(w, idx);
            bus_used = true;
        }
        Fast::LdPost { d, b, w, signed, inc } => {
            g.ld_reg(0, b);
            g.load(w, signed, idx);
            g.st_reg(0, d);
            g.ld_reg(0, b); // (after the load wrote d, as the interpreter)
            g.add_imm(0, 0, inc);
            g.st_reg(0, b);
            bus_used = true;
        }
        Fast::StPost { s, b, w, inc } => {
            g.ld_reg(0, b);
            g.ld_reg(1, s);
            g.store(w, idx);
            g.ld_reg(0, b);
            g.add_imm(0, 0, inc);
            g.st_reg(0, b);
            bus_used = true;
        }
        // (i 16: the immediate increment; else register i's)
        Fast::LdPre { d, b, i, w, signed, inc } => {
            g.ld_reg(0, b);
            if i < 16 {
                g.ld_reg(1, i);
                g.a.add(0, 0, 1);
            } else {
                g.add_imm(0, 0, inc);
            }
            g.st_reg(0, b);
            g.load(w, signed, idx);
            g.st_reg(0, d);
            bus_used = true;
        }
        Fast::StPre { s, b, i, w, inc } => {
            g.ld_reg(0, b);
            if i < 16 {
                g.ld_reg(1, i);
                g.a.add(0, 0, 1);
            } else {
                g.add_imm(0, 0, inc);
            }
            g.st_reg(0, b);
            g.ld_reg(1, s);
            g.store(w, idx);
            bus_used = true;
        }
        Fast::LdPostR { d, b, i, w, signed } => {
            g.ld_reg(KEEP, i); // (the increment as it was before the load wrote d)
            g.ld_reg(0, b);
            g.load(w, signed, idx);
            g.st_reg(0, d);
            g.ld_reg(0, b);
            g.a.add(0, 0, KEEP);
            g.st_reg(0, b);
            bus_used = true;
        }
        Fast::StPostR { s, b, i, w } => {
            g.ld_reg(0, b);
            g.ld_reg(1, s);
            g.store(w, idx);
            g.ld_reg(0, b);
            g.ld_reg(1, i);
            g.a.add(0, 0, 1);
            g.st_reg(0, b);
            bus_used = true;
        }
        Fast::Rmw { b, w, op: rop, off, v } => {
            g.ld_reg(0, b);
            g.add_imm(KEEP, 0, off);
            if let RmwOp::Mov = rop {
                g.a.mov32(1, v);
            } else {
                g.a.mov(0, KEEP);
                g.load(w, false, idx);
                g.a.mov32(2, v);
                match rop {
                    RmwOp::Or => g.a.orr(1, 0, 2),
                    RmwOp::And => g.a.and(1, 0, 2),
                    RmwOp::Xor => g.a.eor(1, 0, 2),
                    RmwOp::Add => g.a.add(1, 0, 2),
                    RmwOp::Sub => g.a.sub(1, 0, 2),
                    RmwOp::Mov => unreachable!(),
                }
            }
            g.a.mov(0, KEEP);
            g.store(w, idx);
            bus_used = true;
        }
        Fast::MemA4 { b, off, op: aop, rk, rv } => {
            g.rhs(rk, rv);
            g.a.mov(KEEP2, 1);
            g.ld_reg(0, b);
            g.add_imm(KEEP, 0, off as u32);
            g.a.mov(0, KEEP);
            g.load(4, false, idx);
            g.a.mov(1, KEEP2);
            g.alu(aop);
            g.a.mov(1, 0);
            g.a.mov(0, KEEP);
            g.store(4, idx);
            bus_used = true;
        }
        Fast::SpAdd { v } => {
            g.a.ldr(0, CPU, off_sp()); g.add_imm(0, 0, v);
            g.a.str(0, CPU, off_sp());
        }
        Fast::RFromSp { d, v } => {
            g.a.ldr(0, CPU, off_sp()); g.add_imm(0, 0, v); g.st_reg(0, d);
        }
        Fast::Mov2 { dh, dl, sh, sl } => {
            g.ld_reg(0, sh); g.ld_reg(1, sl);
            g.st_reg(0, dh); g.st_reg(1, dl);
        }
        Fast::Mov2I { dh, dl, v } => {
            g.a.mov32(0, v); g.st_reg(0, dl);
            g.a.mov32(0, ((v as i32) >> 31) as u32); g.st_reg(0, dh);
        }
        Fast::BitExt { d, s, p, l, signed } => {
            let (p, l) = (p as u32 & 31, l as u32 & 31);
            g.ld_reg(0, s); g.a.lsr_imm(0, 0, p);
            if l != 0 {
                if signed { g.a.sbfx(0, 0, 0, l); } else { g.a.ubfx(0, 0, 0, l); }
            }
            g.st_reg(0, d);
        }
        Fast::BitIns { d, s, p, l } => {
            let (p, l) = (p as u32 & 31, l as u32 & 31);
            let mask = if l == 0 { u32::MAX } else { (1u32 << l) - 1 };
            g.ld_reg(0, s); g.ld_reg(1, d);
            g.a.mov32(2, mask); g.a.and(0, 0, 2); g.a.lsl_imm(0, 0, p);
            g.a.mov32(2, !(mask << p)); g.a.and(1, 1, 2); g.a.orr(0, 0, 1);
            g.st_reg(0, d);
        }
        Fast::PushRets { n } => {
            let (top, bot) = ((n as i32).max(3), (n as i32).min(4));
            for r in std::iter::once(-1).chain((bot..=top).rev()) {
                g.a.ldr(0, CPU, off_sp()); g.a.sub_imm(0, 0, 4);
                g.a.str(0, CPU, off_sp());
                if r < 0 { g.a.ldr(1, CPU, off_rets()); } else { g.ld_reg(1, r as u8); }
                g.store(4, idx);
            }
            bus_used = true;
        }
        Fast::PopPc { n } => {
            let (top, bot) = ((n as i32).max(3), (n as i32).min(4));
            for r in bot..=top {
                g.a.ldr(0, CPU, off_sp()); g.load(4, false, idx); g.st_reg(0, r as u8);
                g.a.ldr(0, CPU, off_sp()); g.a.add_imm(0, 0, 4); g.a.str(0, CPU, off_sp());
            }
            g.a.ldr(0, CPU, off_sp()); g.load(4, false, idx); g.a.mov(KEEP, 0);
            g.a.ldr(0, CPU, off_sp()); g.a.add_imm(0, 0, 4); g.a.str(0, CPU, off_sp());
            g.ind_brk(KEEP, count, true);
        }
        Fast::PopRets => {
            g.a.ldr(0, CPU, off_sp()); g.load(4, false, idx); g.a.str(0, CPU, off_rets());
            g.a.ldr(0, CPU, off_sp()); g.a.add_imm(0, 0, 4); g.a.str(0, CPU, off_sp());
            bus_used = true;
        }
        Fast::Tb { r, half } => {
            g.ld_reg(0, r); g.add_imm(0, 0, next_pc);
            g.load(if half { 2 } else { 1 }, false, idx);
            g.a.lsl_imm(0, 0, 1); g.add_imm(0, 0, next_pc);
            g.ind_brk(0, count, true);
        }
        Fast::PushRet => {
            g.a.ldr(0, CPU, off_sp());
            g.a.sub_imm(0, 0, 4);
            g.a.str(0, CPU, off_sp());
            g.a.ldr(1, CPU, off_rets());
            g.store(4, idx);
            bus_used = true;
        }
        Fast::PopRet => {
            g.a.ldr(0, CPU, off_sp());
            g.load(4, false, idx);
            g.a.mov(KEEP, 0);
            g.a.ldr(0, CPU, off_sp());
            g.a.add_imm(0, 0, 4);
            g.a.str(0, CPU, off_sp());
            g.ind_brk(KEEP, count, true);
        }
        Fast::Un { d, s, k } => {
            g.ld_reg(0, s);
            match k {
                0 => g.a.mvn(0, 0),
                1 => g.a.neg(0, 0),
                // (i32::MIN stays i32::MIN, as wrapping_abs)
                2 => {
                    g.a.cmp_imm(0, 0);
                    g.a.cneg(0, 0, AC::Lt);
                }
                3 => g.a.rev(0, 0),
                4 => g.a.clz(0, 0),
                5 => {
                    g.a.mov32(1, 32767);
                    g.a.cmp(0, 1);
                    g.a.csel(0, 1, 0, AC::Gt);
                    g.a.mov32(1, (-32768i32) as u32);
                    g.a.cmp(0, 1);
                    g.a.csel(0, 1, 0, AC::Lt);
                }
                _ => {
                    g.a.cmp_imm(0, 0);
                    g.a.csel(0, ZR, 0, AC::Lt);
                    g.a.mov32(1, 65535);
                    g.a.cmp(0, 1);
                    g.a.csel(0, 1, 0, AC::Gt);
                }
            }
            g.st_reg(0, d);
        }
        Fast::Div { d, a, b, k } => {
            // sdiv / udiv give 0 for a divisor of 0 and i32::MIN for i32::MIN / -1, as the
            // interpreter; a remainder is a - q * b, and 0 for a divisor of 0
            g.ld_reg(0, a);
            g.ld_reg(1, b);
            if k == 0 || k == 2 {
                g.a.sdiv(2, 0, 1);
            } else {
                g.a.udiv(2, 0, 1);
            }
            if k >= 2 {
                g.a.msub(2, 2, 1, 0);
                g.a.cmp_imm(1, 0);
                g.a.csel(2, ZR, 2, AC::Eq);
            }
            g.st_reg(2, d);
        }
        // (`sti` opens the bus's interrupt window: a helper of its own, `h_sti`)
        Fast::SetIe { on: false } if off_ie() < 4096 => g.a.strb(ZR, CPU, off_ie()),
        Fast::SetIe { on: true } => {
            g.a.mov_x(0, CTX);
            g.a.add_imm(1, DONE, idx);
            g.call(h_sti::<B> as *const () as u64);
            bus_used = true;
        }
        // rev8 / rev16 / revs (exec_raw's route 22: d bits 28-31, the source 24-27, the form
        // 22-23; popcnt, form 3, stays with the executor)
        Fast::Raw { route: 22, raw } if (raw >> 22) & 3 != 3 => {
            g.ld_reg(0, ((raw >> 24) & 0xf) as u8);
            match (raw >> 22) & 3 {
                0 => g.a.rev(0, 0),
                1 => g.a.rev16(0, 0),
                _ => {
                    g.a.rev16(0, 0);
                    g.a.sxth(0, 0);
                }
            }
            g.st_reg(0, ((raw >> 28) & 0xf) as u8);
        }
        Fast::LdPair { p, b, off } => {
            match b {
                Base::Reg(r) => g.ld_reg(0, r),
                Base::Sp => g.a.ldr(0, CPU, off_sp()),
            }
            // (the address kept across both loads, as the interpreter takes it before either)
            g.add_imm(KEEP, 0, off);
            g.a.mov(0, KEEP);
            g.load(4, false, idx);
            g.st_reg(0, 2 * (p & 7));
            g.add_imm(0, KEEP, 4);
            g.load(4, false, idx);
            g.st_reg(0, 2 * (p & 7) + 1);
            bus_used = true;
        }
        Fast::StPair { p, b, off } => {
            match b {
                Base::Reg(r) => g.ld_reg(0, r),
                Base::Sp => g.a.ldr(0, CPU, off_sp()),
            }
            g.add_imm(KEEP, 0, off);
            g.a.mov(0, KEEP);
            g.ld_reg(1, 2 * (p & 7));
            g.store(4, idx);
            g.add_imm(0, KEEP, 4);
            g.ld_reg(1, 2 * (p & 7) + 1);
            g.store(4, idx);
            bus_used = true;
        }
        // ---- everything else: the interpreter's executor for this one op
        other => {
            let p: &'static Fast = Box::leak(Box::new(other));
            g.a.mov_x(0, CTX);
            g.a.ldr_lit_x(1, p as *const Fast as u64);
            g.idx_w2(idx);
            g.a.mov32(3, next_pc);
            g.call(op_helper::<B>(&other, profile));
            let fall = g.a.label();
            g.a.tbz(0, 32, fall); // bit 32: jumped (x0)
            g.ind_brk(0, count, true);
            g.a.bind(fall);
            bus_used = true;
            if other.may_jump() {
                // a jump through the interpreter (`pop` with pc in its mask) that fell
                // through: leave first if the next tick is due, as after any bus access
                g.check_brk(next_pc, count, 0);
                g.chain(next_pc, count);
            }
        }
    }
    bus_used
}

/// Native code for a block prefix of plain micro-ops (none careful), and its body's offset.
#[cfg(target_arch = "aarch64")]
#[allow(clippy::too_many_arguments)]
fn compile<B: Bus>(ops: &[crate::BOp], swaps: &[(Fast, Fast, u8)], then: &[crate::BOp], pc0: u32, mem: &JitMem, guards: &mut Vec<Box<RamGuard>>, profile: bool, itab: u64, stops: Option<bool>) -> Option<(Vec<u8>, usize)> {
    for o in [off_reg(15), off_sp(), off_rets(), off_pc(), off_hold(), off_stop_pc(), off_stop_io(), off_stop_rest()] {
        if o >= 16384 {
            return None;
        }
    }
    let mut a = Asm::new();
    let exit = a.label();
    let mut g = Gen {
        a,
        mem,
        exit,
        h_read: h_read::<B> as *const () as u64,
        h_write: h_write::<B> as *const () as u64,
        h_pair: h_pair::<B> as *const () as u64,
        itab,
        stops: stops.is_some(),
        rc: RegCache::default(),
    };
    // prologue (entry from Rust); chained blocks enter at the body
    g.a.stp_pre(29, 30, SP, -FRAME);
    g.a.stp(CPU, CTX, SP, 16);
    g.a.stp(RAM, XIP, SP, 32);
    g.a.stp(KEEP, KEEP2, SP, 48);
    g.a.stp(25, DONE, SP, 64);
    g.a.stp(LIMIT, 28, SP, 80);
    g.a.mov_x(CPU, 0);
    g.a.mov_x(CTX, 1);
    g.a.mov_x(RAM, 2);
    g.a.mov_x(XIP, 3);
    g.a.mov(LIMIT, 4);
    g.a.mov_x(FLAGS, 5);
    g.a.movz(DONE, 0, 0);
    let body = g.a.here();
    // a runner's stretch (`Cpu::stop_pc`): entered after the call's first block, from the end
    // of a block compiled whole, this is where one of the runner's calls ends. Back to Rust
    // here when the runner could decide otherwise from here on: the stop lies in this block,
    // a helper may have reached a peripheral, the stretch is spent (`STRETCH_ROOM` short of
    // the limit), or this block isn't compiled whole (its rest runs interpreted)
    if let Some(whole) = stops {
        let (go, stop) = (g.a.label(), g.a.label());
        g.a.cbz(DONE, go);
        g.a.ldr(9, CPU, off_stop_pc());
        g.a.cbz(9, go);
        if whole {
            let bytes: u32 = ops.iter().map(|o| o.len as u32).sum();
            g.a.mov32(10, pc0);
            g.a.sub(9, 9, 10);
            g.a.cmp_imm(9, bytes);
            g.a.b_cond(AC::Lo, stop);
            g.a.ldr(9, CPU, off_stop_io());
            g.a.cbnz(9, stop);
            g.a.add_imm(9, DONE, crate::STRETCH_ROOM);
            g.a.cmp(9, LIMIT);
            g.a.b_cond(AC::Lo, go);
        }
        g.a.bind(stop);
        g.exit_imm(pc0, 0);
        g.a.bind(go);
    }
    // the whole prefix must fit the call's limit, else back to Rust before running any of it
    {
        let fits = g.a.label();
        g.a.add_imm(9, DONE, insns(ops) + then.len() as u32);
        g.a.cmp(9, LIMIT);
        g.a.b_cond(AC::Ls, fits);
        g.exit_imm(pc0, 0);
        g.a.bind(fits);
    }

    if !(0x0200_0000..0x0400_0000).contains(&pc0) {
        let off = pc0.wrapping_sub(mem.ram_base) as usize;
        let len = ops.iter().chain(then).map(|op| op.len as usize).sum::<usize>() + 6;
        let len = len.min((mem.ram_len as usize).checked_sub(off)?);
        // SAFETY: jit_mem provides a live RAM allocation, and the range is checked above.
        let bytes = unsafe { std::slice::from_raw_parts(mem.ram.add(off), len) }.into();
        guards.push(Box::new(RamGuard::new(pc0, bytes)));
        let guard = guards.last_mut().unwrap().as_mut() as *mut RamGuard as u64;
        let valid = g.a.label();
        // no code word written since the snapshot was last found current: no call
        g.a.ldr_x(9, CTX, OFF_VER);
        g.a.ldr_lit_x(1, guard);
        g.a.ldr_x(10, 1, 0);
        g.a.cmp_x(9, 10);
        g.a.b_cond(AC::Eq, valid);
        g.a.mov_x(0, CTX);
        g.call(h_ram_ok::<B> as *const () as u64);
        g.a.cbnz(0, valid);
        g.exit_imm(pc0, 0);
        g.a.bind(valid);
    }

    let mut pc = pc0;
    // instructions so far (a swapped pair is two)
    let mut ins = 0u32;
    for op in ops {
        let next_pc = pc + op.len as u32;
        if let Fast::Swap { idx: s } = op.fast {
            // a dual-issue pair run second instruction first (the pair rule's result, as
            // `exec_careful_op` runs it whole): the second with both ticks taken, then the head
            // (registers only); then leave if the next tick is due or an interrupt is pending,
            // as the careful path stops after a pair
            let (head, tail, hlen) = swaps[s as usize];
            let mid = pc + hlen as u32;
            emit_op::<B>(&mut g, tail, ins + 1, next_pc, profile);
            emit_op::<B>(&mut g, head, ins, mid, profile);
            g.check_pair(next_pc, ins + 2, insns(ops) - (ins + 2));
            ins += 2;
            pc = next_pc;
            continue;
        }
        if let Fast::IfBlk { a, rk, fam, alt, has_else: true, rv, then_d, else_d } = op.fast {
            // an if-else block with its then-part (`Jit::entry`: the block's last op): not taken,
            // on at the else part; taken, the then-part, then past the else part. Interrupts wait
            // through the then-part (an exit inside it leaves the skip in `Ctx::pred`, for
            // pred_skips) and after it the careful path stops if one is pending (`check_pair`).
            let te = next_pc.wrapping_add(then_d as u32);
            let ee = te.wrapping_add(else_d as u32);
            let taken = g.a.label();
            g.if_jump(a, rk, fam, alt, rv, taken);
            g.chain(te, ins + 1);
            g.a.bind(taken);
            let (mut tpc, mut j) = (next_pc, ins + 1);
            for (k, t) in then.iter().enumerate() {
                let tnext = tpc + t.len as u32;
                if emit_op::<B>(&mut g, t.fast, j, tnext, profile) && k + 1 < then.len() {
                    g.check_brk_pred(tnext, j + 1, te, ee, next_pc);
                }
                (tpc, j) = (tnext, j + 1);
            }
            if !then.is_empty() {
                g.check_pair(ee, j, 0);
            }
            g.chain(ee, j);
            ins = j;
            break;
        }
        let bus_used = emit_op::<B>(&mut g, op.fast, ins, next_pc, profile);
        if bus_used && !op.fast.may_jump() {
            let rest = insns(ops) - (ins + 1);
            if op.glue {
                // leaving between a glued pair's two instructions: interrupts wait for the second
                g.check_brk_hold(next_pc, ins + 1, rest);
            } else {
                g.check_brk(next_pc, ins + 1, rest);
            }
        }
        ins += 1;
        pc = next_pc;
        if op.fast.may_jump() {
            break; // (the block's last op)
        }
    }
    // fell off the end of the prefix
    if !ops.last().map_or(false, |o| o.fast.may_jump()) {
        g.chain(pc, ins);
    }
    g.a.bind(exit);
    g.a.ldp(LIMIT, 28, SP, 80);
    g.a.ldp(25, DONE, SP, 64);
    g.a.ldp(KEEP, KEEP2, SP, 48);
    g.a.ldp(RAM, XIP, SP, 32);
    g.a.ldp(CPU, CTX, SP, 16);
    g.a.ldp_post(29, 30, SP, FRAME);
    g.a.ret();
    Some((g.a.finish().iter().flat_map(|w| w.to_le_bytes()).collect(), body * 4))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Base, BOp};
    /// 256 bytes of RAM at 0 (no XIP); a word read at `MMIO` and up gives `mmio`, and makes the
    /// next tick due when `due_on_mmio` (a peripheral touched, as the Soc does).
    /// (`irq`: an interrupt pending; a write at `MMIO` and up sets it from the value's bit 0)
    /// (`quiet`: writes don't count as code writes, which would end a block after each)
    struct Memory { data: Vec<u8>, flags: Vec<u8>, version: u64, mmio: u32, due_on_mmio: bool, due: bool, irq: bool, quiet: bool }
    const MMIO: u32 = 0x1000;
    impl Memory {
        fn new() -> Self { Self { data: vec![0; 256], flags: vec![1; 65], version: 0, mmio: 0, due_on_mmio: false, due: false, irq: false, quiet: false } }
        fn wrote(&mut self) { if !self.quiet { self.version += 1; } }
    }
    impl Bus for Memory {
        fn read8(&mut self, a: u32) -> u8 { self.data[a as usize] }
        // (the halfword at a & !1, as the Soc reads one)
        fn read16(&mut self, a: u32) -> u16 { let a = (a & !1) as usize; u16::from_le_bytes(self.data[a..a + 2].try_into().unwrap()) }
        fn read32(&mut self, a: u32) -> u32 {
            if a >= MMIO {
                self.due |= self.due_on_mmio;
                return self.mmio;
            }
            u32::from_le_bytes(self.data[a as usize..a as usize+4].try_into().unwrap())
        }
        fn write8(&mut self, a: u32, v: u8) {
            if a >= MMIO {
                (self.irq, self.due) = (v & 1 != 0, self.due || self.due_on_mmio);
                return;
            }
            self.data[a as usize] = v; self.wrote();
        }
        fn write16(&mut self, a: u32, v: u16) { self.data[a as usize..a as usize+2].copy_from_slice(&v.to_le_bytes()); self.wrote(); }
        fn write32(&mut self, a: u32, v: u32) { self.data[a as usize..a as usize+4].copy_from_slice(&v.to_le_bytes()); self.wrote(); }
        fn tick_room(&self) -> u64 { 1000 }
        fn next_tick_due(&self) -> bool { self.due }
        fn irq_pending(&self) -> bool { self.irq }
        fn add_ticks(&mut self, _: u64) {}
        fn code_writes(&self) -> Option<u64> { Some(self.version) }
        fn jit_mem(&mut self) -> Option<JitMem> {
            Some(JitMem { ram: self.data.as_mut_ptr(), ram_base: 0, ram_len: 256,
                ram_code: self.flags.as_ptr(), xip: std::ptr::null(), xip_base: 0x02000000, xip_len: 0 })
        }
    }
    fn op(fast: Fast) -> BOp { BOp { fast, len: 2, careful: false, pair_head: false, glue: false } }
    fn hot(j: &mut Jit, id: usize, pc: u32, ops: &[BOp], mem: &JitMem) -> *const u8 {
        for _ in 0..HOT { j.entry::<Memory>(id, ops, &[], None, pc, mem, true, false); }
        j.entry::<Memory>(id, ops, &[], None, pc, mem, true, false).unwrap().code
    }
    #[test]
    fn ram_chain_rejects_changed_target_and_respects_budget() {
        let mut bus = Memory::new(); let mem = bus.jit_mem().unwrap();
        let mut cpu = Cpu::new(0); let mut j = Jit::new(false).unwrap();
        let from = hot(&mut j, 0, 0, &[op(Fast::Goto { target: 32 })], &mem);
        hot(&mut j, 1, 32, &[op(Fast::MovI { d: 0, v: 42 })], &mem);
        let (_, slot, _) = unsafe { run(from, &mut cpu, &mut bus, &mem, 0, 10, false) };
        assert!(!slot.is_null());
        unsafe { link(slot, j.body(1).unwrap()); }
        assert_eq!(unsafe { run(from, &mut cpu, &mut bus, &mem, 0, 1, false) }.0, 1);
        assert_eq!(cpu.regs[0], 0);
        assert_eq!(unsafe { run(from, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 2);
        assert_eq!(cpu.regs[0], 42);
        bus.write8(32, 1); cpu.regs[0] = 0;
        assert_eq!(unsafe { run(from, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 1);
        assert_eq!(cpu.pc, 32); assert_eq!(cpu.regs[0], 0);
    }
    /// Returns and computed jumps go straight on into the compiled body of their target through
    /// `Jit::itab`: only when the target's whole prefix fits the limit, its RAM snapshot is
    /// current, and (after a bus access) the next tick is not due; else back to Rust at the target.
    #[test]
    fn computed_jumps_continue_through_the_table() {
        let mut bus = Memory::new(); let mem = bus.jit_mem().unwrap();
        let mut j = Jit::new(false).unwrap();
        let rts = hot(&mut j, 0, 0, &[op(Fast::Rts)], &mem);
        let goto = hot(&mut j, 1, 2, &[op(Fast::GotoR { r: 3 })], &mem);
        let callr = hot(&mut j, 2, 4, &[op(Fast::CallR { r: 3 })], &mem);
        let pop = hot(&mut j, 3, 6, &[op(Fast::PopRet)], &mem);
        hot(&mut j, 4, 32, &[op(Fast::MovI { d: 0, v: 42 }), op(Fast::MovI { d: 1, v: 7 })], &mem);
        let fresh = |pc: u32| { let mut c = Cpu::new(0); c.rets = pc; c.regs[3] = pc; c.sp = MMIO; c };
        for (code, what) in [(rts, "rts"), (goto, "goto R"), (callr, "call R"), (pop, "pop pc")] {
            // into the body at 32, which runs to its end (then exits there: nothing compiled at 36)
            bus.mmio = 32;
            let mut cpu = fresh(32);
            assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 3, "{what}");
            assert_eq!((cpu.regs[0], cpu.regs[1], cpu.pc), (42, 7, 36), "{what}");
            // the limit: the target's prefix does not fit, so back to Rust before it
            let mut cpu = fresh(32);
            assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 2, false) }.0, 1, "{what}");
            assert_eq!((cpu.regs[0], cpu.pc), (0, 32), "{what}");
            // nothing compiled there (and an odd pc that maps to the same entry)
            for to in [64, 33, 32 + (2 << ITAB_BITS)] {
                bus.mmio = to;
                let mut cpu = fresh(to);
                assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 1, "{what}");
                assert_eq!((cpu.regs[0], cpu.pc), (0, to), "{what}");
            }
        }
        // call R leaves its return address
        let mut cpu = fresh(32);
        unsafe { run(callr, &mut cpu, &mut bus, &mem, 0, 10, false) };
        assert_eq!(cpu.rets, 6);
        // a pop from a peripheral that makes the next tick due: leave at the target
        (bus.mmio, bus.due_on_mmio) = (32, true);
        let mut cpu = fresh(32);
        assert_eq!(unsafe { run(pop, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 1);
        assert_eq!((cpu.regs[0], cpu.pc, cpu.sp), (0, 32, MMIO + 4));
        (bus.due, bus.due_on_mmio) = (false, false);
        // the target's code changed (a write the bus counts): its guard sends it back to Rust
        bus.write8(33, 0xff);
        let mut cpu = fresh(32);
        assert_eq!(unsafe { run(rts, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 1);
        assert_eq!((cpu.regs[0], cpu.pc), (0, 32));
        // a reset forgets the table
        j.reset();
        assert!(j.itab.iter().enumerate().all(|(i, e)| e.pc == itab_empty(i).pc && itab_slot(e.pc) != i));
    }

    /// A body evicted from the table by a later one on the same entry goes back in with
    /// `relist` (as run_fast does when an exit with no chain slot lands on it), and the
    /// computed jumps enter it again.
    #[test]
    fn an_evicted_body_is_listed_again() {
        let mut bus = Memory::new(); let mem = bus.jit_mem().unwrap();
        let mut j = Jit::new(false).unwrap();
        let rts = hot(&mut j, 0, 0, &[op(Fast::Rts)], &mem);
        hot(&mut j, 1, 32, &[op(Fast::MovI { d: 0, v: 42 })], &mem);
        let mut go = |to: u32| {
            let mut cpu = Cpu::new(0); cpu.rets = to;
            let k = unsafe { run(rts, &mut cpu, &mut bus, &mem, 0, 10, false) }.0;
            (k, cpu.regs[0], cpu.pc)
        };
        assert_eq!(go(32), (2, 42, 34));
        // a later body on the same entry, 32 KB on (past this bus's RAM: put there by hand)
        let other = 32 + (2 << ITAB_BITS);
        j.itab[itab_slot(32)] = ITab { pc: other, _pad: 0, body: 0 };
        assert_eq!(go(32), (1, 0, 32));
        j.relist(32, j.body(1).unwrap());
        assert_eq!(go(32), (2, 42, 34));
        assert_eq!(go(other), (1, 0, other));
    }

    /// A computed jump to 0xffff_ffff (an erased-flash function pointer, a trashed rets) whose
    /// entry is empty goes back to Rust there, as the interpreter would, not into body 0.
    #[test]
    fn computed_jump_to_all_ones_goes_back_to_rust() {
        let mut bus = Memory::new(); let mem = bus.jit_mem().unwrap();
        let mut j = Jit::new(false).unwrap();
        let rts = hot(&mut j, 0, 0, &[op(Fast::Rts)], &mem);
        let mut cpu = Cpu::new(0); cpu.rets = u32::MAX;
        assert_eq!(unsafe { run(rts, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 1);
        assert_eq!(cpu.pc, u32::MAX);
    }

    /// A swapped pair (`Fast::Swap`) compiled: its second instruction, then the head. Random
    /// register-only heads and random second instructions (no jumps: those end the prefix before
    /// the pair) that `swappable_with` takes must leave what the pair rule (`exec_fast_paired`,
    /// as `step` runs a pair) leaves: registers, sp, rets, pc, memory; and the two count as two
    /// instructions (the limit, the count returned).
    #[test]
    fn compiled_swapped_pairs_match_the_pair_rule() {
        use crate::{Op, RmwOp};
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed as u32 };
        let ops = [Op::Add, Op::Sub, Op::RSub, Op::Or, Op::Xor, Op::And, Op::AndNot, Op::Mul, Op::Shl, Op::Shr, Op::Sar];
        let mut jit = Jit::new(false).unwrap();
        let (mut id, mut n) = (0, 0);
        while n < 3000 {
            let (x, y) = (rnd(), rnd());
            // registers 0..7 for data, 8..11 hold addresses in the test RAM (as below)
            let mut rr = { let mut z = rnd(); move || { z = z.rotate_right(3); (z % 8) as u8 } };
            let op = ops[x as usize / 32 % ops.len()];
            let head = match x % 17 {
                0 => Fast::Mov { d: rr(), s: rr() },
                1 => Fast::MovI { d: rr(), v: x },
                2 => Fast::Alu3 { d: rr(), a: rr(), b: rr(), op },
                3 => Fast::Alu3I { d: rr(), a: rr(), v: x >> 7, op },
                4 => Fast::Alu2 { d: rr(), b: rr(), op },
                5 => Fast::Alu2I { d: rr(), v: x >> 9, op },
                6 => Fast::A4 { d: rr(), s: rr(), op, rk: (x >> 5) as u8 % 3, rv: if (x >> 5) % 3 == 1 { x >> 3 } else { rr() as u32 } },
                7 => Fast::AddRI { d: rr(), s: rr(), v: x >> 4 },
                8 => Fast::ShlI { d: rr(), s: rr(), n: (x >> 8) as u8 % 32 },
                9 => Fast::SarI { d: rr(), s: rr(), n: (x >> 8) as u8 % 32 },
                10 => Fast::Ext { d: rr(), s: rr(), part: (x >> 8) as u8 % 3, signed: x & 0x100 != 0 },
                11 => Fast::BitExt { d: rr(), s: rr(), p: (x >> 8) as u8 % 32, l: (x >> 16) as u8 % 32, signed: x & 0x80 != 0 },
                12 => Fast::BitIns { d: rr(), s: rr(), p: (x >> 8) as u8 % 32, l: (x >> 16) as u8 % 32 },
                13 => Fast::Mov2 { dh: rr(), dl: rr(), sh: rr(), sl: rr() },
                14 => Fast::Mov2I { dh: rr(), dl: rr(), v: x },
                15 => Fast::Un { d: rr(), s: rr(), k: (x >> 8) as u8 % 7 },
                _ => Fast::MinMax { d: rr(), a: rr(), b: rr(), kind: (x >> 8) as u8 % 4 },
            };
            let w = [1u8, 2, 4][y as usize % 3];
            let b = 8 + (y >> 4) as u8 % 3;
            let tail = match y % 13 {
                0 => Fast::Ld { d: rr(), b: Base::Reg(b), off: (y >> 8) % 4, w, signed: y & 0x80 != 0 },
                1 => Fast::St { s: rr(), b: Base::Reg(b), off: (y >> 8) % 4, w },
                2 => Fast::LdPost { d: rr(), b, w, signed: y & 0x80 != 0, inc: 4 },
                3 => Fast::StPost { s: rr(), b, w, inc: 0xffff_fffc },
                4 => Fast::LdIdx { d: rr(), b, i: 11, sh: (y >> 8) as u8 % 3, w, signed: y & 0x80 != 0 },
                5 => Fast::StIdx { s: rr(), b, i: 11, sh: (y >> 8) as u8 % 3, w },
                6 => Fast::Rmw { b, w, op: RmwOp::Xor, off: 1, v: y },
                7 => Fast::MemA4 { b: 8, off: 4, op: Op::Add, rk: 0, rv: rr() as u32 },
                8 => Fast::LdW { d: rr(), b: 8, off: 4 },
                9 => Fast::StW { s: rr(), b: 8, off: 4 },
                10 => Fast::Alu3 { d: rr(), a: rr(), b: rr(), op },
                11 => Fast::Mov { d: rr(), s: rr() },
                _ => Fast::MovI { d: rr(), v: y },
            };
            if !head.swappable_with(tail) {
                continue;
            }
            n += 1;
            let mut bus = Memory::new(); let mut reference = Memory::new();
            for i in 32..256 { let v = rnd() as u8; bus.data[i] = v; reference.data[i] = v; }
            bus.flags.iter_mut().for_each(|f| *f = 0);
            let mem = bus.jit_mem().unwrap();
            let mut cpu = Cpu::new(0);
            for r in 0..8 { cpu.regs[r] = if rnd() % 3 == 0 { rnd() % 40 } else { rnd() }; }
            cpu.regs[8] = 64 + (rnd() % 32) * 4; cpu.regs[9] = 65 + (rnd() % 32) * 2; cpu.regs[10] = 129 + rnd() % 64;
            cpu.regs[11] = rnd() % 16;
            let mut e = Cpu::new(0);
            e.regs = cpu.regs;
            let _ = e.exec_fast_paired(&mut reference, head, true, 2);
            let pc = e.exec_fast_paired(&mut reference, tail, false, 4).unwrap_or(4);
            // the pair alone: it fits a limit of 2, not 1
            let swap = BOp { fast: Fast::Swap { idx: 0 }, len: 4, careful: true, pair_head: true, glue: false };
            for _ in 0..HOT { jit.entry::<Memory>(id, &[swap], &[(head, tail, 2)], None, 0, &mem, true, false); }
            let code = jit.entry::<Memory>(id, &[swap], &[(head, tail, 2)], None, 0, &mem, true, false).unwrap().code;
            id += 1;
            let regs0 = cpu.regs;
            assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 1, false) }.0, 0, "{head:?} {tail:?}");
            assert_eq!(cpu.regs, regs0);
            assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 2, false) }.0, 2, "{head:?} {tail:?}");
            let regs = |c: &Cpu| c.regs.map(|r| format!("{r:08x}"));
            assert_eq!((regs(&cpu), cpu.pc), (regs(&e), pc), "{head:?} then {tail:?}");
            assert_eq!(bus.data, reference.data, "{head:?} then {tail:?}");
        }
    }

    /// A RAM body enters without `h_ram_ok` while the bus's count of code-word writes is the
    /// one its snapshot was last found current at (`RamGuard::ver`); a write the bus counts
    /// sends it to `h_ram_ok`, which compares the snapshot: still the same (the write was not to
    /// its bytes) enters, else back to Rust. A compiled store into its own code word leaves
    /// right after it, and the body is not entered again.
    #[test]
    fn ram_guards_compare_the_write_count_inline() {
        let mut bus = Memory::new(); let mem = bus.jit_mem().unwrap();
        bus.flags.fill(0); // (no watched words yet: compiled stores go straight to RAM)
        let mut j = Jit::new(false).unwrap();
        let ops = [op(Fast::MovI { d: 0, v: 42 }), op(Fast::St { s: 2, b: Base::Reg(1), off: 0, w: 1 })];
        let code = hot(&mut j, 0, 32, &ops, &mem);
        let guard = &j.guards[0];
        assert_eq!(guard.ver, u64::MAX);
        let fresh = |r1: u32| { let mut c = Cpu::new(32); (c.regs[1], c.regs[2]) = (r1, 0x5a); c };
        // the first entry finds the snapshot current (a call), the count is kept
        let mut cpu = fresh(200);
        assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 2);
        assert_eq!((cpu.regs[0], cpu.pc, bus.data[200]), (42, 36, 0x5a));
        assert_eq!(j.guards[0].ver, bus.version);
        // the snapshot changed behind the bus's back (no count): the inline compare enters
        // anyway (the bus counts every write that can reach code: that is what it relies on)
        bus.data[32] ^= 0xff;
        let mut cpu = fresh(200);
        assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 2);
        bus.data[32] ^= 0xff;
        // a counted write elsewhere: h_ram_ok compares, finds it current, takes the new count
        bus.write8(100, 1);
        let mut cpu = fresh(200);
        assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 2);
        assert_eq!(j.guards[0].ver, bus.version);
        // the body's store into its own code (a watched word: through the bus, which counts it):
        // it leaves right after, and the next entry finds the snapshot changed
        bus.flags[33 / 4] = 1;
        let mut cpu = fresh(33);
        assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 2);
        assert_eq!((bus.data[33], cpu.pc), (0x5a, 36));
        let mut cpu = fresh(200);
        assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 0);
        assert_eq!((cpu.regs[0], cpu.pc), (0, 32));
    }

    /// An if-else block compiled with its then-part in tow (`Jit::entry`'s `then`) against the
    /// careful path (`exec_careful`, as run_fast runs it, stopping where a tick comes due): not
    /// taken, on at the else part; taken, the then-part and past the else part, stopping there
    /// with an interrupt pending; a then-part instruction that makes the next tick due leaves
    /// right after it with the skip in pred_skips (or, the last one, past the else part).
    #[test]
    fn if_else_blocks_with_their_then_part_match_the_careful_path() {
        use crate::Op;
        let mut seed = 0x1357_9bdf_2468_ace0u64;
        let mut rnd = move || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed as u32 };
        let mut jit = Jit::new(false).unwrap();
        let mut id = 0;
        for round in 0..3000 {
            let (x, y) = (rnd(), rnd());
            let fam = [0u8, 1, 2, 4, 5, 6][x as usize % 6];
            let (rk, alt) = ((x >> 4) as u8 % 3, x & 0x100 != 0);
            let rv = if rk == 1 { rnd() % 8 } else { (x >> 9) % 4 };
            // (careful, as the decoder makes an if-else block)
            let iff = |then_d, else_d| BOp { careful: true, ..op(Fast::IfBlk { a: 0, rk, fam, alt, has_else: true, rv, then_d, else_d }) };
            // a then-part of 0-4 plain ops, a byte store to a peripheral among them now and then
            let mut then = vec![];
            for k in 0..y % 5 {
                then.push(op(match (rnd() % 4, k) {
                    (0, _) if y & 0x100 != 0 => Fast::St { s: 2, b: Base::Reg(9), off: 0, w: 1 },
                    (1, _) => Fast::Alu2I { d: 1, v: rnd() % 7, op: Op::Add },
                    (2, _) => Fast::MovI { d: 3, v: rnd() },
                    _ => Fast::St { s: 1, b: Base::Reg(8), off: 0, w: 4 },
                }));
            }
            let then_d = 2 * then.len() as u16;
            let else_d = 2 * (1 + rnd() % 4) as u16;
            let (te, ee) = (2 + then_d as u32, 2 + (then_d + else_d) as u32);
            let block = [iff(then_d, else_d)];
            let mut bus = Memory::new();
            bus.flags.fill(0);
            (bus.irq, bus.due_on_mmio) = (rnd() % 3 == 0, rnd() % 2 == 0);
            let mem = bus.jit_mem().unwrap();
            let mut cpu = Cpu::new(0);
            for r in 0..4 { cpu.regs[r] = rnd() % 6; }
            (cpu.regs[8], cpu.regs[9], cpu.regs[2]) = (64 + 4 * (rnd() % 16), MMIO, rnd());
            let mut e = Cpu::new(0);
            e.regs = cpu.regs;
            let mut reference = Memory { data: bus.data.clone(), flags: bus.flags.clone(), version: 0, mmio: 0, due_on_mmio: bus.due_on_mmio, due: false, irq: bus.irq, quiet: false };
            // the careful path: the if (nothing in progress), then its then-part while one is
            let (mut k, mut stop) = (1, false);
            let (mut still, _) = e.exec_careful(&mut reference, block[0].fast, false, false, 2);
            let mut at = 2;
            if still && at != te {
                for t in &then {
                    let (s2, st) = e.exec_careful(&mut reference, t.fast, false, true, at + 2);
                    (still, stop, k, at) = (s2, st, k + 1, at + 2);
                    if stop || reference.next_tick_due() {
                        break;
                    }
                }
            }
            for _ in 0..HOT { jit.entry::<Memory>(id, &block, &[], Some(&then), 0, &mem, true, false); }
            let p = jit.entry::<Memory>(id, &block, &[], Some(&then), 0, &mem, true, false).unwrap();
            id += 1;
            assert_eq!(p.insns, 1 + then.len() as u32);
            let (got, _, got_stop) = unsafe { run(p.code, &mut cpu, &mut bus, &mem, 0, 10, false) };
            let what = format!("round {round}: {:?} then {:?}", block[0].fast, then.iter().map(|o| o.fast).collect::<Vec<_>>());
            assert_eq!((got, cpu.pc, got_stop), (k, e.pc, stop), "{what}");
            assert_eq!((cpu.regs, &cpu.pred_skips, &bus.data), (e.regs, &e.pred_skips, &reference.data), "{what}");
            let _ = (te, ee, still);
        }
    }

    /// Blocks of several ops over all 16 registers (more than the AArch64 code keeps in host
    /// registers, so some are given up and taken again mid-block), loads and stores among them,
    /// against the interpreter running the same ops one after another.
    #[test]
    fn compiled_blocks_match_interpreter() {
        use crate::Op;
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed as u32 };
        let ops = [Op::Add, Op::Sub, Op::RSub, Op::Or, Op::Xor, Op::And, Op::AndNot, Op::Mul, Op::Shl, Op::Shr, Op::Sar];
        let mut jit = Jit::new(false).unwrap();
        for case in 0..3000usize {
            // results go to 0..7 and 12..15: 8..11 keep pointing into the test RAM (8..10 bases, 11 an index)
            let dst = |x: u32| { let d = (x % 12) as u8; if d < 8 { d } else { d + 4 } };
            let mut block = vec![];
            for _ in 0..(2 + rnd() % 14) {
                let (a, b, c) = (rnd(), rnd(), rnd());
                let d = dst(a);
                let s1 = (b % 16) as u8;
                let s2 = (c % 16) as u8;
                let base = 8 + (b % 3) as u8;
                let w = [1u8, 2, 4][(c % 3) as usize];
                let p = [0u8, 1, 2, 3, 6, 7][(b % 6) as usize];
                block.push(match rnd() % 16 {
                    12 => Fast::Un { d, s: s1, k: (c % 7) as u8 },
                    13 => Fast::Div { d, a: s1, b: s2, k: (c % 4) as u8 },
                    14 => Fast::LdPair { p, b: Base::Reg(base), off: 4 * (c % 4) },
                    15 => Fast::StPair { p, b: if a & 1 != 0 { Base::Sp } else { Base::Reg(base) }, off: 4 * (c % 4) },
                    0 => Fast::Alu3 { d, a: s1, b: s2, op: ops[(a >> 8) as usize % ops.len()] },
                    1 => Fast::Alu3I { d, a: s1, v: c, op: ops[(a >> 8) as usize % ops.len()] },
                    2 => Fast::Alu2 { d, b: s2, op: ops[(a >> 8) as usize % ops.len()] },
                    3 => Fast::Mov { d, s: s1 },
                    4 => Fast::MovI { d, v: c },
                    5 => Fast::AddRI { d, s: s1, v: c },
                    6 => Fast::Ld { d, b: Base::Reg(base), off: c % 8, w, signed: a & 1 != 0 },
                    7 => Fast::St { s: s1, b: Base::Reg(base), off: c % 8, w },
                    8 => Fast::LdW { d, b: 8, off: 4 * (c % 4) },
                    9 => Fast::StW { s: s1, b: 8, off: 4 * (c % 4) },
                    10 => Fast::LdPost { d, b: base, w, signed: false, inc: 2 },
                    _ => Fast::Ext { d, s: s1, part: (c % 3) as u8, signed: a & 1 != 0 },
                });
            }
            let ops_b: Vec<BOp> = block.iter().map(|&f| op(f)).collect();
            let mut bus = Memory::new(); let mut reference = Memory::new();
            bus.quiet = true;
            for i in 32..256 { let v = rnd() as u8; bus.data[i] = v; reference.data[i] = v; }
            bus.flags.iter_mut().for_each(|f| *f = 0);
            let mem = bus.jit_mem().unwrap();
            let mut cpu = Cpu::new(0);
            for r in 0..16 { cpu.regs[r] = rnd(); }
            cpu.regs[8] = 64 + (rnd() % 16) * 4; cpu.regs[9] = 65 + (rnd() % 16) * 2; cpu.regs[10] = 129 + rnd() % 32;
            cpu.regs[11] = rnd() % 16;
            cpu.sp = 192; cpu.rets = 0x0200_1234;
            let mut expected = Cpu::new(0);
            expected.regs = cpu.regs; expected.sp = cpu.sp; expected.rets = cpu.rets;
            let mut pc = 0;
            for &f in &block { pc = expected.exec_fast(&mut reference, f, pc + 2).unwrap_or(pc + 2); }
            expected.pc = pc;
            let code = hot(&mut jit, case, 0, &ops_b, &mem);
            assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, block.len() as u32, false) }.0, block.len() as u32, "{block:?}");
            let regs = |c: &Cpu| c.regs.map(|r| format!("{r:08x}"));
            assert_eq!((regs(&cpu), cpu.pc), (regs(&expected), expected.pc), "case {case}: {block:?}");
            assert_eq!(bus.data, reference.data, "case {case}: {block:?}");
        }
    }
    #[test]
    fn a_runners_stretch_leaves_where_a_call_would_end() {
        let mut bus = Memory::new(); let mem = bus.jit_mem().unwrap();
        let mut cpu = Cpu::new(0); let mut j = Jit::new(false).unwrap();
        j.stops = true;
        // 0: goto 32; 32: two ops (bytes 32..36); 64: an op, then one that runs interpreted
        let from = hot(&mut j, 0, 0, &[op(Fast::Goto { target: 32 })], &mem);
        let to = hot(&mut j, 1, 32, &[op(Fast::MovI { d: 0, v: 42 }), op(Fast::MovI { d: 1, v: 7 })], &mem);
        let from2 = hot(&mut j, 2, 96, &[op(Fast::Goto { target: 64 })], &mem);
        let careful = BOp { careful: true, ..op(Fast::Nop) };
        hot(&mut j, 3, 64, &[op(Fast::MovI { d: 2, v: 5 }), careful], &mem);
        for (code, id) in [(from, 1), (from2, 3)] {
            let (_, slot, _) = unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 100, false) };
            unsafe { link(slot, j.body(id).unwrap()); }
        }
        let mut go = |code, stop_pc, stop_io, cap| {
            cpu.regs = [0; 16]; cpu.stop_pc = stop_pc; cpu.stop_io = stop_io;
            let k = unsafe { run(code, &mut cpu, &mut bus, &mem, 0, cap, false) }.0;
            (k, cpu.pc, cpu.regs[0])
        };
        // no stretch, or nothing to stop for: on into the next block
        assert_eq!(go(from, 0, 0, 100), (3, 36, 42));
        assert_eq!(go(from, 0x40, 0, 100), (3, 36, 42));
        // back before it: the stop starts it or lies in it, a peripheral was reached, or the
        // stretch is spent (`STRETCH_ROOM` short of the limit)
        assert_eq!(go(from, 32, 0, 100), (1, 32, 0));
        assert_eq!(go(from, 34, 0, 100), (1, 32, 0));
        assert_eq!(go(from, 36, 0, 100), (3, 36, 42));
        assert_eq!(go(from, 0x40, 1, 100), (1, 32, 0));
        let room = crate::STRETCH_ROOM;
        assert_eq!(go(from, 0x40, 0, 1 + room), (1, 32, 0));
        assert_eq!(go(from, 0x40, 0, 2 + room), (3, 36, 42));
        // a block not compiled whole: only outside a stretch
        assert_eq!(go(from2, 0, 0, 100).0, 2);
        assert_eq!(go(from2, 0x1000, 0, 100), (1, 64, 0));
        // never at the call's first block
        assert_eq!(go(to, 32, 1, 100), (2, 36, 42));
        cpu.stop_pc = 0; cpu.stop_io = 0;
    }
    #[test]
    fn code_store_exits_before_following_instruction() {
        let mut bus = Memory::new(); let mem = bus.jit_mem().unwrap();
        let mut cpu = Cpu::new(0); let mut j = Jit::new(false).unwrap();
        cpu.regs[0] = 123; cpu.regs[1] = 2;
        j.stops = true;
        let code = hot(&mut j, 0, 0, &[
            op(Fast::St { s: 0, b: Base::Reg(1), off: 0, w: 4 }),
            op(Fast::MovI { d: 2, v: 99 }), op(Fast::MovI { d: 3, v: 98 })], &mem);
        assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 10, false) }.0, 1);
        assert_eq!(cpu.pc, 2); assert_eq!(cpu.regs[2], 0);
        assert_eq!(bus.read32(2), 123); assert_eq!(bus.version, 1);
        // (a runner's stretch learns where the block would have ended)
        assert_eq!(cpu.stop_rest, 2);
    }
    #[test]
    fn direct_ops_match_interpreter_with_aliases_and_edge_fields() {
        let mut cases = vec![Fast::SpAdd { v: u32::MAX - 3 }, Fast::RFromSp { d: 0, v: 17 },
            Fast::Mov2 { dh: 1, dl: 0, sh: 0, sl: 1 }, Fast::Mov2I { dh: 0, dl: 1, v: 0x80000001 },
            Fast::PopRets, Fast::Tb { r: 0, half: false }, Fast::Tb { r: 0, half: true }];
        for n in [0, 3, 4, 15] { cases.push(Fast::PushRets { n }); cases.push(Fast::PopPc { n }); }
        for p in [0, 1, 15, 31] { for l in [0, 1, 16, 31] {
            for signed in [false, true] { cases.push(Fast::BitExt { d: 1, s: 1, p, l, signed }); }
            cases.push(Fast::BitIns { d: 1, s: 1, p, l });
            cases.push(Fast::BitIns { d: 1, s: 2, p, l });
        }}
        let mut j = Jit::new(false).unwrap();
        for (id, fast) in cases.into_iter().enumerate() {
            let mut bus = Memory::new(); let mut reference = Memory::new();
            for i in 128..256 { bus.data[i] = i as u8; reference.data[i] = i as u8; }
            let mem = bus.jit_mem().unwrap();
            let mut cpu = Cpu::new(0); let mut expected = Cpu::new(0);
            cpu.regs = [0x87654321; 16]; cpu.regs[0] = 128; cpu.regs[2] = 0xfedcba98;
            cpu.sp = 192; cpu.rets = 0x12345678;
            expected.regs = cpu.regs; expected.sp = cpu.sp; expected.rets = cpu.rets;
            expected.pc = expected.exec_fast(&mut reference, fast, 2).unwrap_or(2);
            let code = hot(&mut j, id, 0, &[op(fast)], &mem);
            assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 1, false) }.0, 1, "{fast:?}");
            assert_eq!((cpu.regs, cpu.sp, cpu.rets, cpu.pc), (expected.regs, expected.sp, expected.rets, expected.pc), "{fast:?}");
            assert_eq!(bus.data, reference.data, "{fast:?}");
        }
    }

    /// Every micro-op the compilers emit inline (and some through the executor), each alone in a block, from random registers
    /// (floats among them: NaN, both zeros, infinities, out of the integers' range): compiled
    /// code and the interpreter must leave the same registers, sp, rets, pc, memory and interrupt
    /// state (`ie`, icfg).
    #[test]
    fn compiled_ops_match_interpreter_from_random_state() {
        use crate::{Cond, Op, RmwOp};
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = move || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed as u32 };
        let floats = [0.0f32, -0.0, 1.0, -1.5, 0.5, f32::NAN, -f32::NAN, f32::INFINITY, f32::NEG_INFINITY,
            2147483648.0, -2147483904.0, 4294967296.0, 3e9, -3e9, 1e-40, 65536.25, f32::MAX, f32::MIN];
        let ops = [Op::Add, Op::Sub, Op::RSub, Op::Or, Op::Xor, Op::And, Op::AndNot, Op::Mul, Op::Shl, Op::Shr, Op::Sar];
        let conds = [Cond::Eq, Cond::Ne, Cond::LtU, Cond::GtU, Cond::LeU, Cond::GeU, Cond::LtS, Cond::GtS, Cond::LeS, Cond::GeS,
            Cond::FEq, Cond::FUne, Cond::FLt, Cond::FUge, Cond::FLe, Cond::FUgt, Cond::FGe, Cond::FUlt, Cond::FGt, Cond::FUle];
        let rmws = [RmwOp::Mov, RmwOp::Or, RmwOp::And, RmwOp::Xor, RmwOp::Add, RmwOp::Sub];
        // registers 8..15 hold addresses inside the test RAM (word, halfword, odd)
        let mut cases = vec![];
        for &op in &ops {
            cases.push(Fast::Alu3 { d: 0, a: 1, b: 2, op });
            cases.push(Fast::Alu3 { d: 1, a: 1, b: 1, op });
            cases.push(Fast::Alu3I { d: 0, a: 1, v: 0xffff_fff3, op });
            cases.push(Fast::Alu3I { d: 0, a: 1, v: 33, op });
            cases.push(Fast::Alu2 { d: 3, b: 2, op });
            cases.push(Fast::Alu2I { d: 3, v: 7, op });
            for rk in 0..3 { cases.push(Fast::A4 { d: 4, s: 5, op, rk, rv: if rk == 1 { 0x8000_0001 } else { 6 } }); }
            cases.push(Fast::MemA4 { b: 8, off: 4, op, rk: 0, rv: 2 });
        }
        for &cond in &conds {
            cases.push(Fast::BrR { a: 1, b: 2, cond, target: 0x40 });
            cases.push(Fast::BrR { a: 1, b: 1, cond, target: 0x40 });
            // (compares with an immediate and decrement-branches are integer only, as decoded)
            if !cond.is_float() {
                cases.push(Fast::BrI { a: 1, v: 0x8000_0000, cond, target: 0x40 });
                cases.push(Fast::DecBr { r: 3, cond, v: 5, target: 0x40 });
            }
        }
        for fop in [0u8, 1, 2, 3, 5, 6, 7, 8, 0x0f, 0x1f, 0x2f, 0x3f, 0x4f, 0x5f, 0x6f, 0x7f, 0x8f, 0x9f, 0xcf, 0xdf, 0xef, 0xff] { // (the ops fm1-isa decodes)
            cases.push(Fast::FOp { d: 0, a: 1, b: 2, op: fop });
            cases.push(Fast::FOp { d: 0, a: 1, b: 1, op: fop });
        }
        for kind in 0..4 { cases.push(Fast::MinMax { d: 0, a: 1, b: 2, kind }); }
        for k in 0..7 { cases.push(Fast::Un { d: 0, s: 1, k }); cases.push(Fast::Un { d: 1, s: 1, k }); }
        for k in 0..4 { cases.push(Fast::Div { d: 0, a: 1, b: 2, k }); cases.push(Fast::Div { d: 2, a: 2, b: 2, k }); }
        for on in [false, true] { cases.push(Fast::SetIe { on }); }
        for (p, b) in [(0, Base::Reg(8)), (1, Base::Reg(9)), (3, Base::Reg(10)), (2, Base::Sp)] {
            for off in [0u32, 4, 6, 0xffff_fffc] { cases.push(Fast::LdPair { p, b, off }); cases.push(Fast::StPair { p, b, off }); }
        }
        // (a pair that holds its own base: the address is taken before the first load)
        cases.push(Fast::LdPair { p: 4, b: Base::Reg(8), off: 4 });
        cases.push(Fast::StPair { p: 4, b: Base::Reg(9), off: 0 });
        for part in 0..3 { for signed in [false, true] { cases.push(Fast::Ext { d: 0, s: 1, part, signed }); } }
        for n in [0, 1, 31] { cases.push(Fast::ShlI { d: 0, s: 1, n }); cases.push(Fast::SarI { d: 0, s: 1, n }); }
        for mk in 0..3 { for ne in [false, true] { cases.push(Fast::BitBr { r: 1, mk, ne, mv: if mk == 1 { 0x10 } else { 2 }, target: 0x40 }); } }
        for (rk, rv) in [(0, 2), (1, 0x80), (2, 2)] { for fam in [0, 1, 2, 4, 5, 6] { for alt in [false, true] {
            cases.push(Fast::IfBlk { a: 1, rk, fam, alt, has_else: false, rv, then_d: 6, else_d: 0 });
        } } }
        for w in [1u8, 2, 4] { for b in [8u8, 9, 10] { for signed in [false, true] {
            cases.push(Fast::Ld { d: 0, b: Base::Reg(b), off: 2, w, signed });
            cases.push(Fast::LdIdx { d: 0, b, i: 11, sh: 1, w, signed });
            cases.push(Fast::LdPost { d: 0, b, w, signed, inc: 4 });
            cases.push(Fast::LdPost { d: b, b, w, signed, inc: 4 });
            for (d, i) in [(0, 11), (0, 16), (b, 11), (b, 16), (11, 11)] {
                cases.push(Fast::LdPre { d, b, i, w, signed, inc: 0xffff_fffe });
            }
            for (d, i) in [(0, 11), (b, 11), (11, 11), (0, b)] { cases.push(Fast::LdPostR { d, b, i, w, signed }); }
        }
            cases.push(Fast::St { s: 1, b: Base::Reg(b), off: 2, w });
            cases.push(Fast::St { s: 1, b: Base::Sp, off: 4, w });
            cases.push(Fast::StIdx { s: 1, b, i: 11, sh: 2, w });
            cases.push(Fast::StPost { s: 1, b, w, inc: 0xffff_fffe });
            for (s, i) in [(1, 11), (1, 16), (b, 11), (b, 16), (11, 11)] { cases.push(Fast::StPre { s, b, i, w, inc: 4 }); }
            for (s, i) in [(1, 11), (b, 11), (11, 11), (1, b)] { cases.push(Fast::StPostR { s, b, i, w }); }
            for &op in &rmws { cases.push(Fast::Rmw { b, w, op, off: 1, v: 0x1234_80ff }); }
        } }
        for (p, l) in [(0, 0), (0, 8), (1, 1), (7, 9), (16, 16), (31, 0), (31, 1), (3, 31)] {
            for signed in [false, true] { cases.push(Fast::BitExt { d: 0, s: 1, p, l, signed }); }
            cases.push(Fast::BitIns { d: 0, s: 1, p, l });
        }
        for n in [0, 3, 4, 7, 15] { cases.push(Fast::PushRets { n }); cases.push(Fast::PopPc { n }); }
        for half in [false, true] { cases.push(Fast::Tb { r: 8, half }); }
        for v in [5, 0x8000_0000] { cases.push(Fast::Mov2I { dh: 0, dl: 1, v }); cases.push(Fast::MovI { d: 2, v }); }
        cases.extend([Fast::Nop, Fast::Mov { d: 0, s: 1 }, Fast::AddRI { d: 0, s: 1, v: 0xffff_fffc },
            Fast::Mov2 { dh: 0, dl: 1, sh: 1, sl: 2 }, Fast::SpAdd { v: 0xffff_fff0 }, Fast::RFromSp { d: 0, v: 8 },
            Fast::BrIEq { a: 1, v: 3, target: 0x40 }, Fast::BrINe { a: 1, v: 3, target: 0x40 },
            Fast::Goto { target: 0x40 }, Fast::Call { target: 0x40 }, Fast::Rts,
            Fast::LdW { d: 0, b: 8, off: 4 }, Fast::LdWSp { d: 0, off: 8 }, Fast::LdBu { d: 0, b: 10, off: 3 },
            Fast::StW { s: 1, b: 8, off: 4 }, Fast::StWSp { s: 1, off: 8 },
            Fast::PopRets, Fast::PushRet, Fast::PopRet,
            Fast::CallR { r: 1 }, Fast::CallR { r: 8 }, Fast::GotoR { r: 2 }, Fast::GotoR { r: 9 },
            Fast::SetIe { on: false }, Fast::SetIe { on: true }, Fast::IcfgRd { d: 3 }, Fast::IcfgWr { s: 4 }]);
        // (route 22: rev8, rev16, revs inline, popcnt through the executor; into another and the same register)
        for form in 0..4u32 { for d in [0u32, 1] { cases.push(Fast::Raw { route: 22, raw: d << 28 | 1 << 24 | form << 22 | 0xe070 }); } }
        let mut jit = Jit::new(false).unwrap();
        for (j, mut fast) in cases.into_iter().enumerate() {
            for round in 0..24 {
                let mut bus = Memory::new(); let mut reference = Memory::new();
                // (bytes 0..32 stay 0: the block's own code, which its RAM guard checks)
                for i in 32..256 { let v = rnd() as u8; bus.data[i] = v; reference.data[i] = v; }
                bus.flags.iter_mut().for_each(|f| *f = 0); // no watched code words
                let mem = bus.jit_mem().unwrap();
                let mut cpu = Cpu::new(0);
                for r in 0..8 {
                    cpu.regs[r] = match rnd() % 4 { 0 => floats[rnd() as usize % floats.len()].to_bits(), 1 => rnd() % 40, _ => rnd() };
                }
                if round % 3 == 0 { cpu.regs[2] = cpu.regs[1]; }
                cpu.regs[8] = 64 + (rnd() % 32) * 4; cpu.regs[9] = 65 + (rnd() % 32) * 2; cpu.regs[10] = 129 + rnd() % 64;
                cpu.regs[11] = rnd() % 16;
                cpu.sp = 192; cpu.rets = 0x0200_1234;
                // (the interrupt state `cli`, `sti` and icfg read and write)
                cpu.ie = rnd() & 1 != 0; cpu.sreg_store[0] = rnd(); cpu.irq_levels = (0..rnd() % 3).map(|_| (rnd() % 8) as u8).collect();
                if let Fast::DecBr { ref mut v, .. } = fast { *v = cpu.regs[3].wrapping_sub(1); }
                // (a divisor of 0 and i32::MIN for the divisions and abs now and then)
                if round % 6 == 1 { cpu.regs[2] = 0; }
                if round % 6 == 2 { cpu.regs[1] = 0x8000_0000; cpu.regs[2] = u32::MAX; }
                cpu.ie = round % 2 == 0;
                let mut expected = Cpu::new(0);
                expected.regs = cpu.regs; expected.sp = cpu.sp; expected.rets = cpu.rets;
                (expected.ie, expected.sreg_store, expected.irq_levels) = (cpu.ie, cpu.sreg_store, cpu.irq_levels.clone());
                expected.pc = expected.exec_fast(&mut reference, fast, 2).unwrap_or(2);
                let code = hot(&mut jit, j * 24 + round, 0, &[op(fast)], &mem);
                assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 1, false) }.0, 1, "{fast:?}");
                let regs = |c: &Cpu| c.regs.map(|r| format!("{r:08x}"));
                assert_eq!((regs(&cpu), cpu.sp, cpu.rets, cpu.pc, cpu.ie), (regs(&expected), expected.sp, expected.rets, expected.pc, expected.ie),
                    "case {j} round {round}: {fast:?}");
                assert_eq!((cpu.ie, cpu.sreg_store), (expected.ie, expected.sreg_store), "case {j} round {round}: {fast:?}");
                assert_eq!(bus.data, reference.data, "case {j} round {round}: {fast:?}");
            }
        }
    }

}
