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
//! returns there). Every body first checks that its instructions still fit the call's tick
//! limit, so the chain never runs past a due tick; the helpers sync the clock with the running
//! instruction count, so timing stays exact across the chain.
//!
//! AArch64 register plan (AAPCS64 callee-saved, so helpers may be called freely):
//!   x19 = &Cpu, x20 = &Ctx, x21 = RAM host base, x22 = XIP host base,
//!   w23 / w24 = values kept across a helper call within one instruction, x25 = code flags,
//!   w26 = instructions done in this call before the current block, w27 = the limit
//!   (instructions this call may run). Guest registers, sp and rets stay in the Cpu struct;
//!   pc is stored at exit.

#[cfg(target_arch = "aarch64")]
use crate::jit_a64::{Asm, Cond as AC, Label, Reg, SP};
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
    _pad: u32,
    /// the chain slot of the exit taken back to Rust (null: not a chainable exit)
    slot: *mut u64,
    /// the bus's count of writes to code words when the call began (NO_VERSION: it keeps none);
    /// a helper that sees it change sets `brk`, so while `brk` is clear it still holds
    code_version: u64,
}
const OFF_BRK: u32 = 24;
const OFF_SLOT: u32 = 32;
const OFF_VERSION: u32 = 40;
const _: () = assert!(std::mem::offset_of!(Ctx, brk) == OFF_BRK as usize);
const _: () = assert!(std::mem::offset_of!(Ctx, slot) == OFF_SLOT as usize);
const _: () = assert!(std::mem::offset_of!(Ctx, code_version) == OFF_VERSION as usize);
const NO_VERSION: u64 = u64::MAX;

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
}

#[derive(Clone, Copy)]
enum Entry {
    Cold(u32),
    /// function start, body (chain target), prefix length
    Code(*const u8, *const u8, u32),
    No,
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
        Some(Jit { cache: CodeCache::new(64 << 20)?, state: Vec::new(), guards: Vec::new(), profile })
    }

    /// The compiled code of block `id` (prefix length), compiling it once it is hot.
    pub(crate) fn entry<B: Bus>(&mut self, id: usize, ops: &[crate::BOp], pc0: u32, mem: &JitMem) -> Option<(*const u8, u32)> {
        if self.state.len() <= id {
            self.state.resize(id + 1, Entry::Cold(0));
        }
        match self.state[id] {
            Entry::Code(p, _, n) => Some((p, n)),
            Entry::No => None,
            Entry::Cold(c) if c + 1 < HOT => {
                self.state[id] = Entry::Cold(c + 1);
                None
            }
            Entry::Cold(_) => {
                let plen = ops.iter().take_while(|o| !o.careful).count();
                let e = if plen == 0 {
                    Entry::No
                } else {
                    match compile::<B>(&ops[..plen], pc0, mem, &mut self.guards, self.profile) {
                        Some((code, body)) => match self.cache.write(&code) {
                            // SAFETY: body is a byte offset inside the code just written
                            Some(p) => Entry::Code(p, unsafe { p.add(body) }, plen as u32),
                            None => Entry::No,
                        },
                        None => Entry::No,
                    }
                };
                self.state[id] = e;
                match e {
                    Entry::Code(p, _, n) => Some((p, n)),
                    _ => None,
                }
            }
        }
    }

    /// The chain target of block `id`, if it is compiled.
    pub(crate) fn body(&self, id: usize) -> Option<*const u8> {
        match self.state.get(id) {
            Some(Entry::Code(_, b, _)) => Some(*b),
            _ => None,
        }
    }

    /// Forget every compiled block (the block table was rebuilt).
    pub fn reset(&mut self) {
        self.state.clear();
        self.cache.used = 0;
        self.guards.clear();
    }
}

