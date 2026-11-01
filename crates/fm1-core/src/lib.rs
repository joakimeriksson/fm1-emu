//! FM-1 CPU core: an interpreting fetch/decode/execute loop.
//!
//! The core never touches peripherals directly — every memory access goes
//! through [`Bus`], so the SoC layer owns the memory map (PLAN.md Phase 3).
//!
//! Semantics model (verified against the V13/V14/uboot/ota corpora, see
//! docs/isa-notes.md):
//! - push `[--sp] = {rets, rN..rB}`: `--sp` then store, iterating the list
//!   in printed order (first item ends up at the highest address);
//! - pop `{pc, rN..rB} = [sp++]`: read ascending, assign the printed list
//!   in REVERSE (last-listed reg gets the lowest slot, pc the highest);
//!   pc (the rets slot, highest) is read last and jumps;
//! - branch/call offsets are byte offsets from pc_next (Rel slots multiplied
//!   by the solver-found factor).

use fm1_isa::{DecodeError, Instruction, SlotKind};

/// The memory bus. Implemented by the SoC (or by plain RAM in unit tests).
///
/// Width-specific accessors keep MMIO side effects honest: a byte read must
/// not silently widen into a word read of a peripheral register.
#[cfg(native_jit)]
mod jit;
#[cfg(all(native_jit, target_arch = "aarch64"))]
mod jit_a64;
#[cfg(all(native_jit, target_arch = "x86_64"))]
mod jit_x64;
#[cfg(native_jit)]
pub use jit::JitMem;
#[cfg(target_arch = "wasm32")]
pub mod jit_wasm;
/// (no JIT on this host: the type exists so buses compile everywhere)
#[cfg(not(native_jit))]
#[derive(Clone, Copy, Debug)]
pub struct JitMem {
    pub ram: *mut u8,
    pub ram_base: u32,
    pub ram_len: u32,
    /// RAM's code-word flags, one byte per 4 bytes (`Bus::code_writes`): the wasm JIT's inline
    /// stores go to the bus for a flagged word.
    pub ram_code: *const u8,
    pub xip: *const u8,
    pub xip_base: u32,
    pub xip_len: u32,
}

pub trait Bus {
    fn read8(&mut self, addr: u32) -> u8;
    fn read16(&mut self, addr: u32) -> u16;
    fn read32(&mut self, addr: u32) -> u32;
    /// An instruction fetch (decoding, not a load): a bus that models faulty loads
    /// (fm1_soc's flaky core 1) answers it with the true contents.
    #[inline(always)]
    fn fetch32(&mut self, addr: u32) -> u32 {
        self.read32(addr)
    }
    fn write8(&mut self, addr: u32, value: u8);
    fn write16(&mut self, addr: u32, value: u16);
    fn write32(&mut self, addr: u32, value: u32);
    /// One clock cycle passed (one instruction). True when something timed
    /// is due, or a peripheral was touched, so the caller must service the
    /// SoC before the next instruction. Buses without a clock never are.
    #[inline(always)]
    fn tick(&mut self) -> bool {
        false
    }
    /// Is any interrupt source pending (before enables and priorities)?
    #[inline(always)]
    fn irq_pending(&self) -> bool {
        false
    }
    /// Host pointers for the JIT's inline memory paths (None: no JIT on this bus).
    fn jit_mem(&mut self) -> Option<JitMem> {
        None
    }
    /// Ticks that can pass before one of them is due.
    fn tick_room(&self) -> u64 {
        0
    }
    /// Let ticks pass that `tick_room` allowed.
    fn add_ticks(&mut self, _n: u64) {}
    /// The cores' shared hardware lock: `lockset` (`set`) takes it for `core` or reports
    /// that the other core holds it (the instruction then retries); `lockclr` releases.
    /// A single-core bus always grants it.
    fn hw_lock(&mut self, _core: u32, _set: bool) -> bool {
        true
    }
    /// Would the next tick be due?
    fn next_tick_due(&self) -> bool {
        true
    }
    /// `idle` on `core`: it waits for an interrupt (a bus with a clock may skip ahead).
    fn idle(&mut self, _core: u32) {}
    /// Interrupts may have just been unmasked (`sti`, `icfg = R`): with one pending, the run
    /// loop must look before the next instruction, as it does after a slow-path instruction.
    fn irq_window(&mut self) {}
    /// The run loop must look before the next instruction, as it does after an instruction off
    /// the micro-op paths: `rti` (the SoC keeps "in an ISR" for its idle skipping, updated there).
    fn service(&mut self) {}
    /// Writes so far to RAM words marked as code (`code_mark`), when the bus counts them: while
    /// the count stays the same, no block built from RAM can have changed. (None: compare.)
    fn code_writes(&self) -> Option<u64> {
        None
    }
    /// RAM bytes lo..hi hold code a block was built from: count writes to them from now on.
    fn code_mark(&mut self, _lo: u32, _hi: u32) {}
}

/// The RAM from `pc` to its end, when the bus can show it (its JIT memory map), and pc is in it.
#[inline]
fn ram_code<B: Bus>(bus: &mut B, pc: u32) -> Option<&'static [u8]> {
    let m = bus.jit_mem()?;
    let off = pc.checked_sub(m.ram_base)?;
    if off >= m.ram_len {
        return None;
    }
    // SAFETY: the bus's RAM outlives the machine; only read here, between instructions
    Some(unsafe { std::slice::from_raw_parts(m.ram.add(off as usize), (m.ram_len - off) as usize) })
}

/// How many instructions of a block (its ops, the first at pc; a swapped pair is two) come before
/// `stop`, if an instruction of it after the first begins there (the second of a swapped pair
/// too: then its head runs alone), and whether that is all of them.
fn len_to(ops: &[BOp], swaps: &[(Fast, Fast, u8)], pc: u32, stop: u32) -> (u32, bool) {
    let mut at = pc;
    let mut k = 0;
    for op in ops {
        if k > 0 && at == stop {
            return (k, false);
        }
        k += 1;
        if let Fast::Swap { idx } = op.fast {
            if at.wrapping_add(swaps[idx as usize].2 as u32) == stop {
                return (k, false);
            }
            k += 1;
        }
        at = at.wrapping_add(op.len as u32);
    }
    (k, true)
}

/// A stretch (`Cpu::stop_pc`) returns once its budget less this is spent: room for the call
/// begun before then to end where it would (a block holds at most 64 instructions: 32 ops, each
/// at most a swapped pair).
pub const STRETCH_ROOM: u32 = 64;

/// a == b (equal lengths). RAM blocks are checked against memory on every entry: in wasm,
/// 8 bytes at a time, since its `memcmp` (compiler_builtins) goes a byte at a time (a fifth of
/// Melodee's time in the browser: its audio code runs from RAM); natively the platform's
/// `memcmp` is faster than this loop (Melodee 16% slower with it).
#[inline]
fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let (a8, b8) = (a.chunks_exact(8), b.chunks_exact(8));
        let (ra, rb) = (a8.remainder(), b8.remainder());
        let w = |c: &[u8]| u64::from_le_bytes(c.try_into().unwrap());
        a8.zip(b8).all(|(x, y)| w(x) == w(y)) && ra.iter().zip(rb).all(|(x, y)| x == y)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        a == b
    }
}

/// CPU architectural state.
#[derive(Debug, Default)]
pub struct Cpu {
    /// Core number reported by `cnum` (the AC791N is dual-core; cpu1 boots
    /// from the vector at 0x02000098 once cpu0 releases it).
    pub core_id: u32,
    pub pc: u32,
    pub regs: [u32; 16],
    pub sp: u32,
    pub ssp: u32,
    pub usp: u32,
    /// User mode. `sp` is the stack in use; `usp` and `ssp` are registers of their own. An
    /// interrupt taken in user mode saves sp in usp and continues on ssp (the stock RTOS's
    /// context switch saves the task's frame through `sp = usp`, picks the next task with
    /// `usp = sp` and gets back to the ISR stack with `sp = ssp`); the `rti` that returns
    /// to user mode keeps sp in ssp and goes on with usp. An interrupt in supervisor mode
    /// stays on its stack. The SPL hands cpu0 over in user mode (crt0s set `sp`, then a
    /// different `ssp`); cpu1 starts in supervisor mode (the stock stub leaves it with
    /// `usp = ..; sp = ..; reti = ..; rti`, where an rti with no interrupt in progress
    /// enters user mode).
    pub user: bool,
    /// Per interrupt depth: was the interrupted code in user mode (bit = depth).
    user_irq: u32,
    pub rets: u32,
    /// Total instructions retired (for traces and tests).
    pub insn_count: u64,
    /// Hardware repeat block `rep N rR { … }` / `rep N #k { … }`: the block
    /// is the N bytes after the rep instruction; when execution falls off
    /// its end the counter is decremented and, if non-zero, pc returns to
    /// the block start. `rep_end == 0` means no rep is active.
    pub rep_start: u32,
    pub rep_end: u32,
    /// Counter register (`rep N rR`, decremented in place) or `None` for
    /// the immediate-count form, which counts in `rep_count`.
    pub rep_reg: Option<usize>,
    pub rep_count: u32,
    /// Predicated `if {} else {}` blocks whose then-part is executing:
    /// (then_end, else_end, then_start) — reaching then_end jumps over the else-part; a jump
    /// from inside the then-part to before its start leaves the construct (`pred_left_back`).
    pub pred_skips: Vec<(u32, u32, u32)>,
    /// Condition flag set by `testset` (see its arm for the assumed sense).
    pub cc: bool,
    /// Arithmetic flags in `psr`'s low bits (F_N | F_C | F_Z | F_V), set by the add / subtract
    /// forms and read by `+ c`, `- !c` and `psr`: clang's 64-bit add / subtract / compare
    /// sequences (and so compiler-rt's soft double) need them. Which forms set them on the real
    /// core is a hardware-probe item; every add / subtract here does, which is what compiled
    /// code (a producer right before its consumer) needs.
    pub flags: u32,
    /// Plain-storage special registers: icfg, reti, retx, rete, sspn, psr.
    pub sreg_store: [u32; 7],
    /// Dual-issue pair in flight: an instruction printed with a trailing `#`
    /// executes together with the next one, which still sees the old
    /// register values (`r5 = r3 #; b[r4+6] = r5` stores the old r5). The
    /// first instruction's register results are held here and committed
    /// after the second has run, unless the second wrote the same register.
    pub pair_pending: Option<([u32; 16], [u32; 16])>,
    /// Name of the last class with no semantics (halt reason).
    pub stuck_on: Option<&'static str>,
    /// `R = #h` rows not found in the observed-encoding table.
    pub unknown_const_count: u64,
    /// Decoded-instruction cache (see `fetch`): compact lines for the hot
    /// path, the full decode beside them.
    pub icache: Vec<Line>,
    pub icache_x: Vec<Option<LineX>>,
    /// Compile hot blocks to native code (AArch64 macOS / Linux / Android, x86-64 Linux / Windows; see jit.rs).
    pub jit_on: bool,
    #[cfg(native_jit)]
    jit: Option<Box<jit::Jit>>,
    /// (the wasm32 build: hot blocks as WebAssembly modules, see jit_wasm.rs)
    #[cfg(target_arch = "wasm32")]
    wjit: Option<Box<jit_wasm::Jit>>,
    /// Straight-line blocks of micro-ops over XIP code (see `block_at`):
    /// the ops of every block back to back, the blocks, and a direct-mapped
    /// pc -> block+1 table (0 = none).
    bops: Vec<BOp>,
    /// `Fast::Swap`'s pairs: (head, second instruction, the head's length)
    swaps: Vec<(Fast, Fast, u8)>,
    blocks: Vec<(u32, u32, u32)>,
    bmap: Vec<u32>,
    /// Per block: for one over RAM code, the bytes it was built from. RAM can change (code
    /// copied in, overlays), so such a block is checked against memory before every entry
    /// and rebuilt when they differ. AArch64 chain targets validate their own snapshots;
    /// WASM regions validate their RAM blocks on entry (`wasm_region_ok`).
    /// The stock firmware's synth renders on cpu1 from RAM: without blocks it ran a step at
    /// a time.
    bbytes: Vec<Option<Box<[u8]>>>,
    /// Per block over RAM: the bus's count of code-word writes when the block was last found
    /// current (`ram_block_ok`: the same count, no compare).
    bver: Vec<u64>,
    /// FM1_NO_RAM_BLOCKS: RAM code a step at a time (the reference)
    pub no_ram_blocks: bool,
    /// Instructions per execute route (index 31 = the micro-op fast path),
    /// and per class name on the slow path — for tuning the dispatcher.
    pub route_hist: [u64; 32],
    pub slow_hist: std::collections::HashMap<&'static str, u64>,
    pub profile: bool,
    pub jit_entries: u64,
    pub jit_insns: u64,
    pub jit_helpers: std::collections::BTreeMap<String, u64>,
    /// Instruction-cache misses (decodes) since reset.
    pub decodes: u64,
    /// Canonical syntax of the instruction executing (from its cache line).
    pub cur_syntax: &'static str,
    /// Global interrupt enable (`sti` / `cli`); `icfg` bit 8 is the second
    /// gate (the firmware's "master enable").
    pub ie: bool,
    /// Priorities of the ISRs in progress, innermost last: a source only
    /// interrupts a lower priority (Felucca nests TIMER5 at 4 into ALNK0
    /// at 3). `rti` pops.
    pub irq_levels: Vec<u8>,
    /// Nesting is open only while the running ISR has `reti` saved on the
    /// stack (Blackfin rule: pushing RETI enables nested interrupts, popping
    /// it disables them). Without this, a tick landing between an ISR's
    /// `{.., reti} = [sp++]` and its `rti` overwrote reti and the `rti`
    /// returned to itself forever.
    pub nest_ok: bool,
    /// A glued pair (see `BOp::glue`) was cut between its two instructions (a tick came due, a
    /// compiled block left after a peripheral access): interrupts wait for the second one, as
    /// they would for a pair in flight. Cleared when that instruction retires (`finish`).
    /// (u32: compiled code stores it.)
    pub hold_irq: u32,
    /// A runner's stop (fm1-soc's `run_cpu1` probing core 1's polling loop), 0 for none. While
    /// set, a `run_fast` call is a stretch: the calls the runner would make one at a time, each
    /// the block at pc up to this pc if it lies inside (`block_len_to`), run back to back, and
    /// it returns only where one of them ends and the runner could decide otherwise: back here,
    /// once `stop_io` is set or code ran interpreted (either may have touched a peripheral, and
    /// so made an interrupt pending), or with the budget less `STRETCH_ROOM` spent (room for
    /// the call begun before then to end where it would).
    pub stop_pc: u32,
    /// Set (while `stop_pc` is) by compiled code's helpers: a bus access off its inline paths,
    /// or an op through the interpreter's executor. (u32: compiled code reads it.)
    pub stop_io: u32,
    /// Set by compiled code that leaves inside a block (after a write to code): how many of its
    /// instructions are left, so a stretch knows where that block's call ends. (u32: compiled
    /// code stores it.)
    pub stop_rest: u32,
    /// Compile the checks for `stop_pc` / `stop_io` into this core's blocks (set before its
    /// JIT is made; core 1's).
    pub stop_checks: bool,
}

/// What instructions can see of a core between instructions, outside any pair, predicated
/// block, repeat loop or interrupt (`Cpu::plain_state`): two equal ones at the same pc, with
/// memory and peripherals unchanged, run on identically. (fm1_soc's polling-loop skip compares
/// them, and puts one back after a probe that found no loop.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PlainState {
    pc: u32,
    regs: [u32; 16],
    sp: u32,
    ssp: u32,
    usp: u32,
    user: bool,
    user_irq: u32,
    rets: u32,
    rep_start: u32,
    rep_reg: Option<usize>,
    rep_count: u32,
    cc: bool,
    flags: u32,
    sreg_store: [u32; 7],
    ie: bool,
    nest_ok: bool,
    unknown_const_count: u64,
    stuck_on: Option<&'static str>,
}

impl PlainState {
    pub fn pc(&self) -> u32 {
        self.pc
    }
}

/// The decoded-instruction cache (`Cpu::fetch_at`) and the block map (`Cpu::block_at`) are
/// direct-mapped on the halfword index: 256K lines cover 512 KiB of XIP code without aliasing
/// (Felucca's image is 441 KiB; 64K lines re-decoded ~1M times per 6 s). Code below the XIP window
/// (RAM: ISRs, flash drivers, a firmware's hot loop) has 64K lines of its own after them, so a
/// RAM line and an XIP line never share a slot: with one map for both, Ravel's spin loop in RAM
/// and the XIP code at the same slot evicted each other 54K times in 8 s, and in the wasm JIT
/// each line decoded anew was a block made anew and a region compiled anew, without end.
const ICACHE_BITS: u32 = 18;
const ICACHE_RAM_BITS: u32 = 16;
const ICACHE_LINES: usize = (1 << ICACHE_BITS) + (1 << ICACHE_RAM_BITS);

#[inline(always)]
fn icache_slot(pc: u32) -> usize {
    if pc < 0x0200_0000 {
        (1 << ICACHE_BITS) + ((pc >> 1) & ((1 << ICACHE_RAM_BITS) - 1)) as usize
    } else {
        ((pc >> 1) & ((1 << ICACHE_BITS) - 1)) as usize
    }
}

impl Cpu {
    /// The core's state when it is plain: no pair in flight, no predicated block, no repeat
    /// loop, no interrupt in progress, no glued pair cut (None otherwise).
    pub fn plain_state(&self) -> Option<PlainState> {
        if self.pair_pending.is_some() || !self.pred_skips.is_empty() || self.rep_end != 0 || self.hold_irq != 0 || !self.irq_levels.is_empty() {
            return None;
        }
        Some(PlainState {
            pc: self.pc,
            regs: self.regs,
            sp: self.sp,
            ssp: self.ssp,
            usp: self.usp,
            user: self.user,
            user_irq: self.user_irq,
            rets: self.rets,
            rep_start: self.rep_start,
            rep_reg: self.rep_reg,
            rep_count: self.rep_count,
            cc: self.cc,
            flags: self.flags,
            sreg_store: self.sreg_store,
            ie: self.ie,
            nest_ok: self.nest_ok,
            unknown_const_count: self.unknown_const_count,
            stuck_on: self.stuck_on,
        })
    }

    /// Back to a plain state taken earlier (`plain_state`): whatever ran since is undone in the
    /// core (not the retired count, nor memory).
    pub fn set_plain_state(&mut self, s: &PlainState) {
        self.pc = s.pc;
        self.regs = s.regs;
        self.sp = s.sp;
        self.ssp = s.ssp;
        self.usp = s.usp;
        self.user = s.user;
        self.user_irq = s.user_irq;
        self.rets = s.rets;
        self.rep_start = s.rep_start;
        self.rep_end = 0;
        self.rep_reg = s.rep_reg;
        self.rep_count = s.rep_count;
        self.pred_skips.clear();
        self.cc = s.cc;
        self.flags = s.flags;
        self.sreg_store = s.sreg_store;
        self.pair_pending = None;
        self.stuck_on = s.stuck_on;
        self.unknown_const_count = s.unknown_const_count;
        self.ie = s.ie;
        self.irq_levels.clear();
        self.nest_ok = s.nest_ok;
        self.hold_irq = 0;
    }

    pub fn jit_profile_json(&self) -> String {
        let helpers = self.jit_helpers.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect::<Vec<_>>().join(",");
        format!("{{\"entries\":{},\"instructions\":{},\"helpers\":{{{helpers}}}}}", self.jit_entries, self.jit_insns)
    }

    /// Count a JIT helper call when profiling. Called for every helper op, so the check is
    /// inlined and the counting kept out of line: a plain call (with the op copied) cost the
    /// wasm build 10-12% on busy firmwares (AMB, TRIBE) with profiling off.
    #[inline(always)]
    fn profile_helper(&mut self, op: &Fast) {
        if self.profile {
            self.profile_helper_count(op);
        }
    }

    #[cold]
    #[inline(never)]
    fn profile_helper_count(&mut self, op: &Fast) {
        let name = match op {
            Fast::Raw { route, .. } => format!("Raw{route}"),
            _ => format!("{op:?}").split([' ', '{']).next().unwrap().to_owned(),
        };
        *self.jit_helpers.entry(name).or_default() += 1;
    }

    pub fn new(entry: u32) -> Self {
        Self {
            pc: entry,
            no_ram_blocks: std::env::var_os("FM1_NO_RAM_BLOCKS").is_some(),
            ..Default::default()
        }
    }

    /// Would an interrupt of priority `prio` be taken now? Only between
    /// whole constructs: not inside a rep block, a predicated `if {}` or a
    /// dual-issue pair, whose state lives in the core rather than on the
    /// stack.
    pub fn irq_ready(&self, prio: u8) -> bool {
        self.ie
            && self.sreg_store[0] & 0x100 != 0
            && self.rep_end == 0
            && self.pred_skips.is_empty()
            && self.pair_pending.is_none()
            && self.hold_irq == 0
            && self.irq_levels.last().map_or(true, |&l| prio > l && self.nest_ok)
    }

    /// Enter an interrupt handler: `reti` = the interrupted pc (the ISR
    /// wrapper saves `{psr, rets, reti}` itself and returns with `rti`).
    pub fn interrupt(&mut self, handler: u32, prio: u8) {
        self.sreg_store[1] = self.pc;
        // From user mode the ISR runs on ssp (the user sp waits in usp: the stock RTOS's
        // context switch saves the task's frame through it and picks the next task's);
        // from supervisor mode it stays on the same stack.
        let depth = self.irq_levels.len().min(31);
        if self.user {
            self.user_irq |= 1 << depth;
            self.usp = self.sp;
            self.sp = self.ssp;
            self.user = false;
        } else {
            self.user_irq &= !(1 << depth);
        }
        self.irq_levels.push(prio);
        self.nest_ok = false;
        self.pc = handler;
    }

    /// The decoded instruction at pc, through a direct-mapped cache. Code in
    /// the XIP window cannot change, so a hit there skips even the fetch;
    /// elsewhere (RAM code) the fetched window must match the cached one.
    #[inline(never)]
    fn fetch<B: Bus>(&mut self, bus: &mut B) -> Result<usize, CoreError> {
        self.fetch_at(bus, self.pc)
    }

    fn fetch_at<B: Bus>(&mut self, bus: &mut B, pc: u32) -> Result<usize, CoreError> {
        if self.icache.is_empty() {
            self.icache = vec![Line::EMPTY; ICACHE_LINES];
            self.icache_x = vec![None; ICACHE_LINES];
        }
        let slot = icache_slot(pc);
        let hit = self.icache[slot].pc == pc;
        if hit && (0x0200_0000..0x0400_0000).contains(&pc) {
            return Ok(slot);
        }
        let lo = bus.fetch32(pc) as u64;
        let hi = bus.fetch32(pc + 4) as u64;
        let win = (hi << 32) | lo;
        // (RAM code: the line stands while the instruction's own bytes do; the rest of the window
        // may be data a short instruction sits next to, which changing must not cost a decode)
        if hit {
            let len = self.icache[slot].len as u32;
            let mask = if len >= 8 { u64::MAX } else { (1u64 << (8 * len)) - 1 };
            if self.icache_x[slot].map_or(false, |x| (x.win ^ win) & mask == 0) {
                return Ok(slot);
            }
        }
        self.decodes += 1;
        let insn = decode_any(win, pc).map_err(CoreError::Decode)?;
        let alu4 = if insn.entry.len == 4 { decode_alu4(insn.raw) } else { None };
        let bittest = if insn.entry.name.starts_with("if") { decode_bittest(insn.raw, insn.entry.len) } else { None };
        let route = route_of(&insn, &alu4, &bittest);
        let syntax = canon_syntax(insn.entry);
        let mut fast = if route == ROUTE_SYNTAX { fast_of(&insn, syntax) } else { fast_raw(route, &insn, &alu4, &bittest) };
        if route == ROUTE_SYNTAX && syntax == "R = cnum" {
            // the core number is fixed per core, and so is this core's cache
            if let Some(sl) = insn.entry.slots.first() {
                fast = Some(Fast::MovI { d: (sl.value(insn.raw) as u8) & 15, v: self.core_id });
            }
        }
        // (in RAM too: the block ends come from the following instructions, and RAM blocks are
        // checked against memory before each entry; code that rewrote only an if-block's body
        // in place, keeping its head, would go unnoticed by `step`'s line cache)
        if route == 14 {
            if let Some(blk) = decode_if_block(insn.raw) {
                let next_pc = pc + insn.entry.len as u32;
                let then_end = self.block_end(bus, next_pc, blk.then_units);
                let else_end = self.block_end(bus, then_end, blk.else_units);
                let (rk, rv) = rhs_kind(blk.rhs);
                let (td, ed) = (then_end.wrapping_sub(next_pc), else_end.wrapping_sub(then_end));
                if td <= 0xffff && ed <= 0xffff {
                    fast = Some(Fast::IfBlk {
                        a: blk.reg as u8, rk, fam: blk.fam as u8, alt: blk.alt,
                        has_else: blk.else_units > 0, rv, then_d: td as u16, else_d: ed as u16,
                    });
                }
            }
        }
        let pair_head = insn.entry.syntax.trim_end().ends_with('#');
        let pre = Pre { route, fast, alu4, bittest, pair_head, syntax };
        let fast = fast.map(Fast::specialise);
        // (a predicated block without an else part is a conditional jump: no careful path)
        let flags_live = fast.is_some() && is_flag_producer(syntax) && flags_read_later(bus, pc + insn.entry.len as u32);
        let careful = pair_head || matches!(fast, Some(Fast::IfBlk { has_else: true, .. })) || flags_live;
        self.icache[slot] = Line { pc, len: insn.entry.len, pair_head, careful, flags_live, fast };
        self.icache_x[slot] = Some(LineX { win, entry: insn.entry, pre });
        Ok(slot)
    }

    /// Run micro-ops back to back: the first instruction has been paid for
    /// by the caller's clock tick, every following one ticks the bus here.
    /// Stops before an instruction that needs `step` (slow path, dual-issue
    /// pair, predicated block, rep loop), after `budget` instructions, or
    /// when the bus reports something due — then the tick for the next
    /// instruction has already happened and `due` is true. Interrupt
    /// readiness can only change at those points, so the caller's per-
    /// instruction interrupt check is exact at block granularity.
    ///
    /// Returns (instructions executed, due). 0 = the instruction at pc
    /// needs `step`.
    #[inline]
    pub fn run_fast<B: Bus>(&mut self, bus: &mut B, budget: u32) -> Result<(u32, bool), CoreError> {
        if self.stop_pc != 0 {
            return self.run_stretch(bus, budget);
        }
        self.run_fast_in::<B, false>(bus, budget)
    }

    /// `run_fast` as a runner's stretch (`stop_pc`): the same loop with the stretch's per-block
    /// bookkeeping, out of line, so that the loop every other call runs has none of it (with it
    /// there, interpreted Melodee ran 15% slower).
    #[inline(never)]
    fn run_stretch<B: Bus>(&mut self, bus: &mut B, budget: u32) -> Result<(u32, bool), CoreError> {
        self.run_fast_in::<B, true>(bus, budget)
    }

    /// `run_fast`'s loop; `STRETCH`: `stop_pc` is set (`run_stretch`).
    #[inline(always)]
    fn run_fast_in<B: Bus, const STRETCH: bool>(&mut self, bus: &mut B, budget: u32) -> Result<(u32, bool), CoreError> {
        if trace_on() {
            return Ok((0, false));
        }
        let mut n = 0u32;
        // pair / predicated block / rep loop in progress: every instruction
        // then takes the careful path (it can end one and free interrupts)
        let mut careful = self.pair_pending.is_some() || !self.pred_skips.is_empty() || self.rep_end != 0 || self.hold_irq != 0;
        // pc and the retired count live in locals on the plain path; they
        // are written back before anything that reads them (`synced`: the
        // part of n already added to insn_count)
        let mut pc = self.pc;
        let mut synced = 0u32;
        let jit_mem = if self.jit_on { bus.jit_mem() } else { None };
        macro_rules! sync {
            () => {{
                self.pc = pc;
                self.insn_count += (n - synced) as u64;
                synced = n;
            }};
        }
        // a runner's stretch (see `stop_pc`): `bound` is where the runner's call in progress
        // ends (n), `interpreted` says code ran other than compiled since the stretch began
        let stretch = STRETCH;
        let mut bound = 0u32;
        let mut interpreted = false;
        loop {
            // (and per op below: the budget holds to the instruction)
            if n >= budget {
                sync!();
                return Ok((n, false));
            }
            if stretch && n == bound && n > 0 && (pc == self.stop_pc || self.stop_io != 0 || interpreted || n >= budget.saturating_sub(STRETCH_ROOM)) {
                sync!();
                return Ok((n, false));
            }
            let blk = match self.block_at(bus, pc) {
                Ok(b) => b,
                Err(e) => {
                    sync!();
                    return Err(e);
                }
            };
            let Some((id, start, len)) = blk else {
                sync!();
                return Ok((n, false));
            };
            // a stretch: the next call begins here; it may run on in compiled code (which leaves
            // only where calls end: see jit.rs's `compile`) if it is this block whole, else only
            // to its end
            let mut whole = false;
            if stretch && n == bound {
                let (to, all) = len_to(&self.bops[start as usize..(start + len) as usize], &self.swaps, pc, self.stop_pc);
                bound = n + to;
                whole = all;
            }
            let limit = if stretch { bound } else { budget };
            // (careful: inside a then-part or a rep loop with nothing else in progress, where a
            // compiled prefix that cannot reach the construct's end still runs: `prefix_inside`)
            #[cfg(native_jit)]
            if self.jit_on && (!careful || self.only_construct()) {
                let inside = careful;
                if let Some(mem) = jit_mem {
                    if self.jit.is_none() {
                        self.jit = jit::Jit::new(self.profile).map(|mut j| {
                            j.stops = self.stop_checks;
                            Box::new(j)
                        });
                    }
                    // (a block about to be compiled that ends in an if-else block: its then-part)
                    let mut then = None;
                    if self.jit.as_ref().is_some_and(|j| j.compiles_next(id)) {
                        then = self.then_of(bus, id, pc);
                        if self.blocks.get(id) != Some(&(pc, start, len)) {
                            continue; // (looking for it rebuilt the block table: look again)
                        }
                    }
                    let code = match self.jit.as_mut() {
                        Some(j) => {
                            let table = bus.code_writes().is_some() && !mem.ram_code.is_null();
                            j.entry::<B>(id, &self.bops[start as usize..(start + len) as usize], &self.swaps, then.as_deref(), pc, &mem, table, inside)
                        }
                        None => None,
                    };
                    let code = code.filter(|p| !inside || self.prefix_inside(pc, p.end));
                    if let Some(jit::Prefix { code, insns, end, whole: compiled_whole, .. }) = code {
                        // (the code checks itself that its ticks fall before anything is due)
                        let extra = if n == 0 { 0 } else { 1 };
                        sync!();
                        // a stretch's call of a block compiled whole (no then-part in tow) runs on
                        // to wherever the code leaves (`stop_rest`: how far inside a block it did)
                        let open = whole && !inside && compiled_whole && self.jit.as_ref().is_some_and(|j| j.stops);
                        if open {
                            self.stop_rest = 0;
                        }
                        // (inside a construct: this prefix and no further, so no chaining on)
                        let cap = if inside { (limit - n).min(insns) } else if open { budget - n } else { limit - n };
                        let entry = pc;
                        // SAFETY: code was compiled for this bus type and memory map
                        let (k, slot, stop) = unsafe { jit::run(code, self, bus, &mem, extra, cap, inside) };
                        if self.profile { self.jit_entries += 1; self.jit_insns += k as u64; }
                        if k > 0 {
                            n += k;
                            if open {
                                bound = n + self.stop_rest;
                            }
                            pc = self.pc;
                            if stop {
                                // a swapped pair ended with an interrupt pending (`exec_careful_op`)
                                self.pc = pc;
                                self.insn_count += (n - synced) as u64;
                                return Ok((n, false));
                            }
                            if let Some(body) = self.jit_body_at(pc, bus.code_writes().is_some() && !mem.ram_code.is_null()) {
                                if !slot.is_null() {
                                    // a chainable exit came back: link it once its target is compiled
                                    // SAFETY: the slot is a chain exit of this JIT's code, body a compiled block
                                    unsafe { jit::link(slot, body) };
                                } else if let Some(j) = self.jit.as_mut() {
                                    // a computed jump (or a stop) landed on a compiled body that
                                    // is not in the table: the next one goes straight in
                                    j.relist(pc, body);
                                }
                            }
                            if inside && !self.pred_skips.is_empty() {
                                // (the then-part's rule for where its last instruction went; after
                                // the link: the slot goes to where the code left, not past a skip)
                                pc = self.finish_inside(entry, end);
                                if self.pred_skips.is_empty() && bus.irq_pending() {
                                    self.insn_count += (n - synced) as u64;
                                    return Ok((n, false));
                                }
                            }
                            // (it left between the two instructions of a glued pair; or inside a
                            // then-part or rep loop)
                            careful = self.hold_irq != 0 || self.rep_end != 0 || !self.pred_skips.is_empty();
                            continue;
                        }
                    }
                }
            }
            #[cfg(target_arch = "wasm32")]
            if self.jit_on && !careful {
                if let Some(mem) = jit_mem {
                    if self.wjit.is_none() {
                        self.wjit = jit_wasm::Jit::new(self.profile).map(Box::new);
                    }
                    let code = match self.wjit.as_mut().map(|j| j.lookup(id)) {
                        // (a region with RAM blocks runs only while they are what memory holds)
                        Some((Some((f, at, r)), _)) => self.wasm_region_ok(bus, r).then_some((f, at)),
                        Some((None, true)) => {
                            // hot: compile its region (it runs from the next visit on)
                            let region = self.wasm_region(bus, id);
                            let ver = bus.code_writes().unwrap_or(u64::MAX);
                            let ok = !region.is_empty() && self.wjit.as_mut().is_some_and(|j| j.install::<B>(&region, &mem, ver));
                            if !ok {
                                if let Some(j) = self.wjit.as_mut() {
                                    j.refuse(id);
                                }
                            }
                            None
                        }
                        _ => None,
                    };
                    if let Some((code, at)) = code {
                        let extra = if n == 0 { 0 } else { 1 };
                        sync!();
                        // (the budget as the interpreter keeps it, to the instruction: each block
                        // of the region runs only while it fits, else the interpreter goes on)
                        let cap = limit - n;
                        // SAFETY: code was compiled for this bus type and memory map
                        let k = unsafe { jit_wasm::run(code, at, self, bus, extra, cap) };
                        if self.profile { self.jit_entries += 1; self.jit_insns += k as u64; }
                        if k > 0 {
                            n += k;
                            // (a stretch: its helpers don't say whether they reached a peripheral)
                            interpreted = true;
                            pc = self.pc;
                            // (it left between the two instructions of a glued pair, or inside an
                            // if-else block's then-part)
                            careful = self.hold_irq != 0 || !self.pred_skips.is_empty();
                            continue;
                        }
                    }
                }
            }
            #[cfg(not(any(native_jit, target_arch = "wasm32")))]
            let _ = (id, &jit_mem);
            // (only the native JITs run on from a block compiled whole)
            #[cfg(not(native_jit))]
            let _ = whole;
            interpreted = true;
            // a RAM block: a store into one of its own later instructions (the bus counts writes
            // to code words) ends it there, and the rest is built again from what memory holds,
            // as `step` fetches it and compiled code's `h_write` leaves
            let code_ver = if (0x0200_0000..0x0400_0000).contains(&pc) { None } else { bus.code_writes() };
            for i in start..start + len {
                // SAFETY: build_block pushed start..start+len into bops and
                // bops is only cleared together with the block table
                let op = unsafe { *self.bops.get_unchecked(i as usize) };
                // the budget, to the instruction: where a call stops does not depend on where
                // blocks begin, so compiled code (which runs a block only while it fits) and
                // the interpreter stop at the same one, and cores interleave the same
                if n >= limit {
                    if i > start && !careful && unsafe { self.bops.get_unchecked(i as usize - 1) }.glue {
                        self.hold_irq = 1;
                    }
                    sync!();
                    return Ok((n, false));
                }
                if n > 0 && bus.tick() {
                    // this instruction's tick is spent; the caller services
                    // the SoC and then runs it without ticking again
                    // (right after a glued pair's head: interrupts wait for this one)
                    if i > start && !careful && unsafe { self.bops.get_unchecked(i as usize - 1) }.glue {
                        self.hold_irq = 1;
                    }
                    sync!();
                    return Ok((n, true));
                }
                let next_pc = pc + op.len as u32;
                if !(careful || op.careful) {
                    // plain micro-op: cannot start or end a pair, block or loop
                    pc = self.exec_fast(bus, op.fast, next_pc).unwrap_or(next_pc);
                    n += 1;
                } else {
                    sync!();
                    // (it counts what it runs in insn_count itself; a swapped pair is two)
                    let r = self.exec_careful_op(bus, i, careful, next_pc, limit - n);
                    n += r & 3;
                    synced = n;
                    pc = self.pc;
                    careful = r & 4 != 0;
                    if r & 8 != 0 {
                        return Ok((n, false));
                    }
                    if r & 16 != 0 {
                        // (a swapped pair cut after its head: the second one's tick is spent)
                        return Ok((n, true));
                    }
                }
                if pc != next_pc {
                    break; // taken branch, skip or loop: look up the next block
                }
                if code_ver.is_some() && !op.glue && bus.code_writes() != code_ver {
                    break; // (its code was written: the next instruction from memory)
                }
            }
        }
    }

    /// May a compiled prefix from pc to `end` run inside the then-part or rep loop in progress,
    /// as the careful path would run it? Only when no instruction of it but the last can end the
    /// construct: the then-parts' ends (where the careful path skips the else part) lie at or
    /// past its end, the rep loop's end does not lie in it or at its end (the careful path counts
    /// down when execution falls there). Then every instruction before the last leaves the
    /// construct as it was, and run_fast applies `finish`'s rule for the last. Pairs: the careful
    /// path runs them with the head's registers held back (`pair_pending`) and does not stop
    /// after one (the construct goes on); compiled code gives the same registers after the pair
    /// and does not stop either (`jit::run`'s `inside`), but it could leave between a glued
    /// pair's two instructions only with the head's registers written (`hold_irq`): so a glued
    /// head must not use the bus, the only way out between the two (`jit::Prefix::inside_ok`).
    #[cfg(native_jit)]
    #[inline]
    fn prefix_inside(&self, pc: u32, end: u32) -> bool {
        !(self.rep_end > pc && self.rep_end <= end) && self.pred_skips.iter().all(|&(te, _, _)| te >= end)
    }

    /// Is a then-part or a rep loop (one of them) all that is in progress?
    #[cfg(native_jit)]
    #[inline(never)]
    fn only_construct(&self) -> bool {
        self.pair_pending.is_none() && self.hold_irq == 0 && ((self.rep_end != 0) != !self.pred_skips.is_empty())
    }

    /// After a compiled prefix (`entry..end`) inside a then-part (`prefix_inside`): `finish`'s
    /// skip rule for where its last instruction went (none before it reached a then-part's end);
    /// the pc. The careful path then stops if that ended the construct and an interrupt is pending.
    #[cfg(native_jit)]
    #[inline(never)]
    fn finish_inside(&mut self, entry: u32, end: u32) -> u32 {
        let pc = self.pc;
        if let Some(i) = self.pred_skips.iter().position(|&(te, _, _)| te == pc) {
            self.pc = self.pred_skips.remove(i).1;
        }
        let (pc, rets) = (self.pc, self.rets);
        // (as `finish`: out past the then-part, or a goto back out of it; the prefix ends at its
        // jump, which lies in the then-part when the prefix does, and `end` is the jump's next pc)
        self.pred_skips.retain(|&(te, _, ts)| pc <= te && !(ts <= entry && end <= te && pred_left_back(pc, ts, end, rets)));
        pc
    }

    /// An op `run_fast` cannot run plainly, packed: bits 0-1 the instructions run, then still
    /// careful (4), stop (8: the pair, block or loop ended with an interrupt pending), the next
    /// tick due (16: it was spent). A `Fast::Swap` pair runs whole, its second instruction first
    /// (it sees the registers from before the head), when nothing is in progress, both fit the
    /// budget and the second one's tick is not due; else as the careful steps it stands for. A
    /// plain op inside an if-else block's then-part (nothing else in progress) applies `finish`'s
    /// skip rule itself. Anything else is `exec_careful`. (Out of line, and told the op's index,
    /// not the op: run_fast's loop keeps its registers.)
    #[inline(never)]
    fn exec_careful_op<B: Bus>(&mut self, bus: &mut B, i: u32, gated: bool, next_pc: u32, room: u32) -> u32 {
        let pack = |k: u32, still: bool, stop: bool, due: bool| k | (still as u32) << 2 | (stop as u32) << 3 | (due as u32) << 4;
        let op = self.bops[i as usize];
        if let Fast::Swap { idx } = op.fast {
            let (head, tail, hlen) = self.swaps[idx as usize];
            let head_next = self.pc.wrapping_add(hlen as u32);
            if !gated && room >= 2 && !bus.next_tick_due() {
                let _ = bus.tick();
                let jumped = self.exec_fast(bus, tail, next_pc);
                let _ = self.exec_fast(bus, head, head_next);
                self.insn_count += 2;
                self.pc = jumped.unwrap_or(next_pc);
                // (the careful path stops after a pair when an interrupt is pending)
                return pack(2, false, bus.irq_pending(), false);
            }
            // a head always leaves its pair in progress: never a stop
            let (still, _) = self.exec_careful(bus, head, true, gated, head_next);
            if self.pc != head_next || room < 2 {
                // (a rep loop or then-part ended at the head; the budget ends there): run_fast
                // goes on at pc as after any careful step
                return pack(1, still, false, false);
            }
            if bus.tick() {
                return pack(1, still, false, true);
            }
            let (still, stop) = self.exec_careful(bus, tail, false, still, next_pc);
            return pack(2, still, stop, false);
        }
        // (not a glued head: the careful path runs it as the pair it is)
        if gated && !op.careful && !op.pair_head && self.pred_skips.len() == 1 && self.pair_pending.is_none() && self.rep_end == 0 && self.hold_irq == 0 {
            // `finish` for one pending skip and nothing else in progress
            let at = self.pc;
            let mut pc = self.exec_fast(bus, op.fast, next_pc).unwrap_or(next_pc);
            self.insn_count += 1;
            let (te, ee, ts) = self.pred_skips[0];
            let mut still = true;
            if ts <= at && at < te && pred_left_back(pc, ts, next_pc, self.rets) {
                self.pred_skips.clear(); // a goto back out of the construct (`finish`)
                still = false;
            } else if pc >= te {
                if pc == te {
                    pc = ee;
                }
                self.pred_skips.clear();
                still = false;
            }
            self.pc = pc;
            return pack(1, still, !still && bus.irq_pending(), false);
        }
        let (still, stop) = self.exec_careful(bus, op.fast, op.pair_head, gated, next_pc);
        pack(1, still, stop, false)
    }

    /// How many instructions the block at pc holds up to `stop` (an instruction of it after the
    /// first, if one starts there) or its end (0: the block is empty, its first instruction needs
    /// `step`): a budget for `run_fast` that runs that much, compiled or not, and stops there.
    /// Instructions, not ops: a `Swap` counts two, so a block may hold more than 32. A `stop` at a
    /// Swap's second instruction counts through the head only, and `run_fast` then runs the head
    /// alone, as the careful path does, leaving the pair pending at `stop`. (cpu1's idle probe: it
    /// looks for the way back to where it began, which may lie inside a block.)
    pub fn block_len_to<B: Bus>(&mut self, bus: &mut B, stop: u32) -> u32 {
        let Ok(Some((_, start, len))) = self.block_at(bus, self.pc) else { return 0 };
        len_to(&self.bops[start as usize..(start + len) as usize], &self.swaps, self.pc, stop).0
    }

    /// The block of micro-ops starting at pc (XIP code only): built on first
    /// use from the decoded lines, ending after anything that may jump, before
    /// an instruction without a micro-op, or at 32 ops. None: pc is outside
    /// XIP or its first instruction needs `step`.
    #[inline]
    fn block_at<B: Bus>(&mut self, bus: &mut B, pc: u32) -> Result<Option<(usize, u32, u32)>, CoreError> {
        let xip = (0x0200_0000..0x0400_0000).contains(&pc);
        if !xip && (self.no_ram_blocks || ram_code(bus, pc).is_none()) {
            return Ok(None);
        }
        if !self.bmap.is_empty() {
            let id = self.bmap[icache_slot(pc)];
            if id != 0 {
                let (bpc, start, len) = self.blocks[id as usize - 1];
                if bpc == pc && (xip || self.ram_block_ok(bus, id as usize - 1)) {
                    // (len 0: remembered that the first instruction here needs `step`)
                    return Ok(if len == 0 { None } else { Some((id as usize - 1, start, len)) });
                }
            }
        }
        self.build_block(bus, pc)
    }

    /// Are region `r`'s blocks over RAM (if any) still what memory holds? Unchanged when the bus's
    /// count of code-word writes is the one they were last found current at; else each is checked
    /// (`ram_block_ok`), and a region with one that changed is dropped.
    #[cfg(target_arch = "wasm32")]
    fn wasm_region_ok<B: Bus>(&mut self, bus: &mut B, r: u32) -> bool {
        let Some(j) = self.wjit.as_ref() else { return false };
        let (ids, ver) = &j.rinfo[r as usize];
        if ids.is_empty() {
            return true;
        }
        let Some(w) = bus.code_writes() else { return false };
        if *ver == w {
            return true;
        }
        let ids = ids.clone();
        let ok = ids.iter().all(|&id| self.ram_block_ok(bus, id));
        if let Some(j) = self.wjit.as_mut() {
            if ok {
                j.rinfo[r as usize].1 = w;
            } else {
                j.drop_region(r);
            }
        }
        ok
    }

    /// The region of hot block `id` for the wasm JIT: it and the blocks reachable from it over
    /// fixed edges (breadth first), each as its plain prefix (up to the first careful op). Blocks
    /// over RAM join only when the bus counts writes to code words (`wasm_region_ok` then checks
    /// them all on entry; a write to one leaves the region): else one is a region's start or not
    /// in it, entered only through `block_at`, which checks its bytes first.
    #[cfg(target_arch = "wasm32")]
    fn wasm_region<B: Bus>(&mut self, bus: &mut B, id: usize) -> Vec<(usize, u32, Vec<BOp>)> {
        let mut out: Vec<(usize, u32, Vec<BOp>)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut queue = std::collections::VecDeque::from([self.blocks[id].0]);
        let mut total = 0;
        while let Some(pc) = queue.pop_front() {
            if out.len() >= jit_wasm::REGION_BLOCKS || total >= jit_wasm::REGION_OPS {
                break;
            }
            if !seen.insert(pc) || (!out.is_empty() && !(0x0200_0000..0x0400_0000).contains(&pc) && bus.code_writes().is_none()) {
                continue;
            }
            let Ok(Some((bid, start, len))) = self.block_at(bus, pc) else { continue };
            let all = &self.bops[start as usize..(start + len) as usize];
            let mut ops: Vec<BOp> = all.iter().take_while(|o| !o.careful).copied().collect();
            let (all_len, tail) = (all.len(), all.get(ops.len()).copied());
            // an if-else block at the end, its then-part plain: compiled with the then-part in
            // tow (`jit_wasm`'s IfBlk), going on at the else part or past it
            if let Some(last) = tail.filter(|_| ops.len() + 1 == all_len) {
                if let Fast::IfBlk { has_else: true, fam, then_d, else_d, .. } = last.fast {
                    let next = pc + ops.iter().chain([&last]).map(|o| o.len as u32).sum::<u32>();
                    if let Some(then) = (fam < 8).then(|| self.then_part(bus, next, then_d)).flatten() {
                        let then_end = next.wrapping_add(then_d as u32);
                        ops.push(last);
                        ops.extend(then);
                        queue.extend([then_end, then_end.wrapping_add(else_d as u32)]);
                        total += ops.len();
                        out.push((bid, pc, ops));
                        continue;
                    }
                }
            }
            if ops.is_empty() {
                continue;
            }
            if ops.len() == all_len {
                let last = ops[ops.len() - 1];
                let end: u32 = pc + ops.iter().map(|o| o.len as u32).sum::<u32>();
                match last.fast {
                    Fast::BrI { target, .. } | Fast::BrR { target, .. } | Fast::BrIEq { target, .. } | Fast::BrINe { target, .. }
                    | Fast::BitBr { target, .. } | Fast::DecBr { target, .. } => queue.extend([end, target]),
                    Fast::Goto { target } => queue.push_back(target),
                    // the callee, and where it returns to (its return finds that block in the
                    // region: `jit_wasm::Gen::jump_local`)
                    Fast::Call { target } => queue.extend([target, end]),
                    Fast::IfBlk { has_else: false, then_d, .. } => queue.extend([end, end.wrapping_add(then_d as u32)]),
                    f if f.may_jump() => {}
                    _ => queue.push_back(end),
                }
            }
            total += ops.len();
            out.push((bid, pc, ops));
        }
        out
    }

    /// The then-part of the if-else block that ends block `id` (at pc), when there is one that
    /// `then_part` gives: what the native JIT compiles in line with the if (`jit::Jit::entry`).
    #[cfg(native_jit)]
    #[inline(never)]
    fn then_of<B: Bus>(&mut self, bus: &mut B, id: usize, pc: u32) -> Option<Vec<BOp>> {
        let (_, start, len) = self.blocks[id];
        let ops = &self.bops[start as usize..(start + len) as usize];
        let last = ops.last()?;
        match last.fast {
            Fast::IfBlk { has_else: true, then_d, .. } if !last.pair_head => {
                let next = pc + ops.iter().map(|o| o.len as u32).sum::<u32>();
                self.then_part(bus, next, then_d)
            }
            _ => None,
        }
    }

    /// The then-part of an if-else block, `then_d` bytes from `pc`, when it is plain micro-ops
    /// at the start of the block there (no jump, pair, careful op): what the JITs compile in
    /// line with the if.
    #[cfg(any(target_arch = "wasm32", native_jit))]
    fn then_part<B: Bus>(&mut self, bus: &mut B, pc: u32, then_d: u16) -> Option<Vec<BOp>> {
        let mut then = Vec::new();
        let mut q = pc;
        let end = pc.wrapping_add(then_d as u32);
        if q != end {
            let (_, start, len) = self.block_at(bus, pc).ok()??;
            for o in &self.bops[start as usize..(start + len) as usize] {
                if q == end {
                    break;
                }
                if o.careful || o.pair_head || o.glue || o.fast.may_jump() {
                    return None;
                }
                then.push(*o);
                q += o.len as u32;
            }
        }
        (q == end).then_some(then)
    }

    /// Is RAM block `id` still built from what memory holds now? By the bus's count of writes to
    /// code words when it keeps one: the same count as when the block was last found current,
    /// nothing has changed; else the bytes are compared. (Sound because every write that can
    /// reach a block's bytes is counted: the bus's own writes check the word's code flag, and
    /// compiled code's inline stores, native and wasm, take the bus for a flagged word. Compiled
    /// RAM chain targets keep separate versioned snapshots, `jit::RamGuard`.)
    #[inline]
    fn ram_block_ok<B: Bus>(&mut self, bus: &mut B, id: usize) -> bool {
        if let Some(w) = bus.code_writes() {
            if self.bver[id] == w {
                return true;
            }
            let ok = self.ram_block_current(bus, id);
            if ok {
                self.bver[id] = w;
            }
            return ok;
        }
        self.ram_block_current(bus, id)
    }

    /// Is RAM block `id` still built from what memory holds now?
    #[inline]
    fn ram_block_current<B: Bus>(&self, bus: &mut B, id: usize) -> bool {
        let (pc, ..) = self.blocks[id];
        match (self.bbytes.get(id).and_then(|b| b.as_deref()), ram_code(bus, pc)) {
            (Some(was), Some(now)) => now.len() >= was.len() && same_bytes(&now[..was.len()], was),
            _ => false,
        }
    }

    /// The compiled body of the block starting at pc, if there is one.
    #[cfg(native_jit)]
    fn jit_body_at(&self, pc: u32, ram_tracked: bool) -> Option<*const u8> {
        // RAM bodies validate their own immutable code snapshot before execution.
        if self.bmap.is_empty() || (!ram_tracked && !(0x0200_0000..0x0400_0000).contains(&pc)) {
            return None;
        }
        let id = self.bmap[icache_slot(pc)];
        if id == 0 || self.blocks[id as usize - 1].0 != pc {
            return None;
        }
        self.jit.as_ref()?.body(id as usize - 1)
    }

    #[inline(never)]
    fn build_block<B: Bus>(&mut self, bus: &mut B, pc: u32) -> Result<Option<(usize, u32, u32)>, CoreError> {
        if self.bmap.is_empty() {
            self.bmap = vec![0; ICACHE_LINES];
        }
        if self.bops.len() > 1 << 22 {
            // (cannot happen for one image, but never grow without bound)
            self.bops.clear();
            self.swaps.clear();
            self.blocks.clear();
            self.bbytes.clear();
            self.bver.clear();
            self.bmap.iter_mut().for_each(|x| *x = 0);
            #[cfg(native_jit)]
            if let Some(j) = self.jit.as_mut() {
                j.reset();
            }
            #[cfg(target_arch = "wasm32")]
            if let Some(j) = self.wjit.as_mut() {
                j.reset();
            }
        }
        let start = self.bops.len() as u32;
        let xip = (0x0200_0000..0x0400_0000).contains(&pc);
        let mut p = pc;
        let code = |bus: &mut B, p: u32| if xip { (0x0200_0000..0x0400_0000).contains(&p) } else { ram_code(bus, p).is_some() };
        // (a glued pair's second instruction always joins its head's block)
        let mut glued = false;
        while ((self.bops.len() as u32 - start) < 32 || glued) && code(bus, p) {
            let slot = self.fetch_at(bus, p)?;
            let l = self.icache[slot];
            let Some(f) = l.fast else { break };
            let mut op = BOp { fast: f, len: l.len, careful: l.careful, pair_head: l.pair_head, glue: false };
            let q = p + l.len as u32;
            glued = false;
            // (a head whose flags are read later keeps the careful path: the JITs do not set them)
            if l.pair_head && !l.flags_live && !f.may_jump() && !matches!(f, Fast::IfBlk { .. }) && code(bus, q) {
                let ts = self.fetch_at(bus, q)?;
                let t = self.icache[ts];
                if let (Some(tf), false, false) = (t.fast, t.pair_head, t.careful) {
                    if let (Some((_, hw)), Some((tr, tw))) = (f.regs_rw(), tf.regs_rw()) {
                        if hw & (tr | tw) == 0 && !matches!(tf, Fast::IfBlk { .. }) {
                            op.careful = false;
                            op.glue = true;
                            glued = true;
                        } else if f.swappable_with(tf) {
                            // the second reads what the head writes: one op for both, the
                            // second first (`exec_careful_op`; careful, the compilers keep away)
                            self.swaps.push((f, tf, l.len));
                            let idx = self.swaps.len() as u32 - 1;
                            self.bops.push(BOp { fast: Fast::Swap { idx }, len: l.len + t.len, careful: true, pair_head: true, glue: false });
                            p = q + t.len as u32;
                            if tf.may_jump() {
                                break;
                            }
                            continue;
                        }
                    }
                }
            }
            self.bops.push(op);
            p = q;
            if f.may_jump() {
                break;
            }
        }
        // an empty block is kept too: the first instruction needs `step`, and block_at need not
        // look again every time execution gets here
        let len = self.bops.len() as u32 - start;
        self.blocks.push((pc, start, len));
        // a RAM block keeps its bytes (6 more: an instruction's window reaches past its length)
        let bytes: Option<Box<[u8]>> = if xip { None } else { ram_code(bus, pc).map(|m| m[..((p - pc) as usize + 6).min(m.len())].into()) };
        if let Some(b) = &bytes {
            // (its pages are watched from now on: writes to them are counted)
            bus.code_mark(pc, pc + b.len() as u32);
        }
        self.bbytes.resize(self.blocks.len() - 1, None);
        self.bbytes.push(bytes);
        {
            self.bver.resize(self.blocks.len() - 1, u64::MAX);
            self.bver.push(bus.code_writes().unwrap_or(u64::MAX));
        }
        let slot = icache_slot(pc);
        self.bmap[slot] = self.blocks.len() as u32;
        Ok(if len == 0 { None } else { Some((self.blocks.len() - 1, start, len)) })
    }

    /// The careful path of `run_fast`: pair heads, predicated blocks, and
    /// anything while a pair, block or rep loop is in progress. A pair,
    /// block or loop holds interrupts back; when this instruction ends one
    /// and an interrupt is pending, the per-step loop would take it right
    /// here, so the block stops (second result). The first result: still in
    /// a pair, block or loop.
    #[inline(never)]
    fn exec_careful<B: Bus>(&mut self, bus: &mut B, f: Fast, head: bool, gated: bool, next_pc: u32) -> (bool, bool) {
        let jumped = if !head && self.pair_pending.is_none() {
            self.exec_fast(bus, f, next_pc)
        } else {
            self.exec_fast_paired(bus, f, head, next_pc)
        };
        self.finish(jumped, next_pc);
        let still = self.pair_pending.is_some() || !self.pred_skips.is_empty() || self.rep_end != 0;
        (still, gated && !still && bus.irq_pending())
    }

    /// A micro-op that heads or closes a dual-issue pair: exactly `step`'s
    /// rule (the head's register results are held back until the second
    /// instruction has run). Out of line: pairs are the rarer case.
    #[inline(never)]
    fn exec_fast_paired<B: Bus>(&mut self, bus: &mut B, f: Fast, head: bool, next_pc: u32) -> Option<u32> {
        let pending = self.pair_pending.take();
        let before = self.regs;
        let jumped = self.exec_fast(bus, f, next_pc);
        if let Some((old, new)) = pending {
            for i in 0..16 {
                if new[i] != old[i] && self.regs[i] == old[i] {
                    self.regs[i] = new[i];
                }
            }
        }
        if head {
            let after = self.regs;
            self.regs = before;
            self.pair_pending = Some((before, after));
        }
        jumped
    }

    /// Fetch/decode/execute one instruction. Fetches a 6-byte window (the
    /// longest pi32v2 instruction); short instructions zero-extend.
    pub fn step<B: Bus>(&mut self, bus: &mut B) -> Result<(), CoreError> {
        let slot = self.fetch(bus)?;
        let l = self.icache[slot];
        let next_pc = self.pc + l.len as u32;
        // hot path: a micro-op outside any dual-issue pair
        if let (Some(f), false, true) = (l.fast, l.pair_head, self.pair_pending.is_none()) {
            if !trace_on() {
                if self.profile {
                    self.route_hist[31] += 1;
                }
                let jumped = self.exec_fast(bus, f, next_pc);
                self.finish(jumped, next_pc);
                return Ok(());
            }
        }
        let x = self.icache_x[slot].expect("decoded line");
        if self.profile {
            self.route_hist[(x.pre.route as usize).min(30)] += 1;
            let tag = format!("{}{}{}{}", if l.fast.is_some() {"F"} else {"-"}, if l.pair_head {"H"} else {"-"}, if self.pair_pending.is_some() {"P"} else {"-"}, x.entry.name);
            let tag: &'static str = Box::leak(tag.into_boxed_str());
            *self.slow_hist.entry(tag).or_insert(0) += 1;
        }
        let insn = Instruction { addr: self.pc, raw: x.win, entry: x.entry };
        let is_pair_head = x.pre.pair_head;
        self.cur_syntax = x.pre.syntax;
        let before = self.regs;
        let pending = self.pair_pending.take();
        let res = self.execute(bus, &insn, &x.pre, next_pc);
        if let Some((old, new)) = pending {
            // commit the pair head's results the second instruction didn't override
            for i in 0..16 {
                if new[i] != old[i] && self.regs[i] == old[i] {
                    self.regs[i] = new[i];
                }
            }
        }
        if is_pair_head && res.is_ok() {
            let after = self.regs;
            self.regs = before;
            self.pair_pending = Some((before, after));
        }
        res
    }

    fn set_sreg_named(&mut self, n: &str, v: u32) {
        match n {
            "rets" => self.rets = v,
            "sp" => self.sp = v,
            "ssp" => self.set_ssp(v),
            "usp" => self.set_usp(v),
            _ => self.set_sreg(n, v),
        }
    }

    /// `ssp` / `usp` as the instructions see them (registers of their own; see `user`).
    pub fn get_ssp(&self) -> u32 {
        self.ssp
    }
    pub fn get_usp(&self) -> u32 {
        self.usp
    }
    pub fn set_ssp(&mut self, v: u32) {
        self.ssp = v;
    }
    pub fn set_usp(&mut self, v: u32) {
        self.usp = v;
    }

    fn sreg_index(n: &str) -> usize {
        match n { "icfg" => 0, "reti" => 1, "retx" => 2, "rete" => 3, "sspn" => 4, "sr4" => 6, _ => 5 }
    }
    /// `icfg` as read: low byte, the ISR priorities in progress (`icfg & 0xff` is the RTOS's
    /// "in interrupt context" test); bit 9, the global enable `sti` / `cli` set and clear (the
    /// SDK's cpu_irq_disabled() is `(icfg & 0x300) != 0x300`, bit 8 being the software master
    /// enable; the stock RTOS asserted in os_mutex_pend while bit 9 read 0). A write sets the
    /// global enable from its bit 9 (`set_icfg`).
    #[inline]
    fn icfg(&self) -> u32 {
        let active = self.irq_levels.iter().fold(0u32, |m, &p| m | 1 << p);
        (self.sreg_store[0] & !0x2ff) | active | if self.ie { 0x200 } else { 0 }
    }
    fn sregs(&self, n: &str) -> u32 {
        if n == "cnum" {
            self.core_id
        } else if n == "icfg" {
            self.icfg()
        } else if n == "psr" {
            (self.sreg_store[5] & !0xf) | self.flags
        } else {
            self.sreg_store[Self::sreg_index(n)]
        }
    }
    /// `icfg = R`: bit 9 is the global enable, as it reads. Felucca's guard is `f = icfg; cli;
    /// ...; icfg = f`, so the write must turn interrupts back on (inside an ISR, Rainbow's beat
    /// left them off for good and the firmware stopped).
    #[inline]
    fn set_icfg(&mut self, v: u32) {
        self.sreg_store[0] = v;
        self.ie = v & 0x200 != 0;
    }
    fn set_sreg(&mut self, n: &str, v: u32) {
        if n == "psr" {
            self.flags = v & 0xf;
        }
        if n == "icfg" {
            return self.set_icfg(v);
        }
        self.sreg_store[Self::sreg_index(n)] = v;
    }

    /// `alu` for the micro-ops, setting the flags on add / subtract.
    #[inline(always)]
    fn alu_f(&mut self, op: Op, a: u32, b: u32) -> u32 {
        match op {
            Op::Add => { let (r, f) = add_flags(a, b, 0); self.flags = f; r }
            Op::Sub => { let (r, f) = add_flags(a, !b, 1); self.flags = f; r }
            Op::RSub => { let (r, f) = add_flags(b, !a, 1); self.flags = f; r }
            _ => alu(op, a, b),
        }
    }

    /// Execute a precomputed micro-op: the same semantics as the matching
    /// `exec_syntax` shape, with operands and branch targets resolved once.
    /// (always inlined: wasm builds otherwise call it per instruction from the block loop)
    #[inline(always)]
    fn exec_fast<B: Bus>(&mut self, bus: &mut B, f: Fast, next_pc: u32) -> Option<u32> {
        let _ = next_pc;
        let base = |cpu: &Self, b: Base| match b { Base::Reg(r) => cpu.regs[r as usize], Base::Sp => cpu.sp };
        match f {
            Fast::Mov { d, s } => self.regs[(d & 15) as usize] = self.regs[(s & 15) as usize],
            Fast::MovI { d, v } => self.regs[(d & 15) as usize] = v,
            Fast::Alu3 { d, a, b, op } => self.regs[(d & 15) as usize] = self.alu_f(op, self.regs[(a & 15) as usize], self.regs[(b & 15) as usize]),
            Fast::Alu3I { d, a, v, op } => self.regs[(d & 15) as usize] = self.alu_f(op, self.regs[(a & 15) as usize], v),
            Fast::Alu2 { d, b, op } => self.regs[(d & 15) as usize] = self.alu_f(op, self.regs[(d & 15) as usize], self.regs[(b & 15) as usize]),
            Fast::Alu2I { d, v, op } => self.regs[(d & 15) as usize] = self.alu_f(op, self.regs[(d & 15) as usize], v),
            Fast::Ld { d, b, off, w, signed } => {
                let a = base(self, b).wrapping_add(off);
                self.regs[(d & 15) as usize] = match (w, signed) {
                    (1, false) => bus.read8(a) as u32,
                    (1, true) => bus.read8(a) as i8 as i32 as u32,
                    (2, false) => bus.read16(a) as u32,
                    (2, true) => bus.read16(a) as i16 as i32 as u32,
                    _ => bus.read32(a),
                };
            }
            Fast::St { s, b, off, w } => {
                let a = base(self, b).wrapping_add(off);
                let v = self.regs[(s & 15) as usize];
                match w {
                    1 => bus.write8(a, v as u8),
                    2 => bus.write16(a, v as u16),
                    _ => bus.write32(a, v),
                }
            }
            Fast::BrI { a, v, cond, target } => {
                if cond_true(cond, self.regs[(a & 15) as usize], v) {
                    return Some(target);
                }
            }
            Fast::BrR { a, b, cond, target } => {
                if cond_true(cond, self.regs[(a & 15) as usize], self.regs[(b & 15) as usize]) {
                    return Some(target);
                }
            }
            Fast::Goto { target } => return Some(target),
            Fast::Call { target } => {
                self.rets = next_pc;
                return Some(target);
            }
            Fast::Rts => return Some(self.rets),
            Fast::A4 { d, s, op, rk, rv } => {
                let rhs = match rk { 0 => self.regs[(rv & 15) as usize], 1 => rv, _ => 1u32 << (self.regs[(rv & 15) as usize] & 31) };
                self.regs[(d & 15) as usize] = self.alu_f(op, self.regs[(s & 15) as usize], rhs);
            }
            Fast::BitBr { r, mk, ne, mv, target } => {
                let mask = match mk { 0 => self.regs[(mv & 15) as usize], 1 => mv, _ => 1u32 << (self.regs[(mv & 15) as usize] & 31) };
                if ((self.regs[(r & 15) as usize] & mask) != 0) == ne {
                    return Some(target);
                }
            }
            Fast::LdIdx { d, b, i, sh, w, signed } => {
                let a = self.regs[(b & 15) as usize].wrapping_add(self.regs[(i & 15) as usize] << sh);
                self.regs[(d & 15) as usize] = ld_w(bus, w, signed, a);
            }
            Fast::StIdx { s, b, i, sh, w } => {
                let a = self.regs[(b & 15) as usize].wrapping_add(self.regs[(i & 15) as usize] << sh);
                st_w(bus, w, a, self.regs[(s & 15) as usize]);
            }
            Fast::Rmw { b, w, op, off, v } => {
                let a = self.regs[(b & 15) as usize].wrapping_add(off);
                let nv = match op {
                    RmwOp::Mov => v,
                    RmwOp::Or => ld_w(bus, w, false, a) | v,
                    RmwOp::And => ld_w(bus, w, false, a) & v,
                    RmwOp::Xor => ld_w(bus, w, false, a) ^ v,
                    RmwOp::Add => ld_w(bus, w, false, a).wrapping_add(v),
                    RmwOp::Sub => ld_w(bus, w, false, a).wrapping_sub(v),
                };
                st_w(bus, w, a, nv);
            }
            Fast::LdPost { d, b, w, signed, inc } => {
                let a = self.regs[(b & 15) as usize];
                self.regs[(d & 15) as usize] = ld_w(bus, w, signed, a);
                self.regs[(b & 15) as usize] = self.regs[(b & 15) as usize].wrapping_add(inc);
            }
            Fast::StPost { s, b, w, inc } => {
                let a = self.regs[(b & 15) as usize];
                st_w(bus, w, a, self.regs[(s & 15) as usize]);
                self.regs[(b & 15) as usize] = self.regs[(b & 15) as usize].wrapping_add(inc);
            }
            Fast::DecBr { r, cond, v, target } => {
                let x = self.regs[(r & 15) as usize].wrapping_sub(1);
                self.regs[(r & 15) as usize] = x;
                if cond_true(cond, x, v) {
                    return Some(target);
                }
            }
            Fast::Nop => {}
            Fast::SpAdd { v } => self.sp = self.sp.wrapping_add(v),
            Fast::SpFromR { s } => self.sp = self.regs[(s & 15) as usize],
            Fast::RFromSp { d, v } => self.regs[(d & 15) as usize] = self.sp.wrapping_add(v),
            Fast::BrIEq { a, v, target } => {
                if self.regs[(a & 15) as usize] == v {
                    return Some(target);
                }
            }
            Fast::BrINe { a, v, target } => {
                if self.regs[(a & 15) as usize] != v {
                    return Some(target);
                }
            }
            Fast::LdW { d, b, off } => {
                let a = self.regs[(b & 15) as usize].wrapping_add(off);
                self.regs[(d & 15) as usize] = bus.read32(a);
            }
            Fast::LdWSp { d, off } => {
                let a = self.sp.wrapping_add(off);
                self.regs[(d & 15) as usize] = bus.read32(a);
            }
            Fast::LdBu { d, b, off } => {
                let a = self.regs[(b & 15) as usize].wrapping_add(off);
                self.regs[(d & 15) as usize] = bus.read8(a) as u32;
            }
            Fast::StW { s, b, off } => {
                let a = self.regs[(b & 15) as usize].wrapping_add(off);
                bus.write32(a, self.regs[(s & 15) as usize]);
            }
            Fast::StWSp { s, off } => {
                let a = self.sp.wrapping_add(off);
                bus.write32(a, self.regs[(s & 15) as usize]);
            }
            Fast::AddRI { d, s, v } => {
                let (r, f) = add_flags(self.regs[(s & 15) as usize], v, 0);
                self.flags = f;
                self.regs[(d & 15) as usize] = r;
            }
            Fast::ShlI { d, s, n } => {
                self.regs[(d & 15) as usize] = self.regs[(s & 15) as usize] << (n & 31);
            }
            Fast::SarI { d, s, n } => {
                self.regs[(d & 15) as usize] = ((self.regs[(s & 15) as usize] as i32) >> (n & 31)) as u32;
            }
            Fast::PushRet => {
                self.sp = self.sp.wrapping_sub(4);
                bus.write32(self.sp, self.rets);
            }
            Fast::PopRet => {
                let j = bus.read32(self.sp);
                self.sp = self.sp.wrapping_add(4);
                return Some(j);
            }
            Fast::Ext { d, s, part, signed } => {
                let x = self.regs[(s & 15) as usize];
                self.regs[(d & 15) as usize] = match (part, signed) {
                    (0, false) => x & 0xff,
                    (0, true) => x as u8 as i8 as i32 as u32,
                    (1, false) => x & 0xffff,
                    (1, true) => x as u16 as i16 as i32 as u32,
                    (_, false) => x >> 16,
                    (_, true) => ((x >> 16) as u16 as i16 as i32) as u32,
                };
            }
            Fast::FOp { d, a, b, op } => {
                let d = (d & 15) as usize;
                self.regs[d] = fpu_d(op, self.regs[d], self.regs[(a & 15) as usize], self.regs[(b & 15) as usize])
            }
            Fast::MemA4 { b, off, op, rk, rv } => {
                let rhs = match rk { 0 => self.regs[(rv & 15) as usize], 1 => rv, _ => 1u32 << (self.regs[(rv & 15) as usize] & 31) };
                let a = (self.regs[(b & 15) as usize] as i64 + off as i64) as u32;
                let cur = bus.read32(a);
                bus.write32(a, alu(op, cur, rhs));
            }
            Fast::BitExt { d, s, p, l, signed } => {
                let src = self.regs[(s & 15) as usize];
                let (p, l) = (p as u32 & 31, l as u32 & 31);
                self.regs[(d & 15) as usize] = if signed {
                    if l == 0 { src >> p } else { (((src >> p) << (32 - l)) as i32 >> (32 - l)) as u32 }
                } else {
                    let mask = if l == 0 { 0xffff_ffffu32 } else { (1u32 << l).wrapping_sub(1) };
                    (src >> p) & mask
                };
            }
            Fast::BitIns { d, s, p, l } => {
                let src = self.regs[(s & 15) as usize];
                let (p, l) = (p as u32 & 31, l as u32 & 31);
                let mask = if l == 0 { 0xffff_ffffu32 } else { (1u32 << l).wrapping_sub(1) };
                let d = (d & 15) as usize;
                self.regs[d] = (self.regs[d] & !(mask << p)) | ((src & mask) << p);
            }
            Fast::PushRets { n } => {
                let n = n as i32;
                let (top, bot) = (n.max(3), n.min(4));
                for r in std::iter::once(-1i32).chain((bot..=top).rev()) {
                    self.sp = self.sp.wrapping_sub(4);
                    let v = if r < 0 { self.rets } else { self.regs[(r & 15) as usize] };
                    bus.write32(self.sp, v);
                }
            }
            Fast::PopPc { n } => {
                let n = n as i32;
                let (top, bot) = (n.max(3), n.min(4));
                for r in bot..=top {
                    self.regs[(r & 15) as usize] = bus.read32(self.sp);
                    self.sp = self.sp.wrapping_add(4);
                }
                let j = bus.read32(self.sp);
                self.sp = self.sp.wrapping_add(4);
                return Some(j);
            }
            Fast::IfBlk { a, rk, fam, alt, has_else, rv, then_d, else_d } => {
                let x = self.regs[(a & 15) as usize];
                let y = match rk { 0 => self.regs[(rv & 15) as usize], 1 => rv, _ => 1u32 << (self.regs[(rv & 15) as usize] & 31) };
                let taken = if fam == 2 { ((x & y) != 0) == alt } else { cmp_cond(fam as u64, alt, x, y) };
                let then_end = next_pc.wrapping_add(then_d as u32);
                if taken {
                    if has_else {
                        let else_end = then_end.wrapping_add(else_d as u32);
                        self.pred_skips.retain(|&(te, _, _)| te != then_end);
                        self.pred_skips.push((then_end, else_end, next_pc));
                    }
                } else {
                    return Some(then_end);
                }
            }
            // the rarer ops out of line, to keep the inlined copies of this match small
            _ => return self.exec_rare(bus, f, next_pc),
        }
        None
    }

    /// `exec_fast` for a `quiet` op (CPU state only: the compilers call it without the bus).
    #[cfg(native_jit)]
    fn exec_quiet(&mut self, f: Fast) {
        match f {
            Fast::SetIe { on: false } => self.ie = false,
            Fast::IcfgRd { d } => self.regs[(d & 15) as usize] = self.icfg(),
            _ => unreachable!("exec_quiet: {f:?}"),
        }
    }

    /// `exec_fast` for the rarer micro-ops.
    #[inline(never)]
    fn exec_rare<B: Bus>(&mut self, bus: &mut B, f: Fast, next_pc: u32) -> Option<u32> {
        match f {
            Fast::CallR { r } => {
                self.rets = next_pc;
                return Some(self.regs[(r & 15) as usize]);
            }
            Fast::GotoR { r } => return Some(self.regs[(r & 15) as usize]),
            Fast::SetIe { on } => {
                self.ie = on;
                if on {
                    bus.irq_window();
                }
            }
            Fast::PushRegs { mask } => {
                for r in (0..16).rev() {
                    if mask >> r & 1 != 0 {
                        self.sp = self.sp.wrapping_sub(4);
                        bus.write32(self.sp, self.regs[r]);
                    }
                }
            }
            Fast::PopRegs { mask } => {
                for r in 0..16 {
                    if mask >> r & 1 != 0 {
                        self.regs[r] = bus.read32(self.sp);
                        self.sp = self.sp.wrapping_add(4);
                    }
                }
            }
            Fast::PushSregs { mask } => {
                if mask & SREG_RETI != 0 {
                    self.nest_ok = true;
                }
                for (i, name) in SREG_LIST.iter().enumerate() {
                    if mask >> i & 1 != 0 {
                        let val = sreg_get_named(self, name);
                        self.sp = self.sp.wrapping_sub(4);
                        bus.write32(self.sp, val);
                    }
                }
                // (nesting may have opened: the run loop looks, as it did after the slow path)
                bus.irq_window();
            }
            Fast::PopSregs { mask } => {
                if mask & SREG_RETI != 0 {
                    self.nest_ok = false;
                }
                for (i, name) in SREG_LIST.iter().enumerate().rev() {
                    if mask >> i & 1 != 0 {
                        let val = bus.read32(self.sp);
                        self.sp = self.sp.wrapping_add(4);
                        self.set_sreg_named(name, val);
                    }
                }
                bus.irq_window();
            }
            Fast::Rti => {
                // back to the mode the interrupt came from; with none in progress, user mode
                let to_user = match self.irq_levels.pop() {
                    Some(_) => self.user_irq >> self.irq_levels.len().min(31) & 1 != 0,
                    None => true,
                };
                if to_user && !self.user {
                    self.ssp = self.sp;
                    self.sp = self.usp;
                    self.user = true;
                }
                // back in an outer ISR: it was interrupted with reti on its stack
                self.nest_ok = !self.irq_levels.is_empty();
                bus.service();
                return Some(self.sregs("reti"));
            }
            Fast::AddC { d, a, b, sub } => {
                let y = self.regs[(b & 15) as usize];
                let (v, f) = add_flags(self.regs[(a & 15) as usize], if sub { !y } else { y }, (self.flags & F_C != 0) as u32);
                self.regs[(d & 15) as usize] = v;
                self.flags = f;
            }
            Fast::IcfgRd { d } => self.regs[(d & 15) as usize] = self.icfg(),
            Fast::IcfgWr { s } => {
                self.set_icfg(self.regs[(s & 15) as usize]);
                bus.irq_window();
            }
            Fast::BlockMove { base, mask, store, post, down } => {
                let base = (base & 15) as usize;
                let start = if down { self.regs[base].wrapping_sub(4 * mask.count_ones()) } else { self.regs[base] };
                let mut addr = start;
                for i in 0..16usize {
                    if mask >> i & 1 != 0 {
                        if store {
                            bus.write32(addr, self.regs[i]);
                        } else {
                            self.regs[i] = bus.read32(addr);
                        }
                        addr = addr.wrapping_add(4);
                    }
                }
                if post {
                    self.regs[base] = if down { start } else { addr };
                }
            }
            Fast::PushMask { mask } => {
                self.sp = self.sp.wrapping_sub(4);
                bus.write32(self.sp, self.rets);
                for r in (0..16).rev() {
                    if mask >> r & 1 != 0 {
                        self.sp = self.sp.wrapping_sub(4);
                        bus.write32(self.sp, self.regs[r]);
                    }
                }
            }
            Fast::PopMask { mask } => {
                for r in 0..16 {
                    if mask >> r & 1 != 0 {
                        self.regs[r] = bus.read32(self.sp);
                        self.sp = self.sp.wrapping_add(4);
                    }
                }
                let j = bus.read32(self.sp);
                self.sp = self.sp.wrapping_add(4);
                return Some(j);
            }
            Fast::MinMax { d, a, b, kind } => {
                let (x, y) = (self.regs[(a & 15) as usize], self.regs[(b & 15) as usize]);
                self.regs[(d & 15) as usize] = match kind {
                    0 => (x as i32).min(y as i32) as u32,
                    1 => (x as i32).max(y as i32) as u32,
                    2 => x.min(y),
                    _ => x.max(y),
                };
            }
            Fast::Raw { route, raw } => self.exec_raw(bus, route, raw),
            Fast::PopRets => {
                self.rets = bus.read32(self.sp);
                self.sp = self.sp.wrapping_add(4);
            }
            Fast::Tb { r, half } => {
                let idx = self.regs[(r & 15) as usize];
                let e = if half { bus.read16(next_pc.wrapping_add(idx)) as u32 } else { bus.read8(next_pc.wrapping_add(idx)) as u32 };
                return Some(next_pc.wrapping_add(2 * e));
            }
            Fast::Mov2 { dh, dl, sh, sl } => {
                let (a, b) = (self.regs[(sh & 15) as usize], self.regs[(sl & 15) as usize]);
                self.regs[(dh & 15) as usize] = a;
                self.regs[(dl & 15) as usize] = b;
            }
            Fast::Mov2I { dh, dl, v } => {
                self.regs[(dl & 15) as usize] = v;
                self.regs[(dh & 15) as usize] = ((v as i32) >> 31) as u32;
            }
            Fast::Un { d, s, k } => {
                let x = self.regs[(s & 15) as usize];
                self.regs[(d & 15) as usize] = match k {
                    0 => !x,
                    1 => x.wrapping_neg(),
                    2 => (x as i32).wrapping_abs() as u32,
                    3 => x.swap_bytes(),
                    4 => x.leading_zeros(),
                    // sat16 (s) / (u): a signed value into -32768..32767 / 0..65535
                    5 => (x as i32).clamp(-32768, 32767) as u32,
                    _ => (x as i32).clamp(0, 65535) as u32,
                };
            }
            Fast::Div { d, a, b, k } => {
                let (a, b) = (self.regs[(a & 15) as usize], self.regs[(b & 15) as usize]);
                self.regs[(d & 15) as usize] = if b == 0 {
                    0
                } else {
                    match k {
                        0 => (a as i32).wrapping_div(b as i32) as u32,
                        1 => a / b,
                        2 => (a as i32).wrapping_rem(b as i32) as u32,
                        _ => a % b,
                    }
                };
            }
            Fast::LdPre { d, b, i, w, signed, inc } => {
                let b = (b & 15) as usize;
                let inc = if i < 16 { self.regs[i as usize] } else { inc };
                self.regs[b] = self.regs[b].wrapping_add(inc);
                self.regs[(d & 15) as usize] = ld_w(bus, w, signed, self.regs[b]);
            }
            Fast::StPre { s, b, i, w, inc } => {
                let b = (b & 15) as usize;
                let inc = if i < 16 { self.regs[i as usize] } else { inc };
                self.regs[b] = self.regs[b].wrapping_add(inc);
                st_w(bus, w, self.regs[b], self.regs[(s & 15) as usize]);
            }
            Fast::LdPostR { d, b, i, w, signed } => {
                let b = (b & 15) as usize;
                let (a, inc) = (self.regs[b], self.regs[(i & 15) as usize]);
                self.regs[(d & 15) as usize] = ld_w(bus, w, signed, a);
                self.regs[b] = self.regs[b].wrapping_add(inc);
            }
            Fast::StPostR { s, b, i, w } => {
                let b = (b & 15) as usize;
                let (a, inc) = (self.regs[b], self.regs[(i & 15) as usize]);
                st_w(bus, w, a, self.regs[(s & 15) as usize]);
                self.regs[b] = self.regs[b].wrapping_add(inc);
            }
            Fast::LdPair { p, b, off } => {
                // (the address before either load: a pair that holds the base overwrites it)
                let a = match b { Base::Reg(r) => self.regs[(r & 15) as usize], Base::Sp => self.sp }.wrapping_add(off);
                let lo = 2 * (p & 7) as usize;
                self.regs[lo] = bus.read32(a);
                self.regs[lo + 1] = bus.read32(a.wrapping_add(4));
            }
            Fast::StPair { p, b, off } => {
                let a = match b { Base::Reg(r) => self.regs[(r & 15) as usize], Base::Sp => self.sp }.wrapping_add(off);
                let lo = 2 * (p & 7) as usize;
                bus.write32(a, self.regs[lo]);
                bus.write32(a.wrapping_add(4), self.regs[lo + 1]);
            }
            _ => unreachable!("exec_rare: a hot op"),
        }
        None
    }

    /// Semantics keyed by the printed shape (`insn.entry.syntax`), with the
    /// operands read from the solver-verified slots in printed order. This
    /// is the fallback for every class without a hand-verified arm: a
    /// shape handled here executes for all its length/encoding variants at
    /// once. `-#i` operands carry their sign in the slot value, and the
    /// trailing ` #` (flag-setting variants) is ignored.
    ///
    /// Returns Ok(None) when the shape is unknown.
    fn exec_syntax<B: Bus>(
        &mut self,
        bus: &mut B,
        insn: &Instruction,
        next_pc: u32,
    ) -> Result<Option<Option<u32>>, CoreError> {
        let s = if self.cur_syntax.is_empty() { canon_syntax(insn.entry) } else { self.cur_syntax };
        let v = |cpu: &Self, i: usize| -> Result<u32, CoreError> { Ok(cpu.slot(insn, i)? as u32) };
        let r = |cpu: &Self, i: usize| -> Result<usize, CoreError> { cpu.reg(insn, i) };
        let mut jump: Option<u32> = None;

        // ---- special registers to / from the stack (ISR wrappers, prologues, context
        // switches): first, as interrupt-heavy firmware runs them constantly (the SMK-37's
        // stock firmware: ~150 k interrupts a second). Push in printed order (first item at the
        // highest address), pop ascending assigning the printed list reversed.
        fn named_list(list: &'static str) -> Option<([&'static str; 10], usize)> {
            let inner = list.strip_prefix('{')?.strip_suffix('}')?;
            let (mut a, mut n) = ([""; 10], 0);
            for t in inner.split(", ") {
                if n == 10 || !matches!(t, "psr" | "rets" | "reti" | "retx" | "rete" | "sr4" | "icfg" | "sp" | "ssp" | "usp") {
                    return None;
                }
                a[n] = t;
                n += 1;
            }
            Some((a, n))
        }
        if let Some((list, n)) = s.strip_prefix("[--sp] = ").and_then(named_list) {
            if list[..n].contains(&"reti") {
                self.nest_ok = true;
            }
            for name in &list[..n] {
                let val = sreg_get_named(self, name);
                self.sp = self.sp.wrapping_sub(4);
                bus.write32(self.sp, val);
            }
            return Ok(Some(None));
        }
        if let Some((list, n)) = s.strip_suffix(" = [sp++]").and_then(named_list) {
            if list[..n].contains(&"reti") {
                self.nest_ok = false;
            }
            for name in list[..n].iter().rev() {
                let val = bus.read32(self.sp);
                self.sp = self.sp.wrapping_add(4);
                self.set_sreg_named(name, val);
            }
            return Ok(Some(None));
        }

        // ---- branches: if[s] (<cond>) goto #i --------------------------
        if (s.starts_with("if (") || s.starts_with("ifs (")) && s.ends_with(") goto #i") {
            let signed = s.starts_with("ifs");
            let cond = &s[s.find('(').unwrap() + 1..s.len() - ") goto #i".len()];
            let (lhs, op, rhs, nslots): (u32, &str, u32, usize) =
                if let Some(rest) = cond.strip_prefix("(R & ") {
                    // (R & x) OP #i : slots r, mask, cmp, off
                    let (mask, op_rhs) = split2(rest, ") ").ok_or(CoreError::MissingSlot { name: insn.entry.name, pc: self.pc, slot: 9 })?;
                    let m = if mask == "R" { self.regs[r(self, 1)?] } else { v(self, 1)? };
                    let op = op_rhs.split(' ').next().unwrap_or("");
                    (self.regs[r(self, 0)?] & m, op, v(self, 2)?, 4)
                } else if let Some(rest) = cond.strip_prefix("--R ") {
                    let rr = r(self, 0)?;
                    self.regs[rr] = self.regs[rr].wrapping_sub(1);
                    let op = rest.split(' ').next().unwrap_or("");
                    (self.regs[rr], op, v(self, 1)?, 3)
                } else if let Some(rest) = cond.strip_prefix("R ") {
                    let mut it = rest.splitn(2, ' ');
                    let op = it.next().unwrap_or("");
                    let rhs_txt = it.next().unwrap_or("");
                    let rhs = if rhs_txt == "R" { self.regs[r(self, 1)?] } else { v(self, 1)? };
                    (self.regs[r(self, 0)?], op, rhs, 3)
                } else {
                    return Ok(None);
                };
            let taken = match (op, signed) {
                ("==", _) => lhs == rhs,
                ("!=", _) => lhs != rhs,
                ("<", false) => lhs < rhs,
                (">", false) => lhs > rhs,
                ("<=", false) => lhs <= rhs,
                (">=", false) => lhs >= rhs,
                ("<", true) => (lhs as i32) < (rhs as i32),
                (">", true) => (lhs as i32) > (rhs as i32),
                ("<=", true) => (lhs as i32) <= (rhs as i32),
                (">=", true) => (lhs as i32) >= (rhs as i32),
                _ => return Ok(None),
            };
            let off = self.slot(insn, nslots - 1)?;
            if taken {
                jump = Some((next_pc as i64 + off) as u32);
            }
            return Ok(Some(jump));
        }

        // ---- memory helpers ----------------------------------------------
        fn ld<B: Bus>(bus: &mut B, w: u8, signed: bool, a: u32) -> u32 {
            match (w, signed) {
                (1, false) => bus.read8(a) as u32,
                (1, true) => bus.read8(a) as i8 as i32 as u32,
                (2, false) => bus.read16(a) as u32,
                (2, true) => bus.read16(a) as i16 as i32 as u32,
                _ => bus.read32(a),
            }
        }
        fn st<B: Bus>(bus: &mut B, w: u8, a: u32, val: u32) {
            match w {
                1 => bus.write8(a, val as u8),
                2 => bus.write16(a, val as u16),
                _ => bus.write32(a, val),
            }
        }
        // width prefix of a memory operand: "b[" / "h[" / "["
        fn width_of(t: &str) -> Option<(u8, &str)> {
            if let Some(x) = t.strip_prefix("b[") { Some((1, x)) }
            else if let Some(x) = t.strip_prefix("h[") { Some((2, x)) }
            else if let Some(x) = t.strip_prefix("[") { Some((4, x)) }
            else { None }
        }

        // ---- loads: R = <mem> [(u)|(s)] ----------------------------------
        if let Some(rhs) = s.strip_prefix("R = ") {
            let (rhs, signed) = if let Some(x) = rhs.strip_suffix(" (s)") { (x, true) }
                else if let Some(x) = rhs.strip_suffix(" (u)") { (x, false) } else { (rhs, false) };
            if let Some((w, inner)) = width_of(rhs) {
                let d = r(self, 0)?;
                let inner = inner.strip_suffix(']').ok_or(CoreError::MissingSlot { name: insn.entry.name, pc: self.pc, slot: 9 })?;
                let (addr, post): (u32, Option<(usize, u32)>) = match inner {
                    "R+#i" => (self.regs[r(self, 1)?].wrapping_add(v(self, 2)?), None),
                    "sp+#i" => (self.sp.wrapping_add(v(self, 1)?), None),
                    "sp" => (self.sp, None),
                    "R+R" => (self.regs[r(self, 1)?].wrapping_add(self.regs[r(self, 2)?]), None),
                    "R+R<<#i" => (self.regs[r(self, 1)?].wrapping_add(self.regs[r(self, 2)?] << (v(self, 3)? & 31)), None),
                    "R++=#i" => { let b = r(self, 1)?; (self.regs[b], Some((b, v(self, 2)?))) }
                    "R++=R" => { let b = r(self, 1)?; (self.regs[b], Some((b, self.regs[r(self, 2)?]))) }
                    "++R=#i" => { let b = r(self, 1)?; self.regs[b] = self.regs[b].wrapping_add(v(self, 2)?); (self.regs[b], None) }
                    "++R=R" => { let b = r(self, 1)?; self.regs[b] = self.regs[b].wrapping_add(self.regs[r(self, 2)?]); (self.regs[b], None) }
                    _ => return Ok(None),
                };
                self.regs[d] = ld(bus, w, signed, addr);
                if let Some((b, inc)) = post {
                    self.regs[b] = self.regs[b].wrapping_add(inc);
                }
                return Ok(Some(None));
            }
        }

        // ---- stores / RMW: <mem> op= x -------------------------------------
        if let Some((w, inner)) = width_of(s) {
            if let Some((addr_txt, rest)) = split2(inner, "] ") {
                let (op, src) = rest.split_once(' ').ok_or(CoreError::MissingSlot { name: insn.entry.name, pc: self.pc, slot: 9 })?;
                // operand slots: address operands first, then the source
                let mut n = 0usize;
                // unknown address forms (block moves `[R+]`, …) fall through
                // to the dedicated handlers below
                let known = matches!(addr_txt, "R+#i" | "sp+#i" | "sp" | "R+R" | "R+R<<#i" | "R++=#i" | "R++=R" | "++R=#i" | "++R=R" | "--sp");
                let known = known && matches!(src, "R" | "#i" | "#h" | "rets");
                let (addr, post): (u32, Option<(usize, u32)>) = if !known { (0, None) } else { match addr_txt {
                    "R+#i" => { n = 2; (self.regs[r(self, 0)?].wrapping_add(v(self, 1)?), None) }
                    "sp+#i" => { n = 1; (self.sp.wrapping_add(v(self, 0)?), None) }
                    "sp" => (self.sp, None),
                    "R+R" => { n = 2; (self.regs[r(self, 0)?].wrapping_add(self.regs[r(self, 1)?]), None) }
                    "R+R<<#i" => { n = 3; (self.regs[r(self, 0)?].wrapping_add(self.regs[r(self, 1)?] << (v(self, 2)? & 31)), None) }
                    "R++=#i" => { n = 2; let b = r(self, 0)?; (self.regs[b], Some((b, v(self, 1)?))) }
                    "R++=R" => { n = 2; let b = r(self, 0)?; (self.regs[b], Some((b, self.regs[r(self, 1)?]))) }
                    "++R=#i" => { n = 2; let b = r(self, 0)?; self.regs[b] = self.regs[b].wrapping_add(v(self, 1)?); (self.regs[b], None) }
                    "++R=R" => { n = 2; let b = r(self, 0)?; self.regs[b] = self.regs[b].wrapping_add(self.regs[r(self, 1)?]); (self.regs[b], None) }
                    _ => { self.sp = self.sp.wrapping_sub(4); (self.sp, None) }
                } };
                // shifted-mask sources (`#i << R`, `~(#i << R)`, `~R`, `R << #i`)
                // are handled by the dedicated arms further down
                let val = if !known { None } else { match src {
                    "R" => Some(self.regs[r(self, n)?]),
                    "#i" | "#h" => Some(v(self, n)?),
                    "rets" => Some(self.rets),
                    _ => None,
                } };
                if let Some(val) = val {
                    let newv = match op {
                        "=" => val,
                        "|=" => ld(bus, w, false, addr) | val,
                        "&=" => ld(bus, w, false, addr) & val,
                        "^=" => ld(bus, w, false, addr) ^ val,
                        "+=" => ld(bus, w, false, addr).wrapping_add(val),
                        "-=" => ld(bus, w, false, addr).wrapping_sub(val),
                        _ => return Ok(None),
                    };
                    st(bus, w, addr, newv);
                    if let Some((b, inc)) = post {
                        self.regs[b] = self.regs[b].wrapping_add(inc);
                    }
                    return Ok(Some(None));
                }
            }
        }

        // ---- register pairs: rH_rL = d[...] / d[...] = rH_rL ---------------
        // the pair names are literal in the syntax (r1_r0, r3_r2, ...); the
        // low register takes the lower address
        let pair_of = |t: &str| -> Option<(usize, usize)> {
            let (h, l) = split2(t.strip_prefix('r')?, "_r")?;
            Some((h.parse().ok()?, l.parse().ok()?))
        };
        if let Some((lhs, rhs)) = split2(s, " = ") {
            let (pair, mem, is_load) = if let Some(p) = pair_of(lhs) {
                (Some(p), rhs, true)
            } else if let Some(p) = pair_of(rhs) {
                (Some(p), lhs, false)
            } else {
                (None, "", false)
            };
            if let (Some((hi, lo)), Some(inner)) = (pair, mem.strip_prefix("d[").and_then(|x| x.strip_suffix(']'))) {
                let (addr, post): (u32, Option<(usize, u32)>) = match inner {
                    "R+#i" => (self.regs[r(self, 0)?].wrapping_add(v(self, 1)?), None),
                    "sp+#i" => (self.sp.wrapping_add(v(self, 0)?), None),
                    "sp" => (self.sp, None),
                    "++R=#i" => {
                        let b = r(self, 0)?;
                        self.regs[b] = self.regs[b].wrapping_add(v(self, 1)?);
                        (self.regs[b], None)
                    }
                    "R++=#i" => { let b = r(self, 0)?; (self.regs[b], Some((b, v(self, 1)?))) }
                    _ => return Ok(None),
                };
                if is_load {
                    self.regs[lo] = bus.read32(addr);
                    self.regs[hi] = bus.read32(addr.wrapping_add(4));
                } else {
                    bus.write32(addr, self.regs[lo]);
                    bus.write32(addr.wrapping_add(4), self.regs[hi]);
                }
                if let Some((b, inc)) = post {
                    self.regs[b] = self.regs[b].wrapping_add(inc);
                }
                return Ok(Some(None));
            }
        }

        // ---- register-pair moves / 64-bit arithmetic ------------------------
        if let Some((lhs, rhs)) = split2(s, " = ") {
            if let Some((dh, dl)) = pair_of(lhs) {
                if let Some((sh_, sl)) = pair_of(rhs) {
                    let (a, b) = (self.regs[sh_], self.regs[sl]);
                    self.regs[dh] = a;
                    self.regs[dl] = b;
                    return Ok(Some(None));
                }
                if rhs == "#i" {
                    // printed operands are rH, rL, #i -> the immediate is slot 2
                    let imm = self.slot(insn, 2)?;
                    self.regs[dl] = imm as u32;
                    self.regs[dh] = (imm >> 32) as u32;
                    return Ok(Some(None));
                }
                if rhs == "R * R (s)" || rhs == "R * R (u)" {
                    let a = self.regs[r(self, 0)?];
                    let b = self.regs[r(self, 1)?];
                    let p = if rhs.ends_with("(s)") { (a as i32 as i64).wrapping_mul(b as i32 as i64) as u64 } else { (a as u64) * (b as u64) };
                    self.regs[dl] = p as u32;
                    self.regs[dh] = (p >> 32) as u32;
                    return Ok(Some(None));
                }
            }
        }
        if let Some((lhs, rhs)) = split2(s, " += ") {
            if let Some((dh, dl)) = pair_of(lhs) {
                if rhs == "R * R (s)" || rhs == "R * R (u)" {
                    let a = self.regs[r(self, 0)?];
                    let b = self.regs[r(self, 1)?];
                    let p = if rhs.ends_with("(s)") { (a as i32 as i64).wrapping_mul(b as i32 as i64) as u64 } else { (a as u64) * (b as u64) };
                    let acc = ((self.regs[dh] as u64) << 32 | self.regs[dl] as u64).wrapping_add(p);
                    self.regs[dl] = acc as u32;
                    self.regs[dh] = (acc >> 32) as u32;
                    return Ok(Some(None));
                }
            }
        }
        for (op, left) in [(" >>= #i", true), (" <<= #i", false)] {
            if let Some(lhs) = s.strip_suffix(op) {
                if let Some((dh, dl)) = pair_of(lhs) {
                    // 64-bit pair shift (`>>=` taken as logical: hardware-probe item)
                    let n = v(self, 0)? & 63;
                    let x = (self.regs[dh] as u64) << 32 | self.regs[dl] as u64;
                    let y = if n == 0 { x } else if left { x << n } else { x >> n };
                    self.regs[dl] = y as u32;
                    self.regs[dh] = (y >> 32) as u32;
                    return Ok(Some(None));
                }
            }
        }

        // ---- multi-register block moves with a register base -----------
        // `{rA, rB, ...} = [rN+]` / `[rN+] = {...}` (eb 00 / eb 20): base =
        // bits(0,3), 16-bit register mask at bits(16,31); the lowest register
        // takes the lowest address (same convention as the sp block pops).
        // `[rN++]` (eb 10 / 30) additionally advances the base past the block.
        // `[-rN]` / `[--rN]` (eb 40..70): the block ends at the base, `--` moves
        // the base down to its start (#59: JIANT's `40 eb 06 00`, {r1, r2} = [-r0]).
        // Base update for the plain `[rN+]` / `[-rN]` forms assumed absent (probe item).
        if let (4, 0xeb, Some((store, wb, down))) = (insn.entry.len, (insn.raw >> 8) & 0xff, block_form(s)) {
            let raw = insn.raw;
            let base = (raw & 0xf) as usize;
            let regs: Vec<usize> = (0..16usize).filter(|&i| (raw >> (16 + i)) & 1 != 0).collect();
            let start = if down { self.regs[base].wrapping_sub(4 * regs.len() as u32) } else { self.regs[base] };
            let mut addr = start;
            for &i in &regs {
                if store {
                    bus.write32(addr, self.regs[i]);
                } else {
                    self.regs[i] = bus.read32(addr);
                }
                addr = addr.wrapping_add(4);
            }
            if wb {
                self.regs[base] = if down { start } else { addr };
            }
            return Ok(Some(None));
        }

        // ---- rotate right `R = R <> #i` (c4 e1): d = bits(28,31), s = bits(20,23),
        // n = bits(16,19) | bit24 << 4 (corpus: `c4 e1 08 01` r0 = r0 <> 24, `29 71` r7 =
        // r2 <> 25). The stock printf-style switch `(c - 'h') <> 1` relies on odd values
        // rotating into the top bit. (The solver's slots overfit the few samples.)
        if s == "R = R <> #i" && insn.entry.len == 4 {
            let raw = insn.raw;
            let d = ((raw >> 28) & 0xf) as usize;
            let src = self.regs[((raw >> 20) & 0xf) as usize];
            let n = (((raw >> 16) & 0xf) | (((raw >> 24) & 1) << 4)) as u32;
            self.regs[d] = src.rotate_right(n);
            return Ok(Some(None));
        }

        // ---- push/pop of named special registers ----------------------------
        // `[--sp] = {psr, rets, reti}` / `{psr, rets, reti} = [sp++]` (interrupt
        // prologues, context switches): same convention as the register
        // blocks — push in printed order (first item at the highest
        // address), pop ascending assigning the printed list reversed.
        // ---- table branches, plain block push/pop, cache ops ----------------
        match s {
            "tbb [R]" | "tbh [R]" => {
                // jump table right after the instruction: pc = next_pc +
                // 2 * table[R] (tbb: byte entries, R = index; tbh: halfword
                // entries, R = 2*index) — matches the vendor annotations,
                // e.g. tbh at 0x12dc, entry 0x1c8 -> 0x166e, the switch default.
                let idx = self.regs[r(self, 0)?];
                let e = if s.starts_with("tbb") { bus.read8(next_pc.wrapping_add(idx)) as u32 } else { bus.read16(next_pc.wrapping_add(idx)) as u32 };
                return Ok(Some(Some(next_pc.wrapping_add(2 * e))));
            }
            "[--sp] = {R*2}" | "{R*2} = [sp++]" | "[--sp] = {R*1}" | "{R*1} = [sp++]" => {
                // block push/pop without rets: 2-byte `6n 04` / `4n 04` with
                // the usual [max(n,3) .. min(n,4)] list; 4-byte `d8 e8` /
                // `d4 e8` carry a 16-bit register mask at bits 16-31.
                let raw = insn.raw;
                let regs: Vec<usize> = if insn.entry.len == 2 {
                    let n = (raw & 0xf) as usize;
                    (n.min(4)..=n.max(3)).rev().collect()
                } else {
                    (0..16usize).rev().filter(|&i| (raw >> (16 + i)) & 1 != 0).collect()
                };
                if s.starts_with("[--sp]") {
                    for &i in &regs {
                        self.sp = self.sp.wrapping_sub(4);
                        bus.write32(self.sp, self.regs[i]);
                    }
                } else {
                    for &i in regs.iter().rev() {
                        self.regs[i] = bus.read32(self.sp);
                        self.sp = self.sp.wrapping_add(4);
                    }
                }
                return Ok(Some(None));
            }
            "ssync" | "btbclr" | "iflush [R]" | "flush [R]" | "flushinv [R]" => return Ok(Some(None)),
            "cc = #i" => { self.cc = v(self, 0)? != 0; return Ok(Some(None)); }
            "callns R" => { self.rets = next_pc; return Ok(Some(Some(self.regs[r(self, 0)?]))); }
            "rti" => {
                // back to the mode the interrupt came from; with none in progress, user mode
                let to_user = match self.irq_levels.pop() {
                    Some(_) => self.user_irq >> self.irq_levels.len().min(31) & 1 != 0,
                    None => true,
                };
                if to_user && !self.user {
                    self.ssp = self.sp;
                    self.sp = self.usp;
                    self.user = true;
                }
                // back in an outer ISR: it was interrupted with reti on its stack
                self.nest_ok = !self.irq_levels.is_empty();
                return Ok(Some(Some(self.sregs("reti"))));
            }
            "R = sextra(R, p:#i, l:#i)" => {
                let d = r(self, 0)?;
                let src = self.regs[r(self, 1)?];
                let p = v(self, 2)? & 31;
                let l = v(self, 3)? & 31;
                let x = if l == 0 { src >> p } else { ((src >> p) << (32 - l)) as i32 as u32 >> (32 - l) };
                self.regs[d] = if l == 0 { x } else { (((src >> p) << (32 - l)) as i32 >> (32 - l)) as u32 };
                let _ = x;
                return Ok(Some(None));
            }
            "R.l = #i" | "R.h = #i" => {
                let d = r(self, 0)?;
                let imm = v(self, 1)? & 0xffff;
                self.regs[d] = if s.starts_with("R.l") { (self.regs[d] & 0xffff_0000) | imm } else { (self.regs[d] & 0xffff) | (imm << 16) };
                return Ok(Some(None));
            }
            "R <<= R" | "R >>= R" | "R >>>= R" | "R <<= #i" | "R >>= #i" | "R >>>= #i" => {
                let d = r(self, 0)?;
                let n = if s.ends_with('R') { self.regs[r(self, 1)?] & 31 } else { v(self, 1)? };
                let kind = s[2..].split('=').next().unwrap_or("<<");
                let x = self.regs[d];
                self.regs[d] = match kind {
                    "<<" => if n >= 32 { 0 } else { x << n },
                    ">>" => if n >= 32 { 0 } else { x >> n },
                    _ => if n >= 32 { ((x as i32) >> 31) as u32 } else { ((x as i32) >> n) as u32 },
                };
                return Ok(Some(None));
            }
            "R = R & ~R" => { let d = r(self, 0)?; self.regs[d] = self.regs[r(self, 1)?] & !self.regs[r(self, 2)?]; return Ok(Some(None)); }
            "R = R & ~(#i << R)" | "R = R | (#i << R)" => {
                let d = r(self, 0)?;
                let a = self.regs[r(self, 1)?];
                let m = v(self, 2)?.wrapping_shl(self.regs[r(self, 3)?] & 31);
                self.regs[d] = if s.contains('~') { a & !m } else { a | m };
                return Ok(Some(None));
            }
            "[R+#i] &= ~(#i << R)" | "[R+#i] |= #i << R" | "[R+#i] &= ~R" | "[R+#i] |= R << #i" => {
                let addr = self.regs[r(self, 0)?].wrapping_add(v(self, 1)?);
                let m = match s {
                    "[R+#i] &= ~R" => self.regs[r(self, 2)?],
                    "[R+#i] |= R << #i" => self.regs[r(self, 2)?].wrapping_shl(v(self, 3)? & 31),
                    _ => v(self, 2)?.wrapping_shl(self.regs[r(self, 3)?] & 31),
                };
                let cur = bus.read32(addr);
                bus.write32(addr, if has(s, "&=") { cur & !m } else { cur | m });
                return Ok(Some(None));
            }
            _ => {}
        }

        // ---- special registers ------------------------------------------------
        // psr's low bits are the arithmetic flags (`Cpu::flags`, set by add / subtract,
        // read by `+ c` / `- !c`); icfg/reti/retx/rete are plain storage.
        let sreg_get = |cpu: &Self, n: &str| -> Option<u32> {
            Some(match n {
                "rets" => cpu.rets,
                "sp" => cpu.sp,
                "ssp" => cpu.get_ssp(),
                "usp" => cpu.get_usp(),
                "psr" | "icfg" | "reti" | "retx" | "rete" | "sspn" | "cnum" => cpu.sregs(n),
                _ => return None,
            })
        };
        if let Some((lhs, rhs)) = split2(s, " = ") {
            if lhs == "R" && !rhs.contains(' ') && rhs != "R" && !rhs.starts_with('#') {
                if let Some(val) = sreg_get(self, rhs) {
                    let d = r(self, 0)?;
                    self.regs[d] = val;
                    return Ok(Some(None));
                }
            }
            if lhs != "R" && !lhs.contains(['[', '{', '.', '_']) && (rhs == "R" || rhs == "#i" || rhs == "#h" || sreg_get(self, rhs).is_some()) {
                let val = if rhs == "R" { self.regs[r(self, 0)?] }
                    else if rhs.starts_with('#') && insn.entry.len == 6 && (insn.raw >> 8) & 0xff == 0xff {
                        // 6-byte `<sreg> = #imm32` (`e0..ef ff <imm32-le>`, like `c0 ff` R = #i)
                        (insn.raw >> 16) as u32
                    }
                    else if rhs.starts_with('#') { v(self, 0)? }
                    else { sreg_get(self, rhs).unwrap() };
                match lhs {
                    "rets" => self.rets = val,
                    "sp" => self.sp = val,
                    "ssp" => self.set_ssp(val),
                    "usp" => self.set_usp(val),
                    "psr" | "icfg" | "reti" | "retx" | "rete" | "sspn" => self.set_sreg(lhs, val),
                    _ => return Ok(None),
                }
                return Ok(Some(None));
            }
        }

        // ---- bit-field / min-max / misc unary -------------------------------
        if let Some(rest) = s.strip_prefix("R <= insert(R, p:#i, l:#i)") {
            if rest.is_empty() {
                let d = r(self, 0)?;
                let src = self.regs[r(self, 1)?];
                let p = v(self, 2)? & 31;
                let l = v(self, 3)? & 31;
                let mask = if l == 0 { 0xffff_ffffu32 } else { (1u32 << l).wrapping_sub(1) };
                self.regs[d] = (self.regs[d] & !(mask << p)) | ((src & mask) << p);
                return Ok(Some(None));
            }
        }
        if s == "R = uextra(R, p:#i, l:#i)" {
            let d = r(self, 0)?;
            let src = self.regs[r(self, 1)?];
            let p = v(self, 2)? & 31;
            let l = v(self, 3)? & 31;
            let mask = if l == 0 { 0xffff_ffffu32 } else { (1u32 << l).wrapping_sub(1) };
            self.regs[d] = (src >> p) & mask;
            return Ok(Some(None));
        }
        match s {
            "R = smin(R, R)" | "R = smax(R, R)" | "R = umin(R, R)" | "R = umax(R, R)" => {
                // e4 and its flag-setting twin f4 share one layout: d bits
                // 28-31, a bits 20-23, b bits 24-27 (the f4 forms have too few
                // corpus samples for the slot solver)
                let raw = insn.raw;
                let (d, a, b) = if insn.entry.len == 4 && matches!((raw >> 8) & 0xff, 0xe4 | 0xf4) {
                    (((raw >> 28) & 0xf) as usize, self.regs[((raw >> 20) & 0xf) as usize], self.regs[((raw >> 24) & 0xf) as usize])
                } else {
                    (r(self, 0)?, self.regs[r(self, 1)?], self.regs[r(self, 2)?])
                };
                self.regs[d] = match &s[4..8] {
                    "smin" => (a as i32).min(b as i32) as u32,
                    "smax" => (a as i32).max(b as i32) as u32,
                    "umin" => a.min(b),
                    _ => a.max(b),
                };
                return Ok(Some(None));
            }
            "R = abs(R)" => { let d = r(self, 0)?; self.regs[d] = (self.regs[r(self, 1)?] as i32).wrapping_abs() as u32; return Ok(Some(None)); }
            "R = clz(R)" => { let d = r(self, 0)?; self.regs[d] = self.regs[r(self, 1)?].leading_zeros(); return Ok(Some(None)); }
            "R = rev8(R)" => { let d = r(self, 0)?; self.regs[d] = self.regs[r(self, 1)?].swap_bytes(); return Ok(Some(None)); }
            "R = R + R + c" => {
                let d = r(self, 0)?;
                let (v, f) = add_flags(self.regs[r(self, 1)?], self.regs[r(self, 2)?], (self.flags & F_C != 0) as u32);
                self.regs[d] = v;
                self.flags = f;
                return Ok(Some(None));
            }
            "R = R - R - !c" => {
                let d = r(self, 0)?;
                let (v, f) = add_flags(self.regs[r(self, 1)?], !self.regs[r(self, 2)?], (self.flags & F_C != 0) as u32);
                self.regs[d] = v;
                self.flags = f;
                return Ok(Some(None));
            }
            "R = R + R (ssat)" => {
                let d = r(self, 0)?;
                self.regs[d] = (self.regs[r(self, 1)?] as i32).saturating_add(self.regs[r(self, 2)?] as i32) as u32;
                return Ok(Some(None));
            }
            _ => {}
        }

        // ---- everything else ------------------------------------------------
        let sh = |x: u32, n: u32, kind: &str| -> u32 {
            match kind {
                "<<" => if n >= 32 { 0 } else { x << n },
                "<<<" => qasl(x, n),
                ">>" => if n >= 32 { 0 } else { x >> n },
                _ => if n >= 32 { ((x as i32) >> 31) as u32 } else { ((x as i32) >> n) as u32 },
            }
        };
        match s {
            "nop" | "csync" | "pfetch [R]"
            | "sti R" | "cli R" => {}
            // the core halts until an interrupt: the bus may let time pass
            "idle" => bus.idle(self.core_id),
            "sti" => self.ie = true,
            "cli" => self.ie = false,
            "R = R" => { let d = r(self, 0)?; self.regs[d] = self.regs[r(self, 1)?]; }
            "R = #i" | "R = #h" => { let d = r(self, 0)?; self.regs[d] = v(self, 1)?; }
            "R = cnum" => { let d = r(self, 0)?; self.regs[d] = self.core_id; }
            "R = sp" => { let d = r(self, 0)?; self.regs[d] = self.sp; }
            "R = sp + #i" => { let d = r(self, 0)?; self.regs[d] = self.sp.wrapping_add(v(self, 1)?); }
            "sp += #i" => {
                // 4-byte `f0 e8 <imm>`: sext13(bits(16,28)) (corpus: `f0 e8 2c 1d` = -724)
                let imm = if insn.entry.len == 4 && insn.raw & 0xffff == 0xe8f0 {
                    let f = ((insn.raw >> 16) & 0x1fff) as u32;
                    if f & 0x1000 != 0 { f | !0x1fff } else { f }
                } else {
                    v(self, 0)?
                };
                self.sp = self.sp.wrapping_add(imm);
            }
            "sp = R" => self.sp = self.regs[r(self, 0)?],
            "R = R + #i" | "R = R + #h" | "R = R - #i" | "R = R & #h" | "R = R | #h"
            | "R = R ^ #h" | "R = R * #h" | "R = R & #i" | "R = R | #i" | "R = R ^ #i" => {
                let d = r(self, 0)?;
                let a = self.regs[r(self, 1)?];
                let b = v(self, 2)?;
                self.regs[d] = match &s[6..7] {
                    "+" => self.alu_f(Op::Add, a, b),
                    "-" => self.alu_f(Op::Sub, a, b),
                    "&" => a & b,
                    "|" => a | b,
                    "^" => a ^ b,
                    _ => a.wrapping_mul(b),
                };
            }
            "R = #h - R" | "R = #i - R" => {
                let d = r(self, 0)?;
                let (a, b) = (v(self, 1)?, self.regs[r(self, 2)?]);
                self.regs[d] = self.alu_f(Op::Sub, a, b);
            }
            "R = R + R" | "R = R - R" | "R = R & R" | "R = R | R" | "R = R ^ R" | "R = R * R"
            | "R = R / R (s)" | "R = R / R (u)" | "R = R % R (s)" | "R = R % R (u)" => {
                let d = r(self, 0)?;
                let a = self.regs[r(self, 1)?];
                let b = self.regs[r(self, 2)?];
                self.regs[d] = match &s[6..7] {
                    "+" => self.alu_f(Op::Add, a, b),
                    "-" => self.alu_f(Op::Sub, a, b),
                    "&" => a & b,
                    "|" => a | b,
                    "^" => a ^ b,
                    "*" => a.wrapping_mul(b),
                    "/" if s.ends_with("(s)") => if b == 0 { 0 } else { (a as i32).wrapping_div(b as i32) as u32 },
                    "/" => if b == 0 { 0 } else { a / b },
                    _ if s.ends_with("(s)") => if b == 0 { 0 } else { (a as i32).wrapping_rem(b as i32) as u32 },
                    _ => if b == 0 { 0 } else { a % b },
                };
            }
            "R += #i" | "R -= #i" | "R |= #i" | "R &= #i" | "R ^= #i" | "R *= #i"
            | "R += R" | "R -= R" | "R |= R" | "R &= R" | "R ^= R" | "R *= R" => {
                let d = r(self, 0)?;
                let b = if s.ends_with('R') { self.regs[r(self, 1)?] } else { v(self, 1)? };
                let a = self.regs[d];
                self.regs[d] = match &s[2..3] {
                    "+" => self.alu_f(Op::Add, a, b),
                    "-" => self.alu_f(Op::Sub, a, b),
                    "|" => a | b,
                    "&" => a & b,
                    "^" => a ^ b,
                    _ => a.wrapping_mul(b),
                };
            }
            "R = R << #i" | "R = R >> #i" | "R = R >>> #i" | "R = R <<< #i"
            | "R = R << R" | "R = R >> R" | "R = R >>> R" => {
                let d = r(self, 0)?;
                let a = self.regs[r(self, 1)?];
                // an immediate count is 1..32, 32 encoded as 0 (the vendor assembler's range;
                // `c0 e1 d0 40` is r4 = r13 << 32, work/probe/mm.S)
                let n = if s.ends_with('R') { self.regs[r(self, 2)?] & 31 } else { match v(self, 2)? { 0 => 32, n => n } };
                // the operator starts at index 6 of "R = R << #i"; index 8 was
                // the space, which made every 4-byte `rX = rY << n` an
                // arithmetic right shift (Felucca's Huffman LUT came out empty)
                let kind = s[6..].split(' ').next().unwrap_or("<<");
                self.regs[d] = sh(a, n, kind);
            }
            // `SD 1a` with bit 3: rD <<<= rS, d = bits(0,2), s = bits(4,7) (the `<<=` layout; the
            // vendor assembler, work/probe/fam.S)
            "R <<<= R" if insn.entry.len == 2 => {
                let (d, src) = ((insn.raw & 7) as usize, ((insn.raw >> 4) & 0xf) as usize);
                self.regs[d] = qasl(self.regs[d], self.regs[src] & 31);
            }
            "R = -R" => { let d = r(self, 0)?; self.regs[d] = self.regs[r(self, 1)?].wrapping_neg(); }
            "R = ~R" => { let d = r(self, 0)?; self.regs[d] = !self.regs[r(self, 1)?]; }
            "R = R.b0 (u)" | "R = R.b0 (s)" | "R = R.l (u)" | "R = R.l (s)" | "R = R.h (u)"
            | "R = R.h (s)" | "R = R.b1 (u)" | "R = R.b2 (u)" | "R = R.b3 (u)" => {
                let d = r(self, 0)?;
                let x = self.regs[r(self, 1)?];
                let signed = s.ends_with("(s)");
                // "R = R.b0 (u)": the part name starts at index 6
                self.regs[d] = match &s[6..8] {
                    "b0" => if signed { x as u8 as i8 as i32 as u32 } else { x & 0xff },
                    "b1" => (x >> 8) & 0xff,
                    "b2" => (x >> 16) & 0xff,
                    "b3" => x >> 24,
                    "l " => if signed { x as u16 as i16 as i32 as u32 } else { x & 0xffff },
                    _ => if signed { ((x >> 16) as u16 as i16 as i32) as u32 } else { x >> 16 },
                };
            }
            "goto #i" => jump = Some((next_pc as i64 + self.slot(insn, 0)?) as u32),
            "goto R" => jump = Some(self.regs[r(self, 0)?]),
            "call #i" => { self.rets = next_pc; jump = Some((next_pc as i64 + self.slot(insn, 0)?) as u32); }
            "call R" => { self.rets = next_pc; jump = Some(self.regs[r(self, 0)?]); }
            "rts" => jump = Some(self.rets),
            "pc = [sp++]" => { jump = Some(bus.read32(self.sp)); self.sp = self.sp.wrapping_add(4); }
            "rets = [sp++]" => { self.rets = bus.read32(self.sp); self.sp = self.sp.wrapping_add(4); }
            _ => return Ok(None),
        }
        Ok(Some(jump))
    }

    /// Address just past `units` instructions starting at `pc`, where a
    /// predicated `if` block counts as one unit including its then/else
    /// parts and a `rep` header includes its block (how the block-size
    /// nibbles of `if (...) {` count, verified on 2057 corpus blocks).
    fn block_end<B: Bus>(&self, bus: &mut B, mut pc: u32, units: u32) -> u32 {
        for _ in 0..units {
            let lo = bus.fetch32(pc) as u64;
            let hi = bus.fetch32(pc + 4) as u64;
            let Ok(insn) = decode_any((hi << 32) | lo, pc) else { return pc };
            pc += insn.entry.len as u32;
            if insn.entry.syntax.trim_end().ends_with('#') {
                // a dual-issue pair (`x #` + the next instruction) is one unit
                // (memset at 0x02042f0e: `{ r2 -= 1 #; b[r3++=1] = r1; goto }` = 2 units)
                let lo = bus.fetch32(pc) as u64;
                let hi = bus.fetch32(pc + 4) as u64;
                if let Ok(second) = decode_any((hi << 32) | lo, pc) {
                    pc += second.entry.len as u32;
                }
                continue;
            }
            let name = insn.entry.name;
            if insn.entry.len == 4 && name.starts_with("if") && !has(name, "goto") {
                if let Some(b) = decode_if_block(insn.raw) {
                    pc = self.block_end(bus, pc, b.then_units);
                    pc = self.block_end(bus, pc, b.else_units);
                }
            } else if name == "rep_i_r" || name == "rep_i_i" {
                pc += 2 * (((insn.raw >> 4) & 0xf) as u32 + 1);
            }
        }
        pc
    }

    fn slot(&self, insn: &Instruction, i: usize) -> Result<i64, CoreError> {
        let slots = insn.entry.slots;
        if i >= slots.len() {
            return Err(CoreError::MissingSlot {
                name: insn.entry.name,
                pc: self.pc,
                slot: i,
            });
        }
        Ok(slots[i].value(insn.raw))
    }

    fn blk(&self, insn: &Instruction, i: usize) -> Result<Vec<u8>, CoreError> {
        let slots = insn.entry.slots;
        if i >= slots.len() || slots[i].kind != SlotKind::Blk {
            return Err(CoreError::MissingSlot {
                name: insn.entry.name,
                pc: self.pc,
                slot: i,
            });
        }
        Ok(slots[i].block_regs(insn.raw))
    }

    fn reg(&self, insn: &Instruction, i: usize) -> Result<usize, CoreError> {
        Ok(self.slot(insn, i)? as usize & 0xf)
    }

    fn load(&self, insn: &Instruction, i: usize) -> Result<u32, CoreError> {
        let n = self.blk(insn, i)?;
        Ok(if n.is_empty() { 0 } else { self.regs[n[0] as usize] })
    }

    fn store_r(&mut self, insn: &Instruction, i: usize, v: u32) -> Result<(), CoreError> {
        let n = self.blk(insn, i)?;
        if !n.is_empty() {
            self.regs[n[0] as usize] = v;
        }
        Ok(())
    }

    fn branch(&self, insn: &Instruction, i: usize) -> Result<Option<u32>, CoreError> {
        let off = self.slot(insn, i)?; // Rel: mult * sext(field)
        Ok(Some((self.pc as i64 + insn.entry.len as i64 + off) as u32))
    }

    fn execute<B: Bus>(
        &mut self,
        bus: &mut B,
        insn: &Instruction,
        pre: &Pre,
        next_pc: u32,
    ) -> Result<(), CoreError> {
        let name = insn.entry.name;
        let mut jumped: Option<u32> = None;
        if trace_on() {
            eprintln!("[{}] c{} pc={:#010x} class={} len={} sp={:#x} r0-11={:x?} ",
                self.insn_count, self.core_id, self.pc, name, insn.entry.len, self.sp, &self.regs[..12]);
        }

        let alu4 = pre.alu4;
        let bittest = pre.bittest;
        #[allow(unused_variables)]
        let (n, other) = (name, name);
        if let Some(f) = pre.fast {
            jumped = self.exec_fast(bus, f, next_pc);
        } else { match pre.route {
            0 => {
                let t = bittest.unwrap();
                let mask = match t.mask {
                    Rhs::Reg(r) => self.regs[r],
                    Rhs::Imm(v) => v,
                    Rhs::Bit(r) => 1u32 << (self.regs[r] & 31),
                };
                if ((self.regs[t.reg] & mask) != 0) == t.ne {
                    jumped = Some((next_pc as i64 + t.off) as u32);
                }
            }
            1 => {
                let a = alu4.unwrap();
                let lhs = self.regs[a.src];
                let rhs = match a.rhs {
                    Rhs::Reg(r) => self.regs[r],
                    Rhs::Imm(v) => v,
                    Rhs::Bit(r) => 1u32 << (self.regs[r] & 31),
                };
                let apply = |x: u32| match a.op {
                    Op::Add => x.wrapping_add(rhs),
                    Op::Sub => x.wrapping_sub(rhs),
                    Op::RSub => rhs.wrapping_sub(x),
                    Op::Or => x | rhs,
                    Op::Xor => x ^ rhs,
                    Op::And => x & rhs,
                    Op::AndNot => x & !rhs,
                    Op::Mul => x.wrapping_mul(rhs),
                    Op::Shl => x.wrapping_shl(rhs & 31),
                    Op::Shr => x.wrapping_shr(rhs & 31),
                    Op::Sar => ((x as i32) >> (rhs & 31)) as u32,
                };
                match a.dst {
                    Dst::Reg(d) => self.regs[d] = if matches!(a.op, Op::Add | Op::Sub | Op::RSub) { self.alu_f(a.op, lhs, rhs) } else { apply(lhs) },
                    Dst::Mem(off) => {
                        let addr = (self.regs[a.src] as i64 + off as i64) as u32;
                        let cur = bus.read32(addr);
                        bus.write32(addr, apply(cur));
                    }
                }
            }
            2 => {}
            3 => {
                // lockset / lockclr: the hardware lock the cores share (the stock RTOS takes
                // it around every kernel critical section; with both cores running, a no-op
                // let a task be queued on a wait list twice). Held by the other core: retry.
                // (The table names them `lockset` / `lockclr`; spelled `lockset_r` here, this
                // arm never ran and both fell through to the syntax executor's no-ops: FreeRTOS
                // SMP in Bubba Machine then corrupted its lists at the first contention.)
                if !bus.hw_lock(self.core_id, name == "lockset") {
                    jumped = Some(self.pc);
                }
            }

            // ---- moves ------------------------------------------------
            4 => {
                // reg = bits(0,2) | bit7<<3 ; imm = concat((8,12),(3,5))
                let r = ((insn.raw & 7) | ((insn.raw >> 7 & 1) << 3)) as usize;
                let imm = if (insn.raw & 0xc0) == 0xc0
                    && (insn.raw >> 8 & 0xff) == 0x14
                {
                    0 // per-corpus special family ("rN = 0"-like alias)
                } else {
                    let lo = ((insn.raw >> 8) & 0x1f) as u32;
                    let hi = ((insn.raw >> 3) & 7) as u32;
                    lo | (hi << 5)
                };
                self.regs[r] = imm;
            }
            5 => {
                let v = ((insn.raw >> 16) & 0xffffffff) as u32;
                self.sp = v;
            }
            6 => {
                let v = ((insn.raw >> 16) & 0xffffffff) as u32;
                self.set_ssp(v);
            }
            7 => {
                let v = ((insn.raw >> 16) & 0xffffffff) as u32;
                self.set_usp(v);
            }

            // ---- arithmetic -------------------------------------------
            8 => {
                // R = #h: reg = bits(28,31); value = composed(bits(24,27), bits(16,23))
                let raw = insn.raw;
                let d = ((raw >> 28) & 0xf) as usize;
                self.regs[d] = composed_imm(((raw >> 24) & 0xf) as u32, ((raw >> 16) & 0xff) as u32);
            }
            9 => {
                // [R(base)+off] |=/&= R(src): base = b3>>4; src = b3&0xf;
                // off = b1 (8-bit)
                let raw = insn.raw;
                let b = ((raw >> 28) & 0xf) as usize;
                let s = ((raw >> 24) & 0xf) as usize;
                let off = (raw >> 8) & 0xff;
                let addr = self.regs[b].wrapping_add(off as u32);
                let cur = bus.read32(addr);
                let v = if has(name, "and") {
                    cur & self.regs[s]
                } else {
                    cur | self.regs[s]
                };
                bus.write32(addr, v);
            }
            10 => {
                // 2-byte `R = R <op> #i` (mask e088): dst = (0,2); src = (4,6);
                // sh = (8,12); bits 3/7 select lsl (00) / lsr (bit7) / asr
                // (bit3|bit7). Corpus: `a2 a2` r2 = r2 >> 2, `9a a2` r2 = r1 >>> 2.
                let raw = insn.raw;
                let d = (raw & 7) as usize;
                let s = ((raw >> 4) & 7) as usize;
                // a count field of 0 shifts by 32 (the vendor disassembly prints
                // `r7 = r5 >>> 32`; 73 such in the stock firmware)
                let sh = ((raw >> 8) & 0x1f) as u32;
                let sh = if sh == 0 { 32 } else { sh };
                let v = self.regs[s];
                self.regs[d] = if has(name, "lsr") {
                    if sh >= 32 { 0 } else { v >> sh }
                } else if has(name, "asr") {
                    ((v as i32) >> sh.min(31)) as u32
                } else {
                    if sh >= 32 { 0 } else { v << sh }
                };
            }

            // ---- loads / stores ---------------------------------------

            // ---- control flow -----------------------------------------
            11 =>
            {
                // 4-byte `if[s] (rN <op> #imm) goto #off` — verified on all
                // 5302 corpus samples (tools/fit_if4, 2026-10-09):
                //   reg = bits(0,3); byte1 picks the family, bit7 the op:
                //   f8 ==/!=, f9 >=/< (u), fc >/<= (u), fd >=/< (s), fe >/<= (s)
                //   imm = bits(25,31) | bits(4,6)<<7, 10 bits, sign-extended
                //   for ==/!= and the signed families (`r14 != -1`); the
                //   unsigned families print it zero-extended, so compare the
                //   zero-extended value (hardware-probe item: imm >= 512).
                //   off = 2 * sext9(bits(16,23) | bit24<<8), from pc + 4.
                let raw = insn.raw;
                let r = self.regs[(raw & 0xf) as usize];
                let fam = (raw >> 8) & 0xff;
                let alt = raw & 0x80 != 0;
                let imm10 = (((raw >> 25) & 0x7f) | (((raw >> 4) & 7) << 7)) as u32;
                let imm_s = if imm10 & 0x200 != 0 { imm10 | !0x3ff } else { imm10 };
                let off = {
                    let f = (((raw >> 16) & 0xff) | (((raw >> 24) & 1) << 8)) as i64;
                    2 * if f & 0x100 != 0 { f - 0x200 } else { f }
                };
                let taken = match (fam, alt) {
                    (0xf8, false) => r == imm_s,
                    (0xf8, true) => r != imm_s,
                    (0xf9, false) => r >= imm10,
                    (0xf9, true) => r < imm10,
                    (0xfc, false) => r > imm10,
                    (0xfc, true) => r <= imm10,
                    (0xfd, false) => (r as i32) >= (imm_s as i32),
                    (0xfd, true) => (r as i32) < (imm_s as i32),
                    (0xfe, false) => (r as i32) > (imm_s as i32),
                    _ => (r as i32) <= (imm_s as i32),
                };
                if taken {
                    jumped = Some((self.pc as i64 + 4 + off) as u32);
                }
            }
            12 =>
            {
                // 6-byte `if[s] (rN <op> #imm) goto #off` (`xx ff`, 1323 corpus
                // samples): byte0 bit0 = alt, bits(1,3) = family (as cmp_cond),
                // bit5 = immediate kind: 0 -> sext12(bits(16,27)),
                // 1 -> composed(bits(24,27), bits(16,23)); reg = bits(28,31);
                // off = 2*sext16(bits(32,47)).
                let raw = insn.raw;
                let b0 = raw & 0xff;
                let fam = (b0 >> 1) & 7;
                let imm = if b0 & 0x20 != 0 {
                    composed_imm(((raw >> 24) & 0xf) as u32, ((raw >> 16) & 0xff) as u32)
                } else {
                    // sign-extended for ==/!= and the signed families, zero-
                    // extended for the unsigned ones (corpus: `if (r2 < 2111)`)
                    let v = ((raw >> 16) & 0xfff) as u32;
                    if v & 0x800 != 0 && !matches!(fam, 1 | 4) { v | !0xfff } else { v }
                };
                let r = self.regs[((raw >> 28) & 0xf) as usize];
                if cmp_cond(fam, b0 & 1 != 0, r, imm) {
                    let off = 2 * ((raw >> 32) as u16 as i16 as i64);
                    jumped = Some((next_pc as i64 + off) as u32);
                }
            }
            13 =>
            {
                let (fam, alt, a, b, off) = decode_cmp_rr_branch(insn.raw, insn.entry.len).unwrap();
                if cmp_cond(fam, alt, self.regs[a], self.regs[b]) {
                    jumped = Some((next_pc as i64 + off) as u32);
                }
            }
            14 =>
            {
                // predicated block `if (rA <op> x) { then... } [else { ... }]`
                let blk = decode_if_block(insn.raw).unwrap();
                let a = self.regs[blk.reg];
                let b = match blk.rhs {
                    Rhs::Reg(r) => self.regs[r],
                    Rhs::Imm(v) => v,
                    Rhs::Bit(r) => 1u32 << (self.regs[r] & 31),
                };
                let taken = if blk.fam == 2 { ((a & b) != 0) == blk.alt } else { cmp_cond(blk.fam, blk.alt, a, b) };
                let then_end = self.block_end(bus, next_pc, blk.then_units);
                let else_end = self.block_end(bus, then_end, blk.else_units);
                if taken {
                    if blk.else_units > 0 {
                        self.pred_skips.retain(|&(te, _, _)| te != then_end);
                        self.pred_skips.push((then_end, else_end, next_pc));
                    }
                } else {
                    jumped = Some(then_end);
                }
            }
            15 => {
                // `R = R.b0|.l (u|s)` (xx 17): d = bits(0,2), s = bits(4,6),
                // bit3 = signed, bit7 = halfword (.l) else byte (.b0)
                let raw = insn.raw;
                let d = (raw & 7) as usize;
                let v = self.regs[((raw >> 4) & 7) as usize];
                let signed = raw & 8 != 0;
                self.regs[d] = match (raw & 0x80 != 0, signed) {
                    (false, false) => v & 0xff,
                    (false, true) => v as u8 as i8 as i32 as u32,
                    (true, false) => v & 0xffff,
                    (true, true) => v as u16 as i16 as i32 as u32,
                };
            }
            27 if insn.entry.len == 4 => self.exec_raw(bus, RAW_POP_RETS_MASK, insn.raw as u32),
            16 | 18 | 19 | 21 | 22 | 27 | 32 | 39 | 40 | 41 => self.exec_raw(bus, pre.route as u8, insn.raw as u32),
            17 =>
            {
                // 4-byte offset loads/stores `R = [rB+#i]` / `[rB+#i] = R` with
                // b/h variants (`50..57 ec/ed/ee`): the post-increment field
                // model (data bits(28,31), base bits(20,23), f = bits(16,19) |
                // bits(24,27)<<4; word/half: store = bit16, off = sext10(f&~1 |
                // bit0<<8 | bit1<<9), bit2 = signed; byte: store = bit1,
                // signed = bit2, off = bit0 ? sext8(f) : f) without a base update.
                let raw = insn.raw;
                let w = match (raw >> 8) & 0xff { 0xec => 4u8, 0xed => 2, _ => 1 };
                let d = ((raw >> 28) & 0xf) as usize;
                let b = ((raw >> 20) & 0xf) as usize;
                let f = (((raw >> 16) & 0xf) | (((raw >> 24) & 0xf) << 4)) as u32;
                let (store, off, signed) = if w == 1 {
                    let off = if raw & 1 != 0 { f as i32 - 256 } else { f as i32 };
                    ((raw >> 1) & 1 != 0, off, (raw >> 2) & 1 != 0)
                } else {
                    let v = (f & !1) | (((raw & 1) as u32) << 8) | ((((raw >> 1) & 1) as u32) << 9);
                    let off = if v & 0x200 != 0 { v as i32 - 0x400 } else { v as i32 };
                    ((raw >> 16) & 1 != 0, off, w == 2 && (raw >> 2) & 1 != 0)
                };
                let addr = self.regs[b].wrapping_add(off as u32);
                if store {
                    match w { 1 => bus.write8(addr, self.regs[d] as u8), 2 => bus.write16(addr, self.regs[d] as u16), _ => bus.write32(addr, self.regs[d]) }
                } else {
                    self.regs[d] = match (w, signed) {
                        (1, false) => bus.read8(addr) as u32,
                        (1, true) => bus.read8(addr) as i8 as i32 as u32,
                        (2, false) => bus.read16(addr) as u32,
                        (2, true) => bus.read16(addr) as i16 as i32 as u32,
                        _ => bus.read32(addr),
                    };
                }
            }
            20 =>
            {
                // indexed `[rB + rI << s]` loads/stores (d8 ec/ed/ee, 1535
                // corpus samples): data reg = bits(28,31), base = bits(20,23),
                // index = bits(24,27); sub-op bits(16,19):
                //   ec: 2 ld w, 3 st w, a ld w <<2, b st w <<2
                //   ed: 0 ld h u, 1 st h, 2 ld h s, 8/9/a same with <<1
                //   ee: 0 ld b u, 1 st b, 2 ld b s
                let raw = insn.raw;
                let kind = (raw >> 8) & 0xff;
                let sub = (raw >> 16) & 0xf;
                let d = ((raw >> 28) & 0xf) as usize;
                let base = self.regs[((raw >> 20) & 0xf) as usize];
                let idx = self.regs[((raw >> 24) & 0xf) as usize];
                let shift = if sub & 8 != 0 { if kind == 0xec { 2 } else { 1 } } else { 0 };
                let addr = base.wrapping_add(idx << shift);
                match (kind, sub & 3) {
                    (0xec, 2) => self.regs[d] = bus.read32(addr),
                    (0xec, 3) => bus.write32(addr, self.regs[d]),
                    (0xed, 0) => self.regs[d] = bus.read16(addr) as u32,
                    (0xed, 1) => bus.write16(addr, self.regs[d] as u16),
                    (0xed, 2) => self.regs[d] = bus.read16(addr) as i16 as i32 as u32,
                    (0xee, 0) => self.regs[d] = bus.read8(addr) as u32,
                    (0xee, 1) => bus.write8(addr, self.regs[d] as u8),
                    (0xee, 2) => self.regs[d] = bus.read8(addr) as i8 as i32 as u32,
                    _ => {
                        self.stuck_on = Some(name);
                        return Err(CoreError::Unsupported { name, pc: self.pc, raw });
                    }
                }
            }
            23 => {
                // `testset b[rN]` (bx 00): atomic test-and-set of a byte.
                // Assumed: cc = (old != 0) ("locked"), byte |= 0x80 — the
                // SDK spins with `testset; ifeq goto retry`, so "eq" must mean
                // "was already taken". Single-core emulation: always acquired.
                let addr = self.regs[(insn.raw & 0xf) as usize];
                let old = bus.read8(addr);
                self.cc = old != 0;
                bus.write8(addr, old | 0x80);
            }
            24 => {
                // `ifeq/ifne goto #off` (40/41 e8): off = 2*sext16(bits(16,31))
                let raw = insn.raw;
                let off = 2 * ((raw >> 16) as u16 as i16 as i64);
                let want = raw & 1 == 0;
                if self.cc == want {
                    jumped = Some((next_pc as i64 + off) as u32);
                }
            }
            25 => {
                let v = self.slot(insn, 0)? as u32;
                let cond = if has(name, "ne") {
                    self.rets != v
                } else {
                    self.rets == v
                };
                if cond {
                    jumped = self.branch(insn, 1)?;
                }
            }

            // ---- stack frames -----------------------------------------
            26 if insn.entry.len == 4 => {
                // `d9 e8 <mask>`: [--sp] = {rets, registers of mask bits(16,31)}, rets
                // first (highest address), then the registers from the highest down
                // (all 8 corpus samples, e.g. `d9 e8 f0 0d` {rets, r11, r10, r8-r4})
                let mask = ((insn.raw >> 16) & 0xffff) as u32;
                self.sp = self.sp.wrapping_sub(4);
                bus.write32(self.sp, self.rets);
                for r in (0..16).rev() {
                    if mask >> r & 1 != 0 {
                        self.sp = self.sp.wrapping_sub(4);
                        bus.write32(self.sp, self.regs[r]);
                    }
                }
            }
            26 => {
                // [--sp] = {rets, rN..rB}: n = bits(0,3); list = rets,
                // [max(n,3)..min(n,4)] descending; --sp then store each.
                let n = (insn.raw & 0xf) as i32;
                let top = n.max(3);
                let bot = n.min(4);
                for r in std::iter::once(-1i32).chain((bot..=top).rev()) {
                    self.sp = self.sp.wrapping_sub(4);
                    let v = if r < 0 { self.rets } else { self.regs[r as usize] };
                    bus.write32(self.sp, v);
                }
            }
            28 if insn.entry.len == 4 => {
                // `d5 e8 <mask>`: {pc, registers of mask} = [sp++], the registers from
                // the lowest up, then pc
                let mask = ((insn.raw >> 16) & 0xffff) as u32;
                for r in 0..16 {
                    if mask >> r & 1 != 0 {
                        self.regs[r] = bus.read32(self.sp);
                        self.sp = self.sp.wrapping_add(4);
                    }
                }
                jumped = Some(bus.read32(self.sp));
                self.sp = self.sp.wrapping_add(4);
            }
            28 => {
                // {pc, rN..rB} = [sp++]: pop ascending = [min(n,4)..max(n,3)] (the
                // push's range: `50 04` is {pc, r3-r0}, `5a 04` {pc, r10-r4})
                // then pc last (highest slot = pushed rets).
                let n = (insn.raw & 0xf) as i32;
                let top = n.max(3);
                let bot = n.min(4);
                for r in bot..=top {
                    self.regs[r as usize] = bus.read32(self.sp);
                    self.sp = self.sp.wrapping_add(4);
                }
                jumped = Some(bus.read32(self.sp));
                self.sp = self.sp.wrapping_add(4);
            }

            // ---- misc -------------------------------------------------
            29 => {
                // `rep N rR {` (03 xx): reg = bits(0,3), N = 2*(bits(4,7)+1)
                //   corpus: `02 03` rep 2 r2, `12 03` rep 4 r2, `ff 03` rep 32 r15
                // `rep N #k {` (8x xx): N as above, k = bits(8,12)+1
                //   corpus: `00 90` rep 2 17, `d0 94` rep 28 21, `40 9c` rep 10 29
                // The block is the N bytes after the rep instruction and runs
                // k times (register form: rR times, rR decremented to 0).
                // A zero register count skips the block: the CRT runs
                // `r2 = 0; rep 2 r2 {…}` unguarded for the empty overlay
                // (needs hardware confirmation, Phase 1.3).
                let raw = raw_v(insn);
                let n = 2 * (((raw >> 4) & 0xf) as u32 + 1);
                self.rep_start = next_pc;
                self.rep_end = next_pc + n;
                if name == "rep_i_r" {
                    // The count is latched at the rep: the register reads what is left (0) and
                    // the block may reuse it (the stock memmove tail is `rep 4 r2 { r2 =
                    // b[r1++=1]; b[r3++=1] = r2 }`; counting in r2 overran into a return
                    // address), while `rep 4 r5 {..}; if (r5 != 0) goto rep` still sees 0.
                    let r = (raw & 0xf) as usize;
                    self.rep_reg = None;
                    self.rep_count = self.regs[r];
                    self.regs[r] = 0;
                } else {
                    self.rep_reg = None;
                    self.rep_count = ((raw >> 8) & 0x1f) as u32 + 1;
                }
                if self.rep_count == 0 {
                    jumped = Some(self.rep_end);
                    self.rep_end = 0;
                }
            }

            ROUTE_FPU => {
                let (op, d, a, b) = decode_fpu(insn.raw);
                self.regs[d as usize] = fpu_d(op, self.regs[d as usize], self.regs[a as usize], self.regs[b as usize]);
            }
            ROUTE_SYNTAX => match self.exec_syntax(bus, insn, next_pc) {
                Ok(Some(j)) => jumped = j,
                Ok(None) | Err(CoreError::MissingSlot { .. }) => {
                    self.stuck_on = Some(other);
                    return Err(CoreError::Unsupported {
                        name: other,
                        pc: self.pc,
                        raw: insn.raw,
                    });
                }
                Err(e) => return Err(e),
            },
            _ => unreachable!("route out of range"),
        } }

        self.finish(jumped, next_pc);
        Ok(())
    }

    /// The instructions decoded from the raw word alone (execute's routes 16, 18, 19, 21, 22,
    /// 32 and 27, whose 4-byte form is RAW_POP_RETS_MASK): shared by `execute` and the `Fast::Raw` micro-op, so the two cannot drift.
    fn exec_raw<B: Bus>(&mut self, bus: &mut B, route: u8, raw: u32) {
        let raw = raw as u64;
        match route {
            16 =>
            {
                // 64-bit register-pair loads/stores `rH_rL = d[...]` / `d[...] = rH_rL`
                // (see `decode_pair_mem`); the low register sits at the lower address
                let m = decode_pair_mem(raw);
                let (hi, lo) = (2 * m.pair + 1, 2 * m.pair);
                let base = match m.base { Some(b) => self.regs[b], None => self.sp };
                let step = match m.index { Some(i) => self.regs[i], None => m.off as u32 };
                let addr = if m.post { base } else { base.wrapping_add(step) };
                // the base update reads the index before a load can overwrite it
                if let Some(b) = m.base.filter(|_| m.pre || m.post) {
                    self.regs[b] = base.wrapping_add(step);
                }
                if m.store {
                    bus.write32(addr, self.regs[lo]);
                    bus.write32(addr.wrapping_add(4), self.regs[hi]);
                } else {
                    self.regs[lo] = bus.read32(addr);
                    self.regs[hi] = bus.read32(addr.wrapping_add(4));
                }
            }
            18 | 19 =>
            {
                // single-register accesses with a base update (see `decode_inc_mem`): a
                // pre-increment updates the base before the access, a post-increment after
                let m = decode_inc_mem(route, raw);
                let base = self.regs[m.b];
                let next = base.wrapping_add(m.inc as u32);
                let a = if m.post { base } else { next };
                if m.pre {
                    self.regs[m.b] = next;
                }
                if m.store {
                    st_w(bus, m.w, a, self.regs[m.d]);
                } else {
                    self.regs[m.d] = ld_w(bus, m.w, m.signed, a);
                }
                if m.post {
                    self.regs[m.b] = next;
                }
            }
            35 =>
            {
                // `5x ed` (see `half_mem`); a pre-increment updates the base first
                let m = half_mem(raw).unwrap();
                let a = self.regs[m.b].wrapping_add(m.off as u32);
                if m.pre {
                    self.regs[m.b] = a;
                }
                if m.store {
                    let v = self.regs[m.d] >> if m.flag { 16 } else { 0 };
                    bus.write16(a, v as u16);
                } else {
                    let v = bus.read16(a);
                    self.regs[m.d] = if m.flag { v as i16 as i32 as u32 } else { v as u32 };
                }
            }
            36 =>
            {
                let (d, s) = (((raw >> 28) & 0xf) as usize, ((raw >> 24) & 0xf) as usize);
                self.regs[d] = self.regs[s].leading_zeros();
            }
            37 =>
            {
                let m = sp_mem_raw(raw as u64).unwrap();
                let a = self.sp.wrapping_add(m.off);
                if m.store {
                    bus.write16(a, self.regs[m.d as usize] as u16);
                } else {
                    self.regs[m.d as usize] = ld_w(bus, m.w, m.signed, a);
                }
            }
            38 =>
            {
                let (d, s) = ((raw & 0xe) as usize, ((raw >> 4) & 0xe) as usize);
                let (lo, hi) = (self.regs[s], self.regs[s + 1]);
                self.regs[d] = lo;
                self.regs[d + 1] = hi;
            }
            34 =>
            {
                let (d, s, signed) = sat16_raw(raw).unwrap();
                let x = self.regs[s as usize] as i32;
                self.regs[d as usize] = if signed { x.clamp(-32768, 32767) } else { x.clamp(0, 65535) } as u32;
            }
            33 =>
            {
                // `d4 e9`: a word at sp + off, loaded or stored (the micro-op's own rule)
                let (r, off) = (((raw >> 28) & 0xf) as usize, ((raw >> 16) & 0xffe) as u32);
                let addr = self.sp.wrapping_add(off);
                if raw & 0x1_0000 != 0 {
                    bus.write32(addr, self.regs[r]);
                } else {
                    self.regs[r] = bus.read32(addr);
                }
            }
            32 =>
            {
                // `[++rB=rI]` / `[rB++=rI]` (see `idx_inc`); a load into the base register
                // leaves the incremented base, as route 19
                let Some(m) = idx_inc(raw) else { return };
                let base = self.regs[m.b];
                let next = base.wrapping_add(self.regs[m.i]);
                let addr = if m.pre { next } else { base };
                if m.store {
                    let v = if m.high { self.regs[m.d] >> 16 } else { self.regs[m.d] };
                    st_w(bus, m.w, addr, v);
                } else {
                    self.regs[m.d] = ld_w(bus, m.w, m.signed, addr);
                }
                self.regs[m.b] = next;
            }
            21 =>
            {
                // 64-bit register-pair ops (corpus-fitted 2026-10-09):
                //   `d0 e1`: rP <<=|>>=|>>>= n  — pair = bits(29,31), op = bits(26,27)
                //            (0 <<=, 2 >>= logical, 3 >>>= arithmetic),
                //            n = bits(16,19) | bits(24,25)<<4
                //   `d8 e1`: rP <<=|>>=|>>>= rN — op = bits(16,17), n = r[bits(24,27)] & 63
                //   `f8 e1`: rP = rA * rB (u|s) — a = bits(20,23), b = bits(24,27),
                //            pair = bits(29,31), signed = bit28
                //   `fc e1`: rP += rA * rB (u|s) — the fields of `f8` (compiler-rt's
                //            wide multiply; not in the corpora)
                //   `fe e1`: rP -= rA * rB (u|s) — the fields of `f8` (vendor assembler;
                //            clang's `acc - (u64)a * b`, probes/mul64.c)
                //   `f6 e1`: rP = rQ / rB (u|s) — q = bits(21,23), b = bits(24,27),
                //            pair = bits(29,31), signed = bit28
                //   `de e1`: rD = rQ >> n (s|u), the pair shifted and saturated into one
                //            register — d = bits(28,31), q = bits(21,23), n = bits(16,19) |
                //            bits(24,25)<<4, signed = bit27 (vendor assembler, work/probe/dsp.S;
                //            FM1 Open's mixers: `r3 = r7_r6 >> 29 (s)`)
                //   `df e1`: the same by a register — q = bits(21,23), r = bits(24,27),
                //            signed = bit16 (`__builtin_pi32v2_shfms64(x, n)`)
                //   `d4 e1`: rP <>= n, a 64-bit rotate right, n as `d0`'s; `dc e1`: by
                //            r[bits(24,27)] (the compiler's `(x >> n) | (x << (64 - n))`)
                                let p = ((raw >> 29) & 7) as usize;
                let (hi, lo) = (2 * p + 1, 2 * p);
                let signed = (raw >> 28) & 1 != 0;
                match raw & 0xff {
                    0xd4 | 0xdc => {
                        let n = if raw & 0xff == 0xdc { self.regs[((raw >> 24) & 0xf) as usize] & 63 }
                            else { (((raw >> 16) & 0xf) | (((raw >> 24) & 3) << 4)) as u32 };
                        let y = ((self.regs[hi] as u64) << 32 | self.regs[lo] as u64).rotate_right(n);
                        self.regs[lo] = y as u32;
                        self.regs[hi] = (y >> 32) as u32;
                    }
                    0xdf => {
                        let q = ((raw >> 21) & 7) as usize;
                        let x = (self.regs[2 * q + 1] as u64) << 32 | self.regs[2 * q] as u64;
                        let n = self.regs[((raw >> 24) & 0xf) as usize] & 63;
                        self.regs[((raw >> 28) & 0xf) as usize] = if raw & 0x1_0000 != 0 {
                            ((x as i64) >> n).clamp(i32::MIN as i64, i32::MAX as i64) as u32
                        } else {
                            (x >> n).min(u32::MAX as u64) as u32
                        };
                    }
                    0xde => {
                        // saturated: the vendor clang never picks it for a plain C truncation
                        // (it shifts the pair, work/probe/pairx.c), only DSP assembly uses it
                        let q = ((raw >> 21) & 7) as usize;
                        let x = (self.regs[2 * q + 1] as u64) << 32 | self.regs[2 * q] as u64;
                        let n = ((raw >> 16) & 0xf) | (((raw >> 24) & 3) << 4);
                        self.regs[((raw >> 28) & 0xf) as usize] = if raw & 0x0800_0000 != 0 {
                            ((x as i64) >> n).clamp(i32::MIN as i64, i32::MAX as i64) as u32
                        } else {
                            (x >> n).min(u32::MAX as u64) as u32
                        };
                    }
                    0xd0 | 0xd8 => {
                        let by_reg = raw & 0xff == 0xd8;
                        let n = if by_reg { self.regs[((raw >> 24) & 0xf) as usize] & 63 }
                            else { (((raw >> 16) & 0xf) | (((raw >> 24) & 3) << 4)) as u32 };
                        let op = if by_reg { (raw >> 16) & 3 } else { (raw >> 26) & 3 };
                        let x = (self.regs[hi] as u64) << 32 | self.regs[lo] as u64;
                        let y = match op {
                            0 | 1 => x.wrapping_shl(n),
                            2 => x.wrapping_shr(n),
                            _ => ((x as i64).wrapping_shr(n)) as u64,
                        };
                        self.regs[lo] = y as u32;
                        self.regs[hi] = (y >> 32) as u32;
                    }
                    0xf8 | 0xfc | 0xfe => {
                        let a = self.regs[((raw >> 20) & 0xf) as usize];
                        let b = self.regs[((raw >> 24) & 0xf) as usize];
                        let mut y = if signed { (a as i32 as i64).wrapping_mul(b as i32 as i64) as u64 } else { (a as u64) * (b as u64) };
                        let acc = (self.regs[hi] as u64) << 32 | self.regs[lo] as u64;
                        match raw & 0xff {
                            0xfc => y = acc.wrapping_add(y),
                            0xfe => y = acc.wrapping_sub(y),
                            _ => {}
                        }
                        self.regs[lo] = y as u32;
                        self.regs[hi] = (y >> 32) as u32;
                    }
                    _ => {
                        let q = ((raw >> 21) & 7) as usize;
                        let x = (self.regs[2 * q + 1] as u64) << 32 | self.regs[2 * q] as u64;
                        let b = self.regs[((raw >> 24) & 0xf) as usize];
                        let y = if b == 0 { 0 } else if signed { (x as i64).wrapping_div(b as i32 as i64) as u64 } else { x / b as u64 };
                        self.regs[lo] = y as u32;
                        self.regs[hi] = (y >> 32) as u32;
                    }
                }
            }
            22 => {
                // `R = rev8(R)` (70 e0 00): d = bits(28,31), src = bits(24,27); `70 e0 80`:
                // rD = revs(rS.l), the low half's bytes swapped and sign-extended (the vendor
                // compiler's `(short)__builtin_bswap16(x)`, work/probe/sem.c); `70 e0 40`
                // rev16, each half's bytes swapped (FM-1 V16 swaps 16-bit samples two at a time
                // with it, rev8 >> 16 for the last one); `70 e0 c0` popcnt (the vendor objdump)
                let d = ((raw >> 28) & 0xf) as usize;
                let x = self.regs[((raw >> 24) & 0xf) as usize];
                self.regs[d] = match (raw >> 22) & 3 {
                    0 => x.swap_bytes(),
                    1 => (x & 0x00ff_00ff) << 8 | (x >> 8) & 0x00ff_00ff,
                    2 => (x as u16).swap_bytes() as i16 as i32 as u32,
                    _ => x.count_ones(),
                };
            }
            27 => {
                // `{rets, rN..rB} = [sp++]` (3n 04): like the pc pops but the
                // top slot restores rets instead of jumping
                let n = (raw & 0xf) as i32;
                for r in n.min(4)..=n.max(3) {
                    self.regs[r as usize] = bus.read32(self.sp);
                    self.sp = self.sp.wrapping_add(4);
                }
                self.rets = bus.read32(self.sp);
                self.sp = self.sp.wrapping_add(4);
            }
            41 => {
                let (d, a, b) = (((raw >> 28) & 0xf) as usize, ((raw >> 20) & 0xf) as usize, ((raw >> 24) & 0xf) as usize);
                self.regs[d] = match dsp_raw(raw as u64).unwrap() {
                    Dsp::QaslImm => qasl(self.regs[a], match (((raw >> 16) & 0xf) | ((raw >> 24) & 1) << 4) as u32 { 0 => 32, n => n }),
                    Dsp::QaslReg => qasl(self.regs[a], self.regs[b] & 31),
                    Dsp::Cls => {
                        let x = self.regs[b];
                        (if (x as i32) < 0 { !x } else { x }).leading_zeros() - 1
                    }
                    Dsp::MinMax => {
                        let (x, y) = (self.regs[a], self.regs[b]);
                        match (raw & 1 != 0, (raw >> 16) & 1 != 0) {
                            (false, true) => (x as i32).max(y as i32) as u32,
                            (false, false) => x.max(y),
                            (true, true) => (x as i32).min(y as i32) as u32,
                            (true, false) => x.min(y),
                        }
                    }
                    Dsp::BrevAdd => (self.regs[d].reverse_bits().wrapping_add(self.regs[b].reverse_bits())).reverse_bits(),
                };
            }
            39 => {
                // `39 e4|f4 AO DB`: rD = rA +|- rB (ssat), subtract with bit 19
                let (a, b) = (self.regs[((raw >> 20) & 0xf) as usize] as i32, self.regs[((raw >> 24) & 0xf) as usize] as i32);
                self.regs[((raw >> 28) & 0xf) as usize] = if raw & 0x8_0000 != 0 { a.saturating_sub(b) } else { a.saturating_add(b) } as u32;
            }
            40 => {
                // `20 e0 II DN`: byte N (bits 26,27) of rD (bits 28,31) = II
                let (d, k) = (((raw >> 28) & 0xf) as usize, 8 * ((raw >> 26) & 3) as u32);
                self.regs[d] = self.regs[d] & !(0xff << k) | (((raw >> 16) & 0xff) as u32) << k;
            }
            RAW_POP_RETS_MASK => {
                // `d1 e8 <mask>`: {rets, registers of mask} = [sp++], the registers from
                // the lowest up, then rets (the inverse of `d9 e8`)
                let mask = ((raw >> 16) & 0xffff) as u32;
                for r in 0..16 {
                    if mask >> r & 1 != 0 {
                        self.regs[r] = bus.read32(self.sp);
                        self.sp = self.sp.wrapping_add(4);
                    }
                }
                self.rets = bus.read32(self.sp);
                self.sp = self.sp.wrapping_add(4);
            }
            _ => unreachable!("exec_raw route"),
        }
    }

    /// Retire an instruction: next pc, predicated-block skips, rep loops.
    #[inline]
    fn finish(&mut self, jumped: Option<u32>, next_pc: u32) {
        self.insn_count += 1;
        self.hold_irq = 0;
        let at = self.pc;
        self.pc = jumped.unwrap_or(next_pc);
        if !self.pred_skips.is_empty() {
            let pc = self.pc;
            if let Some(i) = self.pred_skips.iter().position(|&(te, _, _)| te == pc) {
                let (_, ee, _) = self.pred_skips.remove(i);
                self.pc = ee;
            }
            let (pc, rets, back) = (self.pc, self.rets, jumped.is_some());
            // a branch out of the construct abandons its pending skip: past its then-part, or a
            // goto from inside it back out of it (`pred_left_back`)
            self.pred_skips.retain(|&(te, _, ts)| pc <= te && !(back && ts <= at && at < te && pred_left_back(pc, ts, next_pc, rets)));
        }
        if self.rep_end != 0 && jumped.is_none() && self.pc == self.rep_end {
            // fell off the end of a rep block: count down, loop or finish
            let remaining = match self.rep_reg {
                Some(r) => {
                    self.regs[r] = self.regs[r].wrapping_sub(1);
                    self.regs[r]
                }
                None => {
                    self.rep_count -= 1;
                    self.rep_count
                }
            };
            if remaining != 0 {
                self.pc = self.rep_start;
            } else {
                self.rep_end = 0;
            }
        }
    }
}

pub use fm1_isa::composed_imm;

/// Table-less class for the 64-bit pair op family (`d0/d8/f8/fc/fe/f6 e1`); the
/// raw decoder in `execute` ignores everything but `len`.
static PAIR_OP_RAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "pair_op_raw",
    syntax: "r1_r0 pair op (raw)",
    len: 4,
    count: 0,
    mask: 0,
    match_: 0,
    group: "misc",
    alt: &[],
    samples: &[],
    slots: &[],
};
/// The same family with byte1 `f1`: a dual-issue pair's head (the trailing `#` makes it one)
static PAIR_OP_PAIR_RAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "pair_op_raw",
    syntax: "r1_r0 pair op (raw)  #",
    len: 4,
    count: 0,
    mask: 0,
    match_: 0,
    group: "misc",
    alt: &[],
    samples: &[],
    slots: &[],
};

/// Decode the instruction at `pc` from its 8-byte window: the table, and before it or after it
/// the raw decoders of the families whose table classes fit only the corpus samples. `fetch_at`
/// and `block_end` both decode through here, so a block's size counts the same instructions
/// the core executes.
fn decode_any(win: u64, pc: u32) -> Result<fm1_isa::Instruction, fm1_isa::DecodeError> {
    // the add / subtract-with-carry family before the table: its corpus classes fit only
    // the registers the corpora used (see carry_op_raw)
    let decoded = match carry_op_raw(win) {
        Some(entry) => Ok(fm1_isa::Instruction { addr: pc, raw: win & 0xffff_ffff, entry }),
        // `d8|d9|dc|dd e9`, sp-relative byte / halfword accesses (route 37), before the table too:
        // `r_mov_b_sp_u`'s register slot, solved from r0, r1, r2 and r12 alone, swapped bits 30
        // and 31 (`r4 = b[sp]` loaded r8; tools/fuzz_decode.py against the vendor objdump)
        None if sp_mem_raw(win).is_some() => Ok(fm1_isa::Instruction { addr: pc, raw: win & 0xffff_ffff, entry: &SP_LOAD_CRAW }),
        // `64 e0 00|80 XY`, the special-register moves, before the table too: its `sp = R` class,
        // fitted on r0, r1, r6 and r7, swapped bits 1 and 2 of the register (`sp = r5` set sp from
        // r3: M-VAVE's FM1 Open moved its stack up over its own frame) and knew no r8..r15
        None if let Some(entry) = sreg_move_raw(win) => Ok(fm1_isa::Instruction { addr: pc, raw: win & 0xffff_ffff, entry }),
        // the DSP forms (see pre_table_raw), before the table too
        None if let Some(entry) = pre_table_raw(win & 0xffff_ffff) => Ok(fm1_isa::Instruction { addr: pc, raw: win & 0xffff_ffff, entry }),
        None => fm1_isa::decode_win_cached(win, pc),
    };
    let raw = |entry| Ok(fm1_isa::Instruction { addr: pc, raw: win & 0xffff_ffff, entry });
    match decoded {
        Ok(i) => Ok(i),
        // 64-bit pair ops the corpora never showed (e.g. `r3_r2 >>>= 39`)
        // share one raw layout; decode them without a table class
        Err(_) if matches!((win >> 8) & 0xff, 0xe1 | 0xf1) && matches!(win & 0xff, 0xd0 | 0xd8 | 0xf8 | 0xfc | 0xfe | 0xf6) => {
            // (`f1`: a dual-issue pair's head, e.g. `fe f1 40 61` r7_r6 -= r4 * r1 (u) #)
            let entry = if (win >> 8) & 0xff == 0xf1 { &PAIR_OP_PAIR_RAW } else { &PAIR_OP_RAW };
            Ok(fm1_isa::Instruction { addr: pc, raw: win, entry })
        }
        // `d9 e8` / `d1 e8 <mask>`: push / pop of rets and a register mask; the table's
        // classes match only the masks the corpora used (libm's sin pops {rets, r11-r8, r6-r4})
        Err(_) if win & 0xffff == 0xe8d9 => raw(&RETS_PUSH_MASK_CRAW),
        Err(_) if win & 0xffff == 0xe8d1 => raw(&RETS_POP_MASK_CRAW),
        // `d5 e8 <mask>`: `{pc, mask} = [sp++]`, a return popping a register mask (route 28;
        // all 54 corpus samples fit): the classes match only the corpus masks (Vector37's
        // `{pc, r11-r6, r4}`)
        Err(_) if win & 0xffff == 0xe8d5 => raw(&PC_POP_MASK_CRAW),
        // `d4 e9 <rrrr oooo oooo ooos>`: `rR = [sp+off]` / `[sp+off] = rR`, off = bits 1..11 (a
        // word offset), s = store (route 33; all 20620 corpus samples fit): the classes fit only
        // the corpus offsets (ObervaVe's `r3 = [sp+2384]`)
        Err(_) if win & 0xffff == 0xe9d4 => raw(&SP_WORD_CRAW),
        // `5x ed`: halfword loads / stores at a 10-bit offset, plain or pre-incrementing the base
        // (route 35, `half_mem`; all 30337 corpus samples fit): the classes fit only the corpus
        // offsets (JIANT's `h[++r5=-198] = r0`)
        Err(_) if half_mem(win & 0xffff_ffff).is_some() => raw(&HALF_MEM_CRAW),
        // `80 e1 00 DS` / `80 f1 00 DS` (a dual-issue pair's head): rD = clz(rS). The classes fit
        // only the corpus registers and had no `f1` form (Bubba Machine's)
        Err(_) if win & 0xff_ffff == 0x00_e180 => raw(&CLZ_CRAW),
        Err(_) if win & 0xff_ffff == 0x00_f180 => raw(&CLZ_PAIR_CRAW),
        // `SD 15` / `SD d5` (a dual-issue pair's head): r(D+1)_rD = r(S+1)_rS, both even (route
        // 38; the classes fit only the corpus pairs: Tonitor's `r5_r4 = r11_r10 #`)
        Err(_) if matches!((win >> 8) & 0xff, 0x15 | 0xd5) && win & 0x11 == 0 => raw(if win & 0x8000 != 0 { &MOV2_PAIR_CRAW } else { &MOV2_CRAW }),
        // `3f e5 Ax DB` (and `3f f5`, a dual-issue pair's head) with op 7 / 8: the float
        // multiply-accumulates `rD += rA * rB (f)` / `rD -= rA * rB (f)` (vendor assembler,
        // work/probe/fpu78b.S); the corpora's FPU classes had none (the SMK-37's stock DSP code)
        Err(_) if matches!(win & 0xffff, 0xe53f | 0xf53f) && matches!((win >> 16) & 0xf, 7 | 8) => {
            raw(if win & 0xffff == 0xf53f { &FPU_MAC_PAIR_CRAW } else { &FPU_MAC_CRAW })
        }
        // 4-byte ALU forms decode_alu4 knows the layout of (`6c e8` `[R+#i] <<= #n`, `64 e8`
        // `[R+#i] &= R`, ...): the table's classes fit only the offsets the corpora used
        // (m6502's `[r0+24] <<= 1`, `[r1+-4] &= r0`)
        Err(_) if decode_alu4(win & 0xffff_ffff).is_some() => raw(&ALU4_CRAW),
        // `d0..d7 ec`: word loads / stores at an offset or pre-incremented (route 18)
        Err(_) if matches!(win & 0xff, 0xd0..=0xd7) && (win >> 8) & 0xff == 0xec => raw(&WORD_MEM_CRAW),
        // post-increment loads / stores `[R++=#i]` (route 19 decodes the fields; the classes fit
        // only the corpus increments: `h[r4++=480] = r0`): `d0..d7 ed` halfwords, `d0..d5 ee`
        // bytes, `d8..df ec` words with bit 17 clear (set: route 32's index forms) (the vendor
        // objdump's ranges, tools/fuzz_decode.py)
        Err(_) if match (win >> 8) & 0xff {
            0xed => matches!(win & 0xff, 0xd0..=0xd7),
            0xee => matches!(win & 0xff, 0xd0..=0xd5),
            0xec => matches!(win & 0xff, 0xd8..=0xdf) && (win >> 17) & 1 == 0,
            _ => false,
        } => raw(&POST_INC_CRAW),
        // `64 e0 00 XY` / `64 e0 80 XY`: rX = special register Y / Y = rX; the classes fit
        // only the pairs the corpora used (ChoralRoot 0.21's `r2 = reti`)
        Err(_) if sreg_move_raw(win).is_some() => raw(sreg_move_raw(win).unwrap()),
        // `60 ff` / `61 ff` bit-test branches the table's class does not take (see BIT_TEST_EQ_CRAW)
        Err(_) if win & 0xfe == 0x60 && (win >> 8) & 0xff == 0xff => Ok(fm1_isa::Instruction {
            addr: pc,
            raw: win & 0xffff_ffff_ffff,
            entry: if win & 1 == 0 { &BIT_TEST_EQ_CRAW } else { &BIT_TEST_NE_CRAW },
        }),
        // predicated blocks `if (rA op x) {` (route 14 decodes the fields): the classes fit
        // only the block sizes the corpora used (Quest 0.21's `if ((r13 & r6) != 0) {` with an
        // else part). In family ea, byte0 9x..bx are calls.
        Err(_) if decode_if_block(win & 0xffff_ffff).is_some() && ((win >> 8) & 0xff != 0xea || win & 0x80 == 0) => {
            raw(&IF_BLOCK_CRAW)
        }
        // `d0 e9`: register-pair loads / stores at sp+off (route 16, `decode_pair_mem`; all 9701
        // corpus samples fit): the classes fit only the corpus offsets (`d[sp+1044] = r13_r12`)
        Err(_) if win & 0xffff == 0xe9d0 => raw(&PAIR_SP_CRAW),
        // `70 e0 00 DS` / `70 f0 00 DS`: rD = rev8(rS) (route 22; 690 corpus samples)
        Err(_) if win & 0xff_ffff == 0x00_e070 => raw(&REV8_CRAW),
        Err(_) if win & 0xff_ffff == 0x00_f070 => raw(&REV8_PAIR_CRAW),
        // `70 e0 40 DS` rev16, `70 e0 c0 DS` popcnt (route 22; the vendor objdump --mcpu=r3)
        Err(_) if win & 0xff_ffff == 0x40_e070 => raw(&REV16_CRAW),
        Err(_) if win & 0xff_ffff == 0x40_f070 => raw(&REV16_PAIR_CRAW),
        Err(_) if win & 0xff_ffff == 0xc0_e070 => raw(&POPCNT_CRAW),
        Err(_) if win & 0xff_ffff == 0xc0_f070 => raw(&POPCNT_PAIR_CRAW),
        // `dc` / `de` with ec/ed/ee: loads / stores that pre- or post-increment the base by a
        // register (route 32, `idx_inc`): the classes fit only the corpus registers
        // (SLOOP 2.6's `b[r2++=r0] = r1`)
        Err(_) if idx_inc(win & 0xffff_ffff).is_some() => raw(&IDX_INC_CRAW),
        // `4x ff 00 AB oo oo`: `if (rA op rB) goto`, a 16-bit half-word offset from the next
        // instruction (route 13, `decode_cmp_rr_branch`; all 3004 corpus samples fit): the
        // classes fit only the corpus offsets (Snoop37's `if (r4 < r3) goto -8996`, further back
        // than any the corpora had). Byte 2: 0 (bit 7, the float compare, never seen this way).
        Err(_) if (win >> 8) & 0xff == 0xff && (win & 0xff) >> 4 == 4 && (win >> 16) & 0xff == 0 => {
            Ok(fm1_isa::Instruction { addr: pc, raw: win & 0xffff_ffff_ffff, entry: &CMP_RR_BRANCH6_CRAW })
        }
        // `c4 e1 SN DM` / `c4 f1` (a dual-issue pair's head), bit 27 clear: rD = rS <> n, the
        // field layout `R = R <> #i` already executes from; the classes fit only the corpus
        // registers (FM1 Open's sha256 `r6 = r7 <> 19`). Bit 27 set is `<c>`, through carry.
        Err(_) if matches!(win & 0xffff, 0xe1c4 | 0xf1c4) && win & 0x0e00_0000 == 0 => {
            raw(if win & 0xffff == 0xf1c4 { &ROT_PAIR_CRAW } else { &ROT_CRAW })
        }
        Err(e) => Err(e),
    }
}

/// The DSP forms whose layouts the vendor assembler fixed for every register (work/probe/dsp.S,
/// fam.S, mm.S), the ones FM1 Open's engine and the compiler's `__builtin_pi32v2_*` produce.
/// Decoded before the table: its classes for them fit only the corpus registers, or decode
/// them with no semantics (`r_add_r_brev`, `r_b2_mov_i`, `r_mov_r1_r0_lsr_i_u`, which caught
/// the dual-issue `f0` / `f1` heads); tools/sem_check.py runs them against objdump's text.
/// byte1 `f0` / `f1` / `f4`: a dual-issue pair's head (`#`).
fn pre_table_raw(raw: u64) -> Option<&'static fm1_isa::IsaEntry> {
    let head = matches!((raw >> 8) & 0xff, 0xf0 | 0xf1 | 0xf4);
    let b1 = if head { (raw >> 8) & 0xff ^ 0x10 } else { (raw >> 8) & 0xff };
    let pick = |plain, pair| Some(if head { pair } else { plain });
    match (raw & 0xff, b1) {
        // `39 e4 AO DB`: rD = rA + rB / rA - rB (ssat), the subtract with bit 19 (route 39;
        // FM1 Open's mixers saturate r5 + r3)
        (0x39, 0xe4) => pick(&SSAT_CRAW, &SSAT_PAIR_CRAW),
        // the 64-bit pair forms (route 21): `de` rD = rQ >> n (s|u) and `df` by a register (bits
        // 20 and 26 / 17..19 clear), `d4` rP <>= n (bits 26..28 clear), `dc` rP <>= rN (bits
        // 16..23 clear)
        (0xde, 0xe1) if raw & 0x0410_0000 == 0 => pick(&PAIR_OP_RAW, &PAIR_OP_PAIR_RAW),
        (0xdf, 0xe1) if raw & 0x001e_0000 == 0 => pick(&PAIR_OP_RAW, &PAIR_OP_PAIR_RAW),
        (0xd4, 0xe1) if raw & 0x1cf0_0000 == 0 => pick(&PAIR_OP_RAW, &PAIR_OP_PAIR_RAW),
        (0xdc, 0xe1) if raw & 0x10ff_0000 == 0 => pick(&PAIR_OP_RAW, &PAIR_OP_PAIR_RAW),
        // `20 e0 II DN`: rD.bN = #i, a byte of rD replaced (route 40; FM1 Open's `r14.b3 = 208`)
        (0x20, 0xe0) if (raw >> 24) & 3 == 0 => pick(&BYTE_SET_CRAW, &BYTE_SET_PAIR_CRAW),
        // `70 e0 80 DS`: rD = revs(rS.l) (route 22)
        (0x70, 0xe0) if (raw >> 16) & 0xff == 0x80 => pick(&REV8_CRAW, &REV8_PAIR_CRAW),
        // `78..7b e0 0S DS`: rD = sat16(rS) (route 34)
        (0x78..=0x7b, 0xe0) if (raw >> 16) & 0xfe == 0 => pick(&SAT16_CRAW, &SAT16_PAIR_CRAW),
        _ if dsp_raw(raw).is_some() => pick(&DSP_CRAW, &DSP_PAIR_CRAW),
        _ => None,
    }
}

/// The forms the vendor compiler's builtins produce that no corpus firmware showed (work/probe/bi,
/// fitted with the assembler, work/probe/fam.S); route 41. d = bits(28,31) throughout.
#[derive(Clone, Copy, Debug)]
enum Dsp {
    /// `c0 e1 SN DM`, op bits(25,27) = 2: rD = rS <<< n, n = bits(16,19) | bit24 << 4 (the
    /// `<<` / `>>` / `>>>` classes' layout; `__builtin_pi32v2_qasl`)
    QaslImm,
    /// `c8 e1 A1 DB`: rD = rA <<< rB (sub-op 1 of the `<<` / `>>` / `>>>` by-register forms)
    QaslReg,
    /// `80 e1 01 DS`: rD = cls(rS), the leading bits equal to the sign, the sign not counted
    /// (as `clrsb`; `__builtin_pi32v2_cls`; the `clz` row's op 1)
    Cls,
    /// `34 e4 AX DB` / `35 e4`: rD = max / min(rA, rB), X = 1 signed, 0 unsigned (smax, umax, smin,
    /// umin; the classes fit only the corpus registers)
    MinMax,
    /// `b0 e0 00 DS`: rD += rS (brev), an add whose carry runs from bit 31 down to bit 0 (FFT
    /// addressing; `__builtin_pi32v2_brevadd`)
    BrevAdd,
}
fn dsp_raw(raw: u64) -> Option<Dsp> {
    let pair = matches!((raw >> 8) & 0xff, 0xf0 | 0xf1 | 0xf4);
    let b1 = if pair { (raw >> 8) & 0xff ^ 0x10 } else { (raw >> 8) & 0xff };
    match (raw & 0xff, b1) {
        (0xc0, 0xe1) if (raw >> 25) & 7 == 2 => Some(Dsp::QaslImm),
        (0xc8, 0xe1) if (raw >> 16) & 0xf == 1 => Some(Dsp::QaslReg),
        (0x80, 0xe1) if (raw >> 16) & 0xff == 1 => Some(Dsp::Cls),
        (0xb0, 0xe0) if (raw >> 16) & 0xff == 0 => Some(Dsp::BrevAdd),
        (0x34 | 0x35, 0xe4) if (raw >> 17) & 7 == 0 => Some(Dsp::MinMax),
        _ => None,
    }
}
static DSP_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "dsp_craw", syntax: "R = dsp(R, R) (raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static DSP_PAIR_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "dsp_craw", syntax: "R = dsp(R, R) (raw)  #", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static SSAT_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "ssat_craw", syntax: "R = R +|- R (ssat)", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static SSAT_PAIR_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "ssat_craw", syntax: "R = R +|- R (ssat)  #", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static ROT_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "rot_craw", syntax: "R = R <> #i", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static ROT_PAIR_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "rot_pair_craw", syntax: "R = R <> #i  #", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static SAT16_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "sat16_craw", syntax: "R = sat16(R) (raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static SAT16_PAIR_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "sat16_craw", syntax: "R = sat16(R) (raw)  #", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static BYTE_SET_PAIR_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "byte_set_craw", syntax: "R.b# = #i (raw)  #", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static BYTE_SET_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "byte_set_craw", syntax: "R.b# = #i (raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};

/// `dc` `[++rB=rI]` / `de` `[rB++=rI]` loads and stores (byte1 ec word, ed half, ee byte):
/// data = bits(28,31), base = bits(20,23), index = bits(24,27); bits(16,19): bit16 store,
/// bit17 signed load (ec: bit17 set = this register form, clear = an immediate increment),
/// bit17 of a half store = the high half (`h[r9++=r0] = r0.h`); bit19 (a shifted index, the
/// d8 form's) never appears. All 3462 corpus samples fit.
#[derive(Clone, Copy, Debug)]
struct IdxInc { pre: bool, w: u8, store: bool, signed: bool, high: bool, d: usize, b: usize, i: usize }
/// Route 32 as micro-ops the compilers have (`fast_raw`): `LdPre` / `StPre` / `LdPostR` /
/// `StPostR`, except a store or load of the base register itself and a high-half store.
fn idx_inc_fast(raw: u64) -> Fast {
    match idx_inc(raw) {
        Some(m) if !m.high && m.d != m.b => {
            let (d, b, i, w, signed) = (m.d as u8, m.b as u8, m.i as u8, m.w, m.signed);
            match (m.pre, m.store) {
                (true, false) => Fast::LdPre { d, b, i, w, signed, inc: 0 },
                (true, true) => Fast::StPre { s: d, b, i, w, inc: 0 },
                (false, false) => Fast::LdPostR { d, b, i, w, signed },
                (false, true) => Fast::StPostR { s: d, b, i, w },
            }
        }
        _ => Fast::Raw { route: 32, raw: raw as u32 },
    }
}
fn idx_inc(raw: u64) -> Option<IdxInc> {
    let pre = match raw & 0xff { 0xdc => true, 0xde => false, _ => return None };
    let w = match (raw >> 8) & 0xff { 0xec => 4u8, 0xed => 2, 0xee => 1, _ => return None };
    let sub = (raw >> 16) & 0xf;
    let (store, bit17) = (sub & 1 != 0, sub & 2 != 0);
    if sub & 8 != 0 || (w == 4 && !bit17) || (w == 1 && store && bit17) {
        return None;
    }
    Some(IdxInc {
        pre,
        w,
        store,
        signed: !store && w != 4 && bit17,
        high: store && w == 2 && bit17,
        d: ((raw >> 28) & 0xf) as usize,
        b: ((raw >> 20) & 0xf) as usize,
        i: ((raw >> 24) & 0xf) as usize,
    })
}
static PAIR_SP_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "pair_sp_craw", syntax: "d[sp+#i] = R_R (raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "load", alt: &[], samples: &[], slots: &[],
};
static REV8_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "rev8_craw", syntax: "R = rev8(R)", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static REV8_PAIR_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "rev8_craw", syntax: "R = rev8(R)  #", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static REV16_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "rev8_craw", syntax: "R = rev16(R)", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static REV16_PAIR_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "rev8_craw", syntax: "R = rev16(R)  #", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static POPCNT_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "rev8_craw", syntax: "R = popcnt(R)", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static POPCNT_PAIR_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "rev8_craw", syntax: "R = popcnt(R)  #", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static CMP_RR_BRANCH6_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "if_r_op_r_goto_i_craw", syntax: "if (R op R) goto #i (raw)", len: 6, count: 0, mask: 0, match_: 0,
    group: "branch", alt: &[], samples: &[], slots: &[],
};
static IDX_INC_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "idx_inc_craw", syntax: "[R++=R] = R (raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "load", alt: &[], samples: &[], slots: &[],
};

/// `64 e0 00 XY` rX = Y and `64 e0 80 XY` Y = rX, Y a special register: 0 reti, 1 rete,
/// 3 rets, 5 psr, 6 cnum, b icfg, c usp, d ssp, e sp (the corpora's 40 register / sreg pairs
/// all fit; Y = 2, 4, 7..a and f never appear, and cnum is never written).
fn sreg_move_raw(win: u64) -> Option<&'static fm1_isa::IsaEntry> {
    // (`64 f0`: a dual-issue pair's head, objdump's `r2 = rets  #`: CTL 1.0, 386DX, MC-1)
    let pair = match win & 0xffff { 0xe064 => false, 0xf064 => true, _ => return None };
    let write = match (win >> 16) & 0xff { 0x00 => false, 0x80 => true, _ => return None };
    let i = match (win >> 24) & 0xf { 0 => 0, 1 => 1, 3 => 2, 5 => 3, 6 if !write => 4, 0xb => 5, 0xc => 6, 0xd => 7, 0xe => 8, _ => return None };
    Some(match (write, pair) {
        (false, false) => &SREG_READ_CRAW[i],
        (true, false) => &SREG_WRITE_CRAW[i],
        (false, true) => &SREG_READ_PAIR_CRAW[i],
        (true, true) => &SREG_WRITE_PAIR_CRAW[i],
    })
}
const SREG_SLOTS: &[fm1_isa::SlotSpec] =
    &[fm1_isa::SlotSpec { kind: fm1_isa::SlotKind::Imm, pieces: &[(28, 31)], mult: 0, adj: 0, hi: 0, lo: 0 }];
macro_rules! sreg_entries {
    ($($syntax:expr),*) => {
        [$(fm1_isa::IsaEntry {
            name: "sreg_mov_craw", syntax: $syntax, len: 4, count: 0, mask: 0, match_: 0, group: "misc",
            alt: &[], samples: &[], slots: SREG_SLOTS,
        }),*]
    };
}
static SREG_READ_CRAW: [fm1_isa::IsaEntry; 9] = sreg_entries!(
    "R = reti", "R = rete", "R = rets", "R = psr", "R = cnum", "R = icfg", "R = usp", "R = ssp", "R = sp");
static SREG_WRITE_CRAW: [fm1_isa::IsaEntry; 9] = sreg_entries!(
    "reti = R", "rete = R", "rets = R", "psr = R", "cnum = R", "icfg = R", "usp = R", "ssp = R", "sp = R");
static SREG_READ_PAIR_CRAW: [fm1_isa::IsaEntry; 9] = sreg_entries!(
    "R = reti  #", "R = rete  #", "R = rets  #", "R = psr  #", "R = cnum  #", "R = icfg  #", "R = usp  #", "R = ssp  #",
    "R = sp  #");
static SREG_WRITE_PAIR_CRAW: [fm1_isa::IsaEntry; 9] = sreg_entries!(
    "reti = R  #", "rete = R  #", "rets = R  #", "psr = R  #", "cnum = R  #", "icfg = R  #", "usp = R  #", "ssp = R  #",
    "sp = R  #");
/// `60 ff` / `61 ff`: `if ((rN & #h) ==/!= 0) goto #i` (decode_bittest, route 0). The table's
/// class knew only `61 ff` with a forward offset: Kawa's `60 ff 01 00 ea 33` halted.
static BIT_TEST_EQ_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "if_bit_test_craw", syntax: "if ((R & #h) == 0) goto #i", len: 6, count: 0, mask: 0, match_: 0,
    group: "misc", alt: &[], samples: &[], slots: &[],
};
static BIT_TEST_NE_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "if_bit_test_craw", syntax: "if ((R & #h) != 0) goto #i", len: 6, count: 0, mask: 0, match_: 0,
    group: "misc", alt: &[], samples: &[], slots: &[],
};
static IF_BLOCK_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "if_block_craw", syntax: "if (R op x) { (raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "misc", alt: &[], samples: &[], slots: &[],
};

/// `b8 e0` / `b8 f0` (the dual-issue `#` form) with bits(16,19) = 0: `rD = rA + rB + c`,
/// 2: `rD = rA - rB - !c`; rD = bits(28,31), rA = bits(20,23), rB = bits(24,27) (objdump of
/// clang's 64-bit add / subtract). The table's classes for these were fitted on the few
/// register combinations the corpora showed (`r_mov_r_r_c_l4` matches odd registers only,
/// `r_mov_r_r_c_3` needs bit 31): soft-double code (compiler-rt) hit both gaps.
fn carry_op_raw(win: u64) -> Option<&'static fm1_isa::IsaEntry> {
    if win & 0xff != 0xb8 {
        return None;
    }
    let pair = match (win >> 8) & 0xff { 0xe0 => false, 0xf0 => true, _ => return None };
    match ((win >> 16) & 0xf, pair) {
        (0, false) => Some(&ADC_CRAW),
        (0, true) => Some(&ADC_PAIR_CRAW),
        (2, false) => Some(&SBC_CRAW),
        (2, true) => Some(&SBC_PAIR_CRAW),
        _ => None,
    }
}
const CARRY_SLOTS: &[fm1_isa::SlotSpec] = &[
    fm1_isa::SlotSpec { kind: fm1_isa::SlotKind::Imm, pieces: &[(28, 31)], mult: 0, adj: 0, hi: 0, lo: 0 },
    fm1_isa::SlotSpec { kind: fm1_isa::SlotKind::Imm, pieces: &[(20, 23)], mult: 0, adj: 0, hi: 0, lo: 0 },
    fm1_isa::SlotSpec { kind: fm1_isa::SlotKind::Imm, pieces: &[(24, 27)], mult: 0, adj: 0, hi: 0, lo: 0 },
];
macro_rules! carry_entry {
    ($id:ident, $name:expr, $syntax:expr) => {
        static $id: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
            name: $name, syntax: $syntax, len: 4, count: 0, mask: 0, match_: 0, group: "alu",
            alt: &[], samples: &[], slots: CARRY_SLOTS,
        };
    };
}
carry_entry!(ADC_CRAW, "adc_craw", "R = R + R + c");
static ALU4_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "alu4_craw", syntax: "R op= (alu4 raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static POST_INC_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "post_inc_craw", syntax: "[R++=#i] = R (raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "load", alt: &[], samples: &[], slots: &[],
};
static WORD_MEM_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "word_mem_craw", syntax: "[R+#i] = R (word raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "load", alt: &[], samples: &[], slots: &[],
};
static RETS_PUSH_MASK_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "sp_mov_rets_mask_craw", syntax: "[--sp] = {rets, R*4}", len: 4, count: 0, mask: 0, match_: 0,
    group: "stack", alt: &[], samples: &[], slots: &[],
};
static PC_POP_MASK_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "pc_mask_mov_sp_craw", syntax: "{pc, R*4} = [sp++]", len: 4, count: 0, mask: 0, match_: 0,
    group: "stack", alt: &[], samples: &[], slots: &[],
};
static FPU_MAC_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "fpu_mac_craw", syntax: "R += R * R (f)", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static FPU_MAC_PAIR_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "fpu_mac_craw", syntax: "R += R * R (f) #", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static HALF_MEM_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "half_mem_craw", syntax: "h[R+#i] (raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "load", alt: &[], samples: &[], slots: &[],
};
static SP_LOAD_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "sp_load_craw", syntax: "R = b|h[sp+#i] (raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "load", alt: &[], samples: &[], slots: &[],
};
static MOV2_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "mov2_craw", syntax: "R_R = R_R (raw)", len: 2, count: 0, mask: 0, match_: 0,
    group: "move", alt: &[], samples: &[], slots: &[],
};
static MOV2_PAIR_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "mov2_craw", syntax: "R_R = R_R  #", len: 2, count: 0, mask: 0, match_: 0,
    group: "move", alt: &[], samples: &[], slots: &[],
};
static CLZ_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "clz_pair_craw", syntax: "R = clz(R) (raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static CLZ_PAIR_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "clz_pair_craw", syntax: "R = clz(R)  #", len: 4, count: 0, mask: 0, match_: 0,
    group: "alu", alt: &[], samples: &[], slots: &[],
};
static SP_WORD_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "sp_word_craw", syntax: "R = [sp+#i] (raw)", len: 4, count: 0, mask: 0, match_: 0,
    group: "load", alt: &[], samples: &[], slots: &[],
};
static RETS_POP_MASK_CRAW: fm1_isa::IsaEntry = fm1_isa::IsaEntry {
    name: "rets_mask_mov_sp_craw", syntax: "{rets, R*4} = [sp++]", len: 4, count: 0, mask: 0, match_: 0,
    group: "stack", alt: &[], samples: &[], slots: &[],
};
carry_entry!(ADC_PAIR_CRAW, "adc_pair_craw", "R = R + R + c #");
carry_entry!(SBC_CRAW, "sbc_craw", "R = R - R - !c");
carry_entry!(SBC_PAIR_CRAW, "sbc_pair_craw", "R = R - R - !c #");

fn sreg_get_named(cpu: &Cpu, n: &str) -> u32 {
    match n {
        "rets" => cpu.rets,
        "sp" => cpu.sp,
        "ssp" => cpu.get_ssp(),
        "usp" => cpu.get_usp(),
        _ => cpu.sregs(n),
    }
}

#[derive(Clone, Copy, Debug)]
enum Op { Add, Sub, RSub, Or, Xor, And, AndNot, Mul, Shl, Shr, Sar }
#[derive(Clone, Copy, Debug)]
enum Rhs { Reg(usize), Imm(u32), Bit(usize) }

/// Compare condition shared by every `if` family: `fam` = byte1 & 7 of the
/// 4-byte forms (e8 ==, e9 >= u, ec > u, ed >= s, ee > s), which is also the
/// family code of the 2-byte-immediate (f8/f9/fc/fd/fe) and 6-byte (4x ff)
/// forms; `alt` (bit7 / bit0) flips to !=, <, <=.
fn cmp_cond(fam: u64, alt: bool, a: u32, b: u32) -> bool {
    if fam >= 8 {
        return fcond(cond_of_fam(fam, alt), a, b);
    }
    let base = match fam {
        0 => a == b,
        1 => a >= b,
        4 => a > b,
        5 => (a as i32) >= (b as i32),
        _ => (a as i32) > (b as i32),
    };
    base != alt
}

/// `if[s] (rA <op> rB) goto #off` — 4-byte (byte1 e8/e9/ec/ed/ee, bit7 alt,
/// A = bits(28,31), B = bits(0,3), off = 2*sext9(bits(16,23) | bit24<<8))
/// and 6-byte (`4x ff`: fam = bits(1,3), bit0 alt, A = bits(28,31),
/// B = bits(24,27), off = 2*sext16(bits(32,47))). 2213 corpus samples.
fn decode_cmp_rr_branch(raw: u64, len: u8) -> Option<(u64, bool, usize, usize, i64)> {
    let b0 = raw & 0xff;
    let b1 = (raw >> 8) & 0xff;
    match (len, b1) {
        (4, 0xe8 | 0xe9 | 0xec | 0xed | 0xee) if b0 & 0x70 == 0 => {
            let f = (((raw >> 16) & 0xff) | (((raw >> 24) & 1) << 8)) as i64;
            let off = 2 * if f & 0x100 != 0 { f - 0x200 } else { f };
            // bit 27: the float compare `iff` (families 8..)
            let fam = (b1 & 7) | ((raw >> 27) & 1) << 3;
            Some((fam, b0 & 0x80 != 0, ((raw >> 28) & 0xf) as usize, (raw & 0xf) as usize, off))
        }
        (6, 0xff) if b0 >> 4 == 4 => Some((
            // bit 23: the float compare `iff` (families 8..)
            (b0 >> 1) & 7 | ((raw >> 23) & 1) << 3,
            b0 & 1 != 0,
            ((raw >> 28) & 0xf) as usize,
            ((raw >> 24) & 0xf) as usize,
            2 * ((raw >> 32) as u16 as i16 as i64),
        )),
        _ => None,
    }
}

/// A 64-bit register-pair access `rH_rL = d[...]` / `d[...] = rH_rL`.
#[derive(Clone, Copy, Debug)]
struct PairMem { pair: usize, store: bool, base: Option<usize>, off: i32, index: Option<usize>, pre: bool, post: bool }

/// `5x ec`: pair = bits(29,31) (rH_rL = r2p+1, r2p), store = bit16, base = bits(20,23),
/// off = 4*sext9(bits(18,19) | bits(24,27)<<2 | bit0<<6 | bit1<<7 | bit2<<8); the mode:
///   50..57, bit17 clear `d[R+#]` (1531 corpus samples), set `d[++R=#]` (pre-increment, 44);
///   58..5f, bit17 clear `d[R++=#]` (post-increment, 29); set: the index register
///   bits(24,27), bits(1,2) the mode: `58` `d[R+R]`, `5c` `d[++R=R]` (8 + 3 samples), `5e`
///   `d[R++=R]` (none in the corpus; the vendor objdump's, found by tools/fuzz_decode.py: bit 2
///   alone had made it a pre-increment). The vendor knows no other: `59 5a 5b 5d 5f` are not
///   instructions, decoded here by bits(1,2) all the same (`5a` as `d[R+R]`).
/// `d0 e9`: sp-relative, off = bits(17,27)<<1 (all 810 corpus samples; the earlier
/// bits(17,23)<<1 | bit24<<8 fit only offsets below 512 and sent Felucca's
/// `d[sp+1056] = r11_r10` to sp+32: its LED arrays kept the last frame's keys lit).
/// (Before 2026-10-10 the `5x ec` arm knew only `d[R+#]` with off >= 0: X0X's
/// `d[++r1=r0] = r5_r4` in drum909_init wrote the default voice over the BD's panel.)
fn decode_pair_mem(raw: u64) -> PairMem {
    let pair = ((raw >> 29) & 7) as usize;
    let store = (raw >> 16) & 1 != 0;
    if (raw >> 8) & 0xff != 0xec {
        return PairMem { pair, store, base: None, off: (((raw >> 17) & 0x7ff) << 1) as i32, index: None, pre: false, post: false };
    }
    let base = Some(((raw >> 20) & 0xf) as usize);
    let f = (((raw >> 18) & 3) | (((raw >> 24) & 0xf) << 2) | ((raw & 1) << 6) | (((raw >> 1) & 1) << 7) | (((raw >> 2) & 1) << 8)) as i32;
    let off = 4 * if f & 0x100 != 0 { f - 0x200 } else { f };
    let bit17 = (raw >> 17) & 1 != 0;
    if raw & 8 == 0 {
        PairMem { pair, store, base, off, index: None, pre: bit17, post: false }
    } else if !bit17 {
        PairMem { pair, store, base, off, index: None, pre: false, post: true }
    } else {
        let index = Some(((raw >> 24) & 0xf) as usize);
        PairMem { pair, store, base, off: 0, index, pre: raw & 6 == 4, post: raw & 6 == 6 }
    }
}

/// Did a jump to `pc` by an instruction inside a then-part starting at `ts` (the caller checks
/// that) leave the construct backwards (clang's tail call `if (c) { goto f } else { .. }` with f
/// before it)? Not a call, which returns into the construct: a call sets `rets` to its own next
/// pc, `next_pc`, and nothing else does. (A callee's own loops are jumps by instructions outside
/// the then-part: they leave nothing.)
#[inline]
fn pred_left_back(pc: u32, ts: u32, next_pc: u32, rets: u32) -> bool {
    pc < ts && rets != next_pc
}

#[derive(Clone, Copy, Debug)]
struct IfBlock { fam: u64, alt: bool, reg: usize, rhs: Rhs, then_units: u32, else_units: u32 }

/// Predicated block `if[s] (rA <op> x) {` (4-byte, byte1 e8/e9/ea/ec/ed/ee):
/// rA = bits(0,3); byte0 high nibble h: h&3 = 1 reg (rB = bits(24,27)),
/// 2 composed immediate, 3 sext12(bits(16,27)); bit7 flips the op (for the
/// bit-test family ea: h=2 ==0, h=3 !=0, reg form byte2 bit7 = !=0).
/// Block sizes in bits(28,31): then = bits(30,31)+1 instructions, else =
/// bits(28,29) (2057 corpus blocks).
fn decode_if_block(raw: u64) -> Option<IfBlock> {
    let b0 = raw & 0xff;
    let b1 = (raw >> 8) & 0xff;
    if !matches!(b1, 0xe8 | 0xe9 | 0xea | 0xec | 0xed | 0xee) {
        return None;
    }
    let h = b0 >> 4;
    let fam = b1 & 7;
    let (rhs, alt) = match (fam, h & 7) {
        (2, 1) => (Rhs::Reg(((raw >> 24) & 0xf) as usize), (raw >> 23) & 1 != 0),
        (2, 2) | (2, 3) => (
            Rhs::Imm(composed_imm(((raw >> 24) & 0xf) as u32, ((raw >> 16) & 0xff) as u32)),
            h & 1 != 0,
        ),
        (_, 1) => (Rhs::Reg(((raw >> 24) & 0xf) as usize), h & 8 != 0),
        (_, 2) => (
            Rhs::Imm(composed_imm(((raw >> 24) & 0xf) as u32, ((raw >> 16) & 0xff) as u32)),
            h & 8 != 0,
        ),
        (_, 3) => {
            // sext12, except the unsigned families (e9 >=, ec >) zero-extend
            // (`if (r1 <= 4000) {` must compare against 4000, not -96)
            let v = ((raw >> 16) & 0xfff) as u32;
            let v = if v & 0x800 != 0 && !matches!(fam, 1 | 4) { v | !0xfff } else { v };
            (Rhs::Imm(v), h & 8 != 0)
        }
        _ => return None,
    };
    // bit 23 of the register form (not the bit-test family): the float compare `iff`
    let fam = if fam != 2 && h & 7 == 1 && (raw >> 23) & 1 != 0 { fam | 8 } else { fam };
    Some(IfBlock {
        fam,
        alt,
        reg: (raw & 0xf) as usize,
        rhs,
        then_units: ((raw >> 30) & 3) as u32 + 1,
        else_units: ((raw >> 28) & 3) as u32,
    })
}
#[derive(Clone, Copy, Debug)]
enum Dst { Reg(usize), Mem(i32) }
#[derive(Clone, Copy, Debug)]
struct Alu4 { dst: Dst, src: usize, op: Op, rhs: Rhs }

fn sext6x4(v: u64) -> i32 {
    let v = (v & 0x3f) as i32;
    4 * if v & 0x20 != 0 { v - 0x40 } else { v }
}

/// The 4-byte ALU families with byte1 = e0/e1/e8/eb/ef. Every shape below was
/// verified on all its V13 corpus samples (2026-10-09, see docs/isa-notes.md):
///   e0 b4 / e1 90 / e1 f0   R = R op R: d=bits(28,31) a=bits(20,23) b=bits(24,27),
///                           sub-op bits(16,19): b4 {0 +, 2 -}, 90 {0 |, 1 ^, 2 &}, f0 {0 *}
///   e1 op4 0..3             R = R + sext14(bits(16,27) | op4<<12)
///   e1 op4 4/5/6/7, e       R = R |,^,&,&~,* composed;  e0 op4 e/f: R +,- composed;
///   e0 op4 a                R = composed - R          (d=bits(0,3), s=bits(28,31))
///   e8 64 / 68              [R+off] op= R: base=bits(28,31) src=bits(24,27)
///                           off=4*sext6(bits(18,23)) op=bits(16,17): 64 {|,^,&} 68 {+,-}
///   eb / ef                 [R+off] op= imm: base=bits(28,31) off=4*sext6(bits(0,5))
///                           bits(6,7): ef {0 |, 1 ^, 2 &, 3 &~} composed;
///                           eb {2 += composed, 3 += sext12(bits(16,27))}
fn decode_alu4(raw: u64) -> Option<Alu4> {
    let b0 = (raw & 0xff) as u32;
    // f0/f1 are the flag-setting twins of e0/e1 (printed with a trailing `#`)
    let b1 = match ((raw >> 8) & 0xff) as u32 { 0xf0 => 0xe0, 0xf1 => 0xe1, x => x };
    let op4 = b0 >> 4;
    let code = ((raw >> 24) & 0xf) as u32;
    let m = ((raw >> 16) & 0xff) as u32;
    let imm12 = ((raw >> 16) & 0xfff) as u32;
    let comp = composed_imm(code, m);
    let d_lo = (b0 & 0xf) as usize;
    let s_hi = ((raw >> 28) & 0xf) as usize;
    let reg_dst = |op: Op, rhs: Rhs| Some(Alu4 { dst: Dst::Reg(d_lo), src: s_hi, op, rhs });
    match b1 {
        // (`fX e0` is `R = R - #h`, handled below: only `f0 e1` multiplies)
        // (`b4` only with e0: `b4 e1` is the bitfield extract `R = u/sextra(R, p:#, l:#)`, 294
        // corpus samples, which this arm used to run as `R = R + R`: X0X's fm_log2f got an
        // address for its exponent)
        0xe0 | 0xe1 if (b0 == 0xb4 && b1 == 0xe0) || (matches!(b0, 0x90 | 0xc8 | 0x94 | 0xf0) && b1 == 0xe1) => {
            // three-register forms; c8 = shifts by register, 94 = `R op (1 << R)`
            let sub = ((raw >> 16) & 0xf) as u32;
            let op = match (b0, sub) {
                (0xb4, 0) => Op::Add,
                (0xb4, 2) => Op::Sub,
                (0x90, 0) | (0x94, 0) => Op::Or,
                (0x90, 1) | (0x94, 1) => Op::Xor,
                (0x90, 2) | (0x94, 2) => Op::And,
                // `94 e1 A3 DB`: rD = rA & ~(1 << rB) (FM1 Open's groove capture)
                (0x94, 3) => Op::AndNot,
                (0xf0, 0) => Op::Mul,
                (0xc8, 0) => Op::Shl,
                (0xc8, 2) => Op::Shr,
                (0xc8, 3) => Op::Sar,
                _ => return None,
            };
            let r = ((raw >> 24) & 0xf) as usize;
            Some(Alu4 {
                dst: Dst::Reg(s_hi),
                src: ((raw >> 20) & 0xf) as usize,
                op,
                rhs: if b0 == 0x94 { Rhs::Bit(r) } else { Rhs::Reg(r) },
            })
        }
        0xe0 => match op4 {
            0xe => reg_dst(Op::Add, Rhs::Imm(comp)),
            0xf => reg_dst(Op::Sub, Rhs::Imm(comp)),
            0xa => reg_dst(Op::RSub, Rhs::Imm(comp)),
            _ => None,
        },
        0xe1 => match op4 {
            0..=3 => {
                let v = imm12 | (op4 << 12);
                let v = if v & 0x2000 != 0 { v | !0x3fff } else { v };
                reg_dst(Op::Add, Rhs::Imm(v))
            }
            4 => reg_dst(Op::Or, Rhs::Imm(comp)),
            5 => reg_dst(Op::Xor, Rhs::Imm(comp)),
            6 => reg_dst(Op::And, Rhs::Imm(comp)),
            7 => reg_dst(Op::AndNot, Rhs::Imm(comp)),
            0xe => reg_dst(Op::Mul, Rhs::Imm(comp)),
            _ => None,
        },
        0xe8 if b0 == 0x6c || b0 == 0x6d => {
            // `[R+#i] <<= #n` (6c e8): base bits 28-31, word offset bits
            // 18-23, shift bits 24-27, op bits 16-17 like the e1 shifts;
            // 6d e8: the same with 16 added to the shift (`6d e8 02 16` [r1+0] >>= 22)
            let op = match (raw >> 16) & 3 {
                0 => Op::Shl,
                2 => Op::Shr,
                3 => Op::Sar,
                _ => return None,
            };
            Some(Alu4 {
                dst: Dst::Mem(sext6x4(raw >> 18)),
                src: s_hi,
                op,
                rhs: Rhs::Imm(((raw >> 24) & 0xf) as u32 + 16 * (b0 & 1)),
            })
        }
        0xe8 if b0 == 0x66 => {
            // `[R+#i] op= 1 << R` (66 e8, 273 corpus samples): the 64 e8 layout with a bit
            // operand; op bits 16-17 {0 |, 1 ^, 2 &, 3 &~}
            let op = match (raw >> 16) & 3 {
                0 => Op::Or,
                1 => Op::Xor,
                2 => Op::And,
                _ => Op::AndNot,
            };
            Some(Alu4 {
                dst: Dst::Mem(sext6x4(raw >> 18)),
                src: s_hi,
                op,
                rhs: Rhs::Bit(((raw >> 24) & 0xf) as usize),
            })
        }
        0xe8 if matches!(b0, 0x64 | 0x68) => {
            let op = match (b0, (raw >> 16) & 3) {
                (0x64, 0) => Op::Or,
                (0x64, 1) => Op::Xor,
                (0x64, 2) => Op::And,
                (0x68, 0) => Op::Add,
                (0x68, 2) => Op::Sub,
                _ => return None,
            };
            Some(Alu4 {
                dst: Dst::Mem(sext6x4(raw >> 18)),
                src: s_hi,
                op,
                rhs: Rhs::Reg(((raw >> 24) & 0xf) as usize),
            })
        }
        0xeb | 0xef => {
            let sel = (b0 >> 6) & 3;
            let (op, v) = match (b1, sel) {
                (0xef, 0) => (Op::Or, comp),
                (0xef, 1) => (Op::Xor, comp),
                (0xef, 2) => (Op::And, comp),
                (0xef, 3) => (Op::AndNot, comp),
                (0xeb, 2) => (Op::Add, comp),
                (0xeb, 3) => (Op::Add, if imm12 & 0x800 != 0 { imm12 | !0xfff } else { imm12 }),
                _ => return None,
            };
            Some(Alu4 { dst: Dst::Mem(sext6x4(raw)), src: s_hi, op, rhs: Rhs::Imm(v) })
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
struct BitTest { reg: usize, mask: Rhs, ne: bool, off: i64 }

/// `if ((rN & mask) ==/!= 0) goto #off`, three encodings (verified on all
/// 1358 V13 corpus samples, 2026-10-09):
///   6-byte `60/61 ff`: reg = bits(28,31), bit0 = !=, mask = composed
///                      (bits(24,27), bits(16,23)), off = 2*sext16(bits(32,47))
///   4-byte `5x e8`:    reg = bits(0,3), mask = 1 << bits(27,31), bit25 = !=,
///                      off = 2*sext9(bits(16,23) | bit24<<8)
///   4-byte `fa/fb`:    reg = bits(0,3), mask = r[bits(4,7)], byte1 fb = !=,
///                      off = 2*sext16(bits(16,31))
fn decode_bittest(raw: u64, len: u8) -> Option<BitTest> {
    let b0 = (raw & 0xff) as u32;
    let b1 = ((raw >> 8) & 0xff) as u32;
    let sext = |v: u64, bits: u32| -> i64 {
        let v = (v & ((1u64 << bits) - 1)) as i64;
        if v & (1 << (bits - 1)) != 0 { v - (1 << bits) } else { v }
    };
    match (len, b1) {
        (6, 0xff) if b0 >> 1 == 0x30 => Some(BitTest {
            reg: ((raw >> 28) & 0xf) as usize,
            mask: Rhs::Imm(composed_imm(((raw >> 24) & 0xf) as u32, ((raw >> 16) & 0xff) as u32)),
            ne: b0 & 1 != 0,
            off: 2 * sext(raw >> 32, 16),
        }),
        (4, 0xe8) if b0 >> 4 == 5 => Some(BitTest {
            reg: (raw & 0xf) as usize,
            mask: Rhs::Imm(1u32 << ((raw >> 27) & 0x1f)),
            ne: (raw >> 25) & 1 != 0,
            off: 2 * sext(((raw >> 16) & 0xff) | (((raw >> 24) & 1) << 8), 9),
        }),
        (4, 0xfa | 0xfb) => Some(BitTest {
            reg: (raw & 0xf) as usize,
            mask: Rhs::Reg(((raw >> 4) & 0xf) as usize),
            ne: b1 & 1 != 0,
            off: 2 * sext(raw >> 16, 16),
        }),
        _ => None,
    }
}

/// Observed `R = #h` composed-constant values, keyed (b3 low nibble, b2),
/// from all five vendor-objdump corpora. Kept as the regression oracle for
/// `composed_imm` (see the `composed_matches_observed_table` test).
pub static MOV_H_TABLE: &[(u8, u8, u32)] = &[
        (0x2_u8, 0x64_u8, 0x64006400_u32),
        (0x3_u8, 0x05_u8, 0x05050505_u32),
        (0x3_u8, 0x30_u8, 0x30303030_u32),
        (0x3_u8, 0x32_u8, 0x32323232_u32),
        (0x3_u8, 0xc1_u8, 0xc1c1c1c1_u32),
        (0x3_u8, 0xcc_u8, 0xcccccccc_u32),
        (0x3_u8, 0xff_u8, 0xffffffff_u32),
        (0x4_u8, 0x00_u8, 0x80000000_u32),
        (0x4_u8, 0x08_u8, 0x88000000_u32),
        (0x4_u8, 0x40_u8, 0xc0000000_u32),
        (0x4_u8, 0x60_u8, 0xe0000000_u32),
        (0x4_u8, 0x70_u8, 0xf0000000_u32),
        (0x4_u8, 0x83_u8, 0x41800000_u32),
        (0x4_u8, 0x86_u8, 0x43000000_u32),
        (0x4_u8, 0x87_u8, 0x43800000_u32),
        (0x4_u8, 0x88_u8, 0x44000000_u32),
        (0x4_u8, 0x9f_u8, 0x4f800000_u32),
        (0x4_u8, 0xf0_u8, 0x78000000_u32),
        (0x5_u8, 0x42_u8, 0x30800000_u32),
        (0x5_u8, 0x50_u8, 0x34000000_u32),
        (0x5_u8, 0x60_u8, 0x38000000_u32),
        (0x5_u8, 0x7a_u8, 0x3e800000_u32),
        (0x5_u8, 0x7c_u8, 0x3f000000_u32),
        (0x5_u8, 0x7e_u8, 0x3f800000_u32),
        (0x5_u8, 0xc0_u8, 0x18000000_u32),
        (0x6_u8, 0x00_u8, 0x08000000_u32),
        (0x6_u8, 0x80_u8, 0x04000000_u32),
        (0x6_u8, 0xb0_u8, 0x05800000_u32),
        (0x6_u8, 0xef_u8, 0x07780000_u32),
        (0x7_u8, 0x00_u8, 0x02000000_u32),
        (0x7_u8, 0x80_u8, 0x01000000_u32),
        (0x7_u8, 0xf8_u8, 0x01f00000_u32),
        (0x8_u8, 0x00_u8, 0x00800000_u32),
        (0x9_u8, 0x00_u8, 0x00200000_u32),
        (0x9_u8, 0x10_u8, 0x00240000_u32),
        (0x9_u8, 0x40_u8, 0x00300000_u32),
        (0x9_u8, 0x80_u8, 0x00100000_u32),
        (0xa_u8, 0x10_u8, 0x00090000_u32),
        (0xa_u8, 0x30_u8, 0x000b0000_u32),
        (0xa_u8, 0x70_u8, 0x000f0000_u32),
        (0xa_u8, 0x80_u8, 0x00040000_u32),
        (0xa_u8, 0xa0_u8, 0x00050000_u32),
        (0xa_u8, 0xa2_u8, 0x00051000_u32),
        (0xb_u8, 0x00_u8, 0x00020000_u32),
        (0xb_u8, 0x20_u8, 0x00028000_u32),
        (0xb_u8, 0x3f_u8, 0x0002fc00_u32),
        (0xb_u8, 0x80_u8, 0x00010000_u32),
        (0xb_u8, 0x81_u8, 0x00010200_u32),
        (0xb_u8, 0x82_u8, 0x00010400_u32),
        (0xb_u8, 0x84_u8, 0x00010800_u32),
        (0xb_u8, 0x8c_u8, 0x00011800_u32),
        (0xb_u8, 0x8e_u8, 0x00011c00_u32),
        (0xb_u8, 0x8f_u8, 0x00011e00_u32),
        (0xb_u8, 0x90_u8, 0x00012000_u32),
        (0xb_u8, 0x97_u8, 0x00012e00_u32),
        (0xb_u8, 0x98_u8, 0x00013000_u32),
        (0xb_u8, 0x9a_u8, 0x00013400_u32),
        (0xb_u8, 0x9b_u8, 0x00013600_u32),
        (0xb_u8, 0x9f_u8, 0x00013e00_u32),
        (0xb_u8, 0xa0_u8, 0x00014000_u32),
        (0xb_u8, 0xa9_u8, 0x00015200_u32),
        (0xb_u8, 0xb0_u8, 0x00016000_u32),
        (0xb_u8, 0xb4_u8, 0x00016800_u32),
        (0xb_u8, 0xb5_u8, 0x00016a00_u32),
        (0xb_u8, 0xf8_u8, 0x0001f000_u32),
        (0xc_u8, 0x00_u8, 0x00008000_u32),
        (0xc_u8, 0x1c_u8, 0x00009c00_u32),
        (0xc_u8, 0x40_u8, 0x0000c000_u32),
        (0xc_u8, 0x7e_u8, 0x0000fe00_u32),
        (0xc_u8, 0x7f_u8, 0x0000ff00_u32),
];


fn raw_v(insn: &Instruction) -> u64 {
    insn.raw
}

/// Parse the `rHI_rLO` register-pair token from a d-access class name.

fn sext(v: u32, bits: u8) -> i64 {
    let v = v & ((1u32 << bits) - 1);
    if bits < 32 && v & (1 << (bits - 1)) != 0 {
        (v as i64) - (1i64 << bits)
    } else {
        v as i64
    }
}

#[derive(Debug)]
pub enum CoreError {
    Decode(DecodeError),
    Unsupported { name: &'static str, pc: u32, raw: u64 },
    MissingSlot { name: &'static str, pc: u32, slot: usize },
}

/// A flat RAM bus for unit tests — no peripherals, no side effects.
#[cfg(test)]
mod tests {
    use super::*;

    /// The micro-ops for the ISR wrappers' instructions and the others that went through
    /// exec_syntax (`[--sp] = {psr, rets, reti}` and the other special-register lists, the block
    /// pushes and pops without rets, `rti`, add / subtract with carry, `[R+#i] &= ~R`, the cache
    /// flushes): every encoding of each, from random state, gives exec_syntax's state.
    #[test]
    fn syntax_micro_ops_match_exec_syntax() {
        const PC: u32 = 0x0200_1000;
        fn rnd(x: &mut u64) -> u64 { *x ^= *x << 13; *x ^= *x >> 7; *x ^= *x << 17; *x }
        /// exec_syntax and the micro-op of `insn` (syntax `s`) from the same random state: the same
        fn check(insn: &fm1_isa::Instruction, s: &'static str, x: &mut u64) {
            let raw = insn.raw;
            let f = fast_of(insn, s).unwrap_or_else(|| panic!("no micro-op for {s} ({raw:#x})"));
            let next_pc = PC + insn.entry.len as u32;
            let mut mem = vec![0u8; 0x1000];
            mem.iter_mut().for_each(|b| *b = rnd(x) as u8);
            let (mut a, mut b) = (FlatRam::new(mem.clone()), FlatRam::new(mem));
            let mut ca = Cpu::new(PC);
            ca.regs.iter_mut().for_each(|r| *r = rnd(x) as u32);
            ca.sp = 0x800 + (rnd(x) as u32 & 0xfc);
            ca.ssp = rnd(x) as u32;
            ca.usp = 0x400 + (rnd(x) as u32 & 0xfc);
            ca.user = rnd(x) & 1 != 0;
            ca.user_irq = rnd(x) as u32;
            ca.irq_levels = (0..(rnd(x) % 4)).map(|_| rnd(x) as u8 & 7).collect();
            ca.nest_ok = rnd(x) & 1 != 0;
            ca.flags = rnd(x) as u32 & 0xf;
            ca.sreg_store.iter_mut().for_each(|r| *r = rnd(x) as u32);
            ca.rets = rnd(x) as u32;
            ca.ie = rnd(x) & 1 != 0;
            ca.cur_syntax = "";
            let mut cb = Cpu::new(PC);
            cb.regs = ca.regs;
            cb.sp = ca.sp; cb.ssp = ca.ssp; cb.usp = ca.usp; cb.user = ca.user; cb.user_irq = ca.user_irq;
            cb.irq_levels = ca.irq_levels.clone(); cb.nest_ok = ca.nest_ok; cb.flags = ca.flags;
            cb.sreg_store = ca.sreg_store; cb.rets = ca.rets; cb.ie = ca.ie;
            let ja = ca.exec_syntax(&mut a, insn, next_pc).unwrap_or_else(|e| panic!("{s}: {e:?}")).expect("exec_syntax handles it");
            let jb = cb.exec_fast(&mut b, f, next_pc);
            let st = |c: &Cpu| (c.regs, c.sp, c.ssp, c.usp, c.user, c.user_irq, c.irq_levels.clone(), c.nest_ok, c.flags, c.sreg_store, c.rets, c.ie);
            assert_eq!(ja, jb, "{s} ({raw:#x}): jump");
            assert_eq!(st(&ca), st(&cb), "{s} ({raw:#x}): state");
            assert_eq!(a.mem, b.mem, "{s} ({raw:#x}): memory");
        }
        let shapes = ["[--sp] = {R*2}", "{R*2} = [sp++]", "[--sp] = {R*1}", "{R*1} = [sp++]", "rti", "R = R + R + c", "R = R - R - !c", "flushinv [R]", "iflush [R]", "[R+#i] &= ~R"];
        let mut seen = std::collections::BTreeMap::<&str, u32>::new();
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        for e in fm1_isa::ISA {
            let s = canon_syntax(e);
            let list = s.strip_prefix("[--sp] = ").and_then(sreg_list_mask).is_some() || s.strip_suffix(" = [sp++]").and_then(sreg_list_mask).is_some();
            if !(shapes.contains(&s) || list) {
                continue;
            }
            for _ in 0..200 {
                let raw = (e.match_ | (rnd(&mut x) & !e.mask)) & ((1u64 << (8 * e.len as u32)) - 1);
                let Ok(insn) = decode_any(raw, PC) else { continue };
                if !std::ptr::eq(insn.entry, e) {
                    continue;
                }
                *seen.entry(s).or_default() += 1;
                check(&insn, s, &mut x);
            }
        }
        // the carry ops decode before the table (`carry_op_raw`): `b8 e0|f0 0|2 <regs>`
        for (op, hi) in [(0u64, 0xe0u64), (2, 0xe0), (0, 0xf0), (2, 0xf0)] {
            for _ in 0..200 {
                let raw = 0xb8 | hi << 8 | op << 16 | (rnd(&mut x) & 0xfff0_0000);
                let insn = decode_any(raw, PC).unwrap();
                let s = canon_syntax(insn.entry);
                assert!(s.starts_with("R = R "), "{s}");
                *seen.entry(s).or_default() += 1;
                check(&insn, s, &mut x);
            }
        }
        for s in shapes {
            assert!(seen.contains_key(s), "{s}: no encoding decoded ({seen:?})");
        }
        assert!(seen.keys().any(|s| s.starts_with("[--sp] = {psr")), "a special-register push: {seen:?}");
        assert!(seen.keys().any(|s| s.ends_with("} = [sp++]") && s.contains("reti")), "a special-register pop: {seen:?}");
    }
    struct FlatRam {
        mem: Vec<u8>,
    }

    impl FlatRam {
        fn new(mem: Vec<u8>) -> Self {
            Self { mem }
        }
    }

    impl Bus for FlatRam {
        fn read8(&mut self, addr: u32) -> u8 {
            *self.mem.get(addr as usize).unwrap_or(&0)
        }
        fn read16(&mut self, addr: u32) -> u16 {
            u16::from_le_bytes([self.read8(addr), self.read8(addr.wrapping_add(1))])
        }
        fn read32(&mut self, addr: u32) -> u32 {
            u32::from_le_bytes([
                self.read8(addr),
                self.read8(addr.wrapping_add(1)),
                self.read8(addr.wrapping_add(2)),
                self.read8(addr.wrapping_add(3)),
            ])
        }
        fn write8(&mut self, addr: u32, value: u8) {
            if (addr as usize) < self.mem.len() {
                self.mem[addr as usize] = value;
            }
        }
        fn write16(&mut self, addr: u32, value: u16) {
            for (i, b) in value.to_le_bytes().iter().enumerate() {
                self.write8(addr.wrapping_add(i as u32), *b);
            }
        }
        fn write32(&mut self, addr: u32, value: u32) {
            for (i, b) in value.to_le_bytes().iter().enumerate() {
                self.write8(addr.wrapping_add(i as u32), *b);
            }
        }
    }

    /// A store into a later instruction of the RAM block it runs in: run_fast, interpreted and
    /// compiled, runs the new instruction, as `step` does. The bus counts writes to code words
    /// (`code_mark`), as the Soc's RAM does.
    #[test]
    fn a_store_into_its_own_block_runs_the_new_instruction() {
        struct Smc { ram: Vec<u8>, flags: Vec<u8>, writes: u64 }
        impl Smc {
            fn w8(&mut self, a: u32, v: u8) {
                if let Some(b) = self.ram.get_mut(a as usize) {
                    *b = v;
                    if self.flags[a as usize / 4] != 0 {
                        self.writes += 1;
                    }
                }
            }
        }
        impl Bus for Smc {
            fn read8(&mut self, a: u32) -> u8 { *self.ram.get(a as usize).unwrap_or(&0) }
            fn read16(&mut self, a: u32) -> u16 { u16::from_le_bytes([self.read8(a), self.read8(a.wrapping_add(1))]) }
            fn read32(&mut self, a: u32) -> u32 { u32::from_le_bytes([0, 1, 2, 3].map(|i| self.read8(a.wrapping_add(i)))) }
            fn write8(&mut self, a: u32, v: u8) { self.w8(a, v) }
            fn write16(&mut self, a: u32, v: u16) { for (i, b) in v.to_le_bytes().into_iter().enumerate() { self.w8(a.wrapping_add(i as u32), b) } }
            fn write32(&mut self, a: u32, v: u32) { for (i, b) in v.to_le_bytes().into_iter().enumerate() { self.w8(a.wrapping_add(i as u32), b) } }
            fn next_tick_due(&self) -> bool { false }
            fn jit_mem(&mut self) -> Option<JitMem> {
                Some(JitMem { ram: self.ram.as_mut_ptr(), ram_base: 0, ram_len: self.ram.len() as u32, ram_code: self.flags.as_ptr(),
                    xip: std::ptr::null(), xip_base: 0x0200_0000, xip_len: 0 })
            }
            fn code_writes(&self) -> Option<u64> { Some(self.writes) }
            fn code_mark(&mut self, lo: u32, hi: u32) {
                for w in lo / 4..hi.div_ceil(4) {
                    if let Some(f) = self.flags.get_mut(w as usize) { *f = 1; }
                }
            }
        }
        // at 0x10: h[r0+24] = r6 (r0 + 24 = 0x17: it rewrites the `if (r4 == 0) goto` at 0x16 into
        // `r4 <<= ...`); r4 += -92; r4 = -16; [0x16] if (r4 == 0) goto ...
        let code = [0x8eu8, 0x6c, 0xec, 0x24, 0x14, 0x30, 0x24, 0x46, 0x46, 0x03, 0x00, 0x46, 0x46, 0x03, 0, 0, 0, 0];
        let regs = { let mut r = [0u32; 16]; r[0] = 0x17u32.wrapping_sub(24); r[6] = 0x87b2; r[4] = 0x1000; r };
        let mk = || {
            let mut ram = vec![0u8; 0x1000];
            ram[0x10..0x10 + code.len()].copy_from_slice(&code);
            Smc { flags: vec![0; ram.len() / 4], ram, writes: 0 }
        };
        let run = |how: &str| {
            let (mut cpu, mut bus) = (Cpu::new(0x10), mk());
            cpu.regs = regs;
            cpu.jit_on = how == "jit";
            for round in 0..2 {
                // (round 0 builds the block from the old bytes; round 1 runs it with them back)
                if round == 1 {
                    for (i, &x) in code.iter().enumerate() { bus.write8(0x10 + i as u32, x); }
                    (cpu.pc, cpu.regs, cpu.insn_count) = (0x10, regs, 0);
                }
                if how == "step" {
                    for _ in 0..4 { cpu.step(&mut bus).unwrap(); }
                } else {
                    cpu.run_fast(&mut bus, 4).unwrap();
                }
            }
            (cpu.pc, cpu.regs)
        };
        let want = run("step");
        assert_eq!(run("interpreted"), want, "interpreted");
        assert_eq!(run("jit"), want, "compiled");
    }

    #[test]
    fn nop_runs_at_entry() {
        let mut ram = FlatRam::new(vec![0u8; 16]);
        let mut cpu = Cpu::new(0);
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.insn_count, 1);
        assert_eq!(cpu.pc, 2);
    }

    #[test]
    fn goto_rel_jumps() {
        // `04 81` = goto +2 (from entry 0)
        let mut ram = FlatRam::new(vec![0x04, 0x81, 0, 0, 0u8, 0, 0, 0, 0, 0, 0, 0]);
        let mut cpu = Cpu::new(0);
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.pc, 4); // 2 + 2
    }

    #[test]
    fn mov_imm_l2_loads_algebra() {
        // `40 2a` = r0 = 10 (per corpus)
        let mut ram = FlatRam::new(vec![0x40, 0x2a, 0, 0, 0, 0, 0, 0]);
        let mut cpu = Cpu::new(0);
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[0], 10);
        // `45 24` = r5 = 4
        let mut ram = FlatRam::new(vec![0x45, 0x24, 0, 0, 0, 0, 0, 0]);
        let mut cpu = Cpu::new(0);
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[5], 4);
    }

    #[test]
    fn call_l6_jumps_and_sets_rets() {
        use super::*;
        struct FlatRam { mem: Vec<u8> }
        impl Bus for FlatRam {
            fn read8(&mut self, a: u32) -> u8 { *self.mem.get(a as usize).unwrap_or(&0) }
            fn read16(&mut self, a: u32) -> u16 { u16::from_le_bytes([self.read8(a), self.read8(a+1)]) }
            fn read32(&mut self, a: u32) -> u32 { u32::from_le_bytes([self.read8(a), self.read8(a+1), self.read8(a+2), self.read8(a+3)]) }
            fn write8(&mut self, a: u32, v: u8) { if (a as usize) < self.mem.len() { self.mem[a as usize] = v; } }
            fn write16(&mut self, a: u32, v: u16) { for (i, b) in v.to_le_bytes().iter().enumerate() { self.write8(a + i as u32, *b); } }
            fn write32(&mut self, a: u32, v: u32) { for (i, b) in v.to_le_bytes().iter().enumerate() { self.write8(a + i as u32, *b); } }
        }
        // 80 ff a0 5a c0 ff = call -4171104 (pc 0x1a6): target 0xffc05c52
        let mut ram = FlatRam { mem: vec![0x80, 0xff, 0xa0, 0x5a, 0xc0, 0xff, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0] };
        let mut cpu = Cpu::new(0);
        cpu.step(&mut ram).unwrap();
        eprintln!("after long call: pc={:#x} rets={:#x}", cpu.pc, cpu.rets);
        assert_eq!(cpu.rets, 6);
        assert_eq!(cpu.pc, (6i64 + (-4171104i64)) as u32);
    }
    #[test]
    fn unsupported_class_halts() {
        // `a0 00` = swi 0 — no semantics yet; must halt, not nop-slide
        let mut ram = FlatRam::new(vec![0xa0, 0x00, 0, 0, 0, 0, 0, 0]);
        let mut cpu = Cpu::new(0);
        match cpu.step(&mut ram) {
            Err(CoreError::Unsupported { name, .. }) => {
                assert_eq!(name, "swi_i");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn lockset_retries_while_the_other_core_holds_it() {
        // `41 00` = lockset, `40 00` = lockclr: the cores' hardware lock (`Bus::hw_lock`). Held
        // by the other core, lockset retries (the pc stays); granted, it goes on; lockclr
        // releases. (The ISA table names them `lockset` / `lockclr`: a route that spelled them
        // `lockset_r` / `lockclr_r` never matched, and both ran as no-ops: no lock at all.)
        struct LockRam { mem: Vec<u8>, grant: bool, calls: Vec<(u32, bool)> }
        impl Bus for LockRam {
            fn read8(&mut self, a: u32) -> u8 { *self.mem.get(a as usize).unwrap_or(&0) }
            fn read16(&mut self, a: u32) -> u16 { u16::from_le_bytes([self.read8(a), self.read8(a + 1)]) }
            fn read32(&mut self, a: u32) -> u32 { u32::from_le_bytes([self.read8(a), self.read8(a + 1), self.read8(a + 2), self.read8(a + 3)]) }
            fn write8(&mut self, a: u32, v: u8) { if (a as usize) < self.mem.len() { self.mem[a as usize] = v; } }
            fn write16(&mut self, a: u32, v: u16) { for (i, b) in v.to_le_bytes().iter().enumerate() { self.write8(a + i as u32, *b); } }
            fn write32(&mut self, a: u32, v: u32) { for (i, b) in v.to_le_bytes().iter().enumerate() { self.write8(a + i as u32, *b); } }
            fn hw_lock(&mut self, core: u32, set: bool) -> bool { self.calls.push((core, set)); !set || self.grant }
        }
        let mut ram = LockRam { mem: vec![0x41, 0x00, 0x40, 0x00, 0, 0, 0, 0], grant: false, calls: vec![] };
        let mut cpu = Cpu::new(0);
        cpu.core_id = 1;
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.pc, 0, "held by the other core: lockset retries");
        ram.grant = true;
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.pc, 2, "granted: on to the next instruction");
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.pc, 4, "lockclr");
        assert_eq!(ram.calls, vec![(1, true), (1, true), (1, false)]);
    }

    #[test]
    fn composed_matches_observed_table() {
        for (code, m, v) in MOV_H_TABLE {
            assert_eq!(
                composed_imm(*code as u32, *m as u32),
                *v,
                "composed({code:#x}, {m:#04x})"
            );
        }
        // corpus spot checks beyond the table
        assert_eq!(composed_imm(0xe, 0x01), 0x810); // r0 = r0 | 0x810
        assert_eq!(composed_imm(0xf, 0x7f), 0x3fc); // r0 = r0 & 0x3FC
        assert_eq!(composed_imm(0x1, 0x5f), 0x5f005f); // [r13+0] |= 0x5F005F
        assert_eq!(composed_imm(0x3, 0xff), 0xffff_ffff);
    }

    #[test]
    fn alu4_shapes() {
        // `90 e1 20 01` r0 = r2 | r1 ; `36 e1 f0 4f` r6 = r4 + -16 ;
        // `c0 ef 40 0f` [r0+0] &= 0xFFFFFCFF ; `64 e8 0a 02` [r0+8] &= r2
        let mut mem = vec![0u8; 0x100];
        mem[..16].copy_from_slice(&[
            0x90, 0xe1, 0x20, 0x01, 0x36, 0xe1, 0xf0, 0x4f, 0xc0, 0xef, 0x40, 0x0f, 0x64, 0xe8,
            0x0a, 0x02,
        ]);
        mem[0x40..0x44].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        mem[0x48..0x4c].copy_from_slice(&0x0000_f0f0u32.to_le_bytes());
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[1] = 0x10;
        cpu.regs[2] = 0x0f;
        cpu.regs[4] = 100;
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[0], 0x1f);
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[6], 84);
        cpu.regs[0] = 0x40;
        cpu.step(&mut ram).unwrap();
        assert_eq!(ram.read32(0x40), 0xffff_fcff);
        cpu.step(&mut ram).unwrap();
        assert_eq!(ram.read32(0x48), 0x0000_f0f0 & 0x0f);
        assert_eq!(cpu.pc, 16);
    }

    #[test]
    fn carry_flags_64bit_add_sub() {
        // clang's 64-bit add / subtract (compiler-rt's soft double depends on them):
        //   `24 1c` r4 = r2 + r0 ; `b8 e0 20 46` r4 = r2 + r6 + c (even registers: not in the corpora)
        //   `84 1e` r4 = r0 - r2 ; `b8 e0 12 53` r5 = r1 - r3 - !c ; `64 e0 00 05` r0 = psr
        let mut mem = vec![0u8; 0x100];
        mem[..18].copy_from_slice(&[0x24, 0x1c, 0xb8, 0xe0, 0x20, 0x46, 0x84, 0x1e, 0xb8, 0xe0, 0x12, 0x53,
                                     0x84, 0x1e, 0x64, 0xe0, 0x00, 0x05]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[2] = 0xffff_ffff;
        cpu.regs[0] = 1;
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[4], 0);
        assert!(cpu.flags & F_C != 0, "carry out of the low word");
        cpu.regs[2] = 5;
        cpu.regs[6] = 7;
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[4], 13, "5 + 7 + carry");
        // 0x1_00000000 - 0x0_00000001: borrow out of the low word
        cpu.regs[0] = 0;
        cpu.regs[2] = 1;
        cpu.step(&mut ram).unwrap();
        cpu.regs[1] = 1;
        cpu.regs[3] = 0;
        cpu.step(&mut ram).unwrap();
        assert_eq!((cpu.regs[4], cpu.regs[5]), (0xffff_ffff, 0), "high word takes the borrow");
        // equal low words: no borrow, and psr shows C set (no borrow) for an unsigned >=
        cpu.regs[0] = 9;
        cpu.regs[2] = 9;
        cpu.step(&mut ram).unwrap();
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[0] & (F_C | F_Z), F_C | F_Z);
    }

    #[test]
    fn flag_producer_with_a_later_reader_stays_careful() {
        // FM-106's chorus interpolation (0x2005a8e): clang put a store between a 64-bit add's
        // halves. `b4 e0 e0 40` r4 = r14 + r0 ; `d0 ec 79 b0` [r7+8] = r11 ;
        // `b8 f0 f0 51` r5 = r15 + r1 + c. The JITs do not set the flags: the low half must stay
        // on the careful path, or the high half takes a stale carry.
        let code = [0xb4, 0xe0, 0xe0, 0x40, 0xd0, 0xec, 0x79, 0xb0, 0xb8, 0xf0, 0xf0, 0x51];
        let mut mem = vec![0u8; 0x100];
        mem[..12].copy_from_slice(&code);
        let mut cpu = Cpu::new(0);
        let slot = cpu.fetch_at(&mut FlatRam::new(mem), 0).unwrap();
        assert!(cpu.icache[slot].careful, "producer two instructions before its reader");
        // another add in between replaces the flags: the first one needs no care
        let mut mem = vec![0u8; 0x100];
        mem[..4].copy_from_slice(&code[..4]);
        mem[4..16].copy_from_slice(&code);
        let mut cpu = Cpu::new(0);
        let slot = cpu.fetch_at(&mut FlatRam::new(mem), 0).unwrap();
        assert!(!cpu.icache[slot].careful, "flags replaced before the reader");
    }

    /// An add whose flags a `psr` push reads after a jump: the scan follows the jump (compiled
    /// code, which does not set the flags, would push stale ones). The fall-through path past the
    /// jump has another add first, so a scan of that path alone says no reader.
    #[test]
    fn flags_read_after_a_jump_keep_the_add_careful() {
        let mut mem = vec![0u8; 0x100];
        mem[..0x10].copy_from_slice(&[
            0xb4, 0xe0, 0x00, 0x00, // 0x00 r0 = r0 + r0
            0xc0, 0xea, 0x02, 0x00, // 0x04 goto 0x0c
            0xb4, 0xe0, 0x00, 0x00, // 0x08 r0 = r0 + r0
            0xf2, 0x04, 0x00, 0x00, // 0x0c [--sp] = {psr, sr4, rete}
        ]);
        let mut bus = FlatRam::new(mem);
        assert!(flags_read_later(&mut bus, 0x04), "the push after the jump reads them");
        assert!(!flags_read_later(&mut bus, 0x0c + 2), "nothing after the push reads them");
        let mut cpu = Cpu::new(0);
        let slot = cpu.fetch(&mut bus).unwrap();
        assert!(cpu.icache[slot].flags_live && cpu.icache[slot].careful, "the add stays on the careful path");
    }

    #[test]
    fn pair_head_and_not_shifted_runs() {
        // `94 f1 13 13` r1 = r1 & ~(1 << r3) #, then a nop (DUELL 0.2.20, when HOME is pressed):
        // the pair-head (f1) twin of `94 e1 .3 ..`, missing from the table
        let mut mem = vec![0u8; 0x100];
        mem[..6].copy_from_slice(&[0x94, 0xf1, 0x13, 0x13, 0x00, 0x00]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[1] = 0xff;
        cpu.regs[3] = 3;
        cpu.step(&mut ram).unwrap();
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[1], 0xf7, "r1 &= ~(1 << r3)");
        // `94 f1 63 10` r1 = r6 & ~(1 << r0): registers outside the table row's two corpus samples
        let mut mem = vec![0u8; 0x100];
        mem[..6].copy_from_slice(&[0x94, 0xf1, 0x63, 0x10, 0x00, 0x00]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        (cpu.regs[0], cpu.regs[1], cpu.regs[2], cpu.regs[6]) = (2, 0, 0x55, 0xff);
        cpu.step(&mut ram).unwrap();
        cpu.step(&mut ram).unwrap();
        assert_eq!((cpu.regs[1], cpu.regs[2]), (0xfb, 0x55), "r1 = r6 & ~(1 << r0), r2 untouched");
    }

    #[test]
    fn and_not_takes_a_register_above_r7() {
        // `64 e8 5f bc` [r11+92] &= ~r12 (OberVave 1.5.3, at every key release): the table's
        // register field was three bits wide, so r12 didn't decode
        let mut mem = vec![0u8; 0x200];
        mem[..4].copy_from_slice(&[0x64, 0xe8, 0x5f, 0xbc]);
        mem[0x15c..0x160].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[11] = 0x100;
        cpu.regs[12] = 0x0000_ff00;
        cpu.step(&mut ram).unwrap();
        assert_eq!(ram.read32(0x15c), 0xffff_00ff, "[r11+92] &= ~r12");
    }

    #[test]
    fn corpus_unseen_offsets_decode() {
        // forms whose table classes fitted only the corpus operands (found running m6502 and the
        // SID player firmware): `6c e8 18 01` [r0+24] <<= 1, `64 e8 fe 10` [r1+-4] &= r0,
        // `d1 ed 41 0e` h[r4++=480] = r0
        let mut mem = vec![0u8; 0x400];
        mem[..12].copy_from_slice(&[0x6c, 0xe8, 0x18, 0x01, 0x64, 0xe8, 0xfe, 0x10, 0xd1, 0xed, 0x41, 0x0e]);
        mem[0x118..0x11c].copy_from_slice(&0x4000_0001u32.to_le_bytes());
        mem[0x1fc..0x200].copy_from_slice(&0xffff_00ffu32.to_le_bytes());
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[0] = 0x100;
        cpu.step(&mut ram).unwrap();
        assert_eq!(ram.read32(0x118), 0x8000_0002, "[r0+24] <<= 1");
        cpu.regs[1] = 0x200;
        cpu.regs[0] = 0x0f0f_0f0f;
        cpu.step(&mut ram).unwrap();
        assert_eq!(ram.read32(0x1fc), 0x0f0f_000f, "[r1+-4] &= r0");
        cpu.regs[4] = 0x300;
        cpu.regs[0] = 0xbeef;
        cpu.step(&mut ram).unwrap();
        assert_eq!(ram.read16(0x300), 0xbeef);
        assert_eq!(cpu.regs[4], 0x300 + 480, "post-increment by 480");
    }

    #[test]
    fn word_negative_offsets() {
        // `d7 ec 51 38` [r5+-128] = r3 (Felucca 1.6), `d5 ec 10 2e` r2 = [r1+-544],
        // `d4 ec eb ef` [++r14=-776] = r14: byte0 bit 2 is the offset's sign
        let mut mem = vec![0u8; 0x800];
        mem[..12].copy_from_slice(&[0xd7, 0xec, 0x51, 0x38, 0xd5, 0xec, 0x10, 0x2e, 0xd4, 0xec, 0xeb, 0xef]);
        mem[0x500..0x504].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[5] = 0x480;
        cpu.regs[3] = 0xcafe_f00d;
        cpu.step(&mut ram).unwrap();
        assert_eq!(ram.read32(0x400), 0xcafe_f00d, "[r5+-128] = r3");
        assert_eq!(cpu.regs[5], 0x480, "no write-back");
        cpu.regs[1] = 0x720;
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[2], 0x1234_5678, "r2 = [r1+-544]");
        cpu.regs[14] = 0x708;
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[14], 0x400, "pre-increment by -776");
    }

    #[test]
    fn sreg_moves_any_register() {
        // `64 e0 80 40` reti = r4, `64 e0 00 20` r2 = reti (ChoralRoot 0.21), `64 e0 80 13`
        // rets = r1: the corpora had reti read into r0 and rets written from r0 only
        let mut mem = vec![0u8; 0x100];
        mem[..12].copy_from_slice(&[0x64, 0xe0, 0x80, 0x40, 0x64, 0xe0, 0x00, 0x20, 0x64, 0xe0, 0x80, 0x13]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[4] = 0x0201_2345;
        cpu.regs[1] = 0x0200_0abc;
        for _ in 0..3 {
            cpu.step(&mut ram).unwrap();
        }
        assert_eq!(cpu.regs[2], 0x0201_2345, "r2 = reti");
        assert_eq!(cpu.rets, 0x0200_0abc, "rets = r1");
    }

    /// Felucca's interrupt guard (motion.c): `r1 = icfg; cli; icfg = r1` puts the global enable
    /// back, stepped, interpreted and compiled; a value with bit 9 clear turns it off.
    #[test]
    fn icfg_restores_the_global_enable() {
        // `64 e0 00 1b` r1 = icfg, `60 00` cli, `64 e0 80 1b` icfg = r1 (Rainbow Mode 1.1.5)
        let code = [0x64u8, 0xe0, 0x00, 0x1b, 0x60, 0x00, 0x64, 0xe0, 0x80, 0x1b];
        for how in ["step", "interpreted", "jit"] {
            for on in [true, false] {
                let mut mem = vec![0u8; 0x100];
                mem[..code.len()].copy_from_slice(&code);
                let mut ram = FlatRam::new(mem);
                let mut cpu = Cpu::new(0);
                cpu.ie = on;
                cpu.jit_on = how == "jit";
                if how == "step" {
                    for _ in 0..3 { cpu.step(&mut ram).unwrap(); }
                } else {
                    let mut n = 0;
                    while n < 3 {
                        n += match cpu.run_fast(&mut ram, 3 - n).unwrap() { (0, _) => { cpu.step(&mut ram).unwrap(); 1 } (k, _) => k };
                    }
                }
                assert_eq!(cpu.pc, code.len() as u32, "{how}");
                assert_eq!(cpu.ie, on, "{how}: ie {on} after the guard");
            }
        }
    }

    #[test]
    fn if_block_with_else() {
        // `1d ea 80 a6` if ((r13 & r6) != 0) { 3 units } else { 2 units } (Quest 0.21), then
        // `c0 21`..`c4 21` r0..r4 += 1 as the bodies and `c5 21` r5 += 1 after them
        let mut mem = vec![0u8; 0x100];
        mem[..4].copy_from_slice(&[0x1d, 0xea, 0x80, 0xa6]);
        for r in 0..6u8 {
            mem[4 + 2 * r as usize..6 + 2 * r as usize].copy_from_slice(&[0xc0 | r, 0x21]);
        }
        for (r13, want) in [(0x10u32, [1, 1, 1, 0, 0, 1]), (0x01, [0, 0, 0, 1, 1, 1])] {
            let mut ram = FlatRam::new(mem.clone());
            let mut cpu = Cpu::new(0);
            cpu.regs[13] = r13;
            cpu.regs[6] = 0x30;
            while cpu.pc != 16 {
                cpu.step(&mut ram).unwrap();
            }
            assert_eq!(cpu.regs[..6], want, "r13 = {r13:#x}");
        }
    }

    /// Two programs with an if-else block at `base + 0x80`: `b0 e8 00 10` if (r0 != 0) { 2 units }
    /// else { `40 20` r0 = 0 }, then `c6 21` r6 += 1 past it; f before the block at `base + 0x10`:
    /// `c5 21` r5 += 1, then `c5 21` again (tail) or `80 00` rts (call). The then-part is
    /// `ff ea ..` goto f (clang's tail call, as fm1-dock's c1_wait) or `bf ea ..` call f.
    fn then_part_programs(base: u32) -> [(Vec<u8>, bool); 2] {
        let off = ((0x10i32 - 0x88) / 2) as i16 as u16;
        [(0xffu8, false), (0xbf, true)].map(|(op, call)| {
            let mut mem = vec![0u8; 0x100];
            mem[0x10..0x14].copy_from_slice(&[0xc5, 0x21, if call { 0x80 } else { 0xc5 }, if call { 0x00 } else { 0x21 }]);
            mem[0x80..0x84].copy_from_slice(&[0xb0, 0xe8, 0x00, 0x10]);
            mem[0x84..0x88].copy_from_slice(&[op, 0xea, off as u8, (off >> 8) as u8]);
            mem[0x88..0x8a].copy_from_slice(&[0x40, 0x20]);
            mem[0x8a..0x8c].copy_from_slice(&[0xc6, 0x21]);
            // (each run ends at a goto to itself: `ff ea fe ff`)
            mem[0x14..0x18].copy_from_slice(&[0xff, 0xea, 0xfe, 0xff]);
            mem[0x8c..0x90].copy_from_slice(&[0xff, 0xea, 0xfe, 0xff]);
            let _ = base;
            (mem, call)
        })
    }

    #[test]
    fn a_goto_back_out_of_a_then_part_ends_the_construct() {
        // The goto leaves the construct: its pending skip (and the interrupts it holds back) must
        // not follow it into f, where fm1-dock's audio ISR waited ~0.7 ms for the other core with
        // TIMER5 held off. A call into f returns into the construct: the skip stays, and at the
        // then-part's end it skips the else part (as before).
        for (mem, call) in then_part_programs(0) {
            let mut ram = FlatRam::new(mem.clone());
            let mut cpu = Cpu::new(0x80);
            cpu.regs[0] = 1;
            cpu.ie = true;
            cpu.sreg_store[0] |= 0x100;
            cpu.step(&mut ram).unwrap();
            assert!(!cpu.pred_skips.is_empty() && !cpu.irq_ready(4), "call {call}: in the then-part, no interrupt");
            cpu.step(&mut ram).unwrap();
            assert_eq!(cpu.pc, 0x10, "call {call}: into f");
            assert_eq!(cpu.pred_skips.is_empty(), !call, "call {call}: a goto leaves the construct, a call does not");
            assert_eq!(cpu.irq_ready(4), !call, "call {call}");
            if call {
                for _ in 0..3 {
                    cpu.step(&mut ram).unwrap(); // r5 += 1, rts (to the then-part's end: the skip), r6 += 1
                }
                assert_eq!((cpu.pc, cpu.regs[0], cpu.regs[5], cpu.regs[6]), (0x8c, 1, 1, 1), "the else part skipped");
                assert!(cpu.pred_skips.is_empty());
            }
            // r0 == 0: the else part, then on past the construct
            let mut ram = FlatRam::new(mem);
            let mut cpu = Cpu::new(0x80);
            for _ in 0..3 {
                cpu.step(&mut ram).unwrap();
            }
            assert_eq!((cpu.pc, cpu.regs[0], cpu.regs[5], cpu.regs[6]), (0x8c, 0, 0, 1), "call {call}: r0 == 0");
            assert!(cpu.pred_skips.is_empty());
        }
    }

    /// `a_goto_back_out_of_a_then_part_ends_the_construct` through run_fast, interpreted and
    /// compiled, from XIP (where blocks are built).
    #[cfg(native_jit)]
    #[test]
    fn a_goto_back_out_of_a_then_part_ends_the_construct_in_blocks() {
        for (mem, call) in then_part_programs(0x0200_0000) {
            for jit in [false, true] {
                let mut xip = mem.clone();
                xip.resize(0x200, 0);
                let mut bus = Clocked { ram: vec![0; 0x1000], xip, cycles: 0, next_due: u64::MAX, irq: false, due_on_mmio: false, writes: 0 };
                let mut cpu = Cpu::new(0x0200_0080);
                (cpu.jit_on, cpu.profile) = (jit, true);
                let mut fast = 0;
                // (round after round, so that the blocks get hot and compiled)
                for round in 0..300u32 {
                    (cpu.pc, cpu.regs[0], cpu.regs[5], cpu.regs[6]) = (0x0200_0080, 1, 0, 0);
                    cpu.pred_skips.clear();
                    for _ in 0..12 {
                        match cpu.run_fast(&mut bus, 1 + round % 4).unwrap().0 {
                            0 => cpu.step(&mut bus).map(|_| ()).unwrap(),
                            k => fast += k,
                        }
                    }
                    // at the end, round its goto to itself: the goto's case in f, the call's past the block
                    let (end, r5) = if call { (0x0200_008c, 1) } else { (0x0200_0014, 2) };
                    assert_eq!((cpu.pc, cpu.regs[0], cpu.regs[5], cpu.regs[6]), (end, 1, r5, call as u32),
                               "call {call}, jit {jit}, round {round}");
                    assert!(cpu.pred_skips.is_empty(), "call {call}, jit {jit}, round {round}: the construct is over");
                }
                assert!(fast > 0 && (!jit || cpu.jit_insns > 0), "call {call}, jit {jit}: {fast} fast, {} compiled", cpu.jit_insns);
            }
        }
    }

    /// `if (r0 != 0) { call g } else { r0 = 0 }; r6 += 1`, with g at 0x60 a function of its own
    /// with a loop in it (`call h; goto +; -: pc = [sp++]; +: goto -`) and h at 0x10 (`r5 += 1;
    /// rts`): the construct is in progress all through g (interrupts wait), g's jump back is by
    /// an instruction outside the then-part and ends nothing, and g's return to the then-part's
    /// end skips the else part. (A skip that knew only its end once took g's jump back, near
    /// that end with `rets` left in g by h's return, for a goto out of the then-part.)
    fn callee_loop_program() -> Vec<u8> {
        // (`xx ea lo hi`: the offset in halfwords from the next pc, its high bits in xx's low
        // five: `ff` a goto back, `c0` a goto on, `bf` a call back)
        let jump = |op: u8, next_pc: u32, to: u32| { let d = ((to as i32 - next_pc as i32) / 2) as i16 as u16; [op, 0xea, d as u8, (d >> 8) as u8] };
        let mut mem = vec![0u8; 0x200];
        mem[0x10..0x14].copy_from_slice(&[0xc5, 0x21, 0x80, 0x00]);
        mem[0x60..0x62].copy_from_slice(&[0x10, 0x04]); // [--sp] = rets
        mem[0x62..0x66].copy_from_slice(&jump(0xbf, 0x66, 0x10));
        mem[0x66..0x6a].copy_from_slice(&jump(0xc0, 0x6a, 0x6c));
        mem[0x6a..0x6c].copy_from_slice(&[0x00, 0x04]); // pc = [sp++]
        mem[0x6c..0x70].copy_from_slice(&jump(0xff, 0x70, 0x6a));
        mem[0x80..0x84].copy_from_slice(&[0xb0, 0xe8, 0x00, 0x10]);
        mem[0x84..0x88].copy_from_slice(&jump(0xbf, 0x88, 0x60));
        mem[0x88..0x8a].copy_from_slice(&[0x40, 0x20]);
        mem[0x8a..0x8c].copy_from_slice(&[0xc6, 0x21]);
        mem[0x8c..0x90].copy_from_slice(&[0xff, 0xea, 0xfe, 0xff]);
        mem
    }

    #[test]
    fn a_callees_own_loop_does_not_end_the_callers_construct() {
        let mut ram = FlatRam::new(callee_loop_program());
        let mut cpu = Cpu::new(0x80);
        (cpu.regs[0], cpu.sp, cpu.ie) = (1, 0x200, true);
        cpu.sreg_store[0] |= 0x100;
        let steps = [0x84, 0x60, 0x62, 0x10, 0x12, 0x66, 0x6c, 0x6a];
        for (n, &at) in steps.iter().enumerate() {
            cpu.step(&mut ram).unwrap();
            assert_eq!(cpu.pc, at, "step {n}");
            assert!(!cpu.pred_skips.is_empty() && !cpu.irq_ready(4), "step {n} (pc {:#x}): the construct goes on, no interrupt", cpu.pc);
        }
        cpu.step(&mut ram).unwrap(); // pc = [sp++]: to the then-part's end, over the else part
        assert_eq!((cpu.pc, cpu.regs[0]), (0x8a, 1), "the else part skipped");
        assert!(cpu.pred_skips.is_empty() && cpu.irq_ready(4));
        cpu.step(&mut ram).unwrap();
        assert_eq!((cpu.pc, cpu.regs[0], cpu.regs[5], cpu.regs[6]), (0x8c, 1, 1, 1));
    }

    /// `a_callees_own_loop_does_not_end_the_callers_construct` through run_fast, interpreted and
    /// compiled, from XIP (as `a_goto_back_out_of_a_then_part_ends_the_construct_in_blocks`).
    #[cfg(native_jit)]
    #[test]
    fn a_callees_own_loop_does_not_end_the_callers_construct_in_blocks() {
        for jit in [false, true] {
            let mut xip = callee_loop_program();
            xip.resize(0x200, 0);
            let mut bus = Clocked { ram: vec![0; 0x1000], xip, cycles: 0, next_due: u64::MAX, irq: false, due_on_mmio: false, writes: 0 };
            let mut cpu = Cpu::new(0x0200_0080);
            (cpu.jit_on, cpu.profile) = (jit, true);
            let mut fast = 0;
            for round in 0..300u32 {
                (cpu.pc, cpu.sp, cpu.regs[0], cpu.regs[5], cpu.regs[6]) = (0x0200_0080, 0x1000, 1, 0, 0);
                cpu.pred_skips.clear();
                for _ in 0..16 {
                    match cpu.run_fast(&mut bus, 1 + round % 4).unwrap().0 {
                        0 => cpu.step(&mut bus).map(|_| ()).unwrap(),
                        k => fast += k,
                    }
                }
                assert_eq!((cpu.pc, cpu.sp, cpu.regs[0], cpu.regs[5], cpu.regs[6]), (0x0200_008c, 0x1000, 1, 1, 1), "jit {jit}, round {round}");
                assert!(cpu.pred_skips.is_empty(), "jit {jit}, round {round}: the construct is over");
            }
            assert!(fast > 0 && (!jit || cpu.jit_insns > 0), "jit {jit}: {fast} fast, {} compiled", cpu.jit_insns);
        }
    }

    #[test]
    fn register_increment_accesses() {
        // `de ee 21 10` b[r2++=r0] = r1 (SLOOP 2.6, ORBIT 0.7), `dc ec 02 11` r1 = [++r0=r1],
        // `de ed 93 00` h[r9++=r0] = r0.h
        let mut mem = vec![0u8; 0x100];
        mem[..12].copy_from_slice(&[0xde, 0xee, 0x21, 0x10, 0xdc, 0xec, 0x02, 0x11, 0xde, 0xed, 0x93, 0x00]);
        mem[0x68..0x6c].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        (cpu.regs[0], cpu.regs[1], cpu.regs[2]) = (3, 0xab, 0x40);
        cpu.step(&mut ram).unwrap();
        assert_eq!((ram.read8(0x40), cpu.regs[2]), (0xab, 0x43), "post-increment byte store");
        (cpu.regs[0], cpu.regs[1]) = (0x50, 0x18);
        cpu.step(&mut ram).unwrap();
        assert_eq!((cpu.regs[1], cpu.regs[0]), (0x1234_5678, 0x68), "pre-increment word load");
        (cpu.regs[0], cpu.regs[9]) = (0x0002_0000, 0x70);
        cpu.step(&mut ram).unwrap();
        assert_eq!((ram.read16(0x70), cpu.regs[9]), (2, 0x2_0070), "high half store");
    }

    #[test]
    fn sp_pair_store_and_rev8() {
        // `d0 e9 15 c4` d[sp+1044] = r13_r12 (WaveLoop Kit), `70 e0 00 80` r8 = rev8(r0) (Quest 0.21)
        let mut mem = vec![0u8; 0x600];
        mem[..8].copy_from_slice(&[0xd0, 0xe9, 0x15, 0xc4, 0x70, 0xe0, 0x00, 0x80]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.sp = 0x100;
        (cpu.regs[12], cpu.regs[13], cpu.regs[0]) = (0x1111_1111, 0x2222_2222, 0x1122_3344);
        cpu.step(&mut ram).unwrap();
        assert_eq!((ram.read32(0x514), ram.read32(0x518)), (0x1111_1111, 0x2222_2222));
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[8], 0x4433_2211);
    }

    #[test]
    fn pair_multiply_accumulate() {
        // `fc e1 30 07` r1_r0 += r3 * r7 (u) (compiler-rt's wide multiply)
        let mut mem = vec![0u8; 0x100];
        mem[..4].copy_from_slice(&[0xfc, 0xe1, 0x30, 0x07]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[0] = 0xffff_ffff;
        cpu.regs[1] = 1;
        cpu.regs[3] = 0x1_0000;
        cpu.regs[7] = 0x1_0001;
        cpu.step(&mut ram).unwrap();
        let want = 0x1_ffff_ffffu64 + 0x1_0000u64 * 0x1_0001;
        assert_eq!((cpu.regs[0], cpu.regs[1]), (want as u32, (want >> 32) as u32));
    }

    #[test]
    fn pair_multiply_subtract() {
        // `fe e1 40 61` r7_r6 -= r4 * r1 (u), `fe e1 40 71` the same (s): bit 28 signed, the
        // fields of `f8 e1` (vendor assembler; clang 4.0.1 emits it for `acc - (u64)a * b`)
        let run = |code: [u8; 4], regs: &[(usize, u32)]| {
            let mut mem = vec![0u8; 0x100];
            mem[..4].copy_from_slice(&code);
            let mut ram = FlatRam::new(mem);
            let mut cpu = Cpu::new(0);
            for &(r, v) in regs {
                cpu.regs[r] = v;
            }
            cpu.step(&mut ram).unwrap();
            assert_eq!(cpu.pc, 4);
            cpu.regs
        };
        let pair = |r: [u32; 16], p: usize| (r[2 * p + 1] as u64) << 32 | r[2 * p] as u64;
        // unsigned, borrowing from the high word
        let r = run([0xfe, 0xe1, 0x40, 0x61], &[(6, 5), (7, 1), (4, 0xffff_ffff), (1, 3)]);
        assert_eq!(pair(r, 3), 0x1_0000_0005u64.wrapping_sub(0xffff_ffffu64 * 3));
        // signed: -2 * 0x7fffffff subtracted adds
        let r = run([0xfe, 0xe1, 0x40, 0x71], &[(6, 10), (7, 0), (4, (-2i32) as u32), (1, 0x7fff_ffff)]);
        assert_eq!(pair(r, 3), (10i64 - (-2i64) * 0x7fff_ffff) as u64);
        // the same operands unsigned: a different result
        let r = run([0xfe, 0xe1, 0x40, 0x61], &[(6, 10), (7, 0), (4, (-2i32) as u32), (1, 0x7fff_ffff)]);
        assert_eq!(pair(r, 3), 10u64.wrapping_sub(0xffff_fffeu64 * 0x7fff_ffff));
        // `fe e1 f0 1e` r1_r0 -= r15 * r14 (s), `fe e1 00 ed` r15_r14 -= r0 * r13 (u): the end pairs
        let r = run([0xfe, 0xe1, 0xf0, 0x1e], &[(0, 0), (1, 0), (15, (-7i32) as u32), (14, 9)]);
        assert_eq!(pair(r, 0), 63);
        let r = run([0xfe, 0xe1, 0x00, 0xed], &[(14, 1), (15, 0), (0, 0x8000_0000), (13, 4)]);
        assert_eq!(pair(r, 7), 1u64.wrapping_sub(0x2_0000_0000));
        // a factor inside the pair reads its old value: `fe e1 60 67` r7_r6 -= r6 * r7 (u)
        let r = run([0xfe, 0xe1, 0x60, 0x67], &[(6, 3), (7, 2)]);
        assert_eq!(pair(r, 3), 0x2_0000_0003u64 - 6);
    }

    #[test]
    fn pair_multiply_subtract_dual_issue() {
        // `fe f1 40 61` r7_r6 -= r4 * r1 (u) #  paired with `87 20` [sp] = r7: the store sees
        // the old r7 (the pair convention of pair_head_byte_extract)
        let mut mem = vec![0u8; 0x200];
        mem[..6].copy_from_slice(&[0xfe, 0xf1, 0x40, 0x61, 0x87, 0x20]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.sp = 0x100;
        (cpu.regs[6], cpu.regs[7], cpu.regs[4], cpu.regs[1]) = (0, 0x10, 0x1000, 0x1000);
        while cpu.pc < 6 {
            cpu.step(&mut ram).unwrap();
        }
        assert_eq!((cpu.regs[6], cpu.regs[7]), (0xff00_0000, 0xf));
        assert_eq!(ram.read32(0x100), 0x10);
    }

    #[test]
    fn multiply_immediate_dual_issue() {
        // `ea f1 16 40` r10 = r4 * 0x16 #  paired with `87 20` [sp] = r7 (ADD-1's bank index),
        // and `e3 f1 ff 25` r3 = r2 * 0x1fe00000 # (a rotated immediate) with `85 21` [sp+4] = r5
        let mut mem = vec![0u8; 0x200];
        mem[..12].copy_from_slice(&[0xea, 0xf1, 0x16, 0x40, 0x87, 0x20, 0xe3, 0xf1, 0xff, 0x25, 0x85, 0x21]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.sp = 0x100;
        (cpu.regs[4], cpu.regs[7], cpu.regs[2], cpu.regs[5]) = (3, 0x55, 5, 0x66);
        while cpu.pc < 12 {
            cpu.step(&mut ram).unwrap();
        }
        assert_eq!(cpu.regs[10], 66);
        assert_eq!(cpu.regs[3], 5u32.wrapping_mul(0x1fe0_0000));
        assert_eq!((ram.read32(0x100), ram.read32(0x104)), (0x55, 0x66));
    }

    #[test]
    fn pair_store_and_load() {
        // `50 ec 35 40` d[r3+4] = r5_r4 ; `50 ec 00 20` r3_r2 = d[r0+0]
        let mut mem = vec![0u8; 0x100];
        mem[..8].copy_from_slice(&[0x50, 0xec, 0x35, 0x40, 0x50, 0xec, 0x00, 0x20]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[3] = 0x40;
        cpu.regs[4] = 0x1000;
        cpu.regs[5] = 0x2000;
        cpu.step(&mut ram).unwrap();
        assert_eq!(ram.read32(0x44), 0x1000, "low register at the lower address");
        assert_eq!(ram.read32(0x48), 0x2000);
        cpu.regs[0] = 0x44;
        cpu.step(&mut ram).unwrap();
        assert_eq!((cpu.regs[2], cpu.regs[3]), (0x1000, 0x2000));
    }

    #[test]
    fn pair_head_byte_extract() {
        // `00 d7` r0 = r0.b0 (u) #  paired with `c8 4b` b[r4+11] = r0: the
        // store sees the old r0, r0 ends up as the low byte
        let mut mem = vec![0u8; 0x100];
        mem[..4].copy_from_slice(&[0x00, 0xd7, 0xc8, 0x4b]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[0] = 0xDEAD_BEEF;
        cpu.regs[4] = 0x40;
        cpu.step(&mut ram).unwrap();
        cpu.step(&mut ram).unwrap();
        assert_eq!(ram.read8(0x4b), 0xEF);
        assert_eq!(cpu.regs[0], 0xEF);
    }

    #[test]
    fn shift_imm_high_source_register() {
        // `c0 e1 c8 10` r1 = r12 << 8 and `c0 e1 c1 38` r3 = r12 >> 1
        let mut mem = vec![0u8; 0x100];
        mem[..8].copy_from_slice(&[0xc0, 0xe1, 0xc8, 0x10, 0xc0, 0xe1, 0xc1, 0x38]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[12] = 0x3;
        cpu.step(&mut ram).unwrap();
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[1], 0x300);
        assert_eq!(cpu.regs[3], 0x1);
    }

    #[test]
    fn sp_pair_store_large_offset() {
        // `d0 e9 21 a4` d[sp+1056] = r11_r10 (Felucca ui_leds), read back with
        // `d0 e9 c0 01` r1_r0 = d[sp+448] (pair loads in the corpora stop at 448)
        let mut mem = vec![0u8; 0x800];
        mem[..8].copy_from_slice(&[0xd0, 0xe9, 0x21, 0xa4, 0xd0, 0xe9, 0xc0, 0x01]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.sp = 0x100;
        cpu.regs[10] = 0x1122_3344;
        cpu.regs[11] = 0x5566_7788;
        cpu.step(&mut ram).unwrap();
        assert_eq!(ram.read32(0x100 + 1056), 0x1122_3344);
        assert_eq!(ram.read32(0x100 + 1060), 0x5566_7788);
        cpu.sp = 0x100 + 1056 - 448;
        cpu.step(&mut ram).unwrap();
        assert_eq!((cpu.regs[0], cpu.regs[1]), (0x1122_3344, 0x5566_7788));
    }

    #[test]
    fn pair_access_modes() {
        // the `5x ec` register-pair modes (decode_pair_mem): pre-increment by a register,
        // a negative offset, post-increment, an index register
        let mut mem = vec![0u8; 0x400];
        mem[..16].copy_from_slice(&[
            0x5c, 0xec, 0x13, 0x40, // d[++r1=r0] = r5_r4
            0x57, 0xec, 0x19, 0x8f, // d[r1+-8] = r9_r8
            0x58, 0xec, 0x09, 0x20, // d[r0++=8] = r3_r2
            0x58, 0xec, 0x13, 0x82, // d[r1+r2] = r9_r8
        ]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[0] = 0x40;
        cpu.regs[1] = 0x100;
        (cpu.regs[4], cpu.regs[5]) = (0x11, 0x22);
        (cpu.regs[8], cpu.regs[9]) = (0x33, 0x44);
        (cpu.regs[2], cpu.regs[3]) = (0x55, 0x66);
        cpu.step(&mut ram).unwrap();
        assert_eq!((cpu.regs[1], ram.read32(0x140), ram.read32(0x144)), (0x140, 0x11, 0x22));
        cpu.step(&mut ram).unwrap();
        assert_eq!((ram.read32(0x138), ram.read32(0x13c)), (0x33, 0x44));
        cpu.step(&mut ram).unwrap();
        assert_eq!((cpu.regs[0], ram.read32(0x40), ram.read32(0x44)), (0x48, 0x55, 0x66));
        cpu.regs[2] = 0x80;
        cpu.step(&mut ram).unwrap();
        assert_eq!((ram.read32(0x1c0), ram.read32(0x1c4)), (0x33, 0x44));
    }

    #[test]
    fn bitfield_extract_is_not_an_add() {
        // `b4 e1 a0 4b` r4 = uextra(r4, p:23, l:8): a float's exponent (X0X fm_log2f)
        let mut mem = vec![0u8; 0x100];
        mem[..4].copy_from_slice(&[0xb4, 0xe1, 0xa0, 0x4b]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[4] = 1e-5f32.to_bits();
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[4], 0x6e);
    }

    #[test]
    fn fpu_ops_and_float_compare() {
        // `3f e5 30 57` r5 = r3 + r7 (f), `3f e5 1f 65` r6 = ftoi(r5), `3f e5 8f 26` r2 = itof(r6),
        // `12 e9 80 01` iff (r2 u>= r1) { `40 20` r0 = 0 }
        let mut mem = vec![0u8; 0x100];
        mem[..18].copy_from_slice(&[
            0x3f, 0xe5, 0x30, 0x57, 0x3f, 0xe5, 0x1f, 0x65, 0x3f, 0xe5, 0x8f, 0x26,
            0x12, 0xe9, 0x80, 0x01, 0x40, 0x20,
        ]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[3] = 2.5f32.to_bits();
        cpu.regs[7] = (-7.75f32).to_bits();
        cpu.regs[1] = 0.0f32.to_bits();
        cpu.regs[0] = 1;
        for _ in 0..4 {
            cpu.step(&mut ram).unwrap();
        }
        assert_eq!(f32::from_bits(cpu.regs[5]), -5.25);
        assert_eq!(cpu.regs[6] as i32, -5);
        assert_eq!(f32::from_bits(cpu.regs[2]), -5.0);
        // -5.0 u>= 0.0 is false: the block is skipped, r0 stays 1
        assert_eq!((cpu.pc, cpu.regs[0]), (18, 1));
    }

    #[test]
    fn short_shift_by_32() {
        // `df a0` r7 = r5 >>> 32, `32 a0` r2 = r3 << 32 (count field 0 = 32)
        let mut mem = vec![0u8; 0x100];
        mem[..4].copy_from_slice(&[0xdf, 0xa0, 0x32, 0xa0]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[5] = 0x8000_1234;
        cpu.regs[3] = 0xffff_ffff;
        cpu.step(&mut ram).unwrap();
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[7], 0xffff_ffff);
        assert_eq!(cpu.regs[2], 0);
    }

    #[test]
    fn push_pop_register_mask() {
        // `d9 e8 f0 0d` [--sp] = {rets, r11, r10, r8-r4}, `d5 e8 f0 0d` the matching pop
        let mut mem = vec![0u8; 0x200];
        mem[..8].copy_from_slice(&[0xd9, 0xe8, 0xf0, 0x0d, 0xd5, 0xe8, 0xf0, 0x0d]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.sp = 0x200;
        cpu.rets = 0x44;
        for r in 0..16 {
            cpu.regs[r] = 0x100 + r as u32;
        }
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.sp, 0x200 - 4 * 8);
        assert_eq!(ram.read32(0x1fc), 0x44); // rets on top
        assert_eq!(ram.read32(0x1f8), 0x10b); // then r11
        assert_eq!(ram.read32(0x200 - 32), 0x104); // r4 lowest
        for r in 0..16 {
            cpu.regs[r] = 0;
        }
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.pc, 0x44);
        assert_eq!(cpu.sp, 0x200);
        assert_eq!((cpu.regs[4], cpu.regs[8], cpu.regs[9], cpu.regs[10], cpu.regs[11]), (0x104, 0x108, 0, 0x10a, 0x10b));
    }

    #[test]
    fn rets_mask_pop_and_pair_micro_ops() {
        // `d9 e8 f0 0d` push {rets, r11, r10, r8-r4}, `d1 e8 f0 0d` pop them back with rets
        // (route 27's 4-byte form: no jump), then `40 15` r1_r0 = r5_r4 and `10 04` [--sp] = rets
        let mut mem = vec![0u8; 0x200];
        mem[..12].copy_from_slice(&[0xd9, 0xe8, 0xf0, 0x0d, 0xd1, 0xe8, 0xf0, 0x0d, 0x40, 0x15, 0x10, 0x04]);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.sp = 0x200;
        cpu.rets = 0x44;
        for r in 0..16 {
            cpu.regs[r] = 0x100 + r as u32;
        }
        cpu.step(&mut ram).unwrap();
        for r in 0..16 {
            cpu.regs[r] = 0;
        }
        cpu.rets = 0;
        cpu.step(&mut ram).unwrap();
        assert_eq!((cpu.pc, cpu.sp, cpu.rets), (8, 0x200, 0x44));
        assert_eq!((cpu.regs[4], cpu.regs[8], cpu.regs[9], cpu.regs[10], cpu.regs[11]), (0x104, 0x108, 0, 0x10a, 0x10b));
        cpu.regs[5] = 0x55;
        cpu.step(&mut ram).unwrap();
        assert_eq!((cpu.regs[1], cpu.regs[0]), (0x55, 0x104));
        cpu.step(&mut ram).unwrap();
        assert_eq!((cpu.sp, ram.read32(0x1fc)), (0x1fc, 0x44));
    }

    #[test]
    fn shift_and_add_fields() {
        // `a2 a2` r2 = r2 >> 2 ; `9a a2` r2 = r1 >>> 2 ; `93 1c` r3 = r1 + r2
        let mut ram = FlatRam::new(vec![0xa2, 0xa2, 0x9a, 0xa2, 0x93, 0x1c, 0, 0, 0, 0, 0, 0]);
        let mut cpu = Cpu::new(0);
        cpu.regs[1] = 0xffff_fff0;
        cpu.regs[2] = 0x9e7c;
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[2], 0x9e7c >> 2);
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[2], 0xffff_fffc);
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.regs[3], 0xffff_fff0u32.wrapping_add(0xffff_fffc));
    }

    #[test]
    fn rep_block_runs_register_count_times() {
        // CRT .data copy shape: `12 03` rep 4 r2 { `13 05` r3 = [r1++=4];
        // `c3 05` [r4++=4] = r3 } then `80 00` rts
        let mut mem = vec![0u8; 0x100];
        mem[..8].copy_from_slice(&[0x12, 0x03, 0x13, 0x05, 0xc3, 0x05, 0x80, 0x00]);
        for i in 0..3u32 {
            mem[(0x40 + 4 * i) as usize..][..4].copy_from_slice(&(0x1111 * (i + 1)).to_le_bytes());
        }
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        cpu.regs[1] = 0x40;
        cpu.regs[2] = 3;
        cpu.regs[4] = 0x80;
        for _ in 0..7 {
            cpu.step(&mut ram).unwrap();
        }
        assert_eq!(cpu.pc, 6, "block ran 3 times then fell through");
        assert_eq!(cpu.regs[2], 0);
        assert_eq!(cpu.regs[1], 0x4c);
        assert_eq!(cpu.regs[4], 0x8c);
        assert_eq!(ram.read32(0x88), 0x3333);
        // zero count skips the block entirely
        let mut ram = FlatRam::new(vec![0x02, 0x03, 0xb1, 0x05, 0x80, 0x00, 0, 0, 0, 0]);
        let mut cpu = Cpu::new(0);
        cpu.regs[2] = 0;
        cpu.step(&mut ram).unwrap();
        assert_eq!(cpu.pc, 4);
    }

    /// Encodings checked against the vendor objdump (tools/fuzz_decode.py): one instruction at 0,
    /// registers r0..r15 = 0x1000 + 0x100 * n, memory word at a = a (pokes: other words, to see a
    /// load's sign).
    fn run1(bytes: &[u8], pokes: &[(usize, u32)]) -> (Cpu, FlatRam) {
        let mut mem = vec![0u8; 0x4000];
        for a in (0x40..0x4000).step_by(4) {
            mem[a..a + 4].copy_from_slice(&(a as u32).to_le_bytes());
        }
        for &(a, v) in pokes {
            mem[a..a + 4].copy_from_slice(&v.to_le_bytes());
        }
        mem[..bytes.len()].copy_from_slice(bytes);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        for (n, r) in cpu.regs.iter_mut().enumerate() {
            *r = 0x1000 + 0x100 * n as u32;
        }
        cpu.sp = 0x2000;
        cpu.step(&mut ram).unwrap();
        (cpu, ram)
    }

    /// One instruction with chosen registers (the rest as `run1` sets them).
    fn run_regs(bytes: &[u8], regs: &[(usize, u32)]) -> Cpu {
        let mut mem = vec![0u8; 0x4000];
        mem[..bytes.len()].copy_from_slice(bytes);
        let mut ram = FlatRam::new(mem);
        let mut cpu = Cpu::new(0);
        for (n, r) in cpu.regs.iter_mut().enumerate() {
            *r = 0x1000 + 0x100 * n as u32;
        }
        for &(r, v) in regs {
            cpu.regs[r] = v;
        }
        cpu.sp = 0x2000;
        cpu.step(&mut ram).unwrap();
        cpu
    }

    #[test]
    fn pair_head_special_register_moves() {
        // `64 f0 00 23` r2 = rets #, `64 f0 80 a3` rets = r10 #, `64 f0 00 ae` r10 = sp #, each
        // closed by a nop (objdump of the vendor toolchain; CTL 1.0, 386DX, MC-1 halted on them)
        let run = |bytes: [u8; 4], set: &dyn Fn(&mut Cpu)| {
            let mut mem = vec![0u8; 0x100];
            mem[..4].copy_from_slice(&bytes);
            let mut ram = FlatRam::new(mem);
            let mut cpu = Cpu::new(0);
            set(&mut cpu);
            cpu.step(&mut ram).unwrap();
            cpu.step(&mut ram).unwrap();
            cpu
        };
        assert_eq!(run([0x64, 0xf0, 0x00, 0x23], &|c| c.rets = 0x0201_2345).regs[2], 0x0201_2345);
        assert_eq!(run([0x64, 0xf0, 0x80, 0xa3], &|c| c.regs[10] = 0x0200_1000).rets, 0x0200_1000);
        assert_eq!(run([0x64, 0xf0, 0x00, 0xae], &|c| c.sp = 0x1f80).regs[10], 0x1f80);
    }

    #[test]
    fn bit_test_branches_both_ways() {
        // `60 ff 01 00 fe ff` if ((r0 & 0x1) == 0) goto -4 (from the next instruction: pc 2);
        // `61 ff 03 50 00 80` if ((r5 & 0x3) != 0) goto -65536 (Kawa's `60 ff` halted; the
        // table's `61 ff` class took forward offsets only)
        let run = |bytes: [u8; 6], r: usize, v: u32| {
            let mut mem = vec![0u8; 0x100];
            mem[0x80..0x86].copy_from_slice(&bytes);
            let mut ram = FlatRam::new(mem);
            let mut cpu = Cpu::new(0x80);
            cpu.regs[r] = v;
            cpu.step(&mut ram).unwrap();
            cpu.pc
        };
        assert_eq!(run([0x60, 0xff, 0x01, 0x00, 0xfe, 0xff], 0, 2), 0x82);
        assert_eq!(run([0x60, 0xff, 0x01, 0x00, 0xfe, 0xff], 0, 1), 0x86);
        assert_eq!(run([0x61, 0xff, 0x03, 0x50, 0x00, 0x80], 5, 2), 0x86u32.wrapping_sub(0x10000));
        assert_eq!(run([0x61, 0xff, 0x03, 0x50, 0x00, 0x80], 5, 4), 0x86);
    }

    #[test]
    fn dsp_encodings() {
        // FM1 Open's engine (libfm1_engine.a); the encodings from the vendor assembler
        // (work/probe/dsp.S). `de e1 6d 39` r3 = r7_r6 >> 29 (s): shifted, then saturated
        assert_eq!(run_regs(&[0xde, 0xe1, 0x6d, 0x39], &[(7, 1), (6, 0)]).regs[3], 8);
        assert_eq!(run_regs(&[0xde, 0xe1, 0x6d, 0x39], &[(7, 0x4000_0000), (6, 0)]).regs[3], i32::MAX as u32);
        assert_eq!(run_regs(&[0xde, 0xe1, 0x6d, 0x39], &[(7, 0xffff_ffff), (6, 0xe000_0000)]).regs[3], u32::MAX);
        assert_eq!(run_regs(&[0xde, 0xe1, 0x6d, 0x39], &[(7, 0x8000_0000), (6, 0)]).regs[3], i32::MIN as u32);
        // `de e1 01 02` r0 = r1_r0 >> 33 (u)
        assert_eq!(run_regs(&[0xde, 0xe1, 0x01, 0x02], &[(1, 0xffff_fffe), (0, 5)]).regs[0], 0x7fff_ffff);
        // `d4 f1 05 eb` r15_r14 <c>= #i (a carry rotate, bit 27) is not the plain pair rotate: it halts
        let mut ram = FlatRam::new([0xd4, 0xf1, 0x05, 0xeb].iter().copied().chain([0; 0x3ffc]).collect());
        assert!(Cpu::new(0).step(&mut ram).is_err());
        // `39 e4 20 13` r1 = r2 + r3 (ssat), `39 e4 28 13` r1 = r2 - r3 (ssat)
        assert_eq!(run_regs(&[0x39, 0xe4, 0x20, 0x13], &[(2, 0x7fff_fff0), (3, 0x100)]).regs[1], 0x7fff_ffff);
        assert_eq!(run_regs(&[0x39, 0xe4, 0x20, 0x13], &[(2, 5), (3, -7i32 as u32)]).regs[1], -2i32 as u32);
        assert_eq!(run_regs(&[0x39, 0xe4, 0x28, 0x13], &[(2, 0x8000_0010), (3, 0x100)]).regs[1], 0x8000_0000);
        // `39 e4 e0 f9` r15 = r14 + r9 (ssat)
        assert_eq!(run_regs(&[0x39, 0xe4, 0xe0, 0xf9], &[(14, 3), (9, 4)]).regs[15], 7);
        // `c4 e1 73 61` r6 = r7 <> 19, `c4 e1 af 91` r9 = r10 <> 31
        assert_eq!(run_regs(&[0xc4, 0xe1, 0x73, 0x61], &[(7, 0x0008_0001)]).regs[6], 0x0008_0001u32.rotate_right(19));
        assert_eq!(run_regs(&[0xc4, 0xe1, 0xaf, 0x91], &[(10, 1)]).regs[9], 2);
        // `78 e0 01 35` r3 = sat16(r5) (s), `78 e0 00 35` (u)
        assert_eq!(run_regs(&[0x78, 0xe0, 0x01, 0x35], &[(5, 0x12345)]).regs[3], 32767);
        assert_eq!(run_regs(&[0x78, 0xe0, 0x00, 0x35], &[(5, -5i32 as u32)]).regs[3], 0);
        // `20 e0 d0 ec` r14.b3 = 208, `20 e0 ff 24` r2.b1 = 255
        assert_eq!(run_regs(&[0x20, 0xe0, 0xd0, 0xec], &[(14, 0x1234_5678)]).regs[14], 0xd034_5678);
        assert_eq!(run_regs(&[0x20, 0xe0, 0xff, 0x24], &[(2, 0x1234_5678)]).regs[2], 0x1234_ff78);
        // `94 e1 13 16` r1 = r1 & ~(1 << r6)
        assert_eq!(run_regs(&[0x94, 0xe1, 0x13, 0x16], &[(1, 0xff), (6, 3)]).regs[1], 0xf7);
    }

    #[test]
    fn builtin_encodings() {
        // what the vendor compiler's `__builtin_pi32v2_*` produce (work/probe/bi, fam.S)
        // `<<<` saturates (qasl): `08 a1` r0 = r0 <<< 1, `18 1a` r0 <<<= r1, `c0 e1 a9 14` r1 =
        // r10 <<< 9, `c8 e1 11 1a` r1 = r1 <<< r10
        assert_eq!(run_regs(&[0x08, 0xa1], &[(0, 0x7fff_ffff)]).regs[0], 0x7fff_ffff);
        assert_eq!(run_regs(&[0x08, 0xa1], &[(0, 0x3000_0000)]).regs[0], 0x6000_0000);
        assert_eq!(run_regs(&[0x18, 0x1a], &[(0, -3i32 as u32), (1, 31)]).regs[0], 0x8000_0000);
        assert_eq!(run_regs(&[0xc0, 0xe1, 0xa9, 0x14], &[(10, 0x0040_0000)]).regs[1], 0x7fff_ffff);
        assert_eq!(run_regs(&[0xc0, 0xe1, 0xa9, 0x14], &[(10, 3)]).regs[1], 3 << 9);
        assert_eq!(run_regs(&[0xc8, 0xe1, 0x11, 0x1a], &[(1, -1i32 as u32), (10, 4)]).regs[1], -16i32 as u32);
        // `70 e0 80 14` r1 = revs(r4.l)
        assert_eq!(run_regs(&[0x70, 0xe0, 0x80, 0x14], &[(4, 0x1234_0080)]).regs[1], 0xffff_8000);
        // `d4 e1 01 41` r5_r4 <>= 17, `dc e1 00 43` r5_r4 <>= r3
        let c = run_regs(&[0xd4, 0xe1, 0x01, 0x41], &[(5, 0), (4, 1)]);
        assert_eq!((c.regs[5], c.regs[4]), (0x8000, 0));
        let c = run_regs(&[0xdc, 0xe1, 0x00, 0x43], &[(5, 0), (4, 1), (3, 33)]);
        assert_eq!((c.regs[5], c.regs[4]), (0, 0x8000_0000));
        // `df e1 c1 95` r9 = r13_r12 >> r5 (s), `df e1 20 05` r0 = r3_r2 >> r5 (u)
        assert_eq!(run_regs(&[0xdf, 0xe1, 0xc1, 0x95], &[(13, 0xffff_fff0), (12, 0), (5, 4)]).regs[9], 0x8000_0000);
        assert_eq!(run_regs(&[0xdf, 0xe1, 0x20, 0x05], &[(3, 1), (2, 0x10), (5, 4)]).regs[0], 0x1000_0001);
        // `80 e1 01 14` r1 = cls(r4), `b0 e0 00 14` r1 += r4 (brev)
        assert_eq!(run_regs(&[0x80, 0xe1, 0x01, 0x14], &[(4, 0xffff_00ff)]).regs[1], 15);
        assert_eq!(run_regs(&[0x80, 0xe1, 0x01, 0x14], &[(4, 1)]).regs[1], 30);
        assert_eq!(run_regs(&[0xb0, 0xe0, 0x00, 0x14], &[(1, 0x8000_0000), (4, 0x8000_0000)]).regs[1], 0x4000_0000);
    }

    #[test]
    fn vendor_encodings() {
        // `dc ec cc bc` r11 = [r12++=-820] (the corpus's: dc's bit 2 is the increment's sign)
        let (c, _) = run1(&[0xdc, 0xec, 0xcc, 0xbc], &[]);
        assert_eq!(c.regs[11], 0x1c00);
        assert_eq!(c.regs[12], 0x1c00 - 820);
        // `df ec 00 4f` r4 = [r0++=-16]
        let (c, _) = run1(&[0xdf, 0xec, 0x00, 0x4f], &[]);
        assert_eq!((c.regs[4], c.regs[0]), (0x1000, 0x1000 - 16));
        // `d8 ec 05 1f` [r0++=244] = r1
        let (c, mut m) = run1(&[0xd8, 0xec, 0x05, 0x1f], &[]);
        assert_eq!((m.read32(0x1000), c.regs[0]), (0x1100, 0x1000 + 244));
        // `5e ec 02 21` r3_r2 = d[r0++=r1] (post-increment; `5c` is the pre-increment)
        let (c, _) = run1(&[0x5e, 0xec, 0x02, 0x21], &[]);
        assert_eq!((c.regs[2], c.regs[3], c.regs[0]), (0x1000, 0x1004, 0x2100));
        // `dc e9 00 40` r4 = b[sp] (u), `dc e9 00 80` r8 = b[sp] (u): the corpus class had
        // swapped them
        let (c, _) = run1(&[0xdc, 0xe9, 0x00, 0x40], &[]);
        assert_eq!((c.regs[4], c.regs[8]), (0x00, 0x1800));
        // `d8 e9 00 f4` r15 = h[sp+1024] (u), `d9 e9 00 f4` (s); `d8 e9 03 40` h[sp+2] = r4
        let (c, _) = run1(&[0xd8, 0xe9, 0x00, 0xf4], &[(0x2400, 0x9234)]);
        assert_eq!(c.regs[15], 0x9234);
        let (c, _) = run1(&[0xd9, 0xe9, 0x00, 0xf4], &[(0x2400, 0x9234)]);
        assert_eq!(c.regs[15], 0xffff_9234);
        let (_, mut m) = run1(&[0xd8, 0xe9, 0x03, 0x40], &[]);
        assert_eq!(m.read32(0x2000), 0x1400 << 16 | 0x2000);
        // `d0 ed 02 39` r3 = h[r0++=146] (u), `d4 ed 02 39` (s)
        let (c, _) = run1(&[0xd0, 0xed, 0x02, 0x39], &[(0x1000, 0x8001)]);
        assert_eq!((c.regs[3], c.regs[0]), (0x8001, 0x1000 + 146));
        let (c, _) = run1(&[0xd4, 0xed, 0x02, 0x39], &[(0x1000, 0x8001)]);
        assert_eq!((c.regs[3], c.regs[0]), (0xffff_8001, 0x1000 + 146));
    }

    /// Routes 18 / 19 / 32 as micro-ops (`inc_mem_fast`, `idx_inc_fast`) against exec_raw's
    /// arm, from random encodings (every width, pre- and post-increments, the base register
    /// the data one, route 32's increment register the base or the data one) and registers
    /// (some close together, so the accesses overlap them).
    #[test]
    fn inc_mem_micro_ops_match_the_raw_arm() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed as u32 };
        let mut kinds = std::collections::BTreeMap::new();
        for k in 0..30000u32 {
            let route = [18u8, 19, 32][k as usize % 3];
            let mut raw = rnd() & !0xffff | [0xec, 0xed, 0xee, 0xef][(k / 3) as usize % 4] << 8;
            raw |= if route == 32 { [0xdc, 0xde][(k / 12) as usize % 2] } else { rnd() & 0xff };
            let (d, b) = (raw >> 28, (raw >> 20) & 15);
            match k % 5 {
                0 => raw = raw & !0x00f0_0000 | d << 20,
                1 => raw = raw & !0x0f00_0000 | b << 24,
                2 => raw = raw & !0x0f00_0000 | d << 24,
                _ => {}
            }
            let f = if route == 32 { idx_inc_fast(raw as u64) } else { inc_mem_fast(route, raw) };
            let kind = format!("{f:?}").split([' ', '{']).next().unwrap().to_owned();
            *kinds.entry((route, kind)).or_insert(0) += 1;
            let mut ram = FlatRam::new((0..0x8000).map(|_| rnd() as u8).collect());
            let mut cpu = Cpu::new(0);
            for r in 0..16 {
                cpu.regs[r] = if k % 7 == 0 { 0x2000 + rnd() % 64 * 2 } else { 0x1000 + rnd() % 0x2000 };
            }
            let (mut ref_ram, mut reference) = (FlatRam::new(ram.mem.clone()), Cpu::new(0));
            reference.regs = cpu.regs;
            assert_eq!(cpu.exec_fast(&mut ram, f, 4), None);
            assert_eq!(reference.exec_fast(&mut ref_ram, Fast::Raw { route, raw }, 4), None);
            assert_eq!(cpu.regs, reference.regs, "route {route} raw {raw:08x}: {f:?}");
            assert!(ram.mem == ref_ram.mem, "route {route} raw {raw:08x}: {f:?}");
        }
        // (each form came up, and the raw rest: route 19's post-increment load into its base,
        // route 32's high halves and accesses of its base; route 18 maps whole)
        let forms: Vec<_> = kinds.keys().map(|(r, k)| format!("{r} {k}")).collect();
        assert_eq!(forms, ["18 Ld", "18 LdPre", "18 St", "18 StPre", "19 LdPost", "19 Raw", "19 StPost",
            "32 LdPostR", "32 LdPre", "32 Raw", "32 StPostR", "32 StPre"], "{kinds:?}");
    }

    /// `Fast::Swap`'s rule: for random register-only heads and random second instructions that
    /// `swappable_with` accepts, the second one first and then the head leaves what the pair
    /// rule (`exec_fast_paired`, as `step` runs pairs) leaves: registers, flags, sp, rets, pc,
    /// memory. And the order matters: head first gives something else for some of them.
    #[test]
    fn swapped_pairs_match_the_pair_rule() {
        let seed = std::cell::Cell::new(0x853c_49e6_748f_ea9bu64);
        let rnd = || { let mut s = seed.get(); s ^= s << 13; s ^= s >> 7; s ^= s << 17; seed.set(s); s as u32 };
        let ops = [Op::Add, Op::Sub, Op::RSub, Op::Or, Op::Xor, Op::And, Op::AndNot, Op::Mul, Op::Shl, Op::Shr, Op::Sar];
        let conds = [Cond::Eq, Cond::Ne, Cond::LtU, Cond::GtU, Cond::LeU, Cond::GeU, Cond::LtS, Cond::GtS, Cond::LeS, Cond::GeS];
        let reg_op = |x: u32, r: &dyn Fn() -> u8| -> Fast {
            let op = ops[x as usize / 32 % ops.len()];
            match x % 17 {
                0 => Fast::Mov { d: r(), s: r() },
                1 => Fast::MovI { d: r(), v: x },
                2 => Fast::Alu3 { d: r(), a: r(), b: r(), op },
                3 => Fast::Alu3I { d: r(), a: r(), v: x >> 7, op },
                4 => Fast::Alu2 { d: r(), b: r(), op },
                5 => Fast::Alu2I { d: r(), v: x >> 9, op },
                6 => Fast::A4 { d: r(), s: r(), op, rk: (x >> 5) as u8 % 3, rv: if (x >> 5) % 3 == 1 { x >> 3 } else { r() as u32 } },
                7 => Fast::AddRI { d: r(), s: r(), v: x >> 4 },
                8 => Fast::ShlI { d: r(), s: r(), n: (x >> 8) as u8 % 32 },
                9 => Fast::SarI { d: r(), s: r(), n: (x >> 8) as u8 % 32 },
                10 => Fast::Ext { d: r(), s: r(), part: (x >> 8) as u8 % 3, signed: x & 0x100 != 0 },
                11 => Fast::BitExt { d: r(), s: r(), p: (x >> 8) as u8 % 32, l: (x >> 16) as u8 % 32, signed: x & 0x80 != 0 },
                12 => Fast::BitIns { d: r(), s: r(), p: (x >> 8) as u8 % 32, l: (x >> 16) as u8 % 32 },
                13 => Fast::Mov2 { dh: r(), dl: r(), sh: r(), sl: r() },
                14 => Fast::Mov2I { dh: r(), dl: r(), v: x },
                15 => Fast::Un { d: r(), s: r(), k: (x >> 8) as u8 % 7 },
                _ => Fast::MinMax { d: r(), a: r(), b: r(), kind: (x >> 8) as u8 % 4 },
            }
        };
        let (mut tried, mut swapped, mut order_matters) = (0, 0, 0);
        while swapped < 4000 {
            tried += 1;
            assert!(tried < 1_000_000, "too few swappable pairs");
            let r = || (rnd() % 16) as u8;
            let head = reg_op(rnd(), &r);
            let x = rnd();
            let w = [1u8, 2, 4][x as usize % 3];
            let (target, cond) = (0x0200_0040, conds[x as usize / 8 % conds.len()]);
            let tail = match x % 23 {
                0 => Fast::Ld { d: r(), b: Base::Reg(r()), off: x >> 24, w, signed: x & 8 != 0 },
                1 => Fast::St { s: r(), b: Base::Reg(r()), off: x >> 24, w },
                2 => Fast::LdPost { d: r(), b: r(), w, signed: x & 8 != 0, inc: x >> 26 },
                3 => Fast::StPost { s: r(), b: r(), w, inc: x >> 26 },
                4 => Fast::LdIdx { d: r(), b: r(), i: r(), sh: (x >> 4) as u8 % 3, w, signed: x & 8 != 0 },
                5 => Fast::StIdx { s: r(), b: r(), i: r(), sh: (x >> 4) as u8 % 3, w },
                6 => Fast::Rmw { b: r(), w, op: RmwOp::Add, off: x >> 26, v: x },
                7 => Fast::MemA4 { b: r(), off: (x >> 26) as _, op: Op::Add, rk: 0, rv: r() as u32 },
                8 => Fast::BrI { a: r(), v: x >> 20, cond, target },
                9 => Fast::BrR { a: r(), b: r(), cond, target },
                10 => Fast::BitBr { r: r(), mk: 0, ne: x & 8 != 0, mv: r() as u32, target },
                11 => Fast::Call { target },
                12 => Fast::CallR { r: r() },
                13 => Fast::GotoR { r: r() },
                14 => Fast::Rts,
                15 => Fast::DecBr { r: r(), cond, v: x >> 20, target },
                16 => Fast::Ld { d: r(), b: Base::Sp, off: x >> 26, w, signed: false },
                17 => Fast::St { s: r(), b: Base::Sp, off: x >> 26, w },
                _ => reg_op(rnd(), &r),
            };
            // (as build_block: a pair that cannot be glued, the second reads what the head writes)
            if !head.swappable_with(tail) || head.regs_rw().unwrap().1 & tail.regs_rw().unwrap().0 == 0 {
                continue;
            }
            swapped += 1;
            let mut mem = vec![0u8; 0x8000];
            mem.iter_mut().for_each(|b| *b = rnd() as u8);
            let mut start = Cpu::new(0x0200_0000);
            for i in 0..16 {
                start.regs[i] = match rnd() % 4 { 0 => rnd() % 40, 1 => 0x1000 + rnd() % 0x4000, _ => rnd() };
            }
            (start.flags, start.sp, start.rets) = (rnd() & 15, 0x6000 + rnd() % 0x100, 0x0200_1234);
            let (head_next, next) = (0x0200_0004, 0x0200_0006);
            let state = |c: &Cpu, ram: &FlatRam| (c.regs, c.flags, c.sp, c.rets, ram.mem.clone());
            // the pair rule
            let (mut pair, mut pair_ram) = (Cpu::new(0), FlatRam::new(mem.clone()));
            (pair.regs, pair.flags, pair.sp, pair.rets) = (start.regs, start.flags, start.sp, start.rets);
            let _ = pair.exec_fast_paired(&mut pair_ram, head, true, head_next);
            let pair_pc = pair.exec_fast_paired(&mut pair_ram, tail, false, next).unwrap_or(next);
            assert!(pair.pair_pending.is_none());
            // the second instruction first
            let (mut swap, mut swap_ram) = (Cpu::new(0), FlatRam::new(mem.clone()));
            (swap.regs, swap.flags, swap.sp, swap.rets) = (start.regs, start.flags, start.sp, start.rets);
            let swap_pc = swap.exec_fast(&mut swap_ram, tail, next).unwrap_or(next);
            assert_eq!(swap.exec_fast(&mut swap_ram, head, head_next), None);
            assert!(state(&swap, &swap_ram) == state(&pair, &pair_ram) && swap_pc == pair_pc, "{head:?} then {tail:?}");
            // in program order instead
            let (mut plain, mut plain_ram) = (Cpu::new(0), FlatRam::new(mem));
            (plain.regs, plain.flags, plain.sp, plain.rets) = (start.regs, start.flags, start.sp, start.rets);
            let _ = plain.exec_fast(&mut plain_ram, head, head_next);
            let plain_pc = plain.exec_fast(&mut plain_ram, tail, next).unwrap_or(next);
            if state(&plain, &plain_ram) != state(&pair, &pair_ram) || plain_pc != pair_pc {
                order_matters += 1;
            }
        }
        assert!(order_matters > swapped / 2, "{order_matters} of {swapped}");
    }

    /// The eight register-base block moves (`eb 00..70`: bit 4 the base written back, bit 5 a
    /// store, bit 6 the block below the base), stepped and through the micro-op blocks. #59:
    /// JIANT halted on `40 eb 06 00`, {r1, r2} = [-r0].
    #[test]
    fn block_moves_up_and_down() {
        struct Xip(FlatRam);
        impl Bus for Xip {
            fn read8(&mut self, a: u32) -> u8 { self.0.read8(a & 0xffff) }
            fn read16(&mut self, a: u32) -> u16 { self.0.read16(a & 0xffff) }
            fn read32(&mut self, a: u32) -> u32 { self.0.read32(a & 0xffff) }
            fn write8(&mut self, a: u32, v: u8) { self.0.write8(a & 0xffff, v) }
            fn write16(&mut self, a: u32, v: u16) { self.0.write16(a & 0xffff, v) }
            fn write32(&mut self, a: u32, v: u32) { self.0.write32(a & 0xffff, v) }
            fn next_tick_due(&self) -> bool { false }
        }
        // (op, the block's first address, the base afterwards) with r0 = 0x1000 and {r1, r2}
        let cases = [(0x00u8, 0x1000u32, 0x1000u32), (0x10, 0x1000, 0x1008), (0x20, 0x1000, 0x1000), (0x30, 0x1000, 0x1008),
                     (0x40, 0x0ff8, 0x1000), (0x50, 0x0ff8, 0x0ff8), (0x60, 0x0ff8, 0x1000), (0x70, 0x0ff8, 0x0ff8)];
        for (op, at, base) in cases {
            let store = op & 0x20 != 0;
            let fresh = || {
                let mut mem = vec![0u8; 0x10000];
                mem[..8].copy_from_slice(&[op, 0xeb, 0x06, 0x00, 0, 0, 0, 0]);
                for (k, w) in [0x0ff8u32, 0x0ffc, 0x1000, 0x1004].iter().enumerate() {
                    mem[*w as usize..*w as usize + 4].copy_from_slice(&(0xa0 + k as u32).to_le_bytes());
                }
                let mut cpu = Cpu::new(0x0200_0000);
                (cpu.regs[0], cpu.regs[1], cpu.regs[2]) = (0x1000, 0x11, 0x22);
                (cpu, Xip(FlatRam::new(mem)))
            };
            let check = |cpu: &Cpu, bus: &mut Xip, how: &str| {
                let (lo, hi) = (bus.read32(at), bus.read32(at + 4));
                if store {
                    assert_eq!((lo, hi), (0x11, 0x22), "{op:#04x} {how}: stored");
                } else {
                    assert_eq!((cpu.regs[1], cpu.regs[2]), (lo, hi), "{op:#04x} {how}: loaded");
                    assert_ne!(lo, 0, "{op:#04x}: the data was there");
                }
                assert_eq!(cpu.regs[0], base, "{op:#04x} {how}: the base");
            };
            let (mut cpu, mut bus) = fresh();
            cpu.step(&mut bus).unwrap();
            check(&cpu, &mut bus, "stepped");
            let (mut cpu, mut bus) = fresh();
            let (k, _) = cpu.run_fast(&mut bus, 1).unwrap();
            assert_eq!(k, 1);
            check(&cpu, &mut bus, "run_fast");
        }
        // #64: `41 eb 0a 00`, {r1, r3} = [-r1] (JieLi clang's load of t[i - 1], t[i]): the base,
        // in the list too, takes its loaded word, as nothing writes the base back
        for how in ["stepped", "run_fast"] {
            let mut mem = vec![0u8; 0x10000];
            mem[..8].copy_from_slice(&[0x41, 0xeb, 0x0a, 0x00, 0, 0, 0, 0]);
            mem[0x0ff8..0x1000].copy_from_slice(&[1, 0, 0, 0, 2, 0, 0, 0]);
            let mut cpu = Cpu::new(0x0200_0000);
            (cpu.regs[1], cpu.regs[3]) = (0x1000, 0x33);
            let mut bus = Xip(FlatRam::new(mem));
            if how == "stepped" {
                cpu.step(&mut bus).unwrap();
            } else {
                assert_eq!(cpu.run_fast(&mut bus, 1).unwrap().0, 1);
            }
            assert_eq!((cpu.regs[1], cpu.regs[3]), (1, 2), "{how}");
        }
    }

    /// A swappable pair in a block: run_fast runs it as one op of two instructions; cut by the
    /// budget after its head, or stepped, it gives the same.
    #[test]
    fn swapped_pair_in_a_block() {
        // a bus with XIP code at 0x02000000 (its low 64 KiB alias), no ticks due
        struct Xip(FlatRam);
        impl Bus for Xip {
            fn read8(&mut self, a: u32) -> u8 { self.0.read8(a & 0xffff) }
            fn read16(&mut self, a: u32) -> u16 { self.0.read16(a & 0xffff) }
            fn read32(&mut self, a: u32) -> u32 { self.0.read32(a & 0xffff) }
            fn write8(&mut self, a: u32, v: u8) { self.0.write8(a & 0xffff, v) }
            fn write16(&mut self, a: u32, v: u16) { self.0.write16(a & 0xffff, v) }
            fn write32(&mut self, a: u32, v: u32) { self.0.write32(a & 0xffff, v) }
            fn next_tick_due(&self) -> bool { false }
        }
        // `00 d7` r0 = r0.b0 (u) #  paired with `c8 4b` b[r4+11] = r0 (the store sees the old
        // r0), then two nops
        let code = [0x00, 0xd7, 0xc8, 0x4b, 0x00, 0x00, 0x00, 0x00];
        let fresh = || {
            let mut mem = vec![0u8; 0x10000];
            mem[..code.len()].copy_from_slice(&code);
            let mut cpu = Cpu::new(0x0200_0000);
            (cpu.regs[0], cpu.regs[4]) = (0x1234_5678, 0x1000);
            (cpu, Xip(FlatRam::new(mem)))
        };
        let (mut stepped, mut sbus) = fresh();
        for _ in 0..4 {
            stepped.step(&mut sbus).unwrap();
        }
        assert_eq!((stepped.regs[0], sbus.0.mem[0x100b]), (0x78, 0x78));
        for budget in 1..=4 {
            let (mut cpu, mut bus) = fresh();
            let mut n = 0;
            while n < 4 {
                let (k, _) = cpu.run_fast(&mut bus, budget.min(4 - n)).unwrap();
                assert!(k > 0);
                n += k;
            }
            assert!(matches!(cpu.bops[0].fast, Fast::Swap { .. }), "{:?}", cpu.bops[0]);
            assert_eq!((cpu.regs, cpu.pc, cpu.insn_count), (stepped.regs, stepped.pc, stepped.insn_count));
            assert!(bus.0.mem == sbus.0.mem);
        }
        // cut after the head: the pair is in progress, the second instruction comes next
        let (mut cpu, mut bus) = fresh();
        assert_eq!(cpu.block_len_to(&mut bus, 0x0200_0002), 1);
        assert_eq!(cpu.block_len_to(&mut bus, 0x0200_0006), 3);
        assert_eq!(cpu.run_fast(&mut bus, 1).unwrap(), (1, false));
        assert!(cpu.pair_pending.is_some());
        assert_eq!(cpu.pc, 0x0200_0002);
        assert_eq!(cpu.run_fast(&mut bus, 3).unwrap(), (3, false));
        assert_eq!((cpu.regs, cpu.pc, cpu.insn_count), (stepped.regs, stepped.pc, stepped.insn_count));
        assert!(bus.0.mem == sbus.0.mem);
    }

    /// A test bus with a clock and an interrupt source: XIP code at 0x0200_0000, RAM at 0; a
    /// write to `MMIO` and up raises the interrupt (odd value) or clears it (even), and makes the
    /// next tick due when `due_on_mmio` (as the Soc's peripherals do; core 1's bus does not).
    #[cfg(native_jit)]
    const MMIO: u32 = 0x0100_0000;
    #[cfg(native_jit)]
    struct Clocked { ram: Vec<u8>, xip: Vec<u8>, cycles: u64, next_due: u64, irq: bool, due_on_mmio: bool, writes: u32 }
    #[cfg(native_jit)]
    impl Clocked {
        fn r8(&mut self, a: u32) -> u8 {
            match a {
                0x0200_0000.. => *self.xip.get((a - 0x0200_0000) as usize).unwrap_or(&0),
                MMIO.. => 0,
                _ => *self.ram.get(a as usize).unwrap_or(&0),
            }
        }
        fn w8(&mut self, a: u32, v: u8) {
            match a {
                0x0200_0000.. => {}
                MMIO.. => {
                    self.irq = v & 1 != 0;
                    self.writes += 1;
                    if self.due_on_mmio { self.next_due = 0; }
                }
                _ => if let Some(b) = self.ram.get_mut(a as usize) { *b = v },
            }
        }
    }
    #[cfg(native_jit)]
    impl Bus for Clocked {
        fn read8(&mut self, a: u32) -> u8 { self.r8(a) }
        fn read16(&mut self, a: u32) -> u16 { u16::from_le_bytes([self.r8(a), self.r8(a + 1)]) }
        fn read32(&mut self, a: u32) -> u32 { u32::from_le_bytes([self.r8(a), self.r8(a + 1), self.r8(a + 2), self.r8(a + 3)]) }
        fn write8(&mut self, a: u32, v: u8) { self.w8(a, v) }
        fn write16(&mut self, a: u32, v: u16) { for (i, b) in v.to_le_bytes().into_iter().enumerate() { self.w8(a + i as u32, b) } }
        fn write32(&mut self, a: u32, v: u32) { for (i, b) in v.to_le_bytes().into_iter().enumerate() { self.w8(a + i as u32, b) } }
        fn tick(&mut self) -> bool { self.cycles += 1; self.cycles >= self.next_due }
        fn irq_pending(&self) -> bool { self.irq }
        fn tick_room(&self) -> u64 { self.next_due.saturating_sub(self.cycles + 1) }
        fn add_ticks(&mut self, n: u64) { self.cycles += n; }
        fn next_tick_due(&self) -> bool { self.cycles + 1 >= self.next_due }
        fn jit_mem(&mut self) -> Option<JitMem> {
            Some(JitMem { ram: self.ram.as_mut_ptr(), ram_base: 0, ram_len: self.ram.len() as u32, ram_code: std::ptr::null(),
                xip: self.xip.as_ptr(), xip_base: 0x0200_0000, xip_len: self.xip.len() as u32 })
        }
    }

    /// Runs of a test program on `Clocked`, as cpu0's block loop runs one (the SoC serviced at a
    /// due tick with a random next deadline, random budgets), JIT or interpreted. `before` may
    /// change the CPU before a call (round, a random number). Per call: (instructions, due, pc,
    /// cycles, r0, interrupt pending); then the instructions retired, RAM, peripheral writes,
    /// instructions run compiled, and of those, the ones run inside a then-part or rep loop.
    #[cfg(native_jit)]
    #[allow(clippy::type_complexity)]
    fn drive_clocked(xip: &[u8], jit: bool, (irq, r4, due_on_mmio): (bool, u32, bool), seed: u64, before: &dyn Fn(&mut Cpu, u32, u64))
        -> (Vec<(u32, bool, u32, u64, u32, bool)>, u64, Vec<u8>, u32, u64, u64) {
        let mut bus = Clocked { ram: vec![0; 0x1000], xip: xip.to_vec(), cycles: 0, next_due: 5, irq, due_on_mmio, writes: 0 };
        let mut cpu = Cpu::new(0x0200_0000);
        (cpu.jit_on, cpu.profile) = (jit, true);
        cpu.regs[4] = r4;
        let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        let mut rnd = move || { rng ^= rng << 13; rng ^= rng >> 7; rng ^= rng << 17; rng };
        let (mut trace, mut ticked, mut inside) = (vec![], false, 0);
        for round in 0..3000u32 {
            if ticked || bus.tick() {
                // (the SoC serviced: the next deadline)
                bus.next_due = bus.cycles + 1 + rnd() % 48;
            }
            before(&mut cpu, round, rnd());
            let (was_inside, compiled) = (cpu.rep_end != 0 || !cpu.pred_skips.is_empty(), cpu.jit_insns);
            let budget = 1 + (rnd() % 48) as u32;
            let (k, due) = match cpu.run_fast(&mut bus, budget).unwrap() {
                (0, _) => { cpu.step(&mut bus).unwrap(); (1, false) }
                r => r,
            };
            if was_inside {
                inside += cpu.jit_insns - compiled;
            }
            ticked = due;
            trace.push((k, due, cpu.pc, bus.cycles, cpu.regs[0], bus.irq));
        }
        (trace, cpu.insn_count, bus.ram, bus.writes, cpu.jit_insns, inside)
    }

    /// The test program for `drive_clocked`: `00 d7` r0 = r0.b0 (u) #  paired with `c8 4b`
    /// b[r4+11] = r0 (a swapped pair: the store sees the old r0), `80 07` r0 += 1, nops, `f7 97`
    /// at 0x10, a goto back to the pair. `cut`: at 6 the pair `00 d7` r0 = r0.b0 (u) # `80 07`
    /// r0 += 1, which neither glues nor swaps (careful: the compiled prefix before it ends at 6
    /// without a jump).
    #[cfg(native_jit)]
    fn clocked_loop(cut: bool) -> Vec<u8> {
        let mut xip = vec![0u8; 0x1000];
        xip[..6].copy_from_slice(&[0x00, 0xd7, 0xc8, 0x4b, 0x80, 0x07]);
        if cut {
            xip[6..10].copy_from_slice(&[0x00, 0xd7, 0x80, 0x07]);
        }
        xip[0x10..0x12].copy_from_slice(&[0xf7, 0x97]);
        xip
    }

    /// (irq pending from the start, the store's target: RAM or a peripheral, the peripheral
    /// makes the next tick due)
    #[cfg(native_jit)]
    const CLOCKED_SETUPS: [(bool, u32, bool); 5] =
        [(false, 0x100, false), (true, 0x100, false), (false, MMIO - 11, true), (false, MMIO - 11, false), (true, MMIO - 11, false)];

    /// A swappable pair compiled (native JIT) against the interpreter, through `run_fast` with a
    /// clock and interrupts: random budgets and tick deadlines, an interrupt pending all along,
    /// or raised and cleared by the pair's own store to a peripheral (which makes the next tick
    /// due, as the Soc does, or not, as core 1's bus does). Every call must stop at the same
    /// instruction, with the same result, and leave the same state.
    #[cfg(native_jit)]
    #[test]
    fn compiled_swapped_pairs_stop_where_the_interpreter_does() {
        let xip = clocked_loop(false);
        // (the pair's store raises the interrupt with an odd r0, clears it with an even one:
        // each time round the loop, or as the rounds go)
        let before = |cpu: &mut Cpu, round: u32, _: u64| if cpu.pair_pending.is_none() && round % 7 == 0 { cpu.regs[0] = round & 0x3ff };
        for setup in CLOCKED_SETUPS {
            for seed in 1..=4u64 {
                let (want, got) = (drive_clocked(&xip, false, setup, seed, &before), drive_clocked(&xip, true, setup, seed, &before));
                assert!(got.4 > 1000, "compiled code ran {} instructions", got.4);
                for (i, (w, g)) in want.0.iter().zip(&got.0).enumerate() {
                    assert_eq!(w, g, "call {i} ({setup:?}, seed {seed})");
                }
                assert_eq!((want.1, &want.2, want.3), (got.1, &got.2, got.3));
            }
        }
    }

    /// Compiled prefixes inside a then-part or a rep loop (`Cpu::prefix_inside`) against the
    /// interpreter's careful path, through `run_fast` as above: from time to time, with nothing in
    /// progress, the test opens an if-else block's then-part (its end anywhere in the loop or
    /// past it, the skip to anywhere) or a rep loop (its end anywhere in the loop, 1-4 rounds)
    /// as the instructions that start them would (and gives up one that never ends now and then). The construct's end (skip, count down, stop
    /// for a pending interrupt), the pair in it, ticks and budgets: the same calls, the same state.
    #[cfg(native_jit)]
    #[test]
    fn compiled_prefixes_inside_constructs_match_the_careful_path() {
        let xip = clocked_loop(true);
        let before = |cpu: &mut Cpu, round: u32, x: u64| {
            if cpu.pair_pending.is_none() && round % 7 == 0 {
                cpu.regs[0] = round & 0x3ff;
            }
            if cpu.pair_pending.is_some() || cpu.hold_irq != 0 || x % 3 != 0 {
                return;
            }
            if cpu.rep_end != 0 || !cpu.pred_skips.is_empty() {
                // (one that never ends, its end past the loop or behind the goto: given up now
                // and then)
                if x % 64 == 0 {
                    (cpu.rep_end, cpu.pred_skips) = (0, vec![]);
                }
                return;
            }
            // (anywhere in the loop or just past it, often where the first prefix ends)
            let at = |k: u64| 0x0200_0000 + if k % 4 == 0 { 6 } else { 2 * (k % 10) as u32 };
            if x % 2 == 0 {
                cpu.pred_skips.push((at(x >> 8), at(x >> 16), cpu.pc));
            } else if at(x >> 8) != cpu.pc {
                (cpu.rep_start, cpu.rep_end, cpu.rep_reg, cpu.rep_count) = (cpu.pc, at(x >> 8), None, 1 + (x >> 24) as u32 % 4);
            }
        };
        for setup in CLOCKED_SETUPS {
            let mut inside = 0;
            for seed in 1..=6u64 {
                let (want, got) = (drive_clocked(&xip, false, setup, seed, &before), drive_clocked(&xip, true, setup, seed, &before));
                inside += got.5;
                for (i, (w, g)) in want.0.iter().zip(&got.0).enumerate() {
                    assert_eq!(w, g, "call {i} ({setup:?}, seed {seed})");
                }
                assert_eq!((want.1, &want.2, want.3), (got.1, &got.2, got.3));
            }
            assert!(inside > 5000, "compiled code ran {inside} instructions inside ({setup:?})");
        }
    }
}

/// Substring test for the short syntax/class strings: a plain window scan
/// beats `str::contains`, whose searcher setup dominated the step loop.
#[inline]
fn has(h: &str, n: &str) -> bool {
    let (h, n) = (h.as_bytes(), n.as_bytes());
    n.len() <= h.len() && h.windows(n.len()).any(|w| w == n)
}

fn trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("FM1_TRACE").is_ok())
}

thread_local! {
    static CANON: std::cell::RefCell<std::collections::HashMap<usize, &'static str>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// The printed syntax with the sign of `-#i` folded into the slot and the
/// dual-issue ` #` stripped, memoised per class.
/// Add / subtract shapes that set the flags (`Cpu::flags`).
fn is_flag_producer(s: &str) -> bool {
    s.starts_with("R = R + ") || s.starts_with("R = R - ") || s.starts_with("R += ") || s.starts_with("R -= ")
        || s.starts_with("R = #h - ") || s.starts_with("R = #i - ")
}

/// How far past a flag producer `flags_read_later` looks for a reader.
const FLAG_SCAN: usize = 8;

/// How many instructions `flags_read_later` looks at in all, over every path it follows.
const FLAG_VISITS: usize = 4 * FLAG_SCAN;

/// Does an instruction from pc on read the flags (`+ c`, `- !c`, a `psr` read) before another
/// add / subtract replaces them? A producer whose flags are read stays off the JITs (which do
/// not model the flags) on the careful path. The reader need not be the next instruction:
/// clang schedules other work between a 64-bit add's halves (FM-106's chorus interpolation
/// stores a field between `r4 = r14 + r0` and `r5 = r15 + r1 + c`; compiled, the high half
/// took a stale carry). Nor on the fall-through path: a direct jump, call or branch on the way is
/// followed to its target too (a `[--sp] = {psr, ...}` there pushes the flags: compiled, stale
/// ones), up to FLAG_SCAN instructions a path and FLAG_VISITS in all. Returns and computed jumps
/// end a path. A reader that is never reached costs only speed.
fn flags_read_later<B: Bus>(bus: &mut B, pc: u32) -> bool {
    let (mut todo, mut visits) = (vec![(pc, FLAG_SCAN)], 0);
    while let Some((mut pc, mut left)) = todo.pop() {
        while left > 0 && visits < FLAG_VISITS {
            left -= 1;
            visits += 1;
            let win = (bus.fetch32(pc + 4) as u64) << 32 | bus.fetch32(pc) as u64;
            let Ok(i) = decode_any(win, pc) else { break };
            let s = i.entry.syntax;
            if s.contains("+ c") || s.contains("- !c") || s.contains("= psr") || s.contains("psr,") || s.contains("{psr") {
                return true;
            }
            if is_flag_producer(canon_syntax(i.entry)) {
                break;
            }
            let (on, to) = successors(&i);
            if let Some(t) = to {
                todo.push((t, left));
            }
            if !on {
                break;
            }
            pc = pc.wrapping_add(i.entry.len as u32);
        }
    }
    false
}

/// Where control may go after `insn`, as far as its micro-op says: (on to the next instruction,
/// a direct target). An if-block falls through (its parts are in line).
fn successors(insn: &Instruction) -> (bool, Option<u32>) {
    let alu4 = if insn.entry.len == 4 { decode_alu4(insn.raw) } else { None };
    let bittest = if insn.entry.name.starts_with("if") { decode_bittest(insn.raw, insn.entry.len) } else { None };
    let route = route_of(insn, &alu4, &bittest);
    let fast = if route == ROUTE_SYNTAX { fast_of(insn, canon_syntax(insn.entry)) } else { fast_raw(route, insn, &alu4, &bittest) };
    match fast.map(Fast::specialise) {
        Some(Fast::Goto { target }) => (false, Some(target)),
        Some(Fast::Call { target }) => (true, Some(target)),
        Some(Fast::BrI { target, .. } | Fast::BrR { target, .. } | Fast::BrIEq { target, .. } | Fast::BrINe { target, .. }
            | Fast::BitBr { target, .. } | Fast::DecBr { target, .. }) => (true, Some(target)),
        Some(Fast::IfBlk { .. }) => (true, None),
        Some(f) if f.may_jump() => (false, None),
        _ => (true, None),
    }
}

fn canon_syntax(e: &'static fm1_isa::IsaEntry) -> &'static str {
    let key = e as *const _ as usize;
    if let Some(s) = CANON.with(|c| c.borrow().get(&key).copied()) {
        return s;
    }
    let canon = e.syntax.replace("-#i", "#i").replace("-#h", "#h");
    let s: &'static str = Box::leak(canon.trim_end_matches(" #").trim_end().to_string().into_boxed_str());
    CANON.with(|c| c.borrow_mut().insert(key, s));
    s
}

/// `str::split_once` for short literal patterns without the searcher setup.
#[inline]
fn split2<'a>(h: &'a str, n: &str) -> Option<(&'a str, &'a str)> {
    let (hb, nb) = (h.as_bytes(), n.as_bytes());
    if nb.len() > hb.len() {
        return None;
    }
    let i = hb.windows(nb.len()).position(|w| w == nb)?;
    Some((&h[..i], &h[i + nb.len()..]))
}

/// Everything about an instruction word that does not depend on the
/// machine state, worked out once (`Cpu::fetch`).
#[derive(Clone, Copy, Debug)]
pub struct Pre {
    route: u16,
    /// A micro-op for the commonest generic shapes (see `fast_of`).
    fast: Option<Fast>,
    alu4: Option<Alu4>,
    bittest: Option<BitTest>,
    pair_head: bool,
    syntax: &'static str,
}

/// The hot part of a cached instruction (32 bytes).
#[derive(Clone, Copy, Debug)]
pub struct Line {
    pc: u32,
    len: u8,
    pair_head: bool,
    /// Needs the careful path in `run_fast`: a pair head or a predicated
    /// block (they change the state that holds interrupts back), or `flags_live`.
    careful: bool,
    /// An add / subtract whose flags a later instruction reads (`flags_read_later`).
    flags_live: bool,
    fast: Option<Fast>,
}

impl Line {
    const EMPTY: Line = Line { pc: u32::MAX, len: 0, pair_head: false, careful: false, flags_live: false, fast: None };
}

/// One micro-op of a block.
#[derive(Clone, Copy, Debug)]
struct BOp {
    fast: Fast,
    len: u8,
    careful: bool,
    pair_head: bool,
    /// A pair head whose second instruction reads and writes no register the head writes: run
    /// one after the other they give the pair's result, so the plain path (and compiled code)
    /// runs it like any op. The careful path still runs it as a pair (`pair_head` stays set).
    glue: bool,
}

impl Fast {
    /// The guest registers this op reads and writes (bit masks), for gluing pairs (`BOp::glue`);
    /// None: not worked out (never glued).
    fn regs_rw(&self) -> Option<(u16, u16)> {
        let m = |r: u8| 1u16 << (r & 15);
        let rhs = |rk: u8, rv: u32| if rk == 1 { 0 } else { m(rv as u8) };
        let base = |b: Base| match b { Base::Reg(r) => m(r), Base::Sp => 0 };
        let range = |n: u8| { let n = n as i32; (n.min(4)..=n.max(3)).fold(0u16, |a, r| a | m(r as u8)) };
        Some(match *self {
            Fast::Mov { d, s } | Fast::Ext { d, s, .. } | Fast::BitExt { d, s, .. } | Fast::Un { d, s, .. }
            | Fast::ShlI { d, s, .. } | Fast::SarI { d, s, .. } | Fast::AddRI { d, s, .. } => (m(s), m(d)),
            Fast::MovI { d, .. } | Fast::RFromSp { d, .. } | Fast::IcfgRd { d } => (0, m(d)),
            Fast::FOp { d, a, b, op: 7 | 8 } => (m(a) | m(b) | m(d), m(d)),
            Fast::Alu3 { d, a, b, .. } | Fast::FOp { d, a, b, .. } | Fast::MinMax { d, a, b, .. } | Fast::Div { d, a, b, .. } => (m(a) | m(b), m(d)),
            Fast::Alu3I { d, a, .. } => (m(a), m(d)),
            Fast::Alu2 { d, b, .. } => (m(d) | m(b), m(d)),
            Fast::Alu2I { d, .. } => (m(d), m(d)),
            Fast::BitIns { d, s, .. } => (m(d) | m(s), m(d)),
            Fast::A4 { d, s, rk, rv, .. } => (m(s) | rhs(rk, rv), m(d)),
            Fast::Ld { d, b, .. } => (base(b), m(d)),
            Fast::St { s, b, .. } => (m(s) | base(b), 0),
            Fast::LdW { d, b, .. } | Fast::LdBu { d, b, .. } => (m(b), m(d)),
            Fast::LdWSp { d, .. } => (0, m(d)),
            Fast::StW { s, b, .. } => (m(s) | m(b), 0),
            Fast::StWSp { s, .. } | Fast::SpFromR { s } => (m(s), 0),
            Fast::LdIdx { d, b, i, .. } => (m(b) | m(i), m(d)),
            Fast::StIdx { s, b, i, .. } => (m(s) | m(b) | m(i), 0),
            Fast::Rmw { b, .. } => (m(b), 0),
            Fast::MemA4 { b, rk, rv, .. } => (m(b) | rhs(rk, rv), 0),
            Fast::LdPost { d, b, .. } => (m(b), m(d) | m(b)),
            Fast::StPost { s, b, .. } => (m(s) | m(b), m(b)),
            Fast::LdPre { d, b, i, .. } => (m(b) | if i < 16 { m(i) } else { 0 }, m(d) | m(b)),
            Fast::StPre { s, b, i, .. } => (m(s) | m(b) | if i < 16 { m(i) } else { 0 }, m(b)),
            Fast::LdPostR { d, b, i, .. } => (m(b) | m(i), m(d) | m(b)),
            Fast::StPostR { s, b, i, .. } => (m(s) | m(b) | m(i), m(b)),
            Fast::Mov2 { dh, dl, sh, sl } => (m(sh) | m(sl), m(dh) | m(dl)),
            Fast::Mov2I { dh, dl, .. } => (0, m(dh) | m(dl)),
            Fast::BrI { a, .. } | Fast::BrIEq { a, .. } | Fast::BrINe { a, .. } => (m(a), 0),
            Fast::BrR { a, b, .. } => (m(a) | m(b), 0),
            Fast::BitBr { r, mk, mv, .. } => (m(r) | rhs(mk, mv), 0),
            Fast::DecBr { r, .. } => (m(r), m(r)),
            Fast::Tb { r, .. } | Fast::CallR { r } | Fast::GotoR { r } => (m(r), 0),
            Fast::Goto { .. } | Fast::Call { .. } | Fast::Rts | Fast::Nop | Fast::SpAdd { .. }
            | Fast::PushRet | Fast::PopRet | Fast::PopRets => (0, 0),
            Fast::PushRets { n } => (range(n), 0),
            Fast::PopPc { n } => (0, range(n)),
            Fast::PushMask { mask } => (mask, 0),
            Fast::PopMask { mask } => (0, mask),
            Fast::BlockMove { base, mask, store, post, .. } => {
                let pb = if post { m(base) } else { 0 };
                if store { (mask | m(base), pb) } else { (m(base), mask | pb) }
            }
            Fast::LdPair { p, b, .. } => (base(b), m(2 * (p & 7)) | m(2 * (p & 7) + 1)),
            Fast::StPair { p, b, .. } => (base(b) | m(2 * (p & 7)) | m(2 * (p & 7) + 1), 0),
            Fast::PushRegs { mask } => (mask, 0),
            Fast::PopRegs { mask } => (0, mask),
            Fast::PushSregs { .. } | Fast::PopSregs { .. } | Fast::Rti => (0, 0),
            Fast::AddC { d, a, b, .. } => (m(a) | m(b), m(d)),
            Fast::IfBlk { .. } | Fast::SetIe { .. } | Fast::IcfgWr { .. } | Fast::Raw { .. } | Fast::Swap { .. } => return None,
        })
    }

    /// Can this pair head (not glued: `tail` reads what it writes) run after `tail`, with the
    /// pair's result? The head works on registers only (no bus, sp, rets, special register or
    /// jump), reads and writes nothing the tail writes, and neither reads the flags nor both set
    /// them. The pair rule holds the head's registers back until the tail has run; the
    /// tail first, then the head, is that. (`regs_rw` must be exact for the tail's writes.)
    fn swappable_with(&self, tail: Fast) -> bool {
        match (self.reg_only(), self.regs_rw(), tail.regs_rw()) {
            (true, Some((hr, hw)), Some((_, tw))) => {
                (hr | hw) & tw == 0 && !(self.sets_flags() && tail.sets_flags()) && !self.reads_flags() && !tail.reads_flags()
                    && !matches!(tail, Fast::IfBlk { .. } | Fast::Swap { .. })
            }
            _ => false,
        }
    }

    /// Does this op work on registers only (no bus, sp, rets, special register or jump)?
    fn reg_only(&self) -> bool {
        matches!(self, Fast::Mov { .. } | Fast::MovI { .. } | Fast::Alu3 { .. } | Fast::Alu3I { .. }
            | Fast::Alu2 { .. } | Fast::Alu2I { .. } | Fast::A4 { .. } | Fast::AddRI { .. } | Fast::ShlI { .. }
            | Fast::SarI { .. } | Fast::Ext { .. } | Fast::BitExt { .. } | Fast::BitIns { .. } | Fast::Mov2 { .. }
            | Fast::Mov2I { .. } | Fast::Un { .. } | Fast::MinMax { .. } | Fast::FOp { .. } | Fast::AddC { .. })
    }

    /// Does this op set the arithmetic flags? (`alu_f`'s add / subtract, `AddRI`; no other
    /// micro-op touches them, but `Raw` and `Swap` may; neither is ever swapped). Every op is
    /// listed, so a new one has to say: `swappable_with` is only sound if this is right.
    fn sets_flags(&self) -> bool {
        match *self {
            Fast::Alu3 { op, .. } | Fast::Alu3I { op, .. } | Fast::Alu2 { op, .. } | Fast::Alu2I { op, .. } | Fast::A4 { op, .. } => {
                matches!(op, Op::Add | Op::Sub | Op::RSub)
            }
            Fast::AddRI { .. } | Fast::Raw { .. } | Fast::Swap { .. } | Fast::AddC { .. } => true,
            // (psr in the list: its low bits are the flags)
            Fast::PopSregs { mask } => mask & SREG_PSR != 0,
            Fast::PushRegs { .. } | Fast::PopRegs { .. } | Fast::PushSregs { .. } | Fast::Rti => false,
            Fast::Mov { .. } | Fast::MovI { .. } | Fast::Ld { .. } | Fast::St { .. } | Fast::BrI { .. } | Fast::BrR { .. }
            | Fast::FOp { .. } | Fast::Goto { .. } | Fast::Call { .. } | Fast::Rts | Fast::CallR { .. } | Fast::GotoR { .. }
            | Fast::BitBr { .. } | Fast::LdIdx { .. } | Fast::StIdx { .. } | Fast::Rmw { .. } | Fast::LdPost { .. }
            | Fast::StPost { .. } | Fast::DecBr { .. } | Fast::Ext { .. } | Fast::MemA4 { .. } | Fast::Nop
            | Fast::PushRets { .. } | Fast::PopPc { .. } | Fast::PushMask { .. } | Fast::PopMask { .. } | Fast::PushRet
            | Fast::PopRet | Fast::MinMax { .. } | Fast::IfBlk { .. } | Fast::BrIEq { .. } | Fast::BrINe { .. }
            | Fast::LdW { .. } | Fast::LdWSp { .. } | Fast::LdBu { .. } | Fast::StW { .. } | Fast::StWSp { .. }
            | Fast::ShlI { .. } | Fast::SarI { .. } | Fast::SpAdd { .. } | Fast::SpFromR { .. } | Fast::RFromSp { .. }
            | Fast::BitExt { .. } | Fast::BitIns { .. } | Fast::SetIe { .. } | Fast::IcfgRd { .. } | Fast::IcfgWr { .. }
            | Fast::BlockMove { .. } | Fast::PopRets | Fast::Tb { .. } | Fast::Mov2 { .. } | Fast::Mov2I { .. }
            | Fast::Un { .. } | Fast::Div { .. } | Fast::LdPre { .. } | Fast::StPre { .. } | Fast::LdPostR { .. }
            | Fast::StPostR { .. } | Fast::LdPair { .. } | Fast::StPair { .. } => false,
        }
    }

    /// Does this op read the arithmetic flags? No micro-op does (`addc` / `subc` and `psr` reads
    /// are `step`'s), but `Raw` and `Swap` may (neither is ever swapped). Every op is listed, as
    /// for `sets_flags`, so a new one has to say.
    fn reads_flags(&self) -> bool {
        match *self {
            Fast::Raw { .. } | Fast::Swap { .. } | Fast::AddC { .. } => true,
            Fast::PushSregs { mask } => mask & SREG_PSR != 0,
            Fast::PushRegs { .. } | Fast::PopRegs { .. } | Fast::PopSregs { .. } | Fast::Rti => false,
            Fast::Mov { .. } | Fast::MovI { .. } | Fast::Alu3 { .. } | Fast::Alu3I { .. } | Fast::Alu2 { .. }
            | Fast::Alu2I { .. } | Fast::Ld { .. } | Fast::St { .. } | Fast::BrI { .. } | Fast::BrR { .. } | Fast::FOp { .. }
            | Fast::Goto { .. } | Fast::Call { .. } | Fast::Rts | Fast::CallR { .. } | Fast::GotoR { .. } | Fast::A4 { .. }
            | Fast::BitBr { .. } | Fast::LdIdx { .. } | Fast::StIdx { .. } | Fast::Rmw { .. } | Fast::LdPost { .. }
            | Fast::StPost { .. } | Fast::DecBr { .. } | Fast::Ext { .. } | Fast::MemA4 { .. } | Fast::Nop
            | Fast::PushRets { .. } | Fast::PopPc { .. } | Fast::PushMask { .. } | Fast::PopMask { .. } | Fast::PushRet
            | Fast::PopRet | Fast::MinMax { .. } | Fast::IfBlk { .. } | Fast::BrIEq { .. } | Fast::BrINe { .. }
            | Fast::LdW { .. } | Fast::LdWSp { .. } | Fast::LdBu { .. } | Fast::StW { .. } | Fast::StWSp { .. }
            | Fast::AddRI { .. } | Fast::ShlI { .. } | Fast::SarI { .. } | Fast::SpAdd { .. } | Fast::SpFromR { .. }
            | Fast::RFromSp { .. } | Fast::BitExt { .. } | Fast::BitIns { .. } | Fast::SetIe { .. } | Fast::IcfgRd { .. }
            | Fast::IcfgWr { .. } | Fast::BlockMove { .. } | Fast::PopRets | Fast::Tb { .. } | Fast::Mov2 { .. }
            | Fast::Mov2I { .. } | Fast::Un { .. } | Fast::Div { .. } | Fast::LdPre { .. } | Fast::StPre { .. }
            | Fast::LdPostR { .. } | Fast::StPostR { .. } | Fast::LdPair { .. } | Fast::StPair { .. } => false,
        }
    }

    /// Does this op touch only the CPU (no bus, no clock, no interrupt window) and never jump?
    /// `cli` and `R = icfg`: compiled code calls `Cpu::exec_quiet` for them, not the executor.
    #[cfg(native_jit)]
    fn quiet(&self) -> bool {
        matches!(self, Fast::SetIe { on: false } | Fast::IcfgRd { .. })
    }

    /// May this op change pc to anything but the next instruction?
    fn may_jump(&self) -> bool {
        matches!(
            self,
            Fast::BrI { .. } | Fast::BrR { .. } | Fast::Goto { .. } | Fast::Call { .. } | Fast::Rts
                | Fast::CallR { .. } | Fast::GotoR { .. } | Fast::BitBr { .. } | Fast::DecBr { .. } | Fast::PopPc { .. } | Fast::PopRet | Fast::PopMask { .. }
                | Fast::IfBlk { .. } | Fast::BrIEq { .. } | Fast::BrINe { .. } | Fast::Tb { .. } | Fast::Rti
        )
    }

    /// The same operation in a specialised form where one exists (exactly the
    /// generic arm's semantics with its run-time choices made here).
    fn specialise(self) -> Fast {
        match self {
            Fast::BrI { a, v, cond: Cond::Eq, target } => Fast::BrIEq { a, v, target },
            Fast::BrI { a, v, cond: Cond::Ne, target } => Fast::BrINe { a, v, target },
            Fast::Ld { d, b: Base::Reg(b), off, w: 4, signed: _ } => Fast::LdW { d, b, off },
            Fast::Ld { d, b: Base::Sp, off, w: 4, signed: _ } => Fast::LdWSp { d, off },
            Fast::Ld { d, b: Base::Reg(b), off, w: 1, signed: false } => Fast::LdBu { d, b, off },
            Fast::St { s, b: Base::Reg(b), off, w: 4 } => Fast::StW { s, b, off },
            Fast::St { s, b: Base::Sp, off, w: 4 } => Fast::StWSp { s, off },
            Fast::Alu3I { d, a, v, op: Op::Add } => Fast::AddRI { d, s: a, v },
            Fast::Alu2I { d, v, op: Op::Add } => Fast::AddRI { d, s: d, v },
            Fast::A4 { d, s, op: Op::Add, rk: 1, rv } => Fast::AddRI { d, s, v: rv },
            Fast::A4 { d, s, op: Op::Shl, rk: 1, rv } => Fast::ShlI { d, s, n: (rv & 31) as u8 },
            Fast::A4 { d, s, op: Op::Sar, rk: 1, rv } => Fast::SarI { d, s, n: (rv & 31) as u8 },
            f => f,
        }
    }
}

/// The rest of the decode, used off the hot path.
#[derive(Clone, Copy, Debug)]
pub struct LineX {
    /// The instruction bytes, to validate lines of code outside XIP.
    win: u64,
    entry: &'static fm1_isa::IsaEntry,
    pre: Pre,
}

/// Which arm of `Cpu::execute` handles this instruction: the guards in the
/// same order as there, evaluated once per instruction word and cached.
fn route_of(insn: &Instruction, alu4: &Option<Alu4>, bittest: &Option<BitTest>) -> u16 {
    let name = insn.entry.name;
    match name {
            // `[++R=R]` / `[R++=R]`, the table's classes too (one executed `h[r9++=r0] = r0.h`
            // as an immediate increment); before route 19, whose `++=` test would take it so
            _ if insn.entry.len == 4 && idx_inc(insn.raw).is_some() => 32,
            // `78..7b e0 0S DS`: rD = sat16(rS) (u / s) (the table's class fits few registers)
            _ if insn.entry.len == 4 && sat16_raw(insn.raw).is_some() => 34,
            _ if bittest.is_some() => 0,
            _ if alu4.is_some() => 1,
            "nop" | "csync" | "sti_r" | "cli_r" => 2,
            "lockclr" | "lockset" => 3,
            "r_mov_i_l2" => 4,
            "sp_mov_i" => 5,
            "ssp_mov_i" => 6,
            "usp_mov_i" => 7,
            "r_mov_h" => 8,
            "r_i_or_r" | "r_i_and_r" => 9,
            "r_mov_r_lsl_i" | "r_mov_r_lsr_i" | "r_mov_r_asr_i" => 10,
            n if insn.entry.len == 4
                && n.starts_with("if_r_")
                && has(n, "_i_goto_i")
                && matches!((insn.raw >> 8) & 0xff, 0xf8 | 0xf9 | 0xfc | 0xfd | 0xfe) => 11,
            n if insn.entry.len == 6 && n.starts_with("if") && has(n, "_i_goto_i")
                && (insn.raw >> 8) & 0xff == 0xff && (insn.raw & 0xc0) == 0 => 12,
            n if n.starts_with("if") && has(n, "_r_goto_i")
                && decode_cmp_rr_branch(insn.raw, insn.entry.len).is_some() => 13,
            n if n.starts_with("if") && !has(n, "goto") && insn.entry.len == 4
                && decode_if_block(insn.raw).is_some() => 14,
            _ if insn.entry.len == 2 && (insn.raw >> 8) & 0xff == 0x17 => 15,
            _ if insn.entry.len == 4 && has(insn.entry.syntax, "d[")
                && ((insn.raw & 0xfff0) == 0xec50 || (insn.raw & 0xffff) == 0xe9d0) => 16,
            _ if insn.entry.len == 4 && (insn.raw & 0xf8) == 0x50
                && matches!((insn.raw >> 8) & 0xff, 0xec | 0xed | 0xee)
                && has(insn.entry.syntax, "[R+") && !has(insn.entry.syntax, "[R+R") => 17,
            // word accesses with a negative offset (`d4..d7 ec`): the table's classes fit only the
            // corpus samples (`r_i_mov_r_3` had no slots)
            _ if insn.entry.len == 4 && (insn.raw & 0xfc) == 0xd4 && (insn.raw >> 8) & 0xff == 0xec
                && has(insn.entry.syntax, "[") => 18,
            _ if insn.entry.name == "word_mem_craw" => 18,
            _ if insn.entry.len == 4 && (has(insn.entry.syntax, "[++R=#i]") || has(insn.entry.syntax, "[++R=-#i]"))
                && matches!((insn.raw >> 8) & 0xff, 0xec | 0xed | 0xee)
                && ((insn.raw & 0xf8) == 0x58 || (insn.raw & 0xff) == 0xd0) => 18,
            _ if insn.entry.len == 4 && (insn.raw & 0xf0) == 0xd0
                && matches!((insn.raw >> 8) & 0xff, 0xec | 0xed | 0xee)
                && has(insn.entry.syntax, "++=") => 19,
            _ if insn.entry.len == 4 && insn.raw & 0xff == 0xd8
                && matches!((insn.raw >> 8) & 0xff, 0xec | 0xed | 0xee) => 20,
            _ if insn.entry.len == 4 && matches!((insn.raw >> 8) & 0xff, 0xe1 | 0xf1)
                && matches!(insn.raw & 0xff, 0xd0 | 0xd8 | 0xf8 | 0xfc | 0xfe | 0xf6)
                && insn.entry.syntax.starts_with('r') && has(insn.entry.syntax, "_r") => 21,
            // the 64-bit pair forms `d4` / `dc` / `de` / `df` only as `pre_table_raw` vets them: the
            // table's entries on those bytes include the carry rotates (`r15_r14 <c>= #i`, bit 27),
            // which stay unsupported
            "pair_op_raw" => 21,
            _ if insn.entry.len == 4 && (insn.raw & 0xffff) == 0xe070 || (insn.raw & 0xffff) == 0xf070 => 22,
            "testset_b_r" | "testset_r" => 23,
            "ifeq_goto_i" | "ifne_goto_i" => 24,
            "if_ret_eq_i_goto_i" | "if_ret_ne_i_goto_i" => 25,
            "sp_mov_rets_r_1" | "sp_mov_rets_r_2" | "sp_mov_rets_r_3"
            | "sp_mov_rets_r_4" | "sp_mov_rets_r_6" | "sp_mov_rets_r_7" | "sp_mov_rets_mask_craw" => 26,
            "rets_r_1_mov_sp" | "rets_r_2_mov_sp" | "rets_r_3_mov_sp"
            | "rets_r_4_mov_sp" | "rets_r_6_mov_sp" | "rets_r_8_mov_sp" | "rets_mask_mov_sp_craw" => 27,
            "pc_r_1_mov_sp" | "pc_r_2_mov_sp" | "pc_r_3_mov_sp"
            | "pc_r_4_mov_sp" | "pc_r_mov_sp" | "pc_mask_mov_sp_craw" => 28,
            "sp_word_craw" => 33,
            "half_mem_craw" => 35,
            "clz_pair_craw" => 36,
            "sp_load_craw" => 37,
            "mov2_craw" => 38,
            "ssat_craw" => 39,
            "dsp_craw" => 41,
            "byte_set_craw" => 40,
            "rep_i_r" | "rep_i_i" => 29,
            n if n.starts_with("fpu_") => ROUTE_FPU,
            _ => ROUTE_SYNTAX,
    }
}

#[inline(always)]
fn ld_w<B: Bus>(bus: &mut B, w: u8, signed: bool, a: u32) -> u32 {
    match (w, signed) {
        (1, false) => bus.read8(a) as u32,
        (1, true) => bus.read8(a) as i8 as i32 as u32,
        (2, false) => bus.read16(a) as u32,
        (2, true) => bus.read16(a) as i16 as i32 as u32,
        _ => bus.read32(a),
    }
}

#[inline(always)]
fn st_w<B: Bus>(bus: &mut B, w: u8, a: u32, v: u32) {
    match w {
        1 => bus.write8(a, v as u8),
        2 => bus.write16(a, v as u16),
        _ => bus.write32(a, v),
    }
}

/// `exec_raw`'s code for route 27's 4-byte form (`d1 e8 <mask>`, a rets + register mask pop).
const RAW_POP_RETS_MASK: u8 = 0x80 | 27;

/// The special registers a `[--sp] = {..}` / `{..} = [sp++]` list may name, in the order the
/// ISA prints them (`PushSregs` / `PopSregs` keep a list as a mask over this; a list in another
/// order, or with a name not here, stays with exec_syntax).
const SREG_LIST: [&str; 10] = ["ssp", "usp", "icfg", "psr", "sr4", "rets", "retx", "rete", "reti", "sp"];
const SREG_RETI: u16 = 1 << 8;
const SREG_PSR: u16 = 1 << 3;

/// The mask over `SREG_LIST` of a printed list `{a, b, ..}`, when its names are those and in
/// that order.
fn sreg_list_mask(list: &str) -> Option<u16> {
    let inner = list.strip_prefix('{')?.strip_suffix('}')?;
    let (mut mask, mut last) = (0u16, None);
    for t in inner.split(", ") {
        let i = SREG_LIST.iter().position(|&n| n == t)?;
        if last.is_some_and(|l| i <= l) {
            return None;
        }
        mask |= 1 << i;
        last = Some(i);
    }
    (mask != 0).then_some(mask)
}

/// The route of the generic, syntax-keyed executor (`exec_syntax`).
const ROUTE_SYNTAX: u16 = 30;
const ROUTE_FPU: u16 = 31;

#[derive(Clone, Copy, Debug)]
enum Base { Reg(u8), Sp }

#[derive(Clone, Copy, Debug)]
enum Cond {
    Eq, Ne, LtU, GtU, LeU, GeU, LtS, GtS, LeS, GeS,
    /// float compares (`iff`): ordered ==, <, <=, >=, > and their unordered negations
    FEq, FUne, FLt, FUge, FLe, FUgt, FGe, FUlt, FGt, FUle,
}

impl Cond {
    /// (only the native JITs ask)
    #[cfg_attr(not(native_jit), allow(dead_code))]
    fn is_float(self) -> bool {
        matches!(self, Cond::FEq | Cond::FUne | Cond::FLt | Cond::FUge | Cond::FLe | Cond::FUgt
            | Cond::FGe | Cond::FUlt | Cond::FGt | Cond::FUle)
    }
}

#[derive(Clone, Copy, Debug)]
enum Fast {
    Mov { d: u8, s: u8 },
    MovI { d: u8, v: u32 },
    Alu3 { d: u8, a: u8, b: u8, op: Op },
    Alu3I { d: u8, a: u8, v: u32, op: Op },
    Alu2 { d: u8, b: u8, op: Op },
    Alu2I { d: u8, v: u32, op: Op },
    Ld { d: u8, b: Base, off: u32, w: u8, signed: bool },
    St { s: u8, b: Base, off: u32, w: u8 },
    BrI { a: u8, v: u32, cond: Cond, target: u32 },
    BrR { a: u8, b: u8, cond: Cond, target: u32 },
    /// an FPU op (see `fpu`)
    FOp { d: u8, a: u8, b: u8, op: u8 },
    Goto { target: u32 },
    Call { target: u32 },
    Rts,
    /// `call R` / `goto R`: a call or jump to a register's value
    CallR { r: u8 },
    GotoR { r: u8 },
    /// A dual-issue pair run second instruction first (`Cpu::swaps`, `Fast::swappable_with`):
    /// one op of two instructions, careful, run by `run_fast`'s careful path only.
    Swap { idx: u32 },
    /// `R = R op x` from the 4-byte ALU decoder: rk 0 = register, 1 =
    /// immediate, 2 = `1 << R`.
    A4 { d: u8, s: u8, op: Op, rk: u8, rv: u32 },
    /// `if ((R & mask) ==/!= 0) goto`: mk as `rk` above.
    BitBr { r: u8, mk: u8, ne: bool, mv: u32, target: u32 },
    /// `[b + (i << sh)]` loads and stores.
    LdIdx { d: u8, b: u8, i: u8, sh: u8, w: u8, signed: bool },
    StIdx { s: u8, b: u8, i: u8, sh: u8, w: u8 },
    /// `w[R+#i] op= #v` (op Mov = plain store of the immediate).
    Rmw { b: u8, w: u8, op: RmwOp, off: u32, v: u32 },
    /// `R = w[B++=#inc]` / `w[B++=#inc] = R`.
    LdPost { d: u8, b: u8, w: u8, signed: bool, inc: u32 },
    StPost { s: u8, b: u8, w: u8, inc: u32 },
    /// `if (--R op #v) goto`.
    DecBr { r: u8, cond: Cond, v: u32, target: u32 },
    /// `R = R.b0|l|h (u|s)`: part 0 = b0, 1 = l, 2 = h.
    Ext { d: u8, s: u8, part: u8, signed: bool },
    /// `[R+off] op= x` from the 4-byte ALU decoder (word RMW).
    MemA4 { b: u8, off: i32, op: Op, rk: u8, rv: u32 },
    Nop,
    /// `[--sp] = {rets, ..}` / `{pc, ..} = [sp++]` (arms 26 / 28, n = raw & 0xf)
    PushRets { n: u8 },
    PopPc { n: u8 },
    /// the 4-byte forms with a register mask (arms 26 / 28)
    PushMask { mask: u16 },
    PopMask { mask: u16 },
    /// `[--sp] = rets` / `pc = [sp++]`
    PushRet,
    PopRet,
    /// `[--sp] = {R*n}` / `{R*n} = [sp++]`, no rets: the registers of the mask, the highest
    /// pushed first (the 2-byte forms' [max(n, 3) .. min(n, 4)] as a mask, the 4-byte forms' own)
    PushRegs { mask: u16 },
    PopRegs { mask: u16 },
    /// `[--sp] = {psr, rets, reti}` / `{psr, rets, reti} = [sp++]` and the other lists of special
    /// registers (the ISR wrappers): a bit per name of `SREG_LIST`, pushed in its order and popped
    /// reversed, as exec_syntax's arm (the first in it). Reti in the list opens / closes nesting.
    PushSregs { mask: u16 },
    PopSregs { mask: u16 },
    Rti,
    /// `R = R + R + c` / `R = R - R - !c`
    AddC { d: u8, a: u8, b: u8, sub: bool },
    /// `R = smin|smax|umin|umax(R, R)`: kind 0 smin, 1 smax, 2 umin, 3 umax
    MinMax { d: u8, a: u8, b: u8, kind: u8 },
    /// predicated `if (..) {` (arm 14): block ends as offsets from the
    /// instruction's next pc, worked out at decode (XIP code only)
    IfBlk { a: u8, rk: u8, fam: u8, alt: bool, has_else: bool, rv: u32, then_d: u16, else_d: u16 },
    // specialised forms of the hottest generic ops (`Fast::specialise`):
    // no inner dispatch on condition, width or operation
    BrIEq { a: u8, v: u32, target: u32 },
    BrINe { a: u8, v: u32, target: u32 },
    LdW { d: u8, b: u8, off: u32 },
    LdWSp { d: u8, off: u32 },
    LdBu { d: u8, b: u8, off: u32 },
    StW { s: u8, b: u8, off: u32 },
    StWSp { s: u8, off: u32 },
    AddRI { d: u8, s: u8, v: u32 },
    ShlI { d: u8, s: u8, n: u8 },
    SarI { d: u8, s: u8, n: u8 },
    // stack-pointer moves, bit fields and the global interrupt enable (were slow-path syntax)
    /// `sp += v`
    SpAdd { v: u32 },
    /// `sp = R`
    SpFromR { s: u8 },
    /// `R = sp + v`
    RFromSp { d: u8, v: u32 },
    /// `R = u/sextra(R, p, l)`
    BitExt { d: u8, s: u8, p: u8, l: u8, signed: bool },
    /// `R <= insert(R, p, l)`
    BitIns { d: u8, s: u8, p: u8, l: u8 },
    /// `sti` / `cli`
    SetIe { on: bool },
    /// `R = icfg` / `icfg = R`
    IcfgRd { d: u8 },
    IcfgWr { s: u8 },
    /// `{regs} = [R+]` / `[R+] = {regs}` (and `[R++]`: the base moves past the block); `down`:
    /// `[-R]` / `[--R]`, the block below the base (`--`: the base moves to its start)
    BlockMove { base: u8, mask: u16, store: bool, post: bool, down: bool },
    // were slow path: the raw-decoded routes, and the hottest syntax-keyed shapes
    /// execute's routes decoded from the raw word (pair loads / stores, pre / post-increment
    /// accesses, 64-bit ops, rev8, rets pops): `exec_raw`
    Raw { route: u8, raw: u32 },
    /// `rH_rL = d[B+off]` / `d[B+off] = rH_rL`: route 16 without a base update, the pair's low
    /// register (2p) at the lower address
    LdPair { p: u8, b: Base, off: u32 },
    StPair { p: u8, b: Base, off: u32 },
    /// `{rets} = [sp++]`
    PopRets,
    /// `tbb [R]` / `tbh [R]`: the jump table right after the instruction
    Tb { r: u8, half: bool },
    /// `rA_rB = rC_rD` / `rA_rB = #i` (the immediate sign-extended)
    Mov2 { dh: u8, dl: u8, sh: u8, sl: u8 },
    Mov2I { dh: u8, dl: u8, v: u32 },
    /// `R = ~R | -R | abs(R) | rev8(R) | clz(R)`: k 0..4
    Un { d: u8, s: u8, k: u8 },
    /// `R = R / R (s) | / (u) | % (s) | % (u)`: k 0..3 (by zero: 0)
    Div { d: u8, a: u8, b: u8, k: u8 },
    /// `R = [++B=I]` / `[++B=I] = R` (i 16: the immediate inc) and `R = [B++=I]` / `[B++=I] = R`
    LdPre { d: u8, b: u8, i: u8, w: u8, signed: bool, inc: u32 },
    StPre { s: u8, b: u8, i: u8, w: u8, inc: u32 },
    LdPostR { d: u8, b: u8, i: u8, w: u8, signed: bool },
    StPostR { s: u8, b: u8, i: u8, w: u8 },
}

/// A register-base block move's form, from its syntax: `{regs} = [R+]`, `[R++]`, `[-R]`, `[--R]` and
/// the stores `[R+] = {regs}` ...: (a store, the base written back, the block below the base). The sp
/// forms (`[--sp] = {...}`) are not these.
fn block_form(s: &str) -> Option<(bool, bool, bool)> {
    let (store, m) = if let Some(rest) = s.strip_prefix('[') {
        (true, rest.split_once("] = {")?.0)
    } else if s.starts_with('{') {
        (false, s.split_once("} = [")?.1.strip_suffix(']')?)
    } else {
        return None;
    };
    match m {
        "R+" => Some((store, false, false)),
        "R++" => Some((store, true, false)),
        "-R" => Some((store, false, true)),
        "--R" => Some((store, true, true)),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
enum RmwOp { Mov, Or, And, Xor, Add, Sub }

/// `psr` flag bits, as compiled code reads them: `slt` tests N ^ V (bits 0 and 3), `ult` C
/// (bit 1, set = no borrow, as on ARM); Z (bit 2) is assumed.
pub const F_N: u32 = 1;
pub const F_C: u32 = 2;
pub const F_Z: u32 = 4;
pub const F_V: u32 = 8;

/// a + b + cin with the flags (subtract: a + !b + 1, with borrow: a + !b + C)
#[inline]
fn add_flags(a: u32, b: u32, cin: u32) -> (u32, u32) {
    let w = a as u64 + b as u64 + cin as u64;
    let r = w as u32;
    let f = (r >> 31) * F_N | ((w >> 32) as u32) * F_C | ((r == 0) as u32) * F_Z | (((a ^ r) & (b ^ r)) >> 31) * F_V;
    (r, f)
}

#[inline(always)]
fn alu(op: Op, a: u32, b: u32) -> u32 {
    match op {
        Op::Add => a.wrapping_add(b),
        Op::Sub => a.wrapping_sub(b),
        Op::RSub => b.wrapping_sub(a),
        Op::And => a & b,
        Op::AndNot => a & !b,
        Op::Or => a | b,
        Op::Xor => a ^ b,
        Op::Mul => a.wrapping_mul(b),
        Op::Shl => a.wrapping_shl(b & 31),
        Op::Shr => a.wrapping_shr(b & 31),
        Op::Sar => ((a as i32) >> (b & 31)) as u32,
    }
}

/// `cmp_cond(fam, alt, ..)` as a `Cond`.
fn cond_of_fam(fam: u64, alt: bool) -> Cond {
    match (fam, alt) {
        (0, false) => Cond::Eq,
        (0, true) => Cond::Ne,
        (1, false) => Cond::GeU,
        (1, true) => Cond::LtU,
        (4, false) => Cond::GtU,
        (4, true) => Cond::LeU,
        (5, false) => Cond::GeS,
        (5, true) => Cond::LtS,
        (8, false) => Cond::FEq,
        (8, true) => Cond::FUne,
        (9, false) => Cond::FUge,
        (9, true) => Cond::FLt,
        (12, false) => Cond::FUgt,
        (12, true) => Cond::FLe,
        (13, false) => Cond::FGe,
        (13, true) => Cond::FUlt,
        (14, false) => Cond::FGt,
        (14, true) => Cond::FUle,
        (_, false) => Cond::GtS,
        (_, true) => Cond::LeS,
    }
}

fn rhs_kind(r: Rhs) -> (u8, u32) {
    match r {
        Rhs::Reg(x) => (0, x as u32),
        Rhs::Imm(v) => (1, v),
        Rhs::Bit(x) => (2, x as u32),
    }
}

/// Micro-ops for the raw-decoded arms of `Cpu::execute`; each mirrors its
/// arm exactly (route numbers as in `route_of`).
fn fast_raw(route: u16, insn: &Instruction, alu4: &Option<Alu4>, bittest: &Option<BitTest>) -> Option<Fast> {
    let raw = insn.raw;
    let next_pc = insn.addr.wrapping_add(insn.entry.len as u32);
    match route {
        0 => {
            let t = (*bittest)?;
            let (mk, mv) = rhs_kind(t.mask);
            Some(Fast::BitBr { r: t.reg as u8, mk, ne: t.ne, mv, target: (next_pc as i64 + t.off) as u32 })
        }
        1 => {
            let a = (*alu4)?;
            match a.dst {
                Dst::Reg(d) => {
                    let (rk, rv) = rhs_kind(a.rhs);
                    Some(Fast::A4 { d: d as u8, s: a.src as u8, op: a.op, rk, rv })
                }
                Dst::Mem(off) => {
                    let (rk, rv) = rhs_kind(a.rhs);
                    Some(Fast::MemA4 { b: a.src as u8, off, op: a.op, rk, rv })
                }
            }
        }
        2 => Some(Fast::Nop),
        // (the arms read bits below 32 only; route 21's table-less form carries the whole window)
        27 if insn.entry.len == 4 => Some(Fast::Raw { route: RAW_POP_RETS_MASK, raw: raw as u32 }),
        // accesses that update their base (routes 18 / 19, `[++rB=rI]` / `[rB++=rI]` 32) as the
        // micro-ops that give exactly the arm's result, which the JITs compile inline: in an
        // audio loop they are most of what went through the interpreter's executor (Melodee
        // 1.4.5: 1.5 million a second). The arm stays for a load into the base (its base update
        // comes last, the micro-ops' first or from the loaded value), route 32's store of the
        // base or of a high half, and words idx_inc does not take
        18 | 19 => Some(inc_mem_fast(route as u8, raw as u32)),
        32 => Some(idx_inc_fast(raw)),
        // (pair loads and stores without a base update, and the DSP group's min / max, as
        // micro-ops the JITs compile: in Melodee's and Felucca's audio code, with the others below,
        // the most frequent ops left for the interpreter's executor. Not a dual-issue pair's head:
        // as a micro-op it could be glued to the next instruction, which moved interrupts in Skiff
        // and NMSFM; the raw arm keeps them where they were)
        16 => Some({
            let m = decode_pair_mem(raw);
            if m.pre || m.post || m.index.is_some() || insn.entry.syntax.trim_end().ends_with('#') {
                Fast::Raw { route: 16, raw: raw as u32 }
            } else {
                let (p, b, off) = (m.pair as u8, m.base.map_or(Base::Sp, |b| Base::Reg(b as u8)), m.off as u32);
                if m.store { Fast::StPair { p, b, off } } else { Fast::LdPair { p, b, off } }
            }
        }),
        41 if !insn.entry.syntax.trim_end().ends_with('#') && matches!(dsp_raw(raw), Some(Dsp::MinMax)) => Some(Fast::MinMax {
            d: ((raw >> 28) & 0xf) as u8,
            a: ((raw >> 20) & 0xf) as u8,
            b: ((raw >> 24) & 0xf) as u8,
            // (exec_raw's: bit 0 min, bit 16 signed; MinMax's 0 smin, 1 smax, 2 umin, 3 umax)
            kind: match (raw & 1 != 0, (raw >> 16) & 1 != 0) { (true, true) => 0, (false, true) => 1, (true, false) => 2, (false, false) => 3 },
        }),
        21 | 22 | 27 | 39 | 40 | 41 => Some(Fast::Raw { route: route as u8, raw: raw as u32 }),
        26 if insn.entry.len == 4 => Some(Fast::PushMask { mask: ((raw >> 16) & 0xffff) as u16 }),
        28 if insn.entry.len == 4 => Some(Fast::PopMask { mask: ((raw >> 16) & 0xffff) as u16 }),
        35 => {
            let m = half_mem(raw)?;
            let (d, b) = (m.d as u8, m.b as u8);
            match (m.store, m.pre) {
                (false, false) => Some(Fast::Ld { d, b: Base::Reg(b), off: m.off as u32, w: 2, signed: m.flag }),
                (false, true) => Some(Fast::LdPre { d, b, i: 16, w: 2, signed: m.flag, inc: m.off as u32 }),
                // (the store of the high half has no micro-op: exec_raw's arm. None here sent it to
                // the step's route match, which has no 35 arm: a panic, found by tools/fuzz_decode.py)
                (true, _) if m.flag => Some(Fast::Raw { route: 35, raw: raw as u32 }),
                (true, false) => Some(Fast::St { s: d, b: Base::Reg(b), off: m.off as u32, w: 2 }),
                (true, true) => Some(Fast::StPre { s: d, b, i: 16, w: 2, inc: m.off as u32 }),
            }
        }
        36 => Some(Fast::Un { d: ((raw >> 28) & 0xf) as u8, s: ((raw >> 24) & 0xf) as u8, k: 4 }),
        37 => {
            let m = sp_mem_raw(raw)?;
            if m.store {
                Some(Fast::St { s: m.d, b: Base::Sp, off: m.off, w: 2 })
            } else {
                Some(Fast::Ld { d: m.d, b: Base::Sp, off: m.off, w: m.w, signed: m.signed })
            }
        }
        38 => {
            let (d, s) = ((raw & 0xe) as u8, ((raw >> 4) & 0xe) as u8);
            Some(Fast::Mov2 { dh: d + 1, dl: d, sh: s + 1, sl: s })
        }
        34 => {
            let (d, s, signed) = sat16_raw(raw)?;
            Some(Fast::Un { d, s, k: if signed { 5 } else { 6 } })
        }
        33 => {
            let (r, off) = (((raw >> 28) & 0xf) as u8, ((raw >> 16) & 0xffe) as u32);
            Some(if raw & 0x1_0000 != 0 { Fast::StWSp { s: r, off } } else { Fast::LdWSp { d: r, off } })
        }
        26 => Some(Fast::PushRets { n: (raw & 0xf) as u8 }),
        28 => Some(Fast::PopPc { n: (raw & 0xf) as u8 }),
        15 => Some(Fast::Ext {
            d: (raw & 7) as u8,
            s: ((raw >> 4) & 7) as u8,
            part: if raw & 0x80 != 0 { 1 } else { 0 },
            signed: raw & 8 != 0,
        }),
        4 => {
            let r = ((raw & 7) | ((raw >> 7 & 1) << 3)) as u8;
            let imm = if (raw & 0xc0) == 0xc0 && (raw >> 8 & 0xff) == 0x14 {
                0
            } else {
                (((raw >> 8) & 0x1f) | (((raw >> 3) & 7) << 5)) as u32
            };
            Some(Fast::MovI { d: r, v: imm })
        }
        8 => Some(Fast::MovI {
            d: ((raw >> 28) & 0xf) as u8,
            v: composed_imm(((raw >> 24) & 0xf) as u32, ((raw >> 16) & 0xff) as u32),
        }),
        10 => {
            let op = if has(insn.entry.name, "lsr") { Op::Shr } else if has(insn.entry.name, "asr") { Op::Sar } else { Op::Shl };
            let (d, s, n) = ((raw & 7) as u8, ((raw >> 4) & 7) as u8, ((raw >> 8) & 0x1f) as u32);
            Some(match (n, op) {
                // a count of 0 shifts by 32: logical shifts give 0, >>> the sign fill (= >>> 31)
                (0, Op::Sar) => Fast::A4 { d, s, op, rk: 1, rv: 31 },
                (0, _) => Fast::MovI { d, v: 0 },
                _ => Fast::A4 { d, s, op, rk: 1, rv: n },
            })
        }
        11 => {
            let fam = (raw >> 8) & 0xff;
            let alt = raw & 0x80 != 0;
            let imm10 = (((raw >> 25) & 0x7f) | (((raw >> 4) & 7) << 7)) as u32;
            let imm_s = if imm10 & 0x200 != 0 { imm10 | !0x3ff } else { imm10 };
            let f = (((raw >> 16) & 0xff) | (((raw >> 24) & 1) << 8)) as i64;
            let off = 2 * if f & 0x100 != 0 { f - 0x200 } else { f };
            let (cond, v) = match (fam, alt) {
                (0xf8, false) => (Cond::Eq, imm_s),
                (0xf8, true) => (Cond::Ne, imm_s),
                (0xf9, false) => (Cond::GeU, imm10),
                (0xf9, true) => (Cond::LtU, imm10),
                (0xfc, false) => (Cond::GtU, imm10),
                (0xfc, true) => (Cond::LeU, imm10),
                (0xfd, false) => (Cond::GeS, imm_s),
                (0xfd, true) => (Cond::LtS, imm_s),
                (0xfe, false) => (Cond::GtS, imm_s),
                _ => (Cond::LeS, imm_s),
            };
            Some(Fast::BrI { a: (raw & 0xf) as u8, v, cond, target: (insn.addr as i64 + 4 + off) as u32 })
        }
        12 => {
            let b0 = raw & 0xff;
            let fam = (b0 >> 1) & 7;
            let imm = if b0 & 0x20 != 0 {
                composed_imm(((raw >> 24) & 0xf) as u32, ((raw >> 16) & 0xff) as u32)
            } else {
                let v = ((raw >> 16) & 0xfff) as u32;
                if v & 0x800 != 0 && !matches!(fam, 1 | 4) { v | !0xfff } else { v }
            };
            let off = 2 * ((raw >> 32) as u16 as i16 as i64);
            Some(Fast::BrI { a: ((raw >> 28) & 0xf) as u8, v: imm, cond: cond_of_fam(fam, b0 & 1 != 0), target: (next_pc as i64 + off) as u32 })
        }
        ROUTE_FPU => {
            let (op, d, a, b) = decode_fpu(raw);
            Some(Fast::FOp { d, a, b, op })
        }
        13 => {
            let (fam, alt, a, b, off) = decode_cmp_rr_branch(raw, insn.entry.len)?;
            Some(Fast::BrR { a: a as u8, b: b as u8, cond: cond_of_fam(fam, alt), target: (next_pc as i64 + off) as u32 })
        }
        17 => {
            let w = match (raw >> 8) & 0xff { 0xec => 4u8, 0xed => 2, _ => 1 };
            let d = ((raw >> 28) & 0xf) as u8;
            let b = ((raw >> 20) & 0xf) as u8;
            let f = (((raw >> 16) & 0xf) | (((raw >> 24) & 0xf) << 4)) as u32;
            let (store, off, signed) = if w == 1 {
                let off = if raw & 1 != 0 { f as i32 - 256 } else { f as i32 };
                ((raw >> 1) & 1 != 0, off, (raw >> 2) & 1 != 0)
            } else {
                let v = (f & !1) | (((raw & 1) as u32) << 8) | ((((raw >> 1) & 1) as u32) << 9);
                let off = if v & 0x200 != 0 { v as i32 - 0x400 } else { v as i32 };
                ((raw >> 16) & 1 != 0, off, w == 2 && (raw >> 2) & 1 != 0)
            };
            Some(if store {
                Fast::St { s: d, b: Base::Reg(b), off: off as u32, w }
            } else {
                Fast::Ld { d, b: Base::Reg(b), off: off as u32, w, signed }
            })
        }
        20 => {
            let kind = (raw >> 8) & 0xff;
            let sub = (raw >> 16) & 0xf;
            let d = ((raw >> 28) & 0xf) as u8;
            let b = ((raw >> 20) & 0xf) as u8;
            let i = ((raw >> 24) & 0xf) as u8;
            let sh = if sub & 8 != 0 { if kind == 0xec { 2 } else { 1 } } else { 0 };
            Some(match (kind, sub & 3) {
                (0xec, 2) => Fast::LdIdx { d, b, i, sh, w: 4, signed: false },
                (0xec, 3) => Fast::StIdx { s: d, b, i, sh, w: 4 },
                (0xed, 0) => Fast::LdIdx { d, b, i, sh, w: 2, signed: false },
                (0xed, 1) => Fast::StIdx { s: d, b, i, sh, w: 2 },
                (0xed, 2) => Fast::LdIdx { d, b, i, sh, w: 2, signed: true },
                (0xee, 0) => Fast::LdIdx { d, b, i, sh, w: 1, signed: false },
                (0xee, 1) => Fast::StIdx { s: d, b, i, sh, w: 1 },
                (0xee, 2) => Fast::LdIdx { d, b, i, sh, w: 1, signed: true },
                _ => return None,
            })
        }
        _ => None,
    }
}

#[inline(always)]
fn cond_true(c: Cond, l: u32, r: u32) -> bool {
    match c {
        Cond::Eq => l == r,
        Cond::Ne => l != r,
        Cond::LtU => l < r,
        Cond::GtU => l > r,
        Cond::LeU => l <= r,
        Cond::GeU => l >= r,
        Cond::LtS => (l as i32) < (r as i32),
        Cond::GtS => (l as i32) > (r as i32),
        Cond::LeS => (l as i32) <= (r as i32),
        Cond::GeS => (l as i32) >= (r as i32),
        _ => fcond(c, l, r),
    }
}

/// A float compare on the raw register bits (NaN: ordered compares false, unordered true).
#[cold]
fn fcond(c: Cond, l: u32, r: u32) -> bool {
    let (x, y) = (f32::from_bits(l), f32::from_bits(r));
    match c {
        Cond::FEq => x == y,
        Cond::FUne => !(x == y),
        Cond::FLt => x < y,
        Cond::FUge => !(x < y),
        Cond::FLe => x <= y,
        Cond::FUgt => !(x <= y),
        Cond::FGe => x >= y,
        Cond::FUlt => !(x >= y),
        Cond::FGt => x > y,
        _ => !(x > y),
    }
}

/// The FPU ops (`3f e5`, see fm1-isa's fpu.rs): binary op 0 +, 1 -, 2 *, 3 /, 5 fmin, 6 fmax on
/// (a, b); unary `K f` on a: K 0..3 ftoi, 4..7 ftou, c..f ftof (a whole float), rounding by
/// K & 3 (0 to nearest even, 1 trunc, 2 ceil, 3 floor); 0x8f itof, 0x9f utof.
fn fpu(op: u8, a: u32, b: u32) -> u32 {
    let (x, y) = (f32::from_bits(a), f32::from_bits(b));
    let round = |k: u8| match k & 3 { 0 => x.round_ties_even(), 1 => x.trunc(), 2 => x.ceil(), _ => x.floor() };
    match op {
        0 => (x + y).to_bits(),
        1 => (x - y).to_bits(),
        2 => (x * y).to_bits(),
        3 => (x / y).to_bits(),
        5 => x.min(y).to_bits(),
        6 => x.max(y).to_bits(),
        0x8f => (a as i32 as f32).to_bits(),
        0x9f => (a as f32).to_bits(),
        _ => match op >> 4 {
            k @ 0..=3 => round(k) as i32 as u32,
            k @ 4..=7 => round(k) as u32,
            k => round(k).to_bits(),
        },
    }
}

/// `d8|d9|dc|dd e9 OO RX`: sp-relative byte / halfword accesses, R = bits(28,31), off12 = X << 8 |
/// OO (the vendor objdump's layout, tools/fuzz_decode.py; all corpus samples fit):
///   `dc` rR = b[sp+off] (u), `dd` (s); `d9` rR = h[sp+off] (s), an odd off not an instruction;
///   `d8` the halfword at off & ~1, bit 0 a store `h[sp+off] = rR`, else a load (u).
/// None: not one of these.
#[derive(Clone, Copy, Debug)]
struct SpMem { d: u8, off: u32, w: u8, signed: bool, store: bool }
fn sp_mem_raw(raw: u64) -> Option<SpMem> {
    if (raw >> 8) & 0xff != 0xe9 {
        return None;
    }
    let (b2, b3) = ((raw >> 16) & 0xff, (raw >> 24) & 0xff);
    let (d, off) = ((b3 >> 4) as u8, ((b3 & 0xf) << 8 | b2) as u32);
    match raw & 0xff {
        0xdc => Some(SpMem { d, off, w: 1, signed: false, store: false }),
        0xdd => Some(SpMem { d, off, w: 1, signed: true, store: false }),
        0xd9 if off & 1 == 0 => Some(SpMem { d, off, w: 2, signed: true, store: false }),
        0xd8 => Some(SpMem { d, off: off & !1, w: 2, signed: false, store: off & 1 != 0 }),
        _ => None,
    }
}

/// Routes 18 and 19, the 4-byte single-register accesses with an increment: data = bits(28,31),
/// base = bits(20,23), f = bits(16,19) | bits(24,27)<<4; the width from byte 1 (`ec` word, `ed`
/// half, `ee` byte).
/// Route 18, pre-increment `R = [++rB=#i]` / `[++rB=#i] = R` (446 corpus samples, 0 mismatches):
///   half/byte `58..5f ed/ee`: route 19's fields, the base advanced before the access;
///   word `d0..d7 ec`: store = bit16, bit17 = pre-increment (clear: a plain offset, `[rB+-#i]`,
///   no write-back), off = 4*sext9(bits(18,19) | bits(24,27)<<2 | bit0<<6 | bit1<<7 | bit2<<8):
///   the pair accesses' field (27884 corpus samples, 0 mismatches; Felucca 1.6's `[r5+-128] = r3`).
/// Route 19, post-increment `R = [rB++=#i]` / `[rB++=#i] = R` (`dx ec/ed/ee`; fitted on all 145
/// V13 corpus samples):
///   half: store = bit16; inc = sext10(f&~1 | bit0<<8 | bit1<<9); bit2 = signed (loads)
///   word (`d8..df ec`): store = bit16; inc = sext11(f&~1 | bit0<<8 | bit1<<9 | bit2<<10) (the
///     vendor objdump's, tools/fuzz_decode.py; sext10 had sent Felucca's `r11 = [r12++=-820]` to
///     +204 and DUELL's `[r1++=516]` to -508)
///   byte: store = bit1; bit2 = signed load; inc = bit0 ? sext8(f) : f (a 9-bit two's complement:
///     `b[r4+-163]` = 93-256)
#[derive(Clone, Copy, Debug)]
struct IncMem { d: usize, b: usize, w: u8, store: bool, signed: bool, inc: i32, pre: bool, post: bool }
/// Routes 18 / 19 as micro-ops the compilers have (exec_raw's arm, its rules kept): a
/// pre-increment is `LdPre` / `StPre` (`i: 16`, base == data included), a post-increment
/// `LdPost` / `StPost`, a word at an offset `Ld` / `St`. A post-increment load into its own base
/// must stay raw: exec_raw updates the base after the load, so it leaves the incremented base,
/// while `LdPost` adds to what was loaded (it reads b after the load).
fn inc_mem_fast(route: u8, raw: u32) -> Fast {
    let m = decode_inc_mem(route, raw as u64);
    let (d, b, inc) = (m.d as u8, m.b as u8, m.inc as u32);
    match (m.store, m.pre, m.post) {
        (false, true, _) => Fast::LdPre { d, b, i: 16, w: m.w, signed: m.signed, inc },
        (true, true, _) => Fast::StPre { s: d, b, i: 16, w: m.w, inc },
        (true, false, true) => Fast::StPost { s: d, b, w: m.w, inc },
        (false, false, true) if d != b => Fast::LdPost { d, b, w: m.w, signed: m.signed, inc },
        (true, false, false) => Fast::St { s: d, b: Base::Reg(b), off: inc, w: m.w },
        (false, false, false) => Fast::Ld { d, b: Base::Reg(b), off: inc, w: m.w, signed: m.signed },
        _ => Fast::Raw { route, raw },
    }
}

fn decode_inc_mem(route: u8, raw: u64) -> IncMem {
    let w = match (raw >> 8) & 0xff { 0xec => 4u8, 0xed => 2, _ => 1 };
    let d = ((raw >> 28) & 0xf) as usize;
    let b = ((raw >> 20) & 0xf) as usize;
    let f = (((raw >> 16) & 0xf) | (((raw >> 24) & 0xf) << 4)) as u32;
    let (store, inc, signed) = if w == 1 {
        let inc = if raw & 1 != 0 { f as i32 - 256 } else { f as i32 };
        ((raw >> 1) & 1 != 0, inc, (raw >> 2) & 1 != 0)
    } else if w == 4 && route == 18 {
        let o = ((raw >> 18) & 3) | (((raw >> 24) & 0xf) << 2) | ((raw & 1) << 6) | (((raw >> 1) & 1) << 7) | (((raw >> 2) & 1) << 8);
        let o = if o & 0x100 != 0 { o as i32 - 0x200 } else { o as i32 };
        ((raw >> 16) & 1 != 0, 4 * o, false)
    } else if w == 4 {
        let v = (f & !1) | (((raw & 7) as u32) << 8);
        let inc = if v & 0x400 != 0 { v as i32 - 0x800 } else { v as i32 };
        ((raw >> 16) & 1 != 0, inc, false)
    } else {
        let v = (f & !1) | (((raw & 1) as u32) << 8) | ((((raw >> 1) & 1) as u32) << 9);
        let inc = if v & 0x200 != 0 { v as i32 - 0x400 } else { v as i32 };
        ((raw >> 16) & 1 != 0, inc, (raw >> 2) & 1 != 0)
    };
    let word_off = route == 18 && w == 4 && (raw >> 17) & 1 == 0;
    IncMem { d, b, w, store, signed, inc, pre: route == 18 && !word_off, post: route == 19 }
}

/// `5x ed` halfword accesses: data = bits(28,31), base = bits(20,23), bit16 store, byte 0 bit 2
/// signed (a load) / the high half (a store), bit 3 pre-increment; off = sext10(byte0 & 3 << 8 |
/// bits(24,27) << 4 | bits(17,19) << 1), in bytes. All 30337 corpus samples fit.
#[derive(Clone, Copy, Debug)]
struct HalfMem { d: usize, b: usize, store: bool, flag: bool, pre: bool, off: i32 }
fn half_mem(raw: u64) -> Option<HalfMem> {
    if (raw >> 8) & 0xff != 0xed || raw & 0xf0 != 0x50 {
        return None;
    }
    let (b0, b2, b3) = (raw & 0xff, (raw >> 16) & 0xff, (raw >> 24) & 0xff);
    let x = ((b0 & 3) << 8 | (b3 & 15) << 4 | (b2 & 14)) as i32;
    Some(HalfMem {
        d: (b3 >> 4) as usize,
        b: (b2 >> 4) as usize,
        store: b2 & 1 != 0,
        flag: b0 & 4 != 0,
        pre: b0 & 8 != 0,
        off: if x & 512 != 0 { x - 1024 } else { x },
    })
}

/// `<<<`, the vendor compiler's `__builtin_pi32v2_qasl`: a shift left that saturates to the
/// signed 32-bit range (plain C `x << n` compiles to `<<`; work/probe/sem.c)
fn qasl(x: u32, n: u32) -> u32 {
    let x = x as i32 as i64;
    if x == 0 {
        0
    } else if n >= 32 {
        if x < 0 { i32::MIN as u32 } else { i32::MAX as u32 }
    } else {
        (x << n).clamp(i32::MIN as i64, i32::MAX as i64) as u32
    }
}

/// `78..7b e0 0S DS`: rD = sat16(rS), S 1 signed (s), 0 unsigned (u); byte 0's low bits do not
/// change it (all 22 corpus samples). (d, s, signed).
fn sat16_raw(raw: u64) -> Option<(u8, u8, bool)> {
    if !matches!((raw >> 8) & 0xff, 0xe0 | 0xf0) || raw & 0xfc != 0x78 || (raw >> 16) & 0xfe != 0 {
        return None;
    }
    Some((((raw >> 28) & 0xf) as u8, ((raw >> 24) & 0xf) as u8, (raw >> 16) & 1 != 0))
}

/// `fpu` with the destination's old value: ops 7 and 8 are `d += a * b` / `d -= a * b` (the
/// vendor assembler's `R += R * R (f)`, `R -= R * R (f)`). Not fused: JieLi's clang implements
/// fmaf with a library call rather than with them, so the product is rounded before the add.
fn fpu_d(op: u8, d: u32, a: u32, b: u32) -> u32 {
    let (x, y, acc) = (f32::from_bits(a), f32::from_bits(b), f32::from_bits(d));
    match op {
        7 => (acc + x * y).to_bits(),
        8 => (acc - x * y).to_bits(),
        _ => fpu(op, a, b),
    }
}

/// (op, d, a, b) of an FPU instruction word.
fn decode_fpu(raw: u64) -> (u8, u8, u8, u8) {
    let b2 = ((raw >> 16) & 0xff) as u8;
    let b3 = ((raw >> 24) & 0xff) as u8;
    if b2 & 0xf == 0xf {
        (b2, b3 >> 4, b3 & 0xf, 0)
    } else {
        (b2 & 0xf, b3 >> 4, b2 >> 4, b3 & 0xf)
    }
}

/// A micro-op for an instruction that `exec_syntax` would handle, when its
/// shape is one of the common simple ones; operands come from the same
/// slots in the same order, so the semantics are identical.
fn fast_of(insn: &Instruction, s: &'static str) -> Option<Fast> {
    let slots = insn.entry.slots;
    let val = |i: usize| -> Option<i64> { slots.get(i).map(|sl| sl.value(insn.raw)) };
    let v = |i: usize| -> Option<u32> { val(i).map(|x| x as u32) };
    let r = |i: usize| -> Option<u8> { val(i).map(|x| (x as usize & 0xf) as u8) };
    let next_pc = insn.addr.wrapping_add(insn.entry.len as u32);
    let target = |i: usize| -> Option<u32> { val(i).map(|off| (next_pc as i64 + off) as u32) };
    let op_of = |c: &str| -> Option<Op> {
        Some(match c { "+" => Op::Add, "-" => Op::Sub, "&" => Op::And, "|" => Op::Or, "^" => Op::Xor, "*" => Op::Mul, _ => return None })
    };
    let cond_of = |op: &str, signed: bool| -> Option<Cond> {
        Some(match (op, signed) {
            ("==", _) => Cond::Eq, ("!=", _) => Cond::Ne,
            ("<", false) => Cond::LtU, (">", false) => Cond::GtU, ("<=", false) => Cond::LeU, (">=", false) => Cond::GeU,
            ("<", true) => Cond::LtS, (">", true) => Cond::GtS, ("<=", true) => Cond::LeS, (">=", true) => Cond::GeS,
            _ => return None,
        })
    };
    // `if[s] (--R op #i) goto #i`: decrement, compare, branch (slots r, v, off)
    for (prefix, signed) in [("if (--R ", false), ("ifs (--R ", true)] {
        if let Some(rest) = s.strip_prefix(prefix).and_then(|x| x.strip_suffix(") goto #i")) {
            let (op, rhs) = rest.split_once(' ')?;
            if rhs != "#i" && rhs != "#h" {
                return None;
            }
            return Some(Fast::DecBr { r: r(0)?, cond: cond_of(op, signed)?, v: v(1)?, target: target(2)? });
        }
    }
    match s {
        "sp += #i" => {
            // 4-byte `f0 e8 <imm>`: sext13(bits(16,28)), as exec_syntax
            let imm = if insn.entry.len == 4 && insn.raw & 0xffff == 0xe8f0 {
                let f = ((insn.raw >> 16) & 0x1fff) as u32;
                if f & 0x1000 != 0 { f | !0x1fff } else { f }
            } else {
                v(0)?
            };
            return Some(Fast::SpAdd { v: imm });
        }
        "sp = R" => return Some(Fast::SpFromR { s: r(0)? }),
        "R = sp" => return Some(Fast::RFromSp { d: r(0)?, v: 0 }),
        "R = sp + #i" => return Some(Fast::RFromSp { d: r(0)?, v: v(1)? }),
        "R = uextra(R, p:#i, l:#i)" | "R = sextra(R, p:#i, l:#i)" => {
            return Some(Fast::BitExt { d: r(0)?, s: r(1)?, p: v(2)? as u8 & 31, l: v(3)? as u8 & 31, signed: s.contains("sextra") });
        }
        "R <= insert(R, p:#i, l:#i)" => return Some(Fast::BitIns { d: r(0)?, s: r(1)?, p: v(2)? as u8 & 31, l: v(3)? as u8 & 31 }),
        "cli" => return Some(Fast::SetIe { on: false }),
        "sti" => return Some(Fast::SetIe { on: true }),
        "R = icfg" => return Some(Fast::IcfgRd { d: r(0)? }),
        "icfg = R" => return Some(Fast::IcfgWr { s: r(0)? }),
        // block push / pop without rets (exec_syntax's arm): 2-byte `6n 04` / `4n 04`, the
        // registers [max(n, 3) .. min(n, 4)]; 4-byte `d8 e8` / `d4 e8`, a mask at bits 16-31
        "[--sp] = {R*2}" | "{R*2} = [sp++]" | "[--sp] = {R*1}" | "{R*1} = [sp++]" => {
            let mask = if insn.entry.len == 2 {
                let n = (insn.raw & 0xf) as u32;
                (n.min(4)..=n.max(3)).fold(0u16, |m, r| m | 1 << r)
            } else {
                ((insn.raw >> 16) & 0xffff) as u16
            };
            return Some(if s.starts_with("[--sp]") { Fast::PushRegs { mask } } else { Fast::PopRegs { mask } });
        }
        "rti" => return Some(Fast::Rti),
        "R = R + R + c" => return Some(Fast::AddC { d: r(0)?, a: r(1)?, b: r(2)?, sub: false }),
        "R = R - R - !c" => return Some(Fast::AddC { d: r(0)?, a: r(1)?, b: r(2)?, sub: true }),
        "ssync" | "btbclr" | "iflush [R]" | "flush [R]" | "flushinv [R]" => return Some(Fast::Nop),
        "[R+#i] &= ~R" => return Some(Fast::MemA4 { b: r(0)?, off: v(1)? as i32, op: Op::AndNot, rk: 0, rv: r(2)? as u32 }),
        _ if s.starts_with("[--sp] = {") => {
            if let Some(mask) = sreg_list_mask(&s["[--sp] = ".len()..]) {
                return Some(Fast::PushSregs { mask });
            }
        }
        _ if s.ends_with("} = [sp++]") => {
            if let Some(mask) = sreg_list_mask(&s[..s.len() - " = [sp++]".len()]) {
                return Some(Fast::PopSregs { mask });
            }
        }
        // arithmetic shift by an immediate below 32 (exec_syntax fills larger counts with the sign)
        "R = R >>> #i" if insn.entry.len == 4 && v(2).map_or(false, |n| (1..32).contains(&n)) => {
            return Some(Fast::SarI { d: r(0)?, s: r(1)?, n: v(2)? as u8 });
        }
        _ if insn.entry.len == 4 && (insn.raw >> 8) & 0xff == 0xeb && block_form(s).is_some() => {
            // block moves (exec_syntax's `eb 00..70` arm): base bits 0-3, mask bits 16-31
            let (store, post, down) = block_form(s)?;
            return Some(Fast::BlockMove { base: (insn.raw & 0xf) as u8, mask: (insn.raw >> 16) as u16, store, post, down });
        }
        _ => {}
    }
    // sub-word extracts
    let ext = match s {
        "R = R.b0 (u)" => Some((0, false)),
        "R = R.b0 (s)" => Some((0, true)),
        "R = R.l (u)" => Some((1, false)),
        "R = R.l (s)" => Some((1, true)),
        "R = R.h (u)" => Some((2, false)),
        "R = R.h (s)" => Some((2, true)),
        _ => None,
    };
    if let Some((part, signed)) = ext {
        return Some(Fast::Ext { d: r(0)?, s: r(1)?, part, signed });
    }
    // branches: if[s] (R op #i|R) goto #i
    for (prefix, signed) in [("if (R ", false), ("ifs (R ", true)] {
        if let Some(rest) = s.strip_prefix(prefix).and_then(|x| x.strip_suffix(") goto #i")) {
            let (op, rhs) = rest.split_once(' ')?;
            let cond = cond_of(op, signed)?;
            return match rhs {
                "R" => Some(Fast::BrR { a: r(0)?, b: r(1)?, cond, target: target(2)? }),
                "#i" | "#h" => Some(Fast::BrI { a: r(0)?, v: v(1)?, cond, target: target(2)? }),
                _ => None,
            };
        }
    }
    // loads: R = [b|h][R+#i | sp+#i | sp] [(u)|(s)]
    if let Some(rhs) = s.strip_prefix("R = ") {
        let (rhs, signed) = if let Some(x) = rhs.strip_suffix(" (s)") { (x, true) }
            else if let Some(x) = rhs.strip_suffix(" (u)") { (x, false) } else { (rhs, false) };
        let (w, inner) = if let Some(x) = rhs.strip_prefix("b[") { (1, x) }
            else if let Some(x) = rhs.strip_prefix("h[") { (2, x) }
            else if let Some(x) = rhs.strip_prefix('[') { (4, x) } else { (0, "") };
        if w != 0 {
            return match inner {
                "R++=#i]" => Some(Fast::LdPost { d: r(0)?, b: r(1)?, w, signed, inc: v(2)? }),
                "R++=R]" => Some(Fast::LdPostR { d: r(0)?, b: r(1)?, i: r(2)?, w, signed }),
                "++R=R]" => Some(Fast::LdPre { d: r(0)?, b: r(1)?, i: r(2)?, w, signed, inc: 0 }),
                "++R=#i]" => Some(Fast::LdPre { d: r(0)?, b: r(1)?, i: 16, w, signed, inc: v(2)? }),
                "R+R]" => Some(Fast::LdIdx { d: r(0)?, b: r(1)?, i: r(2)?, sh: 0, w, signed }),
                "R+R<<#i]" => Some(Fast::LdIdx { d: r(0)?, b: r(1)?, i: r(2)?, sh: (v(3)? & 31) as u8, w, signed }),
                "R+#i]" => Some(Fast::Ld { d: r(0)?, b: Base::Reg(r(1)?), off: v(2)?, w, signed }),
                "sp+#i]" => Some(Fast::Ld { d: r(0)?, b: Base::Sp, off: v(1)?, w, signed }),
                "sp]" => Some(Fast::Ld { d: r(0)?, b: Base::Sp, off: 0, w, signed }),
                _ => None,
            };
        }
    }
    // plain stores: [b|h][R+#i | sp+#i | sp] = R
    for (prefix, w) in [("b[", 1u8), ("h[", 2), ("[", 4)] {
        if let Some(rest) = s.strip_prefix(prefix) {
            let rmw = |op: RmwOp| -> Option<Fast> { Some(Fast::Rmw { b: r(0)?, w, op, off: v(1)?, v: v(2)? }) };
            let f = match rest {
                "R++=#i] = R" => Some(Fast::StPost { b: r(0)?, inc: v(1)?, s: r(2)?, w }),
                "R++=R] = R" => Some(Fast::StPostR { b: r(0)?, i: r(1)?, s: r(2)?, w }),
                "++R=R] = R" => Some(Fast::StPre { b: r(0)?, i: r(1)?, s: r(2)?, w, inc: 0 }),
                "++R=#i] = R" => Some(Fast::StPre { b: r(0)?, i: 16, inc: v(1)?, s: r(2)?, w }),
                "R+R] = R" => Some(Fast::StIdx { b: r(0)?, i: r(1)?, sh: 0, s: r(2)?, w }),
                "R+R<<#i] = R" => Some(Fast::StIdx { b: r(0)?, i: r(1)?, sh: (v(2)? & 31) as u8, s: r(3)?, w }),
                "R+#i] = #i" | "R+#i] = #h" => rmw(RmwOp::Mov),
                "R+#i] |= #i" | "R+#i] |= #h" => rmw(RmwOp::Or),
                "R+#i] &= #i" | "R+#i] &= #h" => rmw(RmwOp::And),
                "R+#i] ^= #i" | "R+#i] ^= #h" => rmw(RmwOp::Xor),
                "R+#i] += #i" | "R+#i] += #h" => rmw(RmwOp::Add),
                "R+#i] -= #i" | "R+#i] -= #h" => rmw(RmwOp::Sub),
                "R+#i] = R" => Some(Fast::St { b: Base::Reg(r(0)?), off: v(1)?, s: r(2)?, w }),
                "sp+#i] = R" => Some(Fast::St { b: Base::Sp, off: v(0)?, s: r(1)?, w }),
                "sp] = R" => Some(Fast::St { b: Base::Sp, off: 0, s: r(0)?, w }),
                // (`[--sp] = rets` and the rest: the shapes below)
                _ => None,
            };
            if f.is_some() {
                return f;
            }
        }
    }
    match s {
        "R = R" => Some(Fast::Mov { d: r(0)?, s: r(1)? }),
        "R = #i" | "R = #h" => Some(Fast::MovI { d: r(0)?, v: v(1)? }),
        "R = R + R" | "R = R - R" | "R = R & R" | "R = R | R" | "R = R ^ R" | "R = R * R" => {
            Some(Fast::Alu3 { d: r(0)?, a: r(1)?, b: r(2)?, op: op_of(&s[6..7])? })
        }
        "R = R + #i" | "R = R + #h" | "R = R - #i" | "R = R & #h" | "R = R | #h"
        | "R = R ^ #h" | "R = R * #h" | "R = R & #i" | "R = R | #i" | "R = R ^ #i" => {
            Some(Fast::Alu3I { d: r(0)?, a: r(1)?, v: v(2)?, op: op_of(&s[6..7])? })
        }
        "R += R" | "R -= R" | "R |= R" | "R &= R" | "R ^= R" | "R *= R" => {
            Some(Fast::Alu2 { d: r(0)?, b: r(1)?, op: op_of(&s[2..3])? })
        }
        "R += #i" | "R -= #i" | "R |= #i" | "R &= #i" | "R ^= #i" | "R *= #i" => {
            Some(Fast::Alu2I { d: r(0)?, v: v(1)?, op: op_of(&s[2..3])? })
        }
        "goto #i" => Some(Fast::Goto { target: target(0)? }),
        "[--sp] = rets" => Some(Fast::PushRet),
        "pc = [sp++]" => Some(Fast::PopRet),
        "R = smin(R, R)" | "R = smax(R, R)" | "R = umin(R, R)" | "R = umax(R, R)" => {
            let kind = match &s[4..8] { "smin" => 0, "smax" => 1, "umin" => 2, _ => 3 };
            let raw = insn.raw;
            if insn.entry.len == 4 && matches!((raw >> 8) & 0xff, 0xe4 | 0xf4) {
                Some(Fast::MinMax { d: ((raw >> 28) & 0xf) as u8, a: ((raw >> 20) & 0xf) as u8, b: ((raw >> 24) & 0xf) as u8, kind })
            } else {
                Some(Fast::MinMax { d: r(0)?, a: r(1)?, b: r(2)?, kind })
            }
        }
        "call #i" => Some(Fast::Call { target: target(0)? }),
        "rts" => Some(Fast::Rts),
        "call R" => Some(Fast::CallR { r: r(0)? }),
        "goto R" => Some(Fast::GotoR { r: r(0)? }),
        "{rets} = [sp++]" => Some(Fast::PopRets),
        "tbb [R]" | "tbh [R]" => Some(Fast::Tb { r: r(0)?, half: s.starts_with("tbh") }),
        "R = R & ~R" => Some(Fast::Alu3 { d: r(0)?, a: r(1)?, b: r(2)?, op: Op::AndNot }),
        // (register counts are taken & 31, as `alu` does; immediate counts of 32 and more fill)
        "R <<= R" | "R >>= R" | "R >>>= R" => Some(Fast::Alu2 { d: r(0)?, b: r(1)?, op: shift_op(&s[2..])? }),
        "R <<= #i" | "R >>= #i" | "R >>>= #i" if v(1)? < 32 => Some(Fast::Alu2I { d: r(0)?, v: v(1)?, op: shift_op(&s[2..])? }),
        "R = R << R" | "R = R >> R" | "R = R >>> R" => {
            Some(Fast::A4 { d: r(0)?, s: r(1)?, op: shift_op(&s[6..])?, rk: 0, rv: r(2)? as u32 })
        }
        "R = R << #i" | "R = R >> #i" if (1..32).contains(&v(2)?) => Some(Fast::A4 { d: r(0)?, s: r(1)?, op: shift_op(&s[6..])?, rk: 1, rv: v(2)? }),
        "R = ~R" => Some(Fast::Un { d: r(0)?, s: r(1)?, k: 0 }),
        "R = -R" => Some(Fast::Un { d: r(0)?, s: r(1)?, k: 1 }),
        "R = abs(R)" => Some(Fast::Un { d: r(0)?, s: r(1)?, k: 2 }),
        "R = rev8(R)" => Some(Fast::Un { d: r(0)?, s: r(1)?, k: 3 }),
        "R = clz(R)" => Some(Fast::Un { d: r(0)?, s: r(1)?, k: 4 }),
        "R = R / R (s)" | "R = R / R (u)" | "R = R % R (s)" | "R = R % R (u)" => {
            let k = match (&s[6..7], s.ends_with("(s)")) { ("/", true) => 0, ("/", false) => 1, (_, true) => 2, _ => 3 };
            Some(Fast::Div { d: r(0)?, a: r(1)?, b: r(2)?, k })
        }
        _ => {
            // register pairs: `rA_rB = rC_rD`, `rA_rB = #i` (the names are literal in the syntax)
            let (lhs, rhs) = split2(s, " = ")?;
            let (dh, dl) = pair_regs(lhs)?;
            if rhs == "#i" {
                let imm = val(2)?;
                return (imm == imm as i32 as i64).then_some(Fast::Mov2I { dh, dl, v: imm as u32 });
            }
            let (sh, sl) = pair_regs(rhs)?;
            Some(Fast::Mov2 { dh, dl, sh, sl })
        }
    }
}

/// The operation of a shift operator at the start of `t` (`<<`, `>>`, `>>>`, with or without `=`).
fn shift_op(t: &str) -> Option<Op> {
    if t.starts_with(">>>") { Some(Op::Sar) } else if t.starts_with(">>") { Some(Op::Shr) } else if t.starts_with("<<") && !t.starts_with("<<<") { Some(Op::Shl) } else { None }
}

/// `rH_rL` as (H, L).
fn pair_regs(t: &str) -> Option<(u8, u8)> {
    let (h, l) = split2(t.strip_prefix('r')?, "_r")?;
    let (h, l): (u8, u8) = (h.parse().ok()?, l.parse().ok()?);
    (h < 16 && l < 16).then_some((h, l))
}

// the hot cache line must stay small (see `Line`)
const _: () = assert!(std::mem::size_of::<Line>() <= 32);

#[cfg(test)]
mod size_report {
    #[test]
    fn line_sizes() {
        eprintln!("Fast {} Option<Fast> {} Line {}", std::mem::size_of::<super::Fast>(), std::mem::size_of::<Option<super::Fast>>(), std::mem::size_of::<super::Line>());
    }
}

/// `describe`'s form of an access with a base update: `Q ld|st r<data> pre|post r<base> <step> w<w>`,
/// a load then `u|s`; the step a number or `r<i>`.
fn describe_inc(store: bool, r: u8, pre: bool, b: u8, step: String, w: u8, signed: bool) -> String {
    let mode = if pre { "pre" } else { "post" };
    if store {
        format!("Q st r{} {} r{} {} w{}", r, mode, b, step, w)
    } else {
        format!("Q ld r{} {} r{} {} w{} {}", r, mode, b, step, w, if signed { "s" } else { "u" })
    }
}

/// What the emulator's decoder makes of an instruction window: its length in bytes and its
/// class name, or None when nothing decodes it (`fm1-emu --check-decode`, tools/fuzz_decode.py).
pub fn decode_info(win: u64, pc: u32) -> Option<(usize, &'static str)> {
    decode_any(win, pc).ok().map(|i| (i.entry.len as usize, i.entry.name))
}

/// The micro-op the emulator decodes an instruction window into, in a canonical text form, for
/// auditing the decoders against the vendor disassembly (`fm1-emu --describe`):
///   `T <target>` a branch / call with a fixed target, `L r<d> <base> <off> w<w> u|s` a load,
///   `S r<s> <base> <off> w<w>` a store, `I r<d> <value>` a constant, `Q ..` an access with a base
///   update (`describe_inc`); None: nothing to compare.
/// The same decode path as `Cpu::fetch_at` (without the predicated-block form, which needs the
/// following instructions).
pub fn describe(win: u64, pc: u32) -> Option<String> {
    let insn = decode_any(win, pc).ok()?;
    let alu4 = if insn.entry.len == 4 { decode_alu4(insn.raw) } else { None };
    let bittest = if insn.entry.name.starts_with("if") { decode_bittest(insn.raw, insn.entry.len) } else { None };
    let route = route_of(&insn, &alu4, &bittest);
    let syntax = canon_syntax(insn.entry);
    if route == 16 {
        // register-pair load / store (arm 16): `P rH_rL [++]base[++] off|+rI ld|st`
        let m = decode_pair_mem(insn.raw);
        let b = m.base.map_or("sp".to_string(), |b| format!("r{}", b));
        let base = if m.pre { format!("++{}", b) } else if m.post { format!("{}++", b) } else { b };
        let off = m.index.map_or(format!("{}", m.off), |i| format!("+r{}", i));
        return Some(format!("P r{}_r{} {} {} {}", 2 * m.pair + 1, 2 * m.pair, base, off, if m.store { "st" } else { "ld" }));
    }
    if route == 14 {
        // predicated block (arm 14): `C [s] rA op rhs then_units else_units`
        let b = decode_if_block(insn.raw)?;
        let rhs = match b.rhs { Rhs::Reg(r) => format!("r{}", r), Rhs::Imm(v) => format!("{}", v), Rhs::Bit(r) => format!("1<<r{}", r) };
        let (sign, op) = if b.fam == 2 {
            ("", if b.alt { "&!=0" } else { "&==0" })
        } else {
            match cond_of_fam(b.fam, b.alt) {
                Cond::Eq => ("", "=="), Cond::Ne => ("", "!="),
                Cond::LtU => ("", "<"), Cond::GtU => ("", ">"), Cond::LeU => ("", "<="), Cond::GeU => ("", ">="),
                Cond::LtS => ("s", "<"), Cond::GtS => ("s", ">"), Cond::LeS => ("s", "<="), Cond::GeS => ("s", ">="),
                Cond::FEq => ("f", "=="), Cond::FUne => ("f", "u!="), Cond::FLt => ("f", "<"), Cond::FUge => ("f", "u>="),
                Cond::FLe => ("f", "<="), Cond::FUgt => ("f", "u>"), Cond::FGe => ("f", ">="), Cond::FUlt => ("f", "u<"),
                Cond::FGt => ("f", ">"), Cond::FUle => ("f", "u<="),
            }
        };
        return Some(format!("C{} r{} {} {} {} {}", sign, b.reg, op, rhs, b.then_units, b.else_units));
    }
    if route == 18 || route == 19 {
        // single-register accesses with an increment (arms 18 / 19; a word at an offset: L / S)
        let m = decode_inc_mem(route as u8, insn.raw);
        if !m.pre && !m.post {
            return Some(if m.store { format!("S r{} r{} {} w4", m.d, m.b, m.inc) } else { format!("L r{} r{} {} w4 u", m.d, m.b, m.inc) });
        }
        return Some(describe_inc(m.store, m.d as u8, m.pre, m.b as u8, m.inc.to_string(), m.w, m.signed));
    }
    let fast = if route == ROUTE_SYNTAX { fast_of(&insn, syntax) } else { fast_raw(route, &insn, &alu4, &bittest) }?;
    let base = |b: Base| match b { Base::Reg(r) => format!("r{}", r), Base::Sp => "sp".to_string() };
    let opname = |o: Op| match o {
        Op::Add => "+", Op::Sub => "-", Op::RSub => "rsub", Op::And => "&", Op::AndNot => "&~", Op::Or => "|",
        Op::Xor => "^", Op::Mul => "*", Op::Shl => "<<", Op::Shr => ">>", Op::Sar => ">>>",
    };
    let rhs = |rk: u8, rv: u32| match rk { 0 => format!("r{}", rv), 1 => format!("{}", rv), _ => format!("1<<r{}", rv) };
    let sgn = |s: bool| if s { "s" } else { "u" };
    let step = |i: u8, inc: u32| if i == 16 { (inc as i32).to_string() } else { format!("r{}", i) };
    Some(match fast.specialise() {
        Fast::BrI { target, .. } | Fast::BrR { target, .. } | Fast::BrIEq { target, .. } | Fast::BrINe { target, .. }
        | Fast::BitBr { target, .. } | Fast::DecBr { target, .. } | Fast::Goto { target } | Fast::Call { target } => {
            format!("T {:x}", target)
        }
        Fast::Ld { d, b, off, w, signed } => format!("L r{} {} {} w{} {}", d, base(b), off as i32, w, sgn(signed)),
        Fast::LdW { d, b, off } => format!("L r{} r{} {} w4 u", d, b, off as i32),
        Fast::LdWSp { d, off } => format!("L r{} sp {} w4 u", d, off as i32),
        Fast::LdBu { d, b, off } => format!("L r{} r{} {} w1 u", d, b, off as i32),
        Fast::St { s, b, off, w } => format!("S r{} {} {} w{}", s, base(b), off as i32, w),
        Fast::StW { s, b, off } => format!("S r{} r{} {} w4", s, b, off as i32),
        Fast::StWSp { s, off } => format!("S r{} sp {} w4", s, off as i32),
        Fast::MovI { d, v } => format!("I r{} {}", d, v),
        Fast::Mov { d, s } => format!("A r{} r{} mov", d, s),
        Fast::Alu3 { d, a, b, op } => format!("A r{} r{} {} r{}", d, a, opname(op), b),
        Fast::Alu3I { d, a, v, op } => format!("A r{} r{} {} {}", d, a, opname(op), v),
        Fast::Alu2 { d, b, op } => format!("A r{} r{} {} r{}", d, d, opname(op), b),
        Fast::Alu2I { d, v, op } => format!("A r{} r{} {} {}", d, d, opname(op), v),
        Fast::A4 { d, s, op, rk, rv } => format!("A r{} r{} {} {}", d, s, opname(op), rhs(rk, rv)),
        Fast::AddRI { d, s, v } => format!("A r{} r{} + {}", d, s, v),
        Fast::ShlI { d, s, n } => format!("A r{} r{} << {}", d, s, n),
        Fast::SarI { d, s, n } => format!("A r{} r{} >>> {}", d, s, n),
        Fast::LdIdx { d, b, i, sh, w, signed } => format!("X r{} r{} r{} {} w{} {}", d, b, i, sh, w, sgn(signed)),
        Fast::StIdx { s, b, i, sh, w } => format!("Y r{} r{} r{} {} w{}", s, b, i, sh, w),
        Fast::LdPre { d, b, i, w, signed, inc } => describe_inc(false, d, true, b, step(i, inc), w, signed),
        Fast::StPre { s, b, i, w, inc } => describe_inc(true, s, true, b, step(i, inc), w, false),
        Fast::LdPost { d, b, w, signed, inc } => describe_inc(false, d, false, b, (inc as i32).to_string(), w, signed),
        Fast::StPost { s, b, w, inc } => describe_inc(true, s, false, b, (inc as i32).to_string(), w, false),
        Fast::LdPostR { d, b, i, w, signed } => describe_inc(false, d, false, b, format!("r{}", i), w, signed),
        Fast::StPostR { s, b, i, w } => describe_inc(true, s, false, b, format!("r{}", i), w, false),
        Fast::Rmw { b, w, op, off, v } => {
            let o = match op { RmwOp::Mov => "=", RmwOp::Or => "|=", RmwOp::And => "&=", RmwOp::Xor => "^=", RmwOp::Add => "+=", RmwOp::Sub => "-=" };
            format!("M w{} r{} {} {} {}", w, b, off as i32, o, v)
        }
        Fast::MemA4 { b, off, op, rk, rv } => {
            let o = match op { Op::Or => "|=", Op::And => "&=", Op::Xor => "^=", Op::Add => "+=", Op::Sub => "-=", _ => return None };
            format!("M w4 r{} {} {} {}", b, off, o, rhs(rk, rv))
        }
        Fast::PushRets { n } => {
            let (top, bot) = ((n as i32).max(3), (n as i32).min(4));
            let regs: Vec<String> = (bot..=top).map(|r| format!("r{}", r)).collect();
            format!("U rets {}", regs.join(" "))
        }
        Fast::PopPc { n } => {
            let (top, bot) = ((n as i32).max(3), (n as i32).min(4));
            let regs: Vec<String> = (bot..=top).map(|r| format!("r{}", r)).collect();
            format!("O pc {}", regs.join(" "))
        }
        Fast::PushMask { mask } => {
            let regs: Vec<String> = (0..16).filter(|r| mask >> r & 1 != 0).map(|r| format!("r{}", r)).collect();
            format!("U rets {}", regs.join(" "))
        }
        Fast::PopMask { mask } => {
            let regs: Vec<String> = (0..16).filter(|r| mask >> r & 1 != 0).map(|r| format!("r{}", r)).collect();
            format!("O pc {}", regs.join(" "))
        }
        Fast::PushRet => "U rets".to_string(),
        Fast::PopRet => "O pc".to_string(),
        Fast::Ext { d, s, part, signed } => format!("E r{} r{} {} {}", d, s, ["b0", "l", "h"][part.min(2) as usize], sgn(signed)),
        _ => return None,
    })

}