/// Run compiled code: returns the instructions executed; cpu.pc is set.
///
/// # Safety
/// `code` must come from `Jit::entry` for this bus type and the memory `mem` describes.
pub(crate) unsafe fn run<B: Bus>(code: *const u8, cpu: &mut Cpu, bus: &mut B, mem: &JitMem, extra: u32, cap: u32) -> (u32, *mut u64) {
    // instructions this call may run: their ticks ((k - 1) + extra) must all fit tick_room
    let limit = (bus.tick_room() + 1).saturating_sub(extra as u64).min(1 << 30).min(cap as u64) as u32;
    let mut ctx = Ctx { cpu: cpu as *mut Cpu, bus: bus as *mut B as *mut (), extra, synced: 0, brk: 0, _pad: 0, slot: std::ptr::null_mut(), code_version: bus.code_writes().unwrap_or(NO_VERSION) };
    let f: CodeFn = unsafe { std::mem::transmute(code) };
    let k = f(cpu as *mut Cpu, &mut ctx, mem.ram, mem.xip, limit, mem.ram_code);
    if k > 0 {
        // the ticks of the instructions run, less those the helpers applied already
        let need = (k - 1) + extra;
        if need > ctx.synced {
            bus.add_ticks((need - ctx.synced) as u64);
        }
    }
    (k, ctx.slot)
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
    let v = match w {
        1 => bus.read8(addr) as u32,
        2 => bus.read16(addr) as u32,
        _ => bus.read32(addr),
    };
    if bus.next_tick_due() || bus.code_writes().unwrap_or(NO_VERSION) != ctx.code_version {
        ctx.brk = 1;
    }
    v
}
}

code_abi! {
fn h_write<B: Bus>(ctx: *mut Ctx, addr: u32, v: u32, idx: u32, w: u32) {
    // SAFETY: as h_read.
    let ctx = unsafe { &mut *ctx };
    let bus = unsafe { &mut *(ctx.bus as *mut B) };
    unsafe { sync(ctx, bus, idx) };
    match w {
        1 => bus.write8(addr, v as u8),
        2 => bus.write16(addr, v as u16),
        _ => bus.write32(addr, v),
    }
    if bus.next_tick_due() || bus.code_writes().unwrap_or(NO_VERSION) != ctx.code_version {
        ctx.brk = 1;
    }
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
    if PROF {
        cpu.profile_helper(unsafe { &*op });
    }
    let j = cpu.exec_fast(bus, unsafe { *op }, next_pc);
    if bus.next_tick_due() || bus.code_writes().unwrap_or(NO_VERSION) != ctx.code_version {
        ctx.brk = 1;
    }
    match j {
        Some(t) => (1u64 << 32) | t as u64,
        None => 0,
    }
}
}

// Each RAM body owns an immutable snapshot. Even an old incoming chain remains safe
// after its block has been rebuilt: it must validate the code it actually executes.
#[repr(C)]
struct RamGuard {
    /// the count of code-word writes the bytes were last found current at (UNCHECKED: not yet,
    /// or the bus keeps no count); the generated code compares it with `Ctx::code_version`
    /// itself and calls `h_ram_ok` only when they differ
    version: u64,
    pc: u32,
    bytes: Box<[u8]>,
}
const _: () = assert!(std::mem::offset_of!(RamGuard, version) == 0);
const UNCHECKED: u64 = u64::MAX - 1;

code_abi! {
fn h_ram_ok<B: Bus>(ctx: *mut Ctx, guard: *mut RamGuard) -> u32 {
    let ctx = unsafe { &mut *ctx };
    let bus = unsafe { &mut *(ctx.bus as *mut B) };
    let guard = unsafe { &mut *guard };
    let version = bus.code_writes();
    if version == Some(guard.version) { return 1; }
    let valid = crate::ram_code(bus, guard.pc)
        .is_some_and(|now| now.len() >= guard.bytes.len() && now[..guard.bytes.len()] == *guard.bytes);
    if valid { guard.version = version.unwrap_or(UNCHECKED); }
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
    h_op: u64,
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
    fn ld_reg(&mut self, w: Reg, r: u8) {
        self.a.ldr(w, CPU, off_reg(r));
    }
    fn st_reg(&mut self, w: Reg, r: u8) {
        self.a.str(w, CPU, off_reg(r));
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
    fn call(&mut self, f: u64) {
        self.a.mov64(9, f);
        self.a.blr(9);
    }
    /// Leave with pc = imm, `count` instructions of this block done.
    fn exit_imm(&mut self, pc: u32, count: u32) {
        self.a.mov32(9, pc);
        self.a.str(9, CPU, off_pc());
        self.a.add_imm(0, DONE, count);
        self.a.b(self.exit);
    }
    /// Leave with pc = w(reg).
    fn exit_reg(&mut self, reg: Reg, count: u32) {
        self.a.str(reg, CPU, off_pc());
        self.a.add_imm(0, DONE, count);
        self.a.b(self.exit);
    }
    /// Continue at a fixed pc: straight into its compiled body when the slot holds one,
    /// else back to Rust, which fills the slot once that block is compiled.
    fn chain(&mut self, pc: u32, count: u32) {
        let slot: &'static mut u64 = Box::leak(Box::new(0u64));
        let back = self.a.label();
        self.a.add_imm(DONE, DONE, count);
        self.a.mov64(10, slot as *mut u64 as u64);
        self.a.ldr_x(9, 10, 0);
        self.a.cbz(9, back);
        self.a.br(9);
        self.a.bind(back);
        self.a.mov32(9, pc);
        self.a.str(9, CPU, off_pc());
        self.a.str_x(10, CTX, OFF_SLOT);
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

    /// After an instruction that may have called the bus: leave if the next tick is due.
    fn check_brk_hold(&mut self, next_pc: u32, count: u32) {
        let go = self.a.label();
        self.a.ldr(9, CTX, OFF_BRK);
        self.a.cbz(9, go);
        self.a.mov32(9, 1);
        self.a.str(9, CPU, off_hold());
        self.exit_imm(next_pc, count);
        self.a.bind(go);
    }
    fn check_brk(&mut self, next_pc: u32, count: u32) {
        let go = self.a.label();
        self.a.ldr(9, CTX, OFF_BRK);
        self.a.cbz(9, go);
        self.exit_imm(next_pc, count);
        self.a.bind(go);
    }
}

/// Native code for a block prefix of plain micro-ops (none careful), and its body's offset.
#[cfg(target_arch = "aarch64")]
fn compile<B: Bus>(ops: &[crate::BOp], pc0: u32, mem: &JitMem, guards: &mut Vec<Box<RamGuard>>, profile: bool) -> Option<(Vec<u8>, usize)> {
    for o in [off_reg(15), off_sp(), off_rets(), off_pc(), off_hold()] {
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
        h_op: if profile { h_op::<B, true> as *const () as u64 } else { h_op::<B, false> as *const () as u64 },
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
    // the whole prefix must fit the call's limit, else back to Rust before running any of it
    {
        let fits = g.a.label();
        g.a.add_imm(9, DONE, ops.len() as u32);
        g.a.cmp(9, LIMIT);
        g.a.b_cond(AC::Ls, fits);
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
        // found current at the count of code writes this call began with, and nothing in the
        // call has changed it (brk clear): valid without leaving the generated code (h_ram_ok
        // was a call on every entry to a block in RAM: 7% of Melodee's time on a Pixel 7 Pro)
        let slow = g.a.label();
        g.a.ldr(9, CTX, OFF_BRK);
        g.a.cbnz(9, slow);
        g.a.ldr_x(9, CTX, OFF_VERSION);
        g.a.mov64(10, guard as *mut RamGuard as u64);
        g.a.ldr_x(10, 10, 0);
        g.a.cmp_x(9, 10);
        g.a.b_cond(AC::Eq, valid);
        g.a.bind(slow);
        g.a.mov_x(0, CTX);
        g.a.mov64(1, guard as *mut RamGuard as u64);
        g.call(h_ram_ok::<B> as *const () as u64);
        g.a.cbnz(0, valid);
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
                g.a.b_cond(if matches!(op.fast, Fast::BrIEq { .. }) { AC::Eq } else { AC::Ne }, t);
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
                g.rhs(rk, rv);
                g.ld_reg(0, a);
                let c = if fam == 2 {
                    g.a.tst(0, 1);
                    if alt { AC::Ne } else { AC::Eq }
                } else {
                    g.cmp(fam >= 8, 0, 1);
                    acond(crate::cond_of_fam(fam as u64, alt))
                };
                g.a.b_cond(c, taken);
                g.chain(next_pc.wrapping_add(then_d as u32), count);
                g.a.bind(taken);
                g.chain(next_pc, count);
            }
            // (the multiply-accumulates 7 / 8 read d too: through the interpreter's executor)
            Fast::FOp { d, a, b, op } if !matches!(op, 7 | 8) => {
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
                g.exit_reg(9, count);
            }
            Fast::GotoR { r } => {
                g.ld_reg(9, r);
                g.exit_reg(9, count);
            }
            Fast::CallR { r } => {
                g.ld_reg(9, r);
                g.a.mov32(0, next_pc);
                g.a.str(0, CPU, off_rets());
                g.exit_reg(9, count);
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
                g.exit_reg(KEEP, count);
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
                g.exit_reg(0, count);
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
                g.exit_reg(KEEP, count);
            }
            // ---- everything else: the interpreter's executor for this one op
            other => {
                let p: &'static Fast = Box::leak(Box::new(other));
                g.a.mov_x(0, CTX);
                g.a.mov64(1, p as *const Fast as u64);
                g.idx_w2(idx);
                g.a.mov32(3, next_pc);
                g.call(g.h_op);
                let fall = g.a.label();
                g.a.tbz(0, 32, fall); // bit 32: jumped (x0)
                g.exit_reg(0, count);
                g.a.bind(fall);
                bus_used = true;
                if other.may_jump() {
                    // a branch through the interpreter (the float compares) that fell through
                    g.chain(next_pc, count);
                }
            }
        }
        if bus_used && !op.fast.may_jump() {
            if op.glue {
                // leaving between a glued pair's two instructions: interrupts wait for the second
                g.check_brk_hold(next_pc, count);
            } else {
                g.check_brk(next_pc, count);
            }
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
    struct Memory { data: Vec<u8>, flags: Vec<u8>, version: u64 }
    impl Memory {
        fn new() -> Self { Self { data: vec![0; 256], flags: vec![1; 65], version: 0 } }
    }
    impl Bus for Memory {
        fn read8(&mut self, a: u32) -> u8 { self.data[a as usize] }
        // (the halfword at a & !1, as the Soc reads one)
        fn read16(&mut self, a: u32) -> u16 { let a = (a & !1) as usize; u16::from_le_bytes(self.data[a..a + 2].try_into().unwrap()) }
        fn read32(&mut self, a: u32) -> u32 { u32::from_le_bytes(self.data[a as usize..a as usize+4].try_into().unwrap()) }
        fn write8(&mut self, a: u32, v: u8) { self.data[a as usize] = v; self.version += 1; }
        fn write16(&mut self, a: u32, v: u16) { self.data[a as usize..a as usize+2].copy_from_slice(&v.to_le_bytes()); self.version += 1; }
        fn write32(&mut self, a: u32, v: u32) { self.data[a as usize..a as usize+4].copy_from_slice(&v.to_le_bytes()); self.version += 1; }
        fn tick_room(&self) -> u64 { 1000 }
        fn next_tick_due(&self) -> bool { false }
        fn add_ticks(&mut self, _: u64) {}
        fn code_writes(&self) -> Option<u64> { Some(self.version) }
        fn jit_mem(&mut self) -> Option<JitMem> {
            Some(JitMem { ram: self.data.as_mut_ptr(), ram_base: 0, ram_len: 256,
                ram_code: self.flags.as_ptr(), xip: std::ptr::null(), xip_base: 0x02000000, xip_len: 0 })
        }
    }
    fn op(fast: Fast) -> BOp { BOp { fast, len: 2, careful: false, pair_head: false, glue: false } }
    fn hot(j: &mut Jit, id: usize, pc: u32, ops: &[BOp], mem: &JitMem) -> *const u8 {
        for _ in 0..HOT { j.entry::<Memory>(id, ops, pc, mem); }
        j.entry::<Memory>(id, ops, pc, mem).unwrap().0
    }
    #[test]
    fn ram_chain_rejects_changed_target_and_respects_budget() {
        let mut bus = Memory::new(); let mem = bus.jit_mem().unwrap();
        let mut cpu = Cpu::new(0); let mut j = Jit::new(false).unwrap();
        let from = hot(&mut j, 0, 0, &[op(Fast::Goto { target: 32 })], &mem);
        hot(&mut j, 1, 32, &[op(Fast::MovI { d: 0, v: 42 })], &mem);
        let (_, slot) = unsafe { run(from, &mut cpu, &mut bus, &mem, 0, 10) };
        assert!(!slot.is_null());
        unsafe { *slot = j.body(1).unwrap() as u64; }
        assert_eq!(unsafe { run(from, &mut cpu, &mut bus, &mem, 0, 1) }.0, 1);
        assert_eq!(cpu.regs[0], 0);
        assert_eq!(unsafe { run(from, &mut cpu, &mut bus, &mem, 0, 10) }.0, 2);
        assert_eq!(cpu.regs[0], 42);
        bus.write8(32, 1); cpu.regs[0] = 0;
        assert_eq!(unsafe { run(from, &mut cpu, &mut bus, &mem, 0, 10) }.0, 1);
        assert_eq!(cpu.pc, 32); assert_eq!(cpu.regs[0], 0);
    }
    #[test]
    fn code_store_exits_before_following_instruction() {
        let mut bus = Memory::new(); let mem = bus.jit_mem().unwrap();
        let mut cpu = Cpu::new(0); let mut j = Jit::new(false).unwrap();
        cpu.regs[0] = 123; cpu.regs[1] = 2;
        let code = hot(&mut j, 0, 0, &[
            op(Fast::St { s: 0, b: Base::Reg(1), off: 0, w: 4 }),
            op(Fast::MovI { d: 2, v: 99 })], &mem);
        assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 10) }.0, 1);
        assert_eq!(cpu.pc, 2); assert_eq!(cpu.regs[2], 0);
        assert_eq!(bus.read32(2), 123); assert_eq!(bus.version, 1);
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
            assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 1) }.0, 1, "{fast:?}");
            assert_eq!((cpu.regs, cpu.sp, cpu.rets, cpu.pc), (expected.regs, expected.sp, expected.rets, expected.pc), "{fast:?}");
            assert_eq!(bus.data, reference.data, "{fast:?}");
        }
    }

    /// Every micro-op the compilers emit inline (and some through the executor), each alone in a block, from random registers
    /// (floats among them: NaN, both zeros, infinities, out of the integers' range): compiled
    /// code and the interpreter must leave the same registers, sp, rets, pc and memory.
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
        for fop in [0u8, 1, 2, 3, 5, 6, 7, 8, 0x1f, 0x5f, 0x8f, 0x9f] { // (the ops fm1-isa decodes)
            cases.push(Fast::FOp { d: 0, a: 1, b: 2, op: fop });
            cases.push(Fast::FOp { d: 0, a: 1, b: 1, op: fop });
        }
        for kind in 0..4 { cases.push(Fast::MinMax { d: 0, a: 1, b: 2, kind }); }
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
        }
            cases.push(Fast::St { s: 1, b: Base::Reg(b), off: 2, w });
            cases.push(Fast::St { s: 1, b: Base::Sp, off: 4, w });
            cases.push(Fast::StIdx { s: 1, b, i: 11, sh: 2, w });
            cases.push(Fast::StPost { s: 1, b, w, inc: 0xffff_fffe });
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
            Fast::GotoR { r: 1 }, Fast::CallR { r: 1 }, Fast::GotoR { r: 15 }, Fast::CallR { r: 15 },
            Fast::LdW { d: 0, b: 8, off: 4 }, Fast::LdWSp { d: 0, off: 8 }, Fast::LdBu { d: 0, b: 10, off: 3 },
            Fast::StW { s: 1, b: 8, off: 4 }, Fast::StWSp { s: 1, off: 8 },
            Fast::PopRets, Fast::PushRet, Fast::PopRet]);
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
                if let Fast::DecBr { ref mut v, .. } = fast { *v = cpu.regs[3].wrapping_sub(1); }
                let mut expected = Cpu::new(0);
                expected.regs = cpu.regs; expected.sp = cpu.sp; expected.rets = cpu.rets;
                expected.pc = expected.exec_fast(&mut reference, fast, 2).unwrap_or(2);
                let code = hot(&mut jit, j * 24 + round, 0, &[op(fast)], &mem);
                assert_eq!(unsafe { run(code, &mut cpu, &mut bus, &mem, 0, 1) }.0, 1, "{fast:?}");
                let regs = |c: &Cpu| c.regs.map(|r| format!("{r:08x}"));
                assert_eq!((regs(&cpu), cpu.sp, cpu.rets, cpu.pc), (regs(&expected), expected.sp, expected.rets, expected.pc),
                    "case {j} round {round}: {fast:?}");
                assert_eq!(bus.data, reference.data, "case {j} round {round}: {fast:?}");
            }
        }
    }

}
