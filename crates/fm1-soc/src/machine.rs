//! Running the cores against the SoC: cpu0's block loop (fm1-live and the browser build share
//! it), and cpu1 next to it once the firmware releases it.
//!
//! cpu1 has no clock of its own. cpu0 runs in slices of `SLICE` instructions; after each,
//! cpu1 may run as many instructions as cycles passed (idle skips included, so it keeps the
//! hardware's pace, where both cores run at the CPU clock), through `Core1Bus`, whose tick
//! budget stops its interpreter and compiled code alike. It stops early once it is back at
//! the pc it started from with the same registers: a polling loop with nothing to do (the
//! stock firmware's job loop, X0X's), which would otherwise cost as much as cpu0.
//!
//! The chip interleaves the cores instruction by instruction, so a lock is held for nanoseconds;
//! here a core that finds one held (`lockset` denied, `testset` on a taken byte it spins on:
//! `Soc::waits`) would spin through its whole slice, the holder not running. So it hands over
//! instead: cpu1 ends its run (cpu0's slice is next), cpu0 ends its slice early, and cpu1 then
//! runs on while it holds the lock, to its `lockclr` or within 64 instructions of the store that
//! clears the byte, the clock moving for cpu0's retries, which are not run (a FreeRTOS SMP
//! kernel, Bubba Machine's, takes the hardware lock around every critical section: without
//! this, most of its slow-path instructions were `lockset` retries). cpu0's interrupts wait up
//! to a slice meanwhile.

use crate::flaky::FlakyCore1;
use crate::fwsc::{self, UsrLayout};
use crate::{p33, stock_boot, stock_from_package, FlashPart, Soc, Stock, Wait, XipCache, FLASH_SIZE, RAM_BASE, VEC_BASE};
use fm1_core::{Bus, Cpu, PlainState};

/// cpu0 runs in slices of this many instructions once cpu1 is running, cpu1 after each.
pub const SLICE: u32 = 1024;

/// cpu1's view of the SoC: no clock, a budget of ticks instead. `spent`: the ticks its
/// instructions took, all of them (FM1_STRICT: one that costs more than is left of the budget, or a
/// flash cache miss, still costs all of it; the budget only stops the run)
pub struct Core1Bus<'a> {
    pub soc: &'a mut Soc,
    pub budget: u64,
    /// what cpu0 spins on meanwhile (`run_cpu1`'s `until`): a run that releases the hardware
    /// lock ends at the `lockclr`, so cpu0 gets it at the instruction the chip's would. (A byte
    /// lock's release is a RAM store, which compiled code makes without the bus: that run ends
    /// at its call's end, `SPIN_CALL` instructions at most past the store, compiled or not.)
    pub until: Option<Wait>,
    pub spent: u64,
}

impl Core1Bus<'_> {
    /// A write that reset the chip (PR_PWR) ends cpu1's run at its next tick: cpu0 carries
    /// the reset out at once (`run_blocks`), and cpu1 makes no writes the chip would not.
    #[inline]
    fn stop_at_reset(&mut self) {
        if self.soc.reset_due() {
            self.budget = self.budget.min(1);
        }
    }
}

// Every method of `Bus` is spelled out here, none left to its default. Not passed on to the SoC:
// the clock (`tick`, `tick_room`, `add_ticks`, `next_tick_due`: the budget instead, time stands
// still while core 1 runs), `irq_window` and `service` (they end the run; the SoC's `next_due` is
// cpu0's to zero) and `idle` (core 1's sleep, not cpu0's idle skip).
impl Bus for Core1Bus<'_> {
    fn read8(&mut self, a: u32) -> u8 {
        self.soc.core1_read8(a)
    }
    fn read16(&mut self, a: u32) -> u16 {
        self.soc.core1_read16(a)
    }
    fn read32(&mut self, a: u32) -> u32 {
        let v = self.soc.core1_read32(a);
        if self.soc.periph.core1_polls {
            self.budget = self.budget.min(1); // (`run_cpu1` ends the run there)
        }
        v
    }
    fn fetch32(&mut self, a: u32) -> u32 {
        self.soc.fetch32(a)
    }
    fn write8(&mut self, a: u32, v: u8) {
        self.soc.write8(a, v);
        self.stop_at_reset();
    }
    fn write16(&mut self, a: u32, v: u16) {
        self.soc.write16(a, v);
        self.stop_at_reset();
    }
    fn write32(&mut self, a: u32, v: u32) {
        self.soc.write32(a, v);
        self.stop_at_reset();
    }
    fn tick(&mut self) -> bool {
        let stall = std::mem::take(&mut self.soc.core1_ticks);
        self.spent += 1 + stall;
        self.budget = self.budget.saturating_sub(1 + stall);
        self.budget == 0
    }
    fn fetch_cost(&mut self, pc: u32) {
        fm1_core::Bus::fetch_cost(&mut *self.soc, pc);
    }
    fn irq_pending(&self) -> bool {
        self.soc.periph.any_pending()
    }
    fn jit_mem(&mut self) -> Option<fm1_core::JitMem> {
        // a flaky core 1 runs interpreted: compiled code reads RAM without the bus
        if self.soc.flaky_core1.is_some() {
            return None;
        }
        self.soc.jit_mem()
    }
    fn code_writes(&self) -> Option<u64> {
        self.soc.code_writes()
    }
    fn code_mark(&mut self, lo: u32, hi: u32) {
        self.soc.code_mark(lo, hi)
    }
    fn tick_room(&self) -> u64 {
        self.budget.saturating_sub(1)
    }
    fn add_ticks(&mut self, n: u64) {
        let stall = std::mem::take(&mut self.soc.core1_ticks);
        self.spent += n + stall;
        self.budget = self.budget.saturating_sub(n + stall);
    }
    fn hw_lock(&mut self, core: u32, set: bool) -> bool {
        let got = self.soc.hw_lock(core, set);
        // a `lockclr` with cpu0 spinning on the lock: the run ends after it (see `until`)
        if !set && self.until == Some(Wait::Lock) {
            self.budget = self.budget.min(1);
        }
        got
    }
    fn spin(&mut self, core: u32, addr: u32) {
        self.soc.spin(core, addr)
    }
    fn irq_window(&mut self) {
        // (cpu1's loop looks at its interrupts before every run; a short stretch ends soon)
        if self.soc.periph.any_pending() {
            self.budget = self.budget.min(1);
        }
    }
    fn service(&mut self) {
        self.budget = self.budget.min(1);
    }
    fn idle(&mut self, _core: u32) {
        // the core sleeps: this run ends after the instruction, and `run_cpu1` runs it again
        // only once an interrupt from its bank is pending
        self.soc.cpu1_idle = true;
        self.budget = self.budget.min(1);
    }
    fn next_tick_due(&self) -> bool {
        self.budget <= 1
    }
}

/// cpu1's loads through the SoC, for a runner that steps it against the SoC itself (fm1-emu):
/// the SoC as it is, but for a flaky core 1's misreads (`Soc::flaky_core1`).
pub struct Core1Loads<'a>(pub &'a mut Soc);

// The SoC's own answers but for the loads. Not passed on: `jit_mem` (None, see there) and
// `fetch_cost` (its default, nothing: the flash cache it would charge is FM1_STRICT's, and a
// runner with that clock runs core 1 through `Core1Bus`).
impl Bus for Core1Loads<'_> {
    fn read8(&mut self, a: u32) -> u8 {
        self.0.core1_read8(a)
    }
    fn read16(&mut self, a: u32) -> u16 {
        self.0.core1_read16(a)
    }
    fn read32(&mut self, a: u32) -> u32 {
        self.0.core1_read32(a)
    }
    fn fetch32(&mut self, a: u32) -> u32 {
        self.0.fetch32(a)
    }
    fn write8(&mut self, a: u32, v: u8) {
        self.0.write8(a, v)
    }
    fn write16(&mut self, a: u32, v: u16) {
        self.0.write16(a, v)
    }
    fn write32(&mut self, a: u32, v: u32) {
        self.0.write32(a, v)
    }
    fn tick(&mut self) -> bool {
        self.0.tick()
    }
    fn irq_pending(&self) -> bool {
        self.0.irq_pending()
    }
    fn jit_mem(&mut self) -> Option<fm1_core::JitMem> {
        None // (as Core1Bus's: interpreted, every load through the bus)
    }
    fn tick_room(&self) -> u64 {
        self.0.tick_room()
    }
    fn add_ticks(&mut self, n: u64) {
        self.0.add_ticks(n)
    }
    fn hw_lock(&mut self, core: u32, set: bool) -> bool {
        self.0.hw_lock(core, set)
    }
    fn spin(&mut self, core: u32, addr: u32) {
        self.0.spin(core, addr)
    }
    fn next_tick_due(&self) -> bool {
        self.0.next_tick_due()
    }
    fn idle(&mut self, core: u32) {
        self.0.idle(core)
    }
    fn irq_window(&mut self) {
        self.0.irq_window()
    }
    fn service(&mut self) {
        self.0.service()
    }
    fn code_writes(&self) -> Option<u64> {
        self.0.code_writes()
    }
    fn code_mark(&mut self, lo: u32, hi: u32) {
        self.0.code_mark(lo, hi)
    }
}

/// Exact skipping of cpu0's polling loops. A loop that goes round with nothing to do (FM1-DOOM's
/// and ZP-12's main loops: a watchdog kick through the P33 port and a look at flags the
/// interrupts set, ~110 instructions) leaves the machine as it found it each time round, and
/// until the next timed event nothing it reads can change: every round runs the same.
///
/// `probe_loop` runs cpu0 on in the interpreter through `ProbeBus`, which logs the RAM words
/// stored to (with what they held) and lets through only loads that merely look
/// (`Soc::pure_read`) and stores whose effect is compared afterwards (`Soc::loop_write_ok`: the
/// P33 port, the FM-1's instant ADC). Back at the pc it began at with the same core state, the
/// same port and ADC, and every word the round stored to holding what it held when the round
/// began, the round is a fixed point: the rounds that fit before the next due tick and within
/// the budget are skipped as k * L cycles at once, which leaves the machine as running them
/// would, at the same cycle. A probe that finds no such round (an impure access, an event, too
/// long) undoes everything it ran (core, RAM, port, ADC, clock), so the run goes on exactly as
/// without it.
///
/// Probes run interpreted, a step at a time: where they find nothing, or rounds too long for
/// what they skip (Ctl's and Polly's ~640-instruction rounds, between stretches of real work),
/// they cost more than compiled code running the loop. So probing again after one that failed
/// backs off, and probes spend a credit: one per instruction they step, earned back by what
/// they skip (an instruction compiled code would have run is worth 1/16 of a step, an
/// interpreted one 1/4) and slowly with time (1/2048 a cycle), so loops that show up later
/// are still found.
pub struct LoopSkip {
    /// FM1_NO_LOOPSKIP: off (for A/B runs; the result is the same)
    pub enabled: bool,
    /// no probe before this cycle (after one that failed or skipped little)
    pub(crate) next_try: u64,
    backoff: u64,
    /// probe steps that may still be spent, and the cycle the time's share was last added at
    credit: i64,
    credit_at: u64,
    /// cycles (= instructions) skipped, probes made, probes that found a loop
    pub skipped: u64,
    pub probes: u64,
    pub hits: u64,
    // (buffers kept between probes)
    log: Vec<(u32, u32)>,
    seen: Vec<Visit>,
    scratch: Vec<(u32, u32)>,
}

/// One visit of a probe to the pc it began at: the core, the P33 port, the ADC then, the
/// instructions run before it and the length of the store log.
#[derive(Clone, Copy)]
struct Visit {
    core: PlainState,
    p33: p33::Snap,
    adc: crate::periph::AdcState,
    at: u32,
    log: usize,
}

impl LoopSkip {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            next_try: 0,
            backoff: 0,
            credit: CREDIT_MAX,
            credit_at: 0,
            skipped: 0,
            probes: 0,
            hits: 0,
            log: Vec::new(),
            seen: Vec::new(),
            scratch: Vec::new(),
        }
    }

    /// May a probe start at cycle `now`? (the time's share of the credit added first)
    fn may_probe(&mut self, now: u64) -> bool {
        if now < self.next_try {
            return false;
        }
        let earned = (now.saturating_sub(self.credit_at) / 2048).min(CREDIT_MAX as u64) as i64;
        self.credit = (self.credit + earned).min(CREDIT_MAX);
        self.credit_at = now;
        self.credit > 0
    }

    /// No probe for a while (doubling up to ~17 ms at 240 MHz, from now).
    fn back_off(&mut self, now: u64) {
        self.backoff = (self.backoff * 2).clamp(1 << 11, 1 << 22);
        self.next_try = now + self.backoff;
    }
}

/// The longest a probe follows cpu0 looking for a round.
const PROBE_MAX: u32 = 2048;
/// A probe only where the run has at least this many instructions left, and as many cycles
/// before the next event: a round must fit a few times to pay for the probe.
const PROBE_MIN: u32 = 1024;
/// The most probe steps the credit holds (`LoopSkip`): what skipping has earned beyond it is
/// not kept for later.
const CREDIT_MAX: i64 = 1 << 16;
/// The visits to its starting pc a probe keeps to compare with (the last ones).
const VISITS: usize = 8;

/// cpu0 as a probe sees the SoC (see `LoopSkip`): an access that a skipped round could not
/// repeat is not made (a load reads 0) and marks the probe `bad`; RAM stores are logged.
struct ProbeBus<'a> {
    soc: &'a mut Soc,
    bad: bool,
    /// RAM words (offsets) stored to, with what they held before
    log: &'a mut Vec<(u32, u32)>,
    /// the P33 port before the probe's first store to it
    p33: &'a mut Option<p33::P33>,
}

impl ProbeBus<'_> {
    /// May the load of the word at `a` go ahead?
    #[inline]
    fn load(&mut self, a: u32) -> bool {
        if self.bad {
            return false;
        }
        if (a.wrapping_sub(RAM_BASE) as usize) < self.soc.ram.len() || self.soc.pure_read(a) {
            return true;
        }
        self.bad = true;
        false
    }

    /// May a store of `width` bytes at `a` go ahead? (`word`: the address the SoC's slow path
    /// writes outside RAM: `a` for a word store, the aligned word for narrower ones.)
    #[inline]
    fn store(&mut self, a: u32, width: u32, word: u32) -> bool {
        if self.bad {
            return false;
        }
        let o = a.wrapping_sub(RAM_BASE);
        if (o as usize) < self.soc.ram.len() {
            // not into code (its stores are counted) and not past RAM's end
            if o as usize + width as usize > self.soc.ram.len() || self.soc.ram.is_code(o) {
                self.bad = true;
                return false;
            }
            let w = o & !3;
            self.log.push((w, self.soc.ram.read32(w)));
            // (an unaligned word store reaches into the next word)
            if (o & 3) + width > 4 {
                self.log.push((w + 4, self.soc.ram.read32(w + 4)));
            }
            return true;
        }
        if !self.soc.loop_write_ok(word) {
            self.bad = true;
            return false;
        }
        if self.p33.is_none() && word.wrapping_sub(p33::P33::CON) < 8 {
            *self.p33 = Some(self.soc.p33.clone());
        }
        true
    }
}

// Left to `Bus`'s defaults on purpose: `tick` (`probe_loop` ticks the SoC itself, between steps),
// `jit_mem`, `tick_room`, `add_ticks`, `next_tick_due`, `code_writes` and `code_mark` (a probe
// steps: no compiled code, no blocks built, and a store into code ends it) and `fetch_cost` (no
// probe runs with the cost model: `run_blocks`).
impl Bus for ProbeBus<'_> {
    fn read8(&mut self, a: u32) -> u8 {
        if self.load(a & !3) { self.soc.read8(a) } else { 0 }
    }
    fn read16(&mut self, a: u32) -> u16 {
        if self.load(a & !3) { self.soc.read16(a) } else { 0 }
    }
    fn read32(&mut self, a: u32) -> u32 {
        if self.load(a) { self.soc.read32(a) } else { 0 }
    }
    fn fetch32(&mut self, a: u32) -> u32 {
        if self.load(a) { self.soc.fetch32(a) } else { 0 }
    }
    fn write8(&mut self, a: u32, v: u8) {
        if self.store(a, 1, a & !3) {
            self.soc.write8(a, v)
        }
    }
    fn write16(&mut self, a: u32, v: u16) {
        if self.store(a, 2, a & !3) {
            self.soc.write16(a, v)
        }
    }
    fn write32(&mut self, a: u32, v: u32) {
        if self.store(a, 4, a) {
            self.soc.write32(a, v)
        }
    }
    fn irq_pending(&self) -> bool {
        self.soc.periph.any_pending()
    }
    fn hw_lock(&mut self, _core: u32, _set: bool) -> bool {
        self.bad = true;
        true
    }
    fn spin(&mut self, _core: u32, _addr: u32) {
        self.bad = true; // (a byte lock's round: not a polling loop)
    }
    fn idle(&mut self, _core: u32) {
        self.bad = true;
    }
    fn irq_window(&mut self) {
        // (nothing is pending while a probe runs; were it, the SoC would want a look now)
        if self.soc.periph.any_pending() {
            self.bad = true;
        }
    }
    fn service(&mut self) {
        self.bad = true; // (an rti: not a polling round)
    }
}

/// Look for a polling loop at cpu0's pc (see `LoopSkip`), with the tick of the instruction there
/// paid. Returns the instructions run, skipped rounds included (at most `budget`; the next
/// one's tick unpaid), or 0: no loop, and nothing changed.
fn probe_loop(cpu: &mut Cpu, soc: &mut Soc, budget: u32) -> u32 {
    let Some(start) = cpu.plain_state() else { return 0 };
    let pc0 = start.pc();
    let (cycles0, due0, adc0, insns0) = (soc.periph.cycles, soc.periph.next_due, soc.periph.adc_state(), cpu.insn_count);
    let wdt0 = soc.periph.wdt_at;
    let syntax0 = cpu.cur_syntax;
    let mut ls = std::mem::replace(&mut soc.loop_skip, LoopSkip::new(true));
    ls.probes += 1;
    ls.log.clear();
    ls.seen.clear();
    let mut p33_was: Option<p33::P33> = None;
    let mut i = 0u32;
    let mut visits = 0usize;
    let found = loop {
        if cpu.pc == pc0 {
            if let Some(core) = cpu.plain_state() {
                let (p33, adc) = (soc.p33.snap(), soc.periph.adc_state());
                let visit = Visit { core, p33, adc, at: i, log: ls.log.len() };
                match ls.seen.iter().position(|v| v.core == core && v.p33 == p33 && v.adc == adc) {
                    Some(j) => {
                        // every word the round stored to holds what it held when it began
                        // (the first old value logged for it in the round)
                        let round = &ls.log[ls.seen[j].log..];
                        ls.scratch.clear();
                        ls.scratch.extend_from_slice(round);
                        ls.scratch.sort_by_key(|e| e.0);
                        ls.scratch.dedup_by_key(|e| e.0);
                        if ls.scratch.iter().all(|&(w, old)| soc.ram.read32(w) == old) {
                            break Ok(i - ls.seen[j].at);
                        }
                        // (a first round that set something up: compare with this one on)
                        ls.seen[j] = visit;
                    }
                    // (the last few visits: a loop may settle after a few rounds)
                    None if ls.seen.len() < VISITS => ls.seen.push(visit),
                    None => ls.seen[visits % VISITS] = visit,
                }
                visits += 1;
            }
        }
        if i >= budget || i >= PROBE_MAX {
            break Err(false);
        }
        // (code outside the PC limits is the block loop's: it raises the guard where a block out
        // there is entered, and a step checks no limits)
        if cpu.pc_win.is_some_and(|w| w.iter().all(|&(lo, hi)| !(lo..=hi).contains(&cpu.pc))) {
            break Err(false);
        }
        // (the first instruction's tick is paid; with something due the block loop goes on)
        if i > 0 && soc.periph.tick() {
            break Err(true);
        }
        let mut bus = ProbeBus { soc: &mut *soc, bad: false, log: &mut ls.log, p33: &mut p33_was };
        let ok = cpu.step(&mut bus).is_ok();
        // (a reset asked for: it comes after this round, not after the skipped ones)
        if !ok || bus.bad || soc.periph.any_pending() || soc.reset_due() {
            break Err(false);
        }
        i += 1;
    };
    let ran = match found {
        Ok(l) => {
            // whole rounds whose ticks all fall before the next due one (cycles + k * l stays
            // below it), and within the budget. (A round that feeds the watchdog leaves its
            // deadline as the probe's feed set it, not the last skipped round's: the skip ends
            // before the deadline the feed before the probe set, and the round after it feeds
            // again, so it is reached only where it would be without the skip.)
            let l = l as u64;
            let c = soc.periph.cycles;
            let due = soc.periph.next_due;
            let by_event = due.saturating_sub(c + 1) / l;
            let by_budget = (budget - i) as u64 / l;
            let k = by_event.min(by_budget);
            soc.periph.cycles += k * l;
            cpu.insn_count += k * l;
            let worth = if cpu.jit_on { 16 } else { 4 };
            ls.credit = (ls.credit + (k * l / worth) as i64 - i as i64).min(CREDIT_MAX);
            ls.hits += 1;
            ls.skipped += k * l;
            // a probe interprets a round or two: worth it again when it skipped well over that
            if k * l >= 8 * i as u64 {
                ls.backoff = 0;
                ls.next_try = 0;
            } else if by_event < by_budget {
                ls.next_try = due; // an event is near: look again after it
            } else {
                ls.back_off(soc.periph.cycles);
            }
            i + (k * l) as u32
        }
        Err(event) => {
            // undo it all: the block loop runs from where the probe began, as without it
            for &(w, old) in ls.log.iter().rev() {
                soc.ram.restore32(w, old);
            }
            if let Some(p) = p33_was {
                soc.p33 = p;
            }
            soc.periph.set_adc_state(adc0);
            soc.periph.next_due = due0;
            soc.periph.wdt_at = wdt0;
            soc.reset_req = None;
            soc.periph.cycles = cycles0;
            cpu.set_plain_state(&start);
            cpu.insn_count = insns0;
            cpu.cur_syntax = syntax0;
            ls.credit -= i as i64;
            if event {
                ls.next_try = due0; // look again after it
            } else {
                ls.back_off(cycles0);
            }
            0
        }
    };
    soc.loop_skip = ls;
    ran
}

/// Before cpu0's next block or instruction (`run_blocks`, and a runner's reference loop, one
/// instruction an iteration: fm1-live's FM1_NO_BLOCKS, so the two agree): the PC limits the
/// firmware wrote handed to the core, "in an ISR" for TIMER4's idle skip, and the interrupt that
/// is due taken (the CPU exception once: entering its handler clears it).
pub fn cpu0_between(cpu: &mut Cpu, soc: &mut Soc) {
    if soc.pc_guard_dirty {
        soc.pc_guard_dirty = false;
        cpu.pc_win = soc.pc_windows();
    }
    soc.periph.in_isr = !cpu.irq_levels.is_empty();
    if soc.periph.any_pending() {
        if let Some((irq, prio)) = soc.pending_irq() {
            if cpu.irq_ready(prio) {
                let handler = soc.read32(VEC_BASE + 4 * irq as u32);
                cpu.interrupt(handler, prio);
                // the handler's first instruction may stay in compiled code (a micro-op now):
                // TIMER4's idle skip must see "in an ISR" from that instruction on
                soc.periph.in_isr = true;
                if irq == 1 {
                    soc.periph.exc_pending = false; // (the exception: taken once)
                }
            }
        }
    }
}

/// One instruction of cpu0 in a reference loop (after `cpu0_between`), with the PC limits as
/// `run_blocks` has them, checked here at every instruction: the first one fetched outside them
/// runs, then the CPU exception (DBG_MSG bit 12).
pub fn cpu0_step(cpu: &mut Cpu, soc: &mut Soc) -> Result<(), String> {
    let (_, trip) = cpu.pc_enter(cpu.pc);
    if let Err(e) = cpu.step(soc) {
        return Err(format!("{e:?} at pc {:#010x}", cpu.pc));
    }
    if trip {
        soc.guard_trip(12);
    }
    Ok(())
}

/// cpu0's block loop: interrupts taken between blocks, the SoC serviced when a tick is due.
/// `pending_tick` carries a tick already spent on the next instruction across calls. Returns
/// the instructions run: n, or fewer when cpu0 released cpu1 or the chip reset (it stops right
/// there; after a reset `soc.resets` has counted up, and cpu0 is back at its start), or when
/// it found a lock core 1 holds (`Soc::waits`, with core 1 running: `run_dual` hands over).
pub fn run_blocks(cpu: &mut Cpu, soc: &mut Soc, n: u32, pending_tick: &mut bool) -> Result<u32, String> {
    soc.take_boot(cpu);
    if soc.reset_due() {
        // (one that came due while cpu0 was held, or in cpu1's run: `run_dual`; n may be 0)
        soc.chip_reset(cpu);
        return Ok(0);
    }
    let mut done = 0u32;
    let mut ticked = std::mem::take(pending_tick);
    while done < n {
        if ticked || soc.periph.tick() {
            soc.advance(0);
        }
        ticked = false;
        if soc.reset_due() {
            soc.chip_reset(cpu);
            break;
        }
        cpu0_between(cpu, soc);
        let now = soc.periph.cycles;
        // not while a release is latched: run_fast's return must break below where it would
        // without the skip (lib.rs latches again on any later 0 -> 1 of 0x10008 bit 3)
        // nor on FM1_STRICT's clock, whichever part of it is on: a probe counts an instruction as one
        // tick (the cost model: each its own), its loads from XIP would move the flash cache and
        // charge its misses, and a misaligned access raised in a probe that is then undone would
        // stay raised
        let exact = soc.xip_cache.is_none() && !soc.strict_align && !cpu.cost_model;
        if soc.loop_skip.enabled && exact && !soc.cpu1_release && n - done >= PROBE_MIN && !soc.periph.any_pending() && soc.loop_skip.may_probe(now) {
            if soc.periph.next_due < soc.periph.cycles + PROBE_MIN as u64 {
                soc.loop_skip.next_try = soc.periph.next_due; // too close to the next event
            } else {
                let k = probe_loop(cpu, soc, n - done);
                if k > 0 {
                    done += k;
                    continue; // (the next instruction's tick unpaid)
                }
            }
        }
        let mut stepped = false;
        match cpu.run_fast(soc, n - done) {
            Ok((0, _)) => {
                if let Err(e) = cpu.step(soc) {
                    return Err(format!("{e:?} at pc {:#010x}", cpu.pc));
                }
                done += 1;
                stepped = true;
            }
            Ok((k, due)) => {
                done += k;
                ticked = due;
            }
            Err(e) => return Err(format!("{e:?} at pc {:#010x}", cpu.pc)),
        }
        if std::mem::take(&mut cpu.pc_trip) {
            // code fetched outside the PC limits: that instruction runs, then the CPU exception
            // (DBG_MSG bit 12; on an FM-1 harbor's probes from RAM stopped so, reti the next pc)
            if !stepped {
                if let Err(e) = cpu.step(soc) {
                    return Err(format!("{e:?} at pc {:#010x}", cpu.pc));
                }
                done += 1;
            }
            soc.guard_trip(12);
        }
        if soc.cpu1_release {
            break; // core 1 starts now (run_dual)
        }
        if soc.waits[0].is_some() {
            break; // spinning on a lock core 1 holds: core 1 runs now (run_dual)
        }
    }
    // the next instruction's tick may already have happened: carry it
    *pending_tick = ticked;
    Ok(done)
}

/// cpu1 once cpu0 released it (`0x10008 |= 8`): its entry is the word at 0x01C7FFF8 (the
/// stock firmware's stub at 0x020001B8 and X0X's entry both put it there; older images
/// without it: the RAM job loop at 0x01C022B6). It starts in supervisor mode.
pub fn released_cpu1(soc: &mut Soc, ram_size: usize) -> Option<Cpu> {
    if soc.cpu1_fault.is_some() {
        return None; // (stopped on a fault: held until a chip reset)
    }
    if !std::mem::take(&mut soc.cpu1_release) && soc.read32(0x10008) & 8 == 0 {
        return None;
    }
    soc.cpu1_idle = false;
    let entry = soc.read32(0x01C7_FFF8);
    let in_code = (0x0200_0000..0x0300_0000).contains(&entry) || (RAM_BASE..RAM_BASE + ram_size as u32).contains(&entry);
    if std::env::var_os("FM1_DUAL_TRACE").is_some() {
        eprintln!("cpu1 released at {} ms (entry {entry:#010x})", soc.periph.ms());
    }
    let mut c1 = Cpu::new(if in_code { entry } else { RAM_BASE + 0x22b6 });
    c1.core_id = 1;
    c1.stop_checks = true; // (run_cpu1's probe stops it: see there)
    c1.usp = RAM_BASE + 0x15eb4;
    c1.sp = RAM_BASE + 0x16eb4;
    Some(c1)
}

/// Run cpu1 for up to `cycles` instructions: interrupts from its own bank, then blocks or
/// single steps; back where it started with nothing changed means it is idle. It also stops
/// where it finds a lock cpu0 holds (`Soc::waits`: cpu0's slice is next, the rest of this run
/// forfeited, as a polling loop's) and, given `until` (what cpu0 spins on: `run_dual`), once
/// cpu0 would get it. Returns the instructions run, `cycles` at most (a stretch may run on a
/// little past them) and 2^20 at most: what a run that used all of those was due beyond them is
/// counted in `Soc::core1_dropped` and not run.
pub fn run_cpu1(c1: &mut Cpu, soc: &mut Soc, cycles: u64, until: Option<Wait>) -> Result<u64, String> {
    if soc.core_paused(1) || soc.reset_due() || soc.cpu1_fault.is_some() {
        return Ok(0);
    }
    if soc.cpu1_idle {
        // asleep since its `idle`: on again only with an interrupt from its bank pending
        if !(soc.periph.any_pending() && soc.pending_irq_for(1).is_some()) {
            return Ok(0);
        }
        soc.cpu1_idle = false;
    }
    let probe = (c1.pc, c1.regs, c1.sp);
    c1.probe[..16].copy_from_slice(&c1.regs);
    c1.probe[16] = c1.sp;
    // (with FM1_STRICT's costs, an instruction that ran past the slice's end is owed from the next one)
    let mut cycles = cycles;
    if c1.cost_model {
        let d = soc.core1_debt.min(cycles);
        soc.core1_debt -= d;
        cycles -= d;
        if cycles == 0 {
            return Ok(0);
        }
    }
    soc.core1_charging = true;
    let r = run_cpu1_in(c1, soc, cycles, probe, until);
    soc.core1_charging = false;
    r
}

fn run_cpu1_in(c1: &mut Cpu, soc: &mut Soc, cycles: u64, probe: (u32, [u32; 16], u32), until: Option<Wait>) -> Result<u64, String> {
    let budget = cycles.min(1 << 20);
    let mut left = budget;
    // first a block at a time, looking for the way back to where it started (a polling loop
    // goes round in well under this); then, busy, in big stretches. With cpu0 spinning on what
    // it holds, no probing and no stretch: a run that releases the hardware lock ends at the
    // `lockclr` (`Core1Bus::until`), one that clears a byte lock at the end of a short call
    // (`SPIN_CALL`: the budget holds to the instruction, so compiled code and the interpreter
    // end at the same one, and what cpu0 misses of the release is the same in both, and little).
    let mut probing = if until.is_some() { 0 } else { 512u64 };
    while left > 0 {
        let irq = if soc.periph.any_pending() { soc.pending_irq_for(1) } else { None };
        if let Some((irq, prio)) = irq {
            if c1.irq_ready(prio) {
                let handler = soc.read32(VEC_BASE + 4 * irq as u32);
                c1.interrupt(handler, prio);
            }
        }
        // Probing with nothing pending for core 1, the calls below (a block each) run back to
        // back in one stretch (`Cpu::stop_pc`), which returns only where one of them ends and
        // this loop could decide otherwise: back where it started, after anything that may have
        // touched a peripheral (which alone could make an interrupt pending: time stands still
        // while core 1 runs), or with `probing` or `left` spent. The same calls, checked at the
        // same points. (Melodee 1.4.5's core 1 polls in most slices, about 22 blocks a pass; a
        // call each was 12% of a Pixel 7 Pro's time.)
        let stretch = probing > 0 && irq.is_none() && c1.jit_on && c1.stop_checks;
        let busy = if probing > 0 { 0 } else { left.min(if matches!(until, Some(Wait::Byte(_))) { SPIN_CALL } else { 256 }) };
        // (a stretch: its last call may end up to `STRETCH_ROOM` past what is left, and its ticks
        // never run out)
        let span = if stretch { probing.min(left) + fm1_core::STRETCH_ROOM as u64 } else { busy };
        let budget0 = span.max(64) + 1;
        let mut bus = Core1Bus { soc: &mut *soc, budget: budget0, until, spent: 0 };
        // (probing: the block at pc, to its last instruction or to where the probe began if that
        // lies inside it, whether it runs compiled or not)
        let chunk = if stretch || probing == 0 { span } else { c1.block_len_to(&mut bus, probe.0).max(1) as u64 };
        if stretch {
            c1.stop_pc = probe.0;
            c1.stop_io = 0;
        }
        let k = c1.run_fast(&mut bus, chunk as u32);
        c1.stop_pc = 0;
        let k = match k {
            Ok((0, _)) => {
                if let Err(e) = c1.step(&mut bus) {
                    return Err(format!("cpu1: {e:?} at pc {:#010x}", c1.pc));
                }
                1
            }
            Ok((k, _)) => k as u64,
            Err(e) => return Err(format!("cpu1: {e:?} at pc {:#010x}", c1.pc)),
        };
        // (with FM1_STRICT's costs, core 1's time is the ticks it spent, not its instructions: all
        // of them, not what they took of the budget, which stops at 0)
        let k = if c1.cost_model {
            let stall = std::mem::take(&mut bus.soc.core1_ticks);
            (bus.spent + stall).max(1)
        } else {
            k
        };
        if c1.cost_model && k > left {
            soc.core1_debt += k - left;
        }
        left = left.saturating_sub(k);
        probing = probing.saturating_sub(k);
        // (first: a wait set by the step just made is for this run to take, whatever else ends it)
        if soc.waits[1].take().is_some() {
            break; // spinning on a lock cpu0 holds: cpu0's slice is next
        }
        if soc.reset_due() {
            break; // (cpu1 wrote PR_PWR: `Core1Bus::stop_at_reset`)
        }
        if soc.cpu1_idle {
            break; // asleep (`Core1Bus::idle`)
        }
        if std::mem::take(&mut soc.periph.core1_polls) {
            break; // polling TIMER4, which stands still while core 1 runs
        }
        if c1.pc == probe.0 && c1.sp == probe.2 && c1.regs == probe.1 {
            break; // polling with nothing to do
        }
        if until.is_some_and(|w| !soc.still_waits(0, w)) {
            break; // cpu0 would get what it spins on now: back to it
        }
    }
    // (a run that used its whole budget, cut by the cap: the rest of what it was due is not run)
    if left == 0 {
        soc.core1_dropped += cycles - budget;
    }
    Ok(budget - left)
}

/// cpu1's calls while cpu0 spins on a byte lock it holds (`run_cpu1`'s `until`): how far past
/// the store that clears it cpu1 may run before cpu0 sees it (compiled code stores RAM without
/// the bus, so the store itself cannot end the run, as a `lockclr` does).
const SPIN_CALL: u64 = 64;

/// cpu0 alone ran in stretches of this many instructions until it released cpu1, and the rest
/// of the stretch it released it in was not run. The block loop stops at the release itself
/// now (`run_blocks`), so cpu0 runs alone in one stretch; the instructions it gives up stay the
/// same (to the next multiple of this), so cpu1 starts where it did.
const SOLO_CHUNK: u32 = 4096;

/// Run n instructions of cpu0 through `run0` (the block loop, or a caller's own: it returns the
/// instructions run, fewer than asked only when cpu0 released cpu1 or the chip reset, and it
/// carries out a reset that is due, even when asked for none), with cpu1 alongside once it is
/// released.
pub fn run_dual(
    cpu1: &mut Option<Cpu>,
    soc: &mut Soc,
    n: u32,
    ram_size: usize,
    jit_on: bool,
    mut run0: impl FnMut(&mut Soc, u32) -> Result<u32, String>,
) -> Result<(), String> {
    let mut done = 0;
    // (a chip reset holds core 1 again: then cpu0 alone, until it releases it once more)
    while done < n {
        let resets = soc.resets;
        done = run_dual_from(cpu1, soc, done, n, ram_size, jit_on, &mut run0)?;
        if soc.resets == resets {
            break;
        }
        *cpu1 = None;
    }
    Ok(())
}

/// `run_dual` from `done` instructions on, until n or a chip reset; returns the new `done`.
fn run_dual_from(
    cpu1: &mut Option<Cpu>,
    soc: &mut Soc,
    mut done: u32,
    n: u32,
    ram_size: usize,
    jit_on: bool,
    run0: &mut impl FnMut(&mut Soc, u32) -> Result<u32, String>,
) -> Result<u32, String> {
    let resets = soc.resets;
    soc.cpu1_running = cpu1.is_some();
    while cpu1.is_none() {
        *cpu1 = released_cpu1(soc, ram_size).map(|mut c| {
            c.jit_on = jit_on;
            // (FM1_STRICT costs core 1's instructions as core 0's)
            c.cost_model = soc.xip_cache.is_some();
            c
        });
        if cpu1.is_some() || done >= n {
            break;
        }
        let k = n - done;
        soc.periph.room_cap = k as u64;
        let ran = run0(soc, k)?;
        if soc.resets != resets {
            return Ok(done + ran);
        }
        done = if ran < k { (done + ran).div_ceil(SOLO_CHUNK).saturating_mul(SOLO_CHUNK).min(n) } else { n };
    }
    let Some(c1) = cpu1.as_mut() else { return Ok(done) };
    soc.periph.room_cap = SLICE as u64;
    soc.cpu1_running = true;
    while done < n {
        // a slice of cpu0, then cpu1 for the cycles it took. cpu0 stops short of the slice where
        // it finds a lock core 1 holds (`Soc::waits`): cpu1 gets its cycles, then runs on while
        // it holds the lock (to the slice's end at most), the clock moving for cpu0's retries,
        // which are not run (the chip's cpu0 spins there, its interrupts disabled: the SDK's
        // critical sections); then cpu0 goes on with the rest of the slice. The slice's cycles
        // and instructions stay what they are, retries counted as instructions.
        let mut k = (n - done).min(SLICE);
        while k > 0 {
            let c0 = soc.periph.cycles;
            let ran = if soc.core_paused(0) && !soc.reset_due() {
                soc.advance(k as u64); // held by cpu1 (C0_CON bit 2): time runs on
                if soc.reset_due() {
                    run0(soc, 0)?; // (a reset is cpu0's to carry out, held or not)
                }
                k
            } else {
                run0(soc, k)?
            };
            if soc.resets != resets {
                return Ok(done + ran);
            }
            done += ran;
            k -= ran;
            let dt = soc.periph.cycles - c0;
            run_cpu1_on(c1, soc, dt, None)?;
            // (a denial at the slice's last instruction: taken, and the next slice's try sets it again or not)
            let wait = soc.waits[0].take();
            if k == 0 {
                break;
            }
            // (short of the slice with no lock to wait for: a latched release, see run_blocks;
            // the rest of the slice is forfeited)
            let Some(w) = wait else {
                done += k;
                break;
            };
            let mut spun = if soc.still_waits(0, w) { run_cpu1_on(c1, soc, k as u64, Some(w))? } else { 0 };
            if soc.still_waits(0, w) {
                spun = k as u64; // core 1 stopped still holding it (asleep, polling, a lock of its own): the rest of the slice would spin too
            }
            if spun > 0 {
                soc.advance(spun);
                done += spun as u32;
                k -= spun as u32;
                if soc.reset_due() {
                    run0(soc, 0)?; // (as held: a reset is cpu0's to carry out)
                    if soc.resets != resets {
                        return Ok(done);
                    }
                }
            }
        }
    }
    Ok(done)
}

/// `run_cpu1` for `run_dual`: a fault on core 1 halts the machine, unless the core survives
/// (`Soc::cpu1_survives`): then it stops alone, as the chip's core 1 faults there and stops, and
/// cpu0 goes on (a firmware built for a core 1 that fails falls back to one core: felucca-bsp's
/// cpu2_fallback), and the hardware lock it held is released (cpu0 would spin on `lockset` for
/// ever, nothing to hand the machine to).
fn run_cpu1_on(c1: &mut Cpu, soc: &mut Soc, cycles: u64, until: Option<Wait>) -> Result<u64, String> {
    match run_cpu1(c1, soc, cycles, until) {
        Ok(k) => Ok(k),
        Err(e) if soc.cpu1_survives => {
            eprintln!("cpu1 stopped at {} ms: {e} (the chip's core would fault; cpu0 goes on alone)", soc.periph.ms());
            soc.cpu1_fault = Some(e);
            if soc.lock_owner == Some(1) {
                soc.lock_owner = None;
            }
            soc.waits = [None; 2];
            Ok(0)
        }
        Err(e) => Err(e),
    }
}

/// The FM-1's RAM: 1 MiB from `RAM_BASE`.
pub const RAM_SIZE: usize = 1 << 20;
/// A flash sector: what a save holds of the chip, and what `Machine::changed_sectors` compares.
pub const SECTOR: usize = 4096;
/// Where the boot parameters go (r0 at cpu0's entry): near the top of the FM-1's RAM, so what the
/// CRT reads through them stays inside memory.
pub const BOOT_PARAMS: u32 = RAM_BASE + 0x7fc00;

/// How a machine is set up: what a front end chooses, from its command line, its page or its host.
/// None of it is read from the environment here: a front end that has a variable for one of these
/// reads it itself.
#[derive(Clone, Debug)]
pub struct Options {
    /// The CPU clock in MHz (the chip's nominal 240): the timers count its cycles.
    pub mhz: u64,
    /// RAM in bytes (`RAM_SIZE` on an FM-1).
    pub ram_size: usize,
    /// Hot code runs compiled, on cpu0 and on cpu1 once it is released (`run_dual`).
    pub jit: bool,
    /// A firmware waiting on a timer costs nothing to emulate (`Periph::skip_idle`).
    pub skip_idle: bool,
    /// cpu0 starts in user mode, as the SPL hands it over (fm1-core's `Cpu::user`); off: in supervisor
    /// mode, the old single stack.
    pub user: bool,
    /// As an FM-1 is timed (fm1-live's FM1_STRICT): each instruction its measured cost (fm1_core's
    /// op_cost8) on a clock in eighths of a cycle (the HW units' jobs too), the flash cache, misaligned
    /// RAM accesses fault; the interpreter only, and no loop skip (it counts instructions).
    pub strict: bool,
    /// The LCD's transfers take their time at the SPI clock, as on an FM-1 (`Soc::lcd_timed`).
    pub lcd_timed: bool,
    /// Which part the FM-1's flash is.
    pub flash_part: FlashPart,
    /// A fault on core 1 stops that core alone and cpu0 goes on (`Soc::cpu1_survives`).
    pub cpu1_survives: bool,
    /// A flaky core 1 (flaky.rs): the chance that one of its RAM loads misreads, and its generator's seed.
    pub flaky_core1: Option<(f64, u64)>,
    /// A package boot gives the SFC the chip key, and SFCENC as the loader leaves it (`Soc::sfc_boot`):
    /// what the chip holds below the plain window then shows decrypted through XIP. fm1-live boots
    /// so; the page, the plugin and fm1-emu boot without, and their window shows the chip as stored.
    /// That is how each front end has it today, not a design: the switch keeps each as it is.
    pub sfc: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mhz: 240,
            ram_size: RAM_SIZE,
            jit: false,
            skip_idle: true,
            user: true,
            strict: false,
            lcd_timed: false,
            flash_part: FlashPart::P25q80,
            cpu1_survives: false,
            flaky_core1: None,
            sfc: false,
        }
    }
}

/// An FM-1 as a front end runs it: the cores and the SoC, and what booting, running, halting and
/// saving the flash keep between calls. fm1-live, the page, the plugin and fm1-emu each had these
/// written out; here they are once, and a front end keeps around it only what is its own.
pub struct Machine {
    pub cpu: Cpu,
    /// The second core, from when cpu0 releases it (`run_dual`).
    pub cpu1: Option<Cpu>,
    pub soc: Soc,
    /// The package's USR area (the firmware's patch bank), if it has one, and how its board's stock
    /// firmware lays the bank out in it (`load_syx`).
    pub usr: Option<usize>,
    pub usr_layout: UsrLayout,
    /// a tick already spent on cpu0's next instruction (`run_blocks`)
    pending_tick: bool,
    /// why it halted, once it has
    halt: Option<String>,
    /// the flash chip as booted: what `changed_sectors` compares it with
    flash0: Vec<u8>,
}

impl Machine {
    /// The chip and cpu0 as every boot starts them: the image in the XIP window, cpu0 at `entry`
    /// with r0 at an empty boot-parameter pair.
    fn start(xip: Vec<u8>, entry: u32, o: &Options) -> (Cpu, Soc) {
        let mut soc = Soc::new(o.ram_size, xip);
        // (before the boot: the board sets the flash chip's id from it)
        soc.set_flash_part(o.flash_part);
        soc.periph.cycle_ticks = if o.strict { 8 } else { 1 };
        soc.periph.hz = o.mhz * 1_000_000 * soc.periph.cycle_ticks;
        soc.strict_align = o.strict;
        soc.lcd_timed = o.lcd_timed;
        if o.strict {
            soc.loop_skip.enabled = false;
            soc.xip_cache = Some(XipCache::default());
        }
        soc.periph.skip_idle = o.skip_idle;
        soc.flaky_core1 = o.flaky_core1.map(|(rate, seed)| FlakyCore1::new(rate, seed));
        soc.cpu1_survives = o.cpu1_survives;
        let mut cpu = Cpu::new(entry);
        // (cpu1 follows cpu0's setting: run_dual)
        cpu.jit_on = o.jit && !o.strict;
        cpu.cost_model = o.strict;
        cpu.user = o.user;
        // The SPL enters the app with r0 -> its boot-parameter struct, whose first word points at a
        // chip-info block (the stock app's boot_hwinfo_save copies both): an all-zero pair, until a
        // package's boot writes the real ones there (`stock_boot`).
        soc.write32(BOOT_PARAMS, BOOT_PARAMS + 0x40);
        cpu.regs[0] = BOOT_PARAMS;
        (cpu, soc)
    }

    fn booted(cpu: Cpu, soc: Soc, usr: Option<usize>, usr_layout: UsrLayout) -> Machine {
        let flash0 = soc.flash.mem.clone();
        Machine { cpu, cpu1: None, soc, usr, usr_layout, pending_tick: false, halt: None, flash0 }
    }

    /// A bare image mapped at the XIP base and entered at `entry`: a blank flash chip, no board
    /// but the FM-1's, r0 at the empty boot parameters.
    pub fn from_image(xip: Vec<u8>, entry: u32, o: &Options) -> Machine {
        let (cpu, soc) = Self::start(xip, entry, o);
        Self::booted(cpu, soc, None, UsrLayout::FM1)
    }

    /// A firmware from its update package (`stock_from_package`: its XIP window, its flash chip, its
    /// board), with a DX7 bank for its patch area if one is given. Err: not a package, or not a bank.
    pub fn from_package(pkg: &[u8], syx: Option<&[u8]>, o: &Options) -> Result<Machine, String> {
        Ok(Self::from_stock(stock_from_package(pkg, syx)?, o))
    }

    /// The same from a package already unpacked: its board and flash chip, the SPL's boot parameters
    /// (`stock_boot`) and, with `Options::sfc`, the SFC as the loader leaves it.
    pub fn from_stock(st: Stock, o: &Options) -> Machine {
        let (mut cpu, mut soc) = Self::start(st.xip, st.entry, o);
        cpu.regs[0] = stock_boot(&mut soc, &st.flash, BOOT_PARAMS, st.board);
        if o.sfc {
            soc.sfc_boot(st.key, st.app_end);
        }
        Self::booted(cpu, soc, st.usr, st.usr_layout)
    }

    /// Run n instructions of cpu0 in blocks (`run_blocks`), and cpu1 alongside once the firmware
    /// releases it (`run_dual`). An error is why the machine halted, for good: an instruction the
    /// emulator cannot run, and where. It is kept (`halted`), and a halted machine runs no more:
    /// it gives the same error again.
    #[inline]
    pub fn run(&mut self, n: u32) -> Result<(), String> {
        if let Some(why) = &self.halt {
            return Err(why.clone());
        }
        let jit = self.cpu.jit_on;
        // (cpu1's compiled code counts its helper calls when cpu0's does: `Cpu::profile`)
        if let Some(c1) = self.cpu1.as_mut() {
            c1.profile = self.cpu.profile;
        }
        let Machine { cpu, cpu1, soc, pending_tick, .. } = self;
        let ram_size = soc.ram.len();
        let ran = run_dual(cpu1, soc, n, ram_size, jit, |soc, k| run_blocks(cpu, soc, k, pending_tick));
        if let Err(why) = &ran {
            self.halt = Some(why.clone());
        }
        ran
    }

    /// Why the machine halted, if it has.
    pub fn halted(&self) -> Option<&str> {
        self.halt.as_deref()
    }

    /// Halt the machine from outside, with why: a front end that caught a failure of its own around
    /// `run` (the plugin, a panic). The first reason stays.
    pub fn halt(&mut self, why: String) {
        self.halt.get_or_insert(why);
    }

    /// The flash chip as booted (the package's, with what the boot laid on it).
    pub fn flash_base(&self) -> &[u8] {
        &self.flash0
    }

    /// The sectors of the flash chip that differ from the booted one, each with its flash offset: what
    /// the firmware wrote (its patches, its settings) and what was put over the boot (`restore_sectors`,
    /// `load_syx`). What a save keeps.
    pub fn changed_sectors(&self) -> Vec<(u32, Vec<u8>)> {
        let (now, then) = (&self.soc.flash.mem, &self.flash0);
        (0..now.len().min(then.len()) / SECTOR)
            .filter(|s| now[s * SECTOR..(s + 1) * SECTOR] != then[s * SECTOR..(s + 1) * SECTOR])
            .map(|s| ((s * SECTOR) as u32, now[s * SECTOR..(s + 1) * SECTOR].to_vec()))
            .collect()
    }

    /// Saved sectors (a run's `changed_sectors`) over the flash: each replaces its whole sector on
    /// the chip and, as the firmware's own erases and programs do, in the XIP window
    /// (`Soc::restore_sector`). The base stays the booted chip. All or nothing: with an offset that
    /// is no sector's, a length that is not a sector's or a sector given twice, nothing is written.
    pub fn restore_sectors(&mut self, sectors: &[(u32, &[u8])]) -> Result<(), String> {
        let mut seen = [false; FLASH_SIZE / SECTOR];
        for &(at, data) in sectors {
            let at = at as usize;
            if at >= FLASH_SIZE || at & (SECTOR - 1) != 0 || data.len() != SECTOR {
                return Err("invalid flash sector offset or length".into());
            }
            if std::mem::replace(&mut seen[at / SECTOR], true) {
                return Err("duplicate flash sector".into());
            }
        }
        for &(at, data) in sectors {
            self.soc.restore_sector(at as usize, data);
        }
        Ok(())
    }

    /// A DX7 32-voice bank into the package's patch area (slots 1-32), over the flash as it is now
    /// (restored sectors too). Only the sectors it changes are written, as `restore_sectors` writes
    /// them: the window keeps the installed code elsewhere, the base stays the booted chip, and the
    /// bank is among the changed sectors. Err: this package has no patch area, or the bank is not
    /// one; nothing is written then.
    pub fn load_syx(&mut self, bank: &[u8]) -> Result<(), String> {
        let usr = self.usr.ok_or("this firmware has no patch area")?;
        let mut flash = self.soc.flash.mem.clone();
        fwsc::usr_load_syx(&mut flash, usr, self.usr_layout, bank, 0)?;
        for at in (0..flash.len()).step_by(SECTOR) {
            if flash[at..at + SECTOR] != self.soc.flash.mem[at..at + SECTOR] {
                self.soc.restore_sector(at, &flash[at..at + SECTOR]);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flaky::{FlakyCore1, MISREAD_BIT};
    use crate::periph::Action;
    use crate::{CPU_HZ, XIP_BASE};

    // Polling loops in XIP code; r1 points 544 bytes past the word the loop polls.
    /// `d5 ec 10 2e` r2 = [r1+-544]
    const LOAD: [u8; 4] = [0xd5, 0xec, 0x10, 0x2e];
    /// `d7 ec 51 38` [r5+-128] = r3
    const STORE: [u8; 4] = [0xd7, 0xec, 0x51, 0x38];
    /// `61 ff 20 20 oo oo` if ((r2 & 0x20) != 0) goto back to 0 (from `len` bytes in)
    fn while_set(len: u32) -> [u8; 6] {
        let o = (-((len + 6) as i32) / 2) as u16;
        [0x61, 0xff, 0x20, 0x20, o as u8, (o >> 8) as u8]
    }
    /// PA5: key 5 (row 1, column 0) pulls it low while down
    const KEY: u8 = 5;
    const PA_IN: u32 = 0x50004;

    fn machine(code: &[u8], regs: &[(usize, u32)], skip: bool) -> (Cpu, Soc) {
        let mut xip = code.to_vec();
        xip.resize(code.len() + 256, 0);
        let mut soc = Soc::new(1 << 20, xip);
        soc.loop_skip.enabled = skip;
        // every column of the key matrix selected: 16 zeros into the shift registers (SRCLK, PA3),
        // latched (RCLK, PA1)
        for _ in 0..16 {
            soc.write32(0x50000, 0);
            soc.write32(0x50000, 8);
        }
        soc.write32(0x50000, 0);
        soc.write32(0x50000, 2);
        let mut cpu = Cpu::new(XIP_BASE);
        for &(r, v) in regs {
            cpu.regs[r] = v;
        }
        (cpu, soc)
    }

    /// The same machine with the loop skip and without, run in odd chunks with keys going down and
    /// up at the given cycles: equal after every chunk (core, clock, the polled-at RAM word).
    fn run_both(code: &[u8], regs: &[(usize, u32)], keys: &[(u64, bool)], total: u32) -> (Cpu, Soc) {
        let (mut a, mut sa) = machine(code, regs, true);
        let (mut b, mut sb) = machine(code, regs, false);
        for &(at, down) in keys {
            sa.periph.script.push_back((at, Action::Key { id: KEY, down }));
            sb.periph.script.push_back((at, Action::Key { id: KEY, down }));
        }
        let (mut ta, mut tb) = (false, false);
        let mut done = 0;
        let mut seen = std::collections::BTreeSet::new();
        while done < total {
            let n = 7919.min(total - done);
            assert_eq!(run_blocks(&mut a, &mut sa, n, &mut ta), Ok(n));
            assert_eq!(run_blocks(&mut b, &mut sb, n, &mut tb), Ok(n));
            done += n;
            assert_eq!((a.pc, a.regs, a.insn_count, ta), (b.pc, b.regs, b.insn_count, tb), "after {done}");
            assert_eq!(sa.periph.cycles, sb.periph.cycles, "after {done}");
            assert_eq!(sa.ram.read32(0x1000), sb.ram.read32(0x1000), "after {done}");
            assert_eq!(sa.p33.snap(), sb.p33.snap(), "after {done}");
            seen.insert(a.pc);
        }
        assert_eq!(sb.loop_skip.probes, 0);
        if !keys.is_empty() {
            assert!(seen.len() >= 4, "both loops seen at chunk ends: {seen:x?}");
        }
        (a, sa)
    }

    #[test]
    fn code_outside_the_pc_limits_raises_the_exception() {
        // the PC limits as Dinghy's fm1_guard_enable sets them, around code that is not in them
        // (harbor's probes from RAM on an FM-1, 2026-10-24): DBG_MSG bit 12 and the exception pend.
        // Both windows start at 0..0x07FFFFFF (pi32v2-isa hw/fm1-2026-11-15-excprobe, group 6), so
        // code keeps running between the writes of window 0 and window 1, as felucca-bsp's
        // fm1_guard_enable needs (FM1 Audio Router and DUB-BOX run from RAM through that gap)
        let code = [0x20u8, 0x00].repeat(32); // csync x 32
        let (mut cpu, mut soc) = machine(&code, &[], false);
        assert_eq!(soc.pc_windows(), Some([(0, 0x07ff_ffff); 2]));
        soc.write32(0x1eef100, 0xf << 4); // IRQ 1 enabled, priority 7 (as fm1_irq.h)
        soc.write32(0x1eee384, XIP_BASE + 0x1000); // window 0 (low, high): past the code
        soc.write32(0x1eee380, XIP_BASE + 0x2000);
        let mut t = false;
        run_blocks(&mut cpu, &mut soc, 4, &mut t).unwrap();
        assert_eq!(soc.dbg_cause, 0, "window 1 as at power-on holds the code");
        soc.write32(0x1eee38c, RAM_BASE); // window 1: RAM
        soc.write32(0x1eee388, RAM_BASE + 0xfff);
        run_blocks(&mut cpu, &mut soc, 8, &mut t).unwrap();
        assert_ne!(soc.read32(0x1eee244) & 1 << 12, 0);
        assert!(soc.periph.exc_pending);
        soc.write32(0x1eee248, 1 << 12); // DBG_MSG_CLR
        assert_eq!(soc.read32(0x1eee244) & 1 << 12, 0);
        // the code inside window 0: nothing
        let (mut cpu, mut soc) = machine(&code, &[], false);
        soc.write32(0x1eee384, XIP_BASE);
        soc.write32(0x1eee380, XIP_BASE + 0xfff);
        run_blocks(&mut cpu, &mut soc, 8, &mut t).unwrap();
        assert!(!soc.periph.exc_pending);
        assert_eq!(soc.dbg_cause, 0);
    }

    #[test]
    fn the_exception_is_dropped_when_irq_1_is_not_enabled() {
        // a guard that trips in a firmware without the exception's handler: the cause is kept, but
        // nothing stays pending for good (no loop skip or idle skip ever again)
        let (_, mut soc) = machine(&[0x20, 0x00], &[], false);
        soc.write32(0x1eee340, 3 << 4); // DBG_EN: AXI read / write invalid
        soc.read32(0x100);
        assert!(!soc.periph.exc_pending);
        assert!(!soc.periph.any_pending());
        assert_ne!(soc.read32(0x1eee244) & 1 << 4, 0);
    }

    #[test]
    fn emu_msg_reads_its_causes_and_a_write_clears_them() {
        // fm1_irq.h's init writes all ones: that clears, and is not read back
        let (_, mut soc) = machine(&[0x20, 0x00], &[], false);
        soc.write32(0x1eef0d4, 0xffff_ffff);
        assert_eq!(soc.read32(0x1eef0d4), 0);
        soc.strict_align = true;
        soc.read32(RAM_BASE + 2); // misaligned: bit 0
        assert_eq!(soc.read32(0x1eef0d4), 1);
        soc.write32(0x1eef0d4, 2); // another bit: bit 0 stays
        assert_eq!(soc.read32(0x1eef0d4), 1);
        soc.write32(0x1eef0d4, 1);
        assert_eq!(soc.read32(0x1eef0d4), 0);
        // DBG_MSG: a console word written there is not read back as causes
        soc.write32(0x1eee244, 0x4142_4344);
        assert_eq!(soc.read32(0x1eee244), 0);
    }

    #[test]
    fn misaligned_words_move_the_aligned_down_word() {
        // an FM-1 (pi32v2-isa hw/fm1-2026-11-15-excprobe, group 1): a misaligned word load gives the aligned-down word, a misaligned
        // word store writes the whole word there; a halfword load at +1 the aligned-down halfword
        let (_, mut soc) = machine(&[0x20, 0x00], &[], false);
        soc.strict_align = true;
        soc.write32(RAM_BASE + 0x100, 0x1122_3344);
        soc.write32(RAM_BASE + 0x104, 0x5566_7788);
        assert_eq!(soc.read32(RAM_BASE + 0x103), 0x1122_3344);
        assert_eq!(soc.read16(RAM_BASE + 0x101), 0x3344);
        soc.write32(RAM_BASE + 0x106, 0xa1b2_c3d4);
        assert_eq!((soc.read32(RAM_BASE + 0x100), soc.read32(RAM_BASE + 0x104)), (0x1122_3344, 0xa1b2_c3d4));
        assert_eq!(soc.emu_cause, 1);
        // RAM's last word as well
        let end = RAM_BASE + soc.ram.len() as u32 - 4;
        soc.write32(end, 0x1122_3344);
        assert_eq!(soc.read32(end + 1), 0x1122_3344);
        soc.write32(end + 3, 0x5566_7788);
        assert_eq!(soc.read32(end), 0x5566_7788);
    }

    /// Both PC limit windows to `lo..=hi`
    fn pc_limits(soc: &mut Soc, lo: u32, hi: u32) {
        for high in [0x1eee380, 0x1eee388] {
            soc.write32(high + 4, lo);
            soc.write32(high, hi);
        }
    }

    #[test]
    fn a_chip_reset_clears_the_guards() {
        // after a watchdog or PR_PWR reset the PC limits and the causes are as at power-on: a boot
        // into code outside the old windows takes no exception
        let code = [0x20u8, 0x00].repeat(32);
        let (mut cpu, mut soc) = machine(&code, &[], false);
        soc.write32(0x1eef100, 0xf << 4);
        let mut t = false;
        run_blocks(&mut cpu, &mut soc, 1, &mut t).unwrap(); // (the boot state taken)
        pc_limits(&mut soc, XIP_BASE + 0x1000, XIP_BASE + 0x2000);
        run_blocks(&mut cpu, &mut soc, 8, &mut t).unwrap();
        assert!(cpu.pc_win.is_some() && soc.dbg_cause != 0);
        soc.emu_cause = 1;
        soc.reset_req = Some(crate::p33::P33::RST_SOFT);
        run_blocks(&mut cpu, &mut soc, 8, &mut t).unwrap();
        assert_eq!(soc.resets, 1);
        assert_eq!((soc.dbg_cause, soc.emu_cause, soc.periph.exc_pending), (0, 0, false));
        assert!(cpu.pc_win.is_none() && soc.pc_windows() == Some([(0, 0x07ff_ffff); 2]));
        run_blocks(&mut cpu, &mut soc, 8, &mut t).unwrap();
        assert_eq!(soc.dbg_cause, 0, "the boot runs outside the old windows: no exception");
    }

    #[test]
    fn the_reference_loop_has_the_pc_limits_and_takes_the_exception_once() {
        // `cpu0_between` / `cpu0_step` (fm1-live's FM1_NO_BLOCKS): the PC limits apply at every
        // instruction, and the exception is taken once (a handler that returns is not re-entered)
        let mut code = [0x20u8, 0x00].repeat(32); // csync x 32
        code.resize(0x100, 0);
        code[0x80..0x82].copy_from_slice(&[0x20, 0x00]); // the handler: csync ...
        let (mut cpu, mut soc) = machine(&code, &[], false);
        soc.write32(0x1eef100, 0xf << 4);
        soc.write32(VEC_BASE + 4, XIP_BASE + 0x80);
        pc_limits(&mut soc, XIP_BASE + 0x80, XIP_BASE + 0xff); // the handler only
        cpu.ie = true;
        cpu.sreg_store[0] |= 0x100;
        let mut taken = 0;
        for _ in 0..6 {
            cpu0_between(&mut cpu, &mut soc);
            if cpu.pc == XIP_BASE + 0x80 {
                taken += 1;
                break;
            }
            cpu0_step(&mut cpu, &mut soc).unwrap();
        }
        assert_eq!(taken, 1, "the exception after the first instruction outside the limits");
        assert_ne!(soc.dbg_cause & 1 << 12, 0);
        assert!(!soc.periph.exc_pending, "taken once");
    }

    #[test]
    fn the_pc_limits_trip_once_until_dbg_con_bit_18() {
        // an FM-1 (pi32v2-isa hw/fm1-2026-11-15-excprobe, groups 6 and 8 to 11): after a trip, code outside the limits sets DBG_MSG bit 12 again but
        // raises nothing until DBG_CON bit 18 is written (it reads back 0)
        let code = [0x20u8, 0x00].repeat(32); // csync x 32
        let (mut cpu, mut soc) = machine(&code, &[], false);
        soc.write32(0x1eef100, 0xf << 4);
        pc_limits(&mut soc, XIP_BASE + 0x1000, XIP_BASE + 0x2000);
        let mut t = false;
        run_blocks(&mut cpu, &mut soc, 8, &mut t).unwrap();
        assert!(soc.periph.exc_pending, "the first trip");
        // (the code inside the windows for a while, then the limits narrowed again)
        let trip_again = |cpu: &mut fm1_core::Cpu, soc: &mut Soc| {
            (soc.periph.exc_pending, soc.dbg_cause) = (false, 0);
            cpu.pc = XIP_BASE;
            soc.write32(0x1eee384, XIP_BASE);
            let mut t = false;
            run_blocks(cpu, soc, 4, &mut t).unwrap();
            cpu.pc = XIP_BASE;
            soc.write32(0x1eee384, XIP_BASE + 0x1000);
            run_blocks(cpu, soc, 4, &mut t).unwrap();
            (soc.periph.exc_pending, soc.dbg_cause & 1 << 12 != 0)
        };
        assert_eq!(trip_again(&mut cpu, &mut soc), (false, true), "spent: the cause only");
        soc.write32(0x1eee344, 1 << 17);
        assert_eq!(trip_again(&mut cpu, &mut soc), (false, true), "bit 17 alone does not re-arm");
        soc.write32(0x1eee344, 1 << 18);
        assert_eq!(soc.read32(0x1eee344), 0, "bit 18 reads back 0");
        assert_eq!(trip_again(&mut cpu, &mut soc), (true, true), "re-armed");
    }

    #[test]
    fn null_page_access_raises_the_exception_once_armed() {
        let (_, mut soc) = machine(&[0x20, 0x00], &[], false);
        soc.write32(0x1eef100, 0xf << 4); // IRQ 1 enabled
        soc.read32(0x100);
        assert!(!soc.periph.exc_pending, "not armed");
        soc.write32(0x1eee340, 3 << 4); // DBG_EN: AXI read / write invalid
        soc.read32(0x100);
        assert!(soc.periph.exc_pending);
        assert_ne!(soc.read32(0x1eee244) & 1 << 4, 0);
    }

    #[test]
    fn polling_loop_skipped_to_the_cycle() {
        // wait for the key (2 instructions a round), then for its release (3), and round again:
        // a round too many or too few in one shows as another place in the other
        let mut code = [LOAD.as_slice(), &while_set(4)].concat();
        code.extend_from_slice(&LOAD);
        code.extend_from_slice(&LOAD);
        code.extend_from_slice(&[0x60, 0xff, 0x20, 0x20, 0xf9, 0xff]); // while clear (back to 10)
        code.extend_from_slice(&[0x60, 0xff, 0x00, 0x20, 0xf1, 0xff]); // back to 0
        let keys: Vec<(u64, bool)> = (0..40).map(|i| (20_011 + i * 13_337 + i * i * 7, i % 2 == 0)).collect();
        let (_, soc) = run_both(&code, &[(1, PA_IN + 544)], &keys, 560_000);
        assert!(soc.loop_skip.skipped > 300_000, "{} skipped", soc.loop_skip.skipped);
    }

    #[test]
    fn round_that_stores_what_is_there_skipped() {
        // [0x1000] = r3 every round: the first changes it, the rest store what is there
        let code = [STORE.as_slice(), &LOAD, &while_set(8)].concat();
        let regs = [(1, PA_IN + 544), (5, RAM_BASE + 0x1000 + 128), (3, 0xdead_beef)];
        let (_, soc) = run_both(&code, &regs, &[], 100_000);
        assert_eq!(soc.ram.read32(0x1000), 0xdead_beef);
        assert!(soc.loop_skip.skipped > 90_000, "{} skipped", soc.loop_skip.skipped);
    }

    #[test]
    fn round_that_changes_ram_not_skipped() {
        // r2 = [w]; [w] = 0x20 twice; while r2 bit 5 is clear: the first round leaves w changed
        // (and the core as it was), the second leaves the loop (for a loop on the spot)
        let code = [LOAD.as_slice(), &STORE, &STORE, &[0x60, 0xff, 0x20, 0x20, 0xf7, 0xff], &[0x60, 0xff, 0x00, 0x20, 0xfd, 0xff]].concat();
        let regs = [(1, RAM_BASE + 0x1000 + 544), (5, RAM_BASE + 0x1000 + 128), (3, 0x20)];
        let (cpu, _) = run_both(&code, &regs, &[], 20_000);
        assert_eq!(cpu.pc, XIP_BASE + 18);
    }

    #[test]
    fn watchdog_kick_through_p33_skipped() {
        // P33_CON = 0x11 every round (a transfer: the port's state settles after 255)
        let code = [STORE.as_slice(), &LOAD, &while_set(8)].concat();
        let regs = [(1, PA_IN + 544), (5, crate::p33::P33::CON + 128), (3, 0x11)];
        let (_, soc) = run_both(&code, &regs, &[], 100_000);
        assert!(soc.loop_skip.skipped > 90_000, "{} skipped", soc.loop_skip.skipped);
    }

    /// One P33 access as Felucca's fm1_sys.h makes it (`cmd`: 0x00 write, 0x20 or, 0x80 read).
    fn p33(soc: &mut Soc, cmd: u8, addr: u8, data: u8) -> u8 {
        soc.write32(p33::P33::CON, 1);
        for b in [cmd, addr, data] {
            soc.write32(p33::P33::DAT, b as u32);
            soc.write32(p33::P33::CON, 0x11);
        }
        soc.write32(p33::P33::CON, 0);
        soc.read32(p33::P33::DAT) as u8
    }

    /// `while (PA5 high) { WDT_CON |= 0x40; }  for (;;);` (JieLi's clang -O2): feeds the watchdog
    /// through P33 while key 5 is up, then hangs.
    const FEED: [u8; 54] = [
        0xc0, 0xff, 0x04, 0x00, 0x05, 0x00, 0x01, 0x60, 0x51, 0xe8, 0x14, 0x28, 0xc1, 0xff, 0x08, 0x3e, 0x01, 0x00,
        0x42, 0x31, 0x40, 0xea, 0x01, 0x10, 0x41, 0xea, 0x20, 0x10, 0x92, 0x60, 0x41, 0xea, 0x80, 0x10, 0x92, 0x60,
        0x41, 0xea, 0x40, 0x10, 0x92, 0x60, 0x40, 0xea, 0x00, 0x10, 0x03, 0x60, 0x53, 0xe8, 0xf0, 0x2b, 0xf7, 0x9f,
    ];

    /// `while (PA5 high);  PR_PWR |= 0x10;  for (;;);` (JieLi's clang -O2): a chip reset once key 5
    /// is down.
    const REBOOT: [u8; 48] = [
        0xc0, 0xff, 0x04, 0x00, 0x05, 0x00, 0x01, 0x60, 0x51, 0xe8, 0xfd, 0x2b, 0xc0, 0xff, 0x08, 0x3e, 0x01, 0x00,
        0x40, 0xea, 0x01, 0x00, 0x41, 0xea, 0x20, 0x00, 0x41, 0x31, 0x81, 0x60, 0x41, 0xea, 0xa0, 0x00, 0x81, 0x60,
        0x41, 0xea, 0x10, 0x00, 0x81, 0x60, 0x40, 0xea, 0x00, 0x00, 0xf7, 0x9f,
    ];

    /// `code` with key 5 going down and up at the given cycles, `setup` first, run in odd chunks
    /// with the JIT and interpreted, with the loop skip and without: the same after every chunk
    /// (core, clock, resets). Returns the cycles of the resets, and what the skip skipped.
    fn run_resetting(code: &[u8], setup: fn(&mut Soc), keys: &[(u64, bool)], total: u64) -> (Vec<u64>, u64) {
        run_resetting_in(code, setup, keys, total, 7919)
    }

    /// `run_resetting` with the runner's calls `chunk` instructions long.
    fn run_resetting_in(code: &[u8], setup: fn(&mut Soc), keys: &[(u64, bool)], total: u64, chunk: u32) -> (Vec<u64>, u64) {
        let mut runs = vec![];
        for (jit, skip) in [(false, false), (false, true), (true, false), (true, true)] {
            let (mut cpu, mut soc) = machine(code, &[], skip);
            cpu.jit_on = jit;
            setup(&mut soc);
            for &(at, down) in keys {
                soc.periph.script.push_back((at, Action::Key { id: KEY, down }));
            }
            let (mut ticked, mut chunks, mut resets) = (false, vec![], vec![]);
            while soc.periph.cycles < total {
                run_blocks(&mut cpu, &mut soc, chunk, &mut ticked).unwrap();
                if soc.resets as usize > resets.len() {
                    resets.push(soc.periph.cycles);
                    assert_eq!(cpu.pc, XIP_BASE, "cpu0 from its start");
                }
                chunks.push((cpu.pc, cpu.regs, cpu.insn_count, soc.periph.cycles, soc.resets));
            }
            runs.push((chunks, resets, soc.loop_skip.skipped));
        }
        for r in &runs[1..] {
            assert_eq!(r.0, runs[0].0, "JIT / interpreter, skip / no skip: the same");
        }
        (runs[0].1.clone(), runs[1].2)
    }

    #[test]
    fn watchdog_resets_one_period_after_the_last_feed() {
        const KEY_AT: u64 = 2_000_003;
        let period = CPU_HZ / 1000; // WDT_CON 0: 1 ms
        let setup = |soc: &mut Soc| {
            p33(soc, 0x00, 0x80, 0x40); // WDT_CON = 1 ms, count cleared
            p33(soc, 0x20, 0x80, 0x10); // on
        };
        let (resets, skipped) = run_resetting(&FEED, setup, &[(KEY_AT, true)], 3_000_000);
        // once (a reset stops the watchdog), a period after the last feed, in the last round
        // before the key went down (a round is 13 instructions; the reset comes between blocks)
        assert_eq!(resets.len(), 1, "{resets:?}");
        assert!((KEY_AT + period - 20..KEY_AT + period + 20).contains(&resets[0]), "reset at {}", resets[0]);
        assert!(skipped > 2_000_000, "{skipped} skipped");
    }

    /// `WDT_CON = 0x10;  for (;;);` (FEED's P33 access with the write command and 0x10): the
    /// watchdog switched on from code, 1 ms, never fed.
    const ARM: [u8; 34] = [
        0xc1, 0xff, 0x08, 0x3e, 0x01, 0x00, 0x42, 0x31, 0x40, 0xea, 0x01, 0x10, 0x41, 0xea, 0x00, 0x10, 0x92, 0x60,
        0x41, 0xea, 0x80, 0x10, 0x92, 0x60, 0x41, 0xea, 0x10, 0x10, 0x92, 0x60, 0x40, 0xea, 0x00, 0x10,
    ];

    #[test]
    fn watchdog_armed_from_code_resets_at_the_same_cycle_compiled_or_not() {
        // switched on by code with nothing else due, in calls long enough to run past a period:
        // a reset a period after each boot's arm, compiled or not, with the loop skip or not
        let period = CPU_HZ / 1000;
        let code: Vec<u8> = ARM.iter().chain(&[0xf7, 0x9f]).copied().collect();
        let (resets, _) = run_resetting_in(&code, |_| {}, &[], 5 * period, 1 << 20);
        assert!(resets.len() >= 4, "{resets:?}");
        let mut last = 0;
        for &at in &resets {
            // (a period after the arm, 10-odd instructions into each boot)
            assert!((last + period..last + period + 100).contains(&at), "reset at {at}, the one before at {last}: {resets:?}");
            last = at;
        }
    }

    #[test]
    fn pr_pwr_resets_at_the_same_instruction_compiled_or_not() {
        // down for 200 cycles: it resets, starts again, finds the key still down and resets again
        let (resets, _) = run_resetting(&REBOOT, |_| {}, &[(100_003, true), (100_203, false)], 300_000);
        assert!(resets.len() >= 5 && resets.iter().all(|&c| (100_003..100_250).contains(&c)), "{resets:?}");
    }

    #[test]
    fn pr_pwr_resets_the_chip_and_ram_stays() {
        // a loop on the spot: `goto -2`
        let (mut cpu, mut soc) = machine(&[0xf7, 0x9f], &[(4, 0x1234)], false);
        let mut ticked = false;
        run_blocks(&mut cpu, &mut soc, 1000, &mut ticked).unwrap();
        (cpu.regs[4], cpu.ie) = (0x5678, true);
        soc.write32(RAM_BASE + 0x7fd80, 0x5f627375); // a mailbox word
        soc.write32(0x50008, 0xff); // a port's direction (a chip register)
        p33(&mut soc, 0x20, 0xa0, 0x10); // PR_PWR |= 0x10
        assert!(soc.reset_due());
        let n = run_blocks(&mut cpu, &mut soc, 1000, &mut ticked).unwrap();
        assert!(n < 1000 && soc.resets == 1);
        assert_eq!((cpu.pc, cpu.regs[4], cpu.ie), (XIP_BASE, 0x1234, false), "cpu0 as the loader started it");
        assert_eq!(soc.read32(RAM_BASE + 0x7fd80), 0x5f627375, "RAM stays");
        assert_eq!(soc.read32(0x50008), 0, "the chip's registers as at boot");
        assert_eq!(p33(&mut soc, 0x80, 0x12, 0x5e), p33::P33::RST_SOFT, "RST_SRC: PR_PWR");
        assert_eq!(p33(&mut soc, 0x80, 0xa0, 0x5e), 0, "PR_PWR's bit 4 reads 0");
    }

    #[test]
    fn watchdog_off_or_fed_does_not_reset() {
        // (the loop on the spot is skipped to the next event: here the watchdog's deadline)
        let (mut cpu, mut soc) = machine(&[0xf7, 0x9f], &[], true);
        let mut ticked = false;
        let mut run_to = |cpu: &mut Cpu, soc: &mut Soc, ms: u64| {
            while soc.periph.ms() < ms && soc.resets == 0 {
                run_blocks(cpu, soc, 1 << 20, &mut ticked).unwrap();
            }
        };
        p33(&mut soc, 0x00, 0x80, 0x4a); // 1 s, cleared
        p33(&mut soc, 0x20, 0x80, 0x10); // on
        assert_eq!(p33(&mut soc, 0x80, 0x80, 0x5e), 0x1a, "the clear bit reads 0");
        for t in 1..=3 {
            run_to(&mut cpu, &mut soc, t * 900);
            p33(&mut soc, 0x20, 0x80, 0x40); // fed
        }
        assert_eq!(soc.resets, 0, "fed in time");
        p33(&mut soc, 0x00, 0x80, 0x00); // off
        run_to(&mut cpu, &mut soc, 6000);
        assert_eq!(soc.resets, 0, "switched off");
        p33(&mut soc, 0x00, 0x80, 0x1a); // on again, not fed: 1 s from now
        let on = soc.periph.cycles;
        run_to(&mut cpu, &mut soc, 9000);
        assert_eq!(soc.resets, 1, "run out");
        assert!((on + CPU_HZ * 1000 / 1000..on + CPU_HZ + 2_000_000).contains(&soc.periph.cycles), "{} cycles on", soc.periph.cycles - on);
    }

    /// `PR_PWR |= 0x10;  *(int *)0x01C10000 = 0x600d;  for (;;);` (JieLi's clang -O2): core 1
    /// resets the chip, then stores a word it must not get to.
    const CORE1_REBOOT: [u8; 48] = [
        0xc0, 0xff, 0x08, 0x3e, 0x01, 0x00, 0x40, 0xea, 0x01, 0x00, 0x41, 0xea, 0x20, 0x00, 0x41, 0x31, 0x81, 0x60,
        0x41, 0xea, 0xa0, 0x00, 0x81, 0x60, 0x41, 0xea, 0x10, 0x00, 0x81, 0x60, 0x40, 0xea, 0x00, 0x00, 0xc0, 0xff,
        0x00, 0x00, 0xc1, 0x01, 0x41, 0xe0, 0x0d, 0x60, 0x81, 0x60, 0xf7, 0x9f,
    ];

    /// cpu0 on the spot (`goto -2`) and cpu1 released into `core1`, which follows it; booted
    /// before the release, so a reset holds cpu1 again.
    fn two_cores(core1: &[u8], jit: bool) -> (Cpu, Option<Cpu>, Soc, bool) {
        let (mut cpu, mut soc) = machine(&[&[0xf7, 0x9f], core1].concat(), &[], false);
        cpu.jit_on = jit;
        soc.take_boot(&cpu);
        soc.write32(0x01c7_fff8, XIP_BASE + 2);
        soc.write32(0x10008, 8);
        let (mut c1, mut t) = (None, false);
        run_dual(&mut c1, &mut soc, SLICE, 1 << 20, jit, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
        assert!(c1.is_some(), "core 1 started");
        (cpu, c1, soc, t)
    }

    #[test]
    fn watchdog_resets_while_core_1_holds_core_0() {
        for jit in [false, true] {
            let (mut cpu, mut c1, mut soc, mut t) = two_cores(&[0xf7, 0x9f], jit);
            soc.write32(0x1eee000, 4); // C0_CON: cpu0 held
            p33(&mut soc, 0x00, 0x80, 0x40); // WDT_CON = 1 ms, count cleared
            p33(&mut soc, 0x20, 0x80, 0x10); // on
            let (on, held) = (soc.periph.cycles, cpu.insn_count);
            while soc.resets == 0 && soc.periph.cycles < on + 2 * CPU_HZ / 1000 {
                run_dual(&mut c1, &mut soc, SLICE, 1 << 20, jit, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
                assert!(soc.resets == 1 || cpu.insn_count == held, "cpu0 ran while held");
            }
            assert_eq!(soc.resets, 1, "reset while held");
            let late = soc.periph.cycles - on - CPU_HZ / 1000;
            assert!(late <= SLICE as u64, "{late} cycles late");
            assert!(c1.is_none() && !soc.core_paused(0), "core 1 held again, core 0 running");
        }
    }

    #[test]
    fn pr_pwr_from_core_1_stops_it_there() {
        for jit in [false, true] {
            let (mut cpu, mut c1, mut soc, mut t) = two_cores(&CORE1_REBOOT, jit);
            for _ in 0..4 {
                run_dual(&mut c1, &mut soc, SLICE, 1 << 20, jit, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
            }
            assert_eq!(soc.resets, 1);
            assert!(c1.is_none(), "core 1 held again");
            assert_eq!(soc.read32(RAM_BASE + 0x10000), 0, "core 1 ran on after its reset (jit {jit})");
        }
    }

    /// `idle;  *(int *)0x01C10000 = 0x600d;  for (;;);`: core 1 sleeps, then stores a word.
    const CORE1_IDLE: [u8; 20] = [
        0x01, 0x00, 0xc0, 0xff, 0x00, 0x00, 0xc1, 0x01, 0x41, 0xe0, 0x0d, 0x60, 0x81, 0x60, 0xf7, 0x9f, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn core_1_sleeps_on_idle_until_an_interrupt_from_its_bank_is_pending() {
        for jit in [false, true] {
            let (mut cpu, mut c1, mut soc, mut t) = two_cores(&CORE1_IDLE, jit);
            let mut slices = |c1: &mut Option<Cpu>, soc: &mut Soc, n: usize| {
                for _ in 0..n {
                    run_dual(c1, soc, SLICE, 1 << 20, jit, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
                }
            };
            slices(&mut c1, &mut soc, 8);
            assert!(soc.cpu1_idle, "asleep after idle (jit {jit})");
            assert_eq!(soc.read32(RAM_BASE + 0x10000), 0, "ran on past idle (jit {jit})");
            // a software interrupt (ILAT bit 0, IRQ 120) enabled for cpu0's bank only: not its
            soc.write32(0x1eef13c, 0x3);
            soc.write32(0x1eef1a0, 1);
            slices(&mut c1, &mut soc, 8);
            assert!(soc.cpu1_idle, "woke on an interrupt of the other core's bank (jit {jit})");
            assert_eq!(soc.read32(RAM_BASE + 0x10000), 0);
            // enabled in its own bank (0x1EEF300): it wakes and goes on after the idle
            soc.write32(0x1eef33c, 0x3);
            slices(&mut c1, &mut soc, 8);
            assert!(!soc.cpu1_idle, "still asleep (jit {jit})");
            assert_eq!(soc.read32(RAM_BASE + 0x10000), 0x600d, "did not go on after the idle (jit {jit})");
        }
    }

    /// Core 1 on words no instruction matches (erased flash, as COSMOS 0.23's core 1 entry).
    const CORE1_ERASED: [u8; 8] = [0xff; 8];

    #[test]
    fn a_fault_on_core_1_halts_the_machine_unless_it_survives() {
        for jit in [false, true] {
            // the default: the machine halts there, as any fault
            let (mut cpu, mut soc) = machine(&[&[0xf7, 0x9f], CORE1_ERASED.as_slice()].concat(), &[], false);
            cpu.jit_on = jit;
            soc.take_boot(&cpu);
            soc.write32(0x01c7_fff8, XIP_BASE + 2);
            soc.write32(0x10008, 8);
            let (mut c1, mut t) = (None, false);
            let mut halted = false;
            for _ in 0..4 {
                if run_dual(&mut c1, &mut soc, SLICE, 1 << 20, jit, |s, k| run_blocks(&mut cpu, s, k, &mut t)).is_err() {
                    halted = true;
                    break;
                }
            }
            assert!(halted, "ran on past core 1's fault (jit {jit})");
            // opted in: core 1 stops alone, cpu0 goes on, the fault is kept until a reset
            let (mut cpu, mut c1, mut soc, mut t) = {
                let (mut cpu, mut soc) = machine(&[&[0xf7, 0x9f], CORE1_ERASED.as_slice()].concat(), &[], false);
                cpu.jit_on = jit;
                soc.cpu1_survives = true;
                soc.take_boot(&cpu);
                soc.write32(0x01c7_fff8, XIP_BASE + 2);
                soc.write32(0x10008, 8);
                (cpu, None, soc, false)
            };
            let before = cpu.insn_count;
            for _ in 0..4 {
                run_dual(&mut c1, &mut soc, SLICE, 1 << 20, jit, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
            }
            assert!(soc.cpu1_fault.is_some(), "the fault kept (jit {jit})");
            assert!(cpu.insn_count > before + 2 * SLICE as u64, "cpu0 went on (jit {jit})");
            // core 1 stays where it stopped
            let stopped = c1.as_ref().map(|c| (c.pc, c.insn_count));
            for _ in 0..4 {
                run_dual(&mut c1, &mut soc, SLICE, 1 << 20, jit, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
            }
            assert_eq!(c1.as_ref().map(|c| (c.pc, c.insn_count)), stopped, "core 1 ran on after its fault (jit {jit})");
        }
    }

    #[test]
    fn a_core_1_that_faults_holding_the_lock_releases_it() {
        // cpu0: lockset, then a word stored, then on the spot; core 1 holds the lock (as state) and
        // faults at once: cpu0 gets the lock and stores, where it would retry `lockset` for ever
        const CPU0: [u8; 14] = [0x41, 0x00, 0xc0, 0xff, 0x04, 0x00, 0xc1, 0x01, 0x41, 0xe0, 0x01, 0x00, 0x81, 0x60];
        for jit in [false, true] {
            let (mut cpu, mut soc) = machine(&[CPU0.as_slice(), &[0xf7, 0x9f], CORE1_ERASED.as_slice()].concat(), &[], false);
            cpu.jit_on = jit;
            soc.cpu1_survives = true;
            soc.take_boot(&cpu);
            soc.write32(0x01c7_fff8, XIP_BASE + 16);
            soc.write32(0x10008, 8);
            soc.lock_owner = Some(1);
            let (mut c1, mut t) = (None, false);
            for _ in 0..6 {
                run_dual(&mut c1, &mut soc, SLICE, 1 << 20, jit, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
            }
            assert!(soc.cpu1_fault.is_some(), "core 1 faulted (jit {jit})");
            assert_eq!(soc.read32(RAM_BASE + 0x10004), 1, "cpu0 never got the lock core 1 held (jit {jit})");
        }
    }

    // The two cores on a lock (hand-assembled: `lockset` 41 00, `lockclr` 40 00, `testset b[rN]`
    // bN 00, `ifeq goto` back to the instruction before 40 e8 fd ff, `rN = #i32` cN ff + 4 bytes,
    // `[rB] = rA` and `rN += 1` as `st` / `add1` spell them, `goto rN` dN 00, nop 00 00).
    const LOCKSET: [u8; 2] = [0x41, 0x00];
    const LOCKCLR: [u8; 2] = [0x40, 0x00];
    const NOP: [u8; 2] = [0x00, 0x00];
    const IFEQ_BACK: [u8; 4] = [0x40, 0xe8, 0xfd, 0xff];
    fn imm32(r: u8, v: u32) -> Vec<u8> {
        [vec![0xc0 | r, 0xff], v.to_le_bytes().to_vec()].concat()
    }
    fn st(base: u8, src: u8) -> [u8; 2] {
        (0x6080u16 | src as u16 | (base as u16) << 4).to_le_bytes()
    }
    fn add1(r: u8) -> [u8; 2] {
        (0x21c0u16 | r as u16).to_le_bytes()
    }
    fn goto_r(r: u8) -> [u8; 2] {
        [0xd0 | r, 0x00]
    }
    fn testset(r: u8) -> [u8; 2] {
        [0xb0 | r, 0x00]
    }
    /// The words the lock tests share: the byte lock (its low byte), cpu0's passes, cpu1's rounds.
    const WORD: u32 = RAM_BASE + 0x10000;
    const PASSES: u32 = RAM_BASE + 0x10004;
    const ROUNDS: u32 = RAM_BASE + 0x10008;
    fn word(soc: &Soc, a: u32) -> u32 {
        soc.ram.read32(a - RAM_BASE)
    }

    /// cpu0's code at XIP_BASE with its registers, cpu1's right after it (released before the
    /// first slice, its registers 0, `ram` words preset), the interpreter and the JIT in
    /// lockstep: after every slice the two machines agree on the clock, both cores' counts and
    /// pcs, and the three shared words; `check` sees the interpreter's after each slice.
    fn two_cores_in_lockstep(cpu0: &[u8], regs: &[(usize, u32)], cpu1: &[u8], ram: &[(u32, u32)], lock: bool, slices: usize, mut check: impl FnMut(usize, &Cpu, &Cpu, &Soc)) {
        let code = [cpu0, cpu1].concat();
        let mut ms = [false, true].map(|jit| {
            let (mut cpu, mut soc) = machine(&code, regs, false);
            cpu.jit_on = jit;
            for &(a, v) in ram {
                soc.write32(a, v);
            }
            if lock {
                soc.lock_owner = Some(1);
            }
            soc.take_boot(&cpu);
            soc.write32(0x01c7_fff8, XIP_BASE + cpu0.len() as u32);
            soc.write32(0x10008, 8);
            (cpu, None::<Cpu>, soc, false)
        });
        for s in 0..slices {
            for (jit, (cpu, c1, soc, t)) in ms.iter_mut().enumerate() {
                run_dual(c1, soc, SLICE, 1 << 20, jit == 1, |s, k| run_blocks(cpu, s, k, t)).unwrap();
                assert!(c1.is_some(), "core 1 started (jit {jit})");
            }
            let state = |(cpu, c1, soc, _): &(Cpu, Option<Cpu>, Soc, bool)| {
                let c1 = c1.as_ref().unwrap();
                (soc.periph.cycles, cpu.insn_count, cpu.pc, c1.insn_count, c1.pc, word(soc, WORD), word(soc, PASSES), word(soc, ROUNDS))
            };
            assert_eq!(state(&ms[0]), state(&ms[1]), "the interpreter and the JIT part after slice {s}");
            let (cpu, c1, soc, _) = &ms[0];
            check(s, cpu, c1.as_ref().unwrap(), soc);
        }
    }

    #[test]
    fn cpu0_gets_the_hardware_lock_core_1_holds_within_its_slice() {
        // cpu0: lockset, a pass counted, lockclr, round again. cpu1 starts inside its critical
        // section, holding the lock (`lock_owner`), and holds it 60 instructions of every 66:
        // without the handover cpu0's slice spun through its retries while cpu1 stood still, and
        // it passed only when cpu1 had stopped between lockclr and lockset
        let cpu0 = [&LOCKSET[..], &add1(5), &st(3, 5), &LOCKCLR, &NOP, &NOP, &goto_r(6)].concat();
        let e = XIP_BASE + cpu0.len() as u32;
        let (l, h) = (e + 20, e + 22);
        let cpu1 = [&imm32(6, l)[..], &imm32(7, h), &imm32(1, ROUNDS), &goto_r(7), &LOCKSET, &[NOP; 60].concat(), &LOCKCLR, &add1(5), &st(1, 5), &NOP, &NOP, &goto_r(6)].concat();
        assert_eq!(&cpu1[20..22], &LOCKSET);
        let regs = [(3, PASSES), (6, XIP_BASE)];
        let mut cycles = None;
        two_cores_in_lockstep(&cpu0, &regs, &cpu1, &[], true, 16, |s, cpu, c1, soc| {
            let c0 = *cycles.get_or_insert(soc.periph.cycles - SLICE as u64);
            assert_eq!(soc.periph.cycles - c0, SLICE as u64 * (s as u64 + 1), "the slice's cycles, slice {s}");
            assert!(word(soc, PASSES) >= s as u32 + 1, "cpu0 passed {} times in {} slices (at {:#x}, the lock {:?})", word(soc, PASSES), s + 1, cpu.pc, soc.lock_owner);
            assert!(word(soc, ROUNDS) >= s as u32, "cpu1 went {} rounds in {} slices (at {:#x})", word(soc, ROUNDS), s + 1, c1.pc);
        });
    }

    #[test]
    fn cpu0_gets_the_byte_lock_core_1_holds_within_its_slice() {
        // the same with the SDK's byte spinlock (`testset b[r]; ifeq goto` back), the word cleared
        // to release; cpu1 starts inside its section with the byte taken
        // (cpu0 keeps the byte 3 instructions of every 24: a cpu1 that finds it taken at the end
        // of cpu0's slice forfeits its run, so a cpu0 that held it most of the time would starve it)
        let cpu0 = [&testset(2)[..], &IFEQ_BACK, &add1(5), &st(3, 5), &st(1, 0), &[NOP; 16].concat(), &goto_r(6)].concat();
        let e = XIP_BASE + cpu0.len() as u32;
        let (l, h) = (e + 32, e + 38);
        let cpu1 = [&imm32(6, l)[..], &imm32(7, h), &imm32(2, WORD), &imm32(1, WORD), &imm32(4, ROUNDS), &goto_r(7), &testset(2), &IFEQ_BACK, &[NOP; 60].concat(), &st(1, 0), &add1(5), &st(4, 5), &NOP, &NOP, &goto_r(6)].concat();
        assert_eq!(&cpu1[32..34], &testset(2));
        let regs = [(1, WORD), (2, WORD), (3, PASSES), (6, XIP_BASE)];
        let mut cycles = None;
        two_cores_in_lockstep(&cpu0, &regs, &cpu1, &[(WORD, 0x80)], false, 16, |s, cpu, _, soc| {
            let c0 = *cycles.get_or_insert(soc.periph.cycles - SLICE as u64);
            assert_eq!(soc.periph.cycles - c0, SLICE as u64 * (s as u64 + 1), "the slice's cycles, slice {s}");
            assert!(word(soc, PASSES) >= s as u32 + 1, "cpu0 passed {} times in {} slices", word(soc, PASSES), s + 1);
            assert!(word(soc, ROUNDS) >= s as u32 / 4, "cpu1 went {} rounds in {} slices", word(soc, ROUNDS), s + 1);
            assert!(cpu.insn_count >= 4 * (s as u64 + 1));
        });
    }

    #[test]
    fn a_try_lock_taken_once_records_no_wait() {
        // both cores `testset` a byte that is always taken and go on (a try-lock, a claim-a-job
        // byte): no loop back to it, so no wait: cpu0 runs its whole slice and cpu1 its whole run
        let cpu0 = [&testset(2)[..], &add1(5), &st(3, 5), &goto_r(6)].concat();
        let e = XIP_BASE + cpu0.len() as u32;
        let cpu1 = [&imm32(6, e + 18)[..], &imm32(2, WORD), &imm32(4, ROUNDS), &testset(2), &add1(5), &st(4, 5), &goto_r(6)].concat();
        assert_eq!(&cpu1[18..20], &testset(2));
        let regs = [(2, WORD), (3, PASSES), (6, XIP_BASE)];
        let mut last = None;
        // (a taken byte is 0xff: testset leaves 0xff, an FM-1)
        two_cores_in_lockstep(&cpu0, &regs, &cpu1, &[(WORD, 0xff)], false, 8, |s, cpu, c1, soc| {
            if let Some((i0, i1)) = last {
                assert_eq!(cpu.insn_count - i0, SLICE as u64, "cpu0's instructions in slice {s}");
                assert!(c1.insn_count - i1 >= SLICE as u64 - 64, "cpu1 ran {} instructions in slice {s}", c1.insn_count - i1);
            }
            last = Some((cpu.insn_count, c1.insn_count));
            assert_eq!(word(soc, WORD) & 0xff, 0xff, "the byte stays taken");
        });
    }

    #[test]
    fn a_reset_mid_slice_still_runs_n() {
        // the watchdog runs out inside one of cpu0's slices beside core 1: run_dual still runs n
        // instructions of cpu0 (the slice's to the reset, then from its start)
        let (mut cpu, mut c1, mut soc, mut t) = two_cores(&[0xf7, 0x9f], false);
        p33(&mut soc, 0x00, 0x80, 0x40);
        p33(&mut soc, 0x20, 0x80, 0x10);
        let (before, n) = (cpu.insn_count, 2 * CPU_HZ as u32 / 1000 + 333);
        run_dual(&mut c1, &mut soc, n, 1 << 20, false, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
        assert_eq!(soc.resets, 1);
        assert_eq!(cpu.insn_count - before, n as u64);
    }

    #[test]
    fn a_reset_abandons_the_flash_command_under_way() {
        let (mut cpu, mut soc) = machine(&[0xf7, 0x9f], &[], false);
        let mut ticked = false;
        run_blocks(&mut cpu, &mut soc, 100, &mut ticked).unwrap();
        let spi = |soc: &mut Soc, b: u32| soc.flash.write(crate::spiflash::SPI0_BASE + 8, b);
        // write enable, then a page program to 0x1000 with two bytes sent, chip select still low
        soc.flash.set_cs(true);
        spi(&mut soc, 0x06);
        soc.flash.set_cs(false);
        soc.flash.set_cs(true);
        for b in [0x02, 0x00, 0x10, 0x00, 0x12, 0x34] {
            spi(&mut soc, b);
        }
        p33(&mut soc, 0x20, 0xa0, 0x10);
        run_blocks(&mut cpu, &mut soc, 100, &mut ticked).unwrap();
        assert_eq!(soc.resets, 1);
        // the firmware's first command after the reset: chip select low, then high
        soc.flash.set_cs(true);
        soc.flash.set_cs(false);
        assert_eq!(soc.flash.mem[0x1000..0x1002], [0xff, 0xff], "the old page program's bytes programmed");
    }

    #[test]
    fn no_reset_for_a_cpu0_that_ran_before_the_runner() {
        // the boot state is cpu0's before its first instruction: without it a reset would be half
        // one (the peripherals', cpu0 running on): none is done
        let (mut cpu, mut soc) = machine(&[0xf7, 0x9f], &[], false);
        cpu.step(&mut soc).unwrap();
        let mut ticked = false;
        cpu.regs[4] = 7;
        p33(&mut soc, 0x20, 0xa0, 0x10);
        run_blocks(&mut cpu, &mut soc, 1000, &mut ticked).unwrap();
        assert!(soc.resets == 0 && !soc.reset_due() && cpu.regs[4] == 7, "a reset");
        assert_eq!(run_blocks(&mut cpu, &mut soc, 1000, &mut ticked), Ok(1000), "cpu0 runs on");
    }

    #[test]
    fn probe_without_a_loop_changes_nothing() {
        // a store (the P33 port, RAM), then the counting timer word: the probe undoes the store
        let code = [STORE.as_slice(), &LOAD, &[0x60, 0xff, 0x00, 0x20, 0xf9, 0xff]].concat();
        for to in [crate::p33::P33::CON, RAM_BASE + 0x1000] {
            let (mut cpu, mut soc) = machine(&code, &[(1, 0x1080c + 544), (5, to + 128), (3, 0x11)], true);
            let before = (cpu.plain_state(), cpu.insn_count, soc.periph.cycles, soc.p33.snap(), soc.ram.read32(0x1000));
            assert_eq!(probe_loop(&mut cpu, &mut soc, 5000), 0);
            assert_eq!(before, (cpu.plain_state(), cpu.insn_count, soc.periph.cycles, soc.p33.snap(), soc.ram.read32(0x1000)));
            assert_eq!((soc.loop_skip.probes, soc.loop_skip.hits, soc.ticks), (1, 0, 0));
        }
    }

    #[test]
    fn loop_with_side_effects_not_skipped() {
        // a timer word that counts its reads (`Soc::ticks`), a port pin written, a counter in RAM
        let load = |at: u32| [(1, at + 544)];
        let (a, soc) = run_both(&[LOAD.as_slice(), &[0x60, 0xff, 0x00, 0x20, 0xfb, 0xff]].concat(), &load(0x1080c), &[], 50_000);
        assert!(soc.loop_skip.probes > 0);
        assert_eq!((soc.loop_skip.hits, soc.ticks), (0, 25_000));
        assert_eq!(a.pc, XIP_BASE);
        let code = [STORE.as_slice(), &LOAD, &while_set(8)].concat();
        let (_, soc) = run_both(&code, &[(1, PA_IN + 544), (5, 0x50000 + 128), (3, 0)], &[], 50_000);
        assert!(soc.loop_skip.probes > 0);
        assert_eq!(soc.loop_skip.hits, 0);
        // a P33 transfer, then the counting timer word: the probe's transfer is undone
        let code = [STORE.as_slice(), &LOAD, &[0x60, 0xff, 0x00, 0x20, 0xf9, 0xff]].concat();
        let (_, soc) = run_both(&code, &[(1, 0x1080c + 544), (5, crate::p33::P33::CON + 128), (3, 0x11)], &[], 50_000);
        assert!(soc.loop_skip.probes > 0);
        assert_eq!(soc.loop_skip.hits, 0);
        // r3 = [r1 += 4]-style changes show in the core: `13 05` r3 = [r1++=4] polls on and on
        let code = [[0x13, 0x05].as_slice(), &[0x60, 0xff, 0x00, 0x20, 0xf9, 0xff]].concat();
        let (_, soc) = run_both(&code, &[(1, RAM_BASE)], &[], 50_000);
        assert_eq!(soc.loop_skip.hits, 0);
    }

    /// RAM code: `13 05` r3 = [r1++=4], eight times, then `80 00` rts; the words it loads are 0.
    fn soc_with_loads() -> Soc {
        let mut soc = Soc::new(1 << 20, vec![0; 16]);
        for i in 0..8 {
            soc.write16(RAM_BASE + 2 * i, 0x0513);
        }
        soc.write16(RAM_BASE + 16, 0x0080);
        soc
    }

    fn core_at_ram(id: u32) -> Cpu {
        let mut c = Cpu::new(RAM_BASE);
        c.core_id = id;
        c.regs[1] = RAM_BASE + 0x100;
        c
    }

    /// FM1_STRICT: `n` instructions of `r0 += 1` from XIP (cold: a flash cache miss every 32 of
    /// them), then `goto -2`; the clock's ticks until the core running them is at the `goto`, on
    /// core 0 alone, or on core 1 with core 0 spinning
    fn strict_straight_line(n: usize, on_core1: bool) -> u64 {
        let line: Vec<u8> = std::iter::repeat_n([0xc0, 0x21], n).flatten().chain([0xf7, 0x9f]).collect();
        let code = if on_core1 { [&[0xf7, 0x9f][..], &line].concat() } else { line };
        let end = XIP_BASE + code.len() as u32 - 2;
        let (mut cpu, mut soc) = machine(&code, &[], false);
        cpu.cost_model = true;
        soc.periph.cycle_ticks = 8;
        soc.xip_cache = Some(crate::XipCache::default());
        let mut t = false;
        if !on_core1 {
            let c0 = soc.periph.cycles;
            while cpu.pc != end {
                run_blocks(&mut cpu, &mut soc, 1, &mut t).unwrap();
            }
            return soc.periph.cycles - c0;
        }
        soc.take_boot(&cpu);
        soc.write32(0x01c7_fff8, XIP_BASE + 2);
        soc.write32(0x10008, 8);
        let mut c1 = None;
        run_dual(&mut c1, &mut soc, 1, 1 << 20, false, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
        let c0 = soc.periph.cycles;
        while c1.as_ref().is_none_or(|c| c.pc != end) {
            run_dual(&mut c1, &mut soc, 1, 1 << 20, false, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
        }
        soc.periph.cycles - c0
    }

    #[test]
    fn strict_costs_core_1_as_core_0() {
        // the same code costs core 1 what it costs core 0, its cache misses included (core 1's
        // budget ran out mid-instruction and dropped the rest: a miss counted as what was left)
        let (c0, c1) = (strict_straight_line(15_000, false), strict_straight_line(15_000, true));
        let d = c1.abs_diff(c0) as f64 / c0 as f64;
        assert!(d < 0.02, "core 0 {c0} ticks, core 1 {c1} ({:+.1} %)", 100.0 * (c1 as f64 / c0 as f64 - 1.0));
    }

    #[test]
    fn release_forfeits_the_rest_of_its_4096() {
        // cpu0 releases cpu1 (0x10008 |= 8) with its `at`th instruction, then spins; cpu1 spins
        for at in [1251u32, 4096, 4097] {
            let mut code: Vec<u8> = std::iter::repeat_n(LOAD, at as usize - 1).flatten().collect();
            code.extend_from_slice(&STORE);
            let spin = code.len() as u32;
            code.extend_from_slice(&[0x60, 0xff, 0x00, 0x20, 0xfd, 0xff]);
            let (mut cpu, mut soc) = machine(&code, &[(1, RAM_BASE + 544), (5, 0x10008 + 128), (3, 8)], true);
            soc.write32(0x01c7_fff8, XIP_BASE + spin);
            let mut cpu1 = None;
            let mut pt = false;
            run_dual(&mut cpu1, &mut soc, 20_000, 1 << 20, false, |soc, k| run_blocks(&mut cpu, soc, k, &mut pt)).unwrap();
            assert!(cpu1.as_ref().is_some_and(|c| c.pc == XIP_BASE + spin));
            // cpu0 alone ran up to the release, then nothing up to the next multiple of 4096
            assert_eq!(cpu.insn_count, (at + 20_000 - at.div_ceil(4096) * 4096) as u64, "release at {at}");
        }
    }

    #[test]
    fn flaky_core1_misreads_loads_not_fetches_nor_core0() {
        let mut soc = soc_with_loads();
        soc.flaky_core1 = Some(FlakyCore1::new(1.0, 1));
        // core 1, stepped against the SoC (fm1-emu): every load misreads, the code still runs
        let mut c1 = core_at_ram(1);
        for _ in 0..8 {
            c1.step(&mut Core1Loads(&mut soc)).unwrap();
            assert_eq!(c1.regs[3], MISREAD_BIT);
        }
        assert_eq!(c1.pc, RAM_BASE + 16);
        // core 1 in blocks (fm1-live, the browser), the JIT asked for: the same
        let mut c1 = core_at_ram(1);
        c1.jit_on = true;
        let mut bus = Core1Bus { soc: &mut soc, budget: 1 << 20, until: None, spent: 0 };
        assert!(bus.jit_mem().is_none(), "a flaky core 1 runs interpreted");
        let mut n = 0;
        while c1.pc != RAM_BASE + 16 && n < 100 {
            n += match c1.run_fast(&mut bus, 8).unwrap() {
                (0, _) => {
                    c1.step(&mut bus).unwrap();
                    1
                }
                (k, _) => k,
            };
            assert_eq!(c1.regs[3], MISREAD_BIT);
        }
        assert_eq!(c1.regs[1], RAM_BASE + 0x120);
        assert_eq!(soc.flaky_core1.as_ref().unwrap().misreads, 16);
        // core 0 reads what is there
        let mut c0 = core_at_ram(0);
        for _ in 0..8 {
            c0.step(&mut soc).unwrap();
            assert_eq!(c0.regs[3], 0);
        }
        assert_eq!(soc.flaky_core1.as_ref().unwrap().loads, 16);
    }

    #[test]
    fn flaky_core1_only_in_ram() {
        let mut soc = Soc::new(1 << 20, 0x1234_5678u32.to_le_bytes().to_vec());
        soc.flaky_core1 = Some(FlakyCore1::new(1.0, 1));
        let mut bus = Core1Loads(&mut soc);
        assert_eq!(bus.read32(crate::XIP_BASE), 0x1234_5678, "flash is read right");
        assert_eq!(bus.read32(RAM_BASE), MISREAD_BIT);
        assert_eq!(bus.fetch32(RAM_BASE), 0, "an instruction fetch is right");
        assert_eq!(soc.flaky_core1.as_ref().unwrap().loads, 1);
    }

    #[test]
    fn core_1_polling_timer4_ends_its_run() {
        // the count stands still within core 1's run: a deadline loop on it gets nowhere before
        // the next run, and would spin through the whole budget (Frodhes' c1_pause)
        let mut soc = Soc::new(1 << 20, vec![0; 0x1000]);
        soc.periph.skip_idle = true;
        soc.write32(0x10800, 1);
        let c = soc.periph.cycles;
        let mut bus = Core1Bus { soc: &mut soc, budget: 1 << 20, until: None, spent: 0 };
        bus.read32(0x10804);
        bus.read32(0x10804);
        assert!(bus.budget > 1, "two reads are not a poll");
        bus.read32(0x10804);
        assert!(bus.budget <= 1, "the run goes on polling");
        assert_eq!(bus.soc.periph.cycles, c);
        // the next run (the clock moved on, `run_cpu1` took the poll) starts counting again
        assert!(std::mem::take(&mut bus.soc.periph.core1_polls));
        bus.soc.periph.cycles += 1000;
        bus.budget = 1 << 20;
        bus.read32(0x10804);
        assert!(bus.budget > 1);
    }

    #[test]
    fn good_core1_reads_what_is_there() {
        let mut soc = soc_with_loads();
        let mut c1 = core_at_ram(1);
        soc.write32(RAM_BASE + 0x100, 0xdead_beef);
        c1.step(&mut Core1Loads(&mut soc)).unwrap();
        assert_eq!(c1.regs[3], 0xdead_beef);
        assert!(Core1Bus { soc: &mut soc, budget: 1, until: None, spent: 0 }.jit_mem().is_some());
    }

    /// `run_both` with the JIT on or off and a setup for both, in uneven chunks; after every chunk
    /// equal in more: the core's whole state, the ADC, the pending latches, the timer word's reads,
    /// the first 16 KiB of RAM.
    fn run_both_jit(code: &[u8], regs: &[(usize, u32)], jit: bool, setup: &dyn Fn(&mut Soc), total: u32) -> (Cpu, Soc) {
        let (mut a, mut sa) = machine(code, regs, true);
        let (mut b, mut sb) = machine(code, regs, false);
        a.jit_on = jit;
        b.jit_on = jit;
        setup(&mut sa);
        setup(&mut sb);
        let (mut ta, mut tb) = (false, false);
        let mut done = 0u32;
        let mut x = 12345u32;
        while done < total {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            let n = (1024 + (x >> 8) % 3000).min(total - done);
            assert_eq!(run_blocks(&mut a, &mut sa, n, &mut ta), Ok(n));
            assert_eq!(run_blocks(&mut b, &mut sb, n, &mut tb), Ok(n));
            done += n;
            assert_eq!((a.pc, a.regs, a.insn_count, ta, a.plain_state()), (b.pc, b.regs, b.insn_count, tb, b.plain_state()), "after {done}");
            assert_eq!(sa.periph.cycles, sb.periph.cycles, "after {done}");
            assert_eq!(sa.periph.adc_state(), sb.periph.adc_state(), "after {done}");
            assert_eq!(sa.p33.snap(), sb.p33.snap(), "after {done}");
            assert_eq!(sa.periph.any_pending(), sb.periph.any_pending(), "after {done}");
            assert_eq!(sa.ticks, sb.ticks, "after {done}");
            for w in (0..0x4000u32).step_by(4) {
                assert_eq!(sa.ram.read32(w), sb.ram.read32(w), "ram {w:#x} after {done}");
            }
        }
        assert_eq!(sb.loop_skip.probes, 0);
        (a, sa)
    }

    #[test]
    fn timer5_pending_poll_skipped_to_the_cycle() {
        // r2 = [0x10900] until bit 15 (T5 pending, ~29 k cycles a period), then ack ([0x10900] =
        // 0x4001), 200 loads and round again: a skip one round long or short shows as another pc
        let mut code = [LOAD.as_slice(), &[0x60, 0xff, 0x00, 0x2c, 0xfb, 0xff], &STORE].concat();
        for _ in 0..200 {
            code.extend_from_slice(&LOAD);
        }
        let o = (-(code.len() as i32 + 6) / 2) as u16;
        code.extend_from_slice(&[0x60, 0xff, 0x00, 0x20, o as u8, (o >> 8) as u8]);
        let regs = [(1, 0x10900 + 544), (5, 0x10900 + 128), (3, 0x4001)];
        let setup = |s: &mut Soc| {
            s.write32(0x10908, 731);
            s.write32(0x10900, 1);
        };
        for jit in [false, true] {
            let (_, soc) = run_both_jit(&code, &regs, jit, &setup, 3_000_000);
            assert!(soc.loop_skip.skipped > 1_000_000, "jit {jit}: {} skipped", soc.loop_skip.skipped);
        }
    }

    #[test]
    fn adc_poll_with_the_pot_moving_skipped_to_the_cycle() {
        // [0x13100] = 0x50 (a conversion, no IRQ), r2 = [0x13104], round again; the pot moves at
        // uneven times, so a skip past a move shows in r2
        let code = [STORE.as_slice(), &LOAD, &[0x60, 0xff, 0x00, 0x20, 0xf9, 0xff]].concat();
        let regs = [(1, 0x13104 + 544), (5, 0x13100 + 128), (3, 0x50)];
        let setup = |s: &mut Soc| {
            for i in 0..30u64 {
                s.periph.script.push_back((10_007 + i * 33_331 + i * i * 3, Action::Adc { ch: 0, value: (i * 37 % 1024) as u16 }));
            }
        };
        for jit in [false, true] {
            let (_, soc) = run_both_jit(&code, &regs, jit, &setup, 1_200_000);
            assert!(soc.loop_skip.skipped > 500_000, "jit {jit}: {} skipped", soc.loop_skip.skipped);
        }
    }

    #[test]
    fn adc_poll_of_a_pulled_up_row_converts_every_round() {
        // [0x13100] = 0x50 (ch 0 = row PA7, pulled up: every conversion moves the noise
        // generator), r2 = [0x13104], round again: no round repeats, and skip or not, the
        // generator ends where it does without the skip
        let code = [STORE.as_slice(), &LOAD, &[0x60, 0xff, 0x00, 0x20, 0xf9, 0xff]].concat();
        let regs = [(1, 0x13104 + 544), (5, 0x13100 + 128), (3, 0x50)];
        let setup = |s: &mut Soc| {
            s.write32(0x50008, 1 << 7);
            s.write32(0x50010, 1 << 7);
        };
        for jit in [false, true] {
            let (_, soc) = run_both_jit(&code, &regs, jit, &setup, 300_000);
            assert_eq!(soc.loop_skip.skipped, 0, "jit {jit}");
        }
    }

    #[test]
    fn failed_probe_undoes_the_adc_start() {
        // [0x13100] = 0x50, then the counting timer word (refused): the conversion is undone,
        // and the next event is where it was
        let code = [STORE.as_slice(), &LOAD, &[0x60, 0xff, 0x00, 0x20, 0xf9, 0xff]].concat();
        let (mut cpu, mut soc) = machine(&code, &[(1, 0x1080c + 544), (5, 0x13100 + 128), (3, 0x50)], true);
        soc.periph.next_due = soc.periph.cycles + 100_000;
        let before = (soc.periph.adc_state(), soc.periph.next_due, soc.periph.cycles, cpu.plain_state());
        assert_eq!(probe_loop(&mut cpu, &mut soc, 5000), 0);
        assert_eq!(before, (soc.periph.adc_state(), soc.periph.next_due, soc.periph.cycles, cpu.plain_state()));
    }

    #[test]
    fn cpu0_polling_what_cpu1_stores_skipped_to_the_cycle() {
        // cpu1: 3000 loads, then [w] = 0x20, then spins. cpu0: polls w while bit 5 is clear, then
        // 200 loads, then spins. With the skip and without: the same after every run_dual.
        const W: u32 = RAM_BASE + 0x2000;
        let mut code = [LOAD.as_slice(), &[0x60, 0xff, 0x20, 0x20, 0xfb, 0xff]].concat();
        for _ in 0..200 {
            code.extend_from_slice(&LOAD);
        }
        code.extend_from_slice(&[0x60, 0xff, 0x00, 0x20, 0xfd, 0xff]);
        let c1_at = code.len() as u32;
        for _ in 0..3000 {
            code.extend_from_slice(&LOAD);
        }
        code.extend_from_slice(&STORE);
        code.extend_from_slice(&[0x60, 0xff, 0x00, 0x20, 0xfd, 0xff]);
        let both = |skip: bool| {
            let (mut cpu, soc) = machine(&code, &[(1, W + 544)], skip);
            cpu.jit_on = true;
            let mut c1 = Cpu::new(XIP_BASE + c1_at);
            c1.core_id = 1;
            c1.jit_on = true;
            c1.regs[1] = RAM_BASE + 544;
            c1.regs[5] = W + 128;
            c1.regs[3] = 0x20;
            (cpu, soc, Some(c1))
        };
        let (mut a, mut sa, mut a1) = both(true);
        let (mut b, mut sb, mut b1) = both(false);
        let (mut ta, mut tb) = (false, false);
        for i in 0..60u32 {
            let n = 3000 + (i * 977) % 5000;
            run_dual(&mut a1, &mut sa, n, 1 << 20, true, |s, k| run_blocks(&mut a, s, k, &mut ta)).unwrap();
            run_dual(&mut b1, &mut sb, n, 1 << 20, true, |s, k| run_blocks(&mut b, s, k, &mut tb)).unwrap();
            assert_eq!((a.pc, a.regs, a.insn_count), (b.pc, b.regs, b.insn_count), "call {i}");
            let (x, y) = (a1.as_ref().unwrap(), b1.as_ref().unwrap());
            assert_eq!((x.pc, x.regs, x.insn_count), (y.pc, y.regs, y.insn_count), "cpu1, call {i}");
            assert_eq!(sa.periph.cycles, sb.periph.cycles, "call {i}");
        }
        assert!(sa.loop_skip.hits > 0);
    }

    #[test]
    fn pure_reads_change_nothing_and_hold_still() {
        // every address `pure_read` takes, on both boards with every timer, the OS tick and the ADC
        // running: the same word 777 cycles later (no event between), and no counter, random
        // generator or unknown-access log moved. A new peripheral word that counts or clears on
        // read and is taken by mistake fails here.
        for board in [crate::Board::Fm1, crate::Board::Smk37] {
            let mut soc = Soc::new(1 << 20, vec![0; 0x1000]);
            soc.set_board(board);
            soc.write32(0x10800, 1);
            soc.write32(0x10804, 0);
            soc.write32(0x10908, 60_000);
            soc.write32(0x10900, 1);
            soc.write32(0x1eef0f4, 2_400_000);
            soc.write32(0x1eef0ec, 1);
            for i in 0..4u32 {
                soc.write32(0x10408 + 0x100 * i, 50_000);
                soc.write32(0x10400 + 0x100 * i, 1);
            }
            soc.write32(0x12e20, 256);
            soc.write32(0x12e00, 0x800);
            soc.write32(0x13100, 0x50);
            soc.advance(0);
            soc.periph.cycles += 1000;
            let mut bad = Vec::new();
            for (lo, hi) in [(0x10000u32, 0x60000u32), (0x1eee000, 0x1eee300), (0x1eef000, 0x1eef400)] {
                for at in (lo..hi).step_by(4) {
                    if !soc.pure_read(at) {
                        continue;
                    }
                    let before = (soc.ticks, soc.unknown_log.len(), soc.rand);
                    let v1 = soc.read32(at);
                    soc.periph.cycles += 777;
                    let v2 = soc.read32(at);
                    soc.periph.cycles -= 777;
                    if v1 != v2 || before != (soc.ticks, soc.unknown_log.len(), soc.rand) {
                        bad.push((at, v1, v2));
                    }
                }
            }
            assert!(bad.is_empty(), "{board:?}: {bad:x?}");
        }
    }

    #[test]
    fn a_second_release_with_core_1_running_latches_nothing() {
        // 0x10008 bit 3 set (core 1 starts), cleared and set again: no second release is latched.
        // (Nothing would take it with core 1 running, and run_blocks would break after every
        // run_fast return: cpu0 one instruction a slice, its time nearly stopped.)
        let (mut cpu, mut soc) = machine(&[0x60, 0xff, 0x00, 0x20, 0xfd, 0xff], &[], true);
        soc.write32(0x01C7_FFF8, XIP_BASE);
        soc.write32(0x10008, 8);
        let (mut c1, mut t) = (None, false);
        run_dual(&mut c1, &mut soc, 8192, 1 << 20, false, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
        assert!(c1.is_some() && !soc.cpu1_release, "core 1 started, the release taken");
        soc.write32(0x10008, 0);
        soc.write32(0x10008, 8);
        assert!(!soc.cpu1_release, "a second release latched");
        let before = cpu.insn_count;
        for _ in 0..4 {
            run_dual(&mut c1, &mut soc, 8192, 1 << 20, false, |s, k| run_blocks(&mut cpu, s, k, &mut t)).unwrap();
        }
        assert!(cpu.insn_count - before >= 4 * 8000, "cpu0 ran {} in 4 calls of 8192", cpu.insn_count - before);
    }

    #[test]
    fn latched_release_in_the_dual_phase_stops_cpu0_where_it_would_without_the_skip() {
        // cpu1 already runs and the firmware sets 0x10008 bit 3 again (0 -> 1): `cpu1_release`
        // is latched and nothing takes it, so run_blocks breaks after every run_fast return.
        // cpu0 polls a RAM word from RAM code a step at a time: a probe must not run on past
        // such a break. (A latch only the first release makes now: `a_second_release_...`.)
        let both = |skip: bool| {
            let (_, mut soc) = machine(&[0x60, 0xff, 0x00, 0x20, 0xfd, 0xff], &[], skip);
            soc.ram.load(0x8000, &[LOAD.as_slice(), &[0x60, 0xff, 0x00, 0x20, 0xf9, 0xff]].concat());
            let mut cpu = Cpu::new(RAM_BASE + 0x8000);
            cpu.no_ram_blocks = true;
            cpu.regs[1] = RAM_BASE + 0x100 + 544;
            let mut c1 = Cpu::new(XIP_BASE);
            c1.core_id = 1;
            soc.write32(0x10008, 0);
            soc.write32(0x10008, 8);
            assert!(soc.cpu1_release);
            (cpu, soc, Some(c1))
        };
        let (mut a, mut sa, mut a1) = both(true);
        let (mut b, mut sb, mut b1) = both(false);
        let (mut ta, mut tb) = (false, false);
        for i in 0..20 {
            run_dual(&mut a1, &mut sa, 8192, 1 << 20, false, |s, k| run_blocks(&mut a, s, k, &mut ta)).unwrap();
            run_dual(&mut b1, &mut sb, 8192, 1 << 20, false, |s, k| run_blocks(&mut b, s, k, &mut tb)).unwrap();
            assert_eq!((a.pc, a.insn_count, sa.periph.cycles), (b.pc, b.insn_count, sb.periph.cycles), "call {i}");
        }
    }

    #[test]
    fn a_chip_reset_clears_the_hw_units() {
        // the FFT / vector unit, the EQ and the sample-rate converter with a job done and their
        // registers set: after a reset they read as at power-on (JL_FFT's CON 0, not the 0x10100
        // it reads between jobs), the EQ's RAM as the firmware left it, and their jobs still count
        let code = [0x20u8, 0x00].repeat(32);
        let (mut cpu, mut soc) = machine(&code, &[], false);
        let mut t = false;
        run_blocks(&mut cpu, &mut soc, 1, &mut t).unwrap(); // (the boot state taken)
        let d = RAM_BASE + 0x100;
        soc.write32(d, (64 << 16) | 2); // a 64-point real FFT
        soc.write32(d + 4, RAM_BASE + 0x1000);
        soc.write32(d + 8, RAM_BASE + 0x2000);
        soc.write32(0x1ee0404, d);
        soc.write32(0x1ee0408, 0x1234);
        soc.write32(0x1ee0400, (1 << 8) | (1 << 6) | 1);
        soc.write32(crate::hweq::RAM + 4, 0x3f80_0000);
        soc.write32(crate::hweq::BASE + 0x10, RAM_BASE + 0x3000);
        soc.write32(crate::hweq::BASE + 0x14, RAM_BASE + 0x3800);
        soc.write32(crate::hweq::BASE, 1);
        soc.write32(crate::hwsrc::BASE + 4, 48_000 << 16 | 44_100);
        soc.write32(crate::hwsrc::BASE + 0x20, RAM_BASE + 0x4000);
        soc.write32(crate::hwsrc::BASE, 8);
        soc.periph.cycles += 1_000_000;
        assert_eq!(soc.read32(0x1ee0400), 0x1_0180, "JL_FFT: a job done");
        assert_eq!(soc.read32(crate::hweq::BASE), 0x8000_0000, "JL_EQ: a run done");
        assert_eq!(soc.read32(crate::hwsrc::BASE), 0x80, "JL_SRC: a run done");
        soc.reset_req = Some(crate::p33::P33::RST_SOFT);
        run_blocks(&mut cpu, &mut soc, 8, &mut t).unwrap();
        assert_eq!(soc.resets, 1);
        assert_eq!((soc.read32(0x1ee0400), soc.read32(0x1ee0404), soc.read32(0x1ee0408)), (0, 0, 0));
        assert_eq!((soc.read32(crate::hweq::BASE), soc.read32(crate::hweq::BASE + 0x10), soc.read32(crate::hweq::BASE + 0x14)), (0, 0, 0));
        assert_eq!((soc.read32(crate::hwsrc::BASE), soc.read32(crate::hwsrc::BASE + 4), soc.read32(crate::hwsrc::BASE + 0x20)), (0, 0, 0));
        assert_eq!(soc.read32(crate::hweq::RAM + 4), 0x3f80_0000, "the EQ's RAM stays, as RAM does");
        assert_eq!((soc.hwdsp.jobs, soc.hweq.runs, soc.hwsrc.runs), (1, 1, 1));
    }

    #[test]
    fn core_1s_time_past_a_runs_cap_is_counted() {
        // core 1 busy (`r5 += 1; goto r6` back to it: never where it started): a run it is due more
        // than 2^20 instructions of runs 2^20, and the rest is counted as dropped
        let code = [&add1(5)[..], &goto_r(6)].concat();
        let busy = |soc: &mut Soc, cycles: u64| {
            let mut c1 = Cpu::new(XIP_BASE);
            c1.core_id = 1;
            c1.regs[6] = XIP_BASE;
            let ran = run_cpu1(&mut c1, soc, cycles, None).unwrap();
            assert_eq!(c1.regs[5] as u64, c1.insn_count.div_ceil(2), "it ran them");
            ran
        };
        let (_, mut soc) = machine(&code, &[], false);
        assert_eq!((busy(&mut soc, 1 << 20), soc.core1_dropped), (1 << 20, 0), "all of a run of 2^20");
        assert_eq!((busy(&mut soc, (1 << 20) + 5000), soc.core1_dropped), (1 << 20, 5000));
        assert_eq!((busy(&mut soc, 3 << 20), soc.core1_dropped), (1 << 20, 5000 + (2 << 20)), "it adds up");
        // a core 1 on the spot (`goto -2`) ends its run where it started: it gave up the rest
        // itself, nothing was cut
        let (_, mut soc) = machine(&[0xf7, 0x9f], &[], false);
        let mut c1 = Cpu::new(XIP_BASE);
        c1.core_id = 1;
        assert!(run_cpu1(&mut c1, &mut soc, 3 << 20, None).unwrap() < 1024);
        assert_eq!(soc.core1_dropped, 0);
    }

    #[test]
    fn a_probe_leaves_a_round_outside_the_pc_limits_to_the_block_loop() {
        // a round that leaves the PC limits (XIP alone here) for code in RAM and comes back: the
        // block loop sets the guard's cause where the block out there is entered. A probe steps,
        // which checks no limits: it must not run such a round, let alone skip its repeats.
        let code = [LOAD.as_slice(), &goto_r(6)].concat();
        let both = |skip: bool| {
            let (cpu, mut soc) = machine(&code, &[(1, RAM_BASE + 0x1000 + 544), (6, RAM_BASE + 0x8000), (7, XIP_BASE)], skip);
            soc.ram.load(0x8000, &[&NOP[..], &goto_r(7)].concat());
            pc_limits(&mut soc, XIP_BASE, XIP_BASE + 0xfff);
            (cpu, soc, false)
        };
        let (mut a, mut b) = (both(true), both(false));
        for i in 0..8 {
            let ran = [&mut a, &mut b].map(|(cpu, soc, t)| run_blocks(cpu, soc, 8192, t).unwrap());
            assert_eq!(ran[0], ran[1], "call {i}");
            assert_eq!((a.0.pc, a.0.insn_count, a.1.periph.cycles, a.1.dbg_cause), (b.0.pc, b.0.insn_count, b.1.periph.cycles, b.1.dbg_cause), "call {i}");
        }
        assert_eq!(b.1.dbg_cause, 1 << 12, "the round does leave the limits");
        assert!(a.1.loop_skip.probes > 0 && a.1.loop_skip.hits == 0, "probed, and left alone");
    }

    #[test]
    fn no_probe_on_the_strict_clock() {
        // a loop a probe finds; with the flash cache, the cost model or strict alignment on
        // (FM1_STRICT's parts) none is made, whether or not the runner switched the skip off
        let code = [LOAD.as_slice(), &[0x60, 0xff, 0x00, 0x20, 0xfb, 0xff]].concat();
        let probes = |set: fn(&mut Cpu, &mut Soc)| {
            let (mut cpu, mut soc) = machine(&code, &[(1, RAM_BASE + 0x1000 + 544)], true);
            set(&mut cpu, &mut soc);
            let mut t = false;
            run_blocks(&mut cpu, &mut soc, 100_000, &mut t).unwrap();
            (soc.loop_skip.probes, soc.loop_skip.hits)
        };
        assert!(probes(|_, _| {}).1 > 0, "found and skipped on the plain clock");
        assert_eq!(probes(|_, s| s.xip_cache = Some(crate::XipCache::default())), (0, 0));
        assert_eq!(probes(|c, _| c.cost_model = true), (0, 0));
        assert_eq!(probes(|_, s| s.strict_align = true), (0, 0));
    }
}

#[cfg(test)]
mod machine_tests {
    use super::*;
    use crate::periph::Action;
    use crate::{Board, XIP_BASE, XIP_FLASH_OFFSET};

    /// A package booted as the front ends each had it written out before `Machine` (fm1-live's
    /// lines; the page's and the plugin's were these without the strict clock and `sfc_boot`): the
    /// oracle for `Machine`'s boot.
    fn by_hand(pkg: &[u8], jit: bool, strict: bool, sfc: bool) -> (Cpu, Soc) {
        let st = stock_from_package(pkg, None).unwrap();
        let mut soc = Soc::new(1 << 20, st.xip.clone());
        soc.set_flash_part(FlashPart::P25q80);
        soc.periph.cycle_ticks = if strict { 8 } else { 1 };
        soc.periph.hz = 240 * 1_000_000 * soc.periph.cycle_ticks;
        soc.strict_align = strict;
        if strict {
            soc.loop_skip.enabled = false;
            soc.xip_cache = Some(XipCache::default());
        }
        soc.periph.skip_idle = true;
        let mut cpu = Cpu::new(st.entry);
        cpu.jit_on = jit && !strict;
        cpu.cost_model = strict;
        cpu.user = true;
        let params = RAM_BASE + 0x7fc00;
        soc.write32(params, params + 0x40);
        cpu.regs[0] = stock_boot(&mut soc, &st.flash, params, st.board);
        if sfc {
            soc.sfc_boot(st.key, st.app_end);
        }
        (cpu, soc)
    }

    /// FM-1 SID's package (in the tree as a submodule: skipped where that is not checked out, but
    /// not in CI, which checks it out and must not pass on nothing), booted and run through
    /// `Machine` and by hand, with the JIT and interpreted, with the chip key and without, and on
    /// the strict clock: the same machine after the same chunks, to the last byte of RAM and audio.
    #[test]
    fn a_package_boots_and_runs_as_the_recipe_written_out() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fm1-sid/web/sidplayer.fwsc");
        let pkg = match std::fs::read(path) {
            Ok(p) => p,
            Err(_) if std::env::var_os("CI").is_none() => return eprintln!("no fm1-sid/web/sidplayer.fwsc: skipped"),
            Err(e) => panic!("{path}: {e}"),
        };
        let flash = stock_from_package(&pkg, None).unwrap().flash;
        // (the JIT, the strict clock, the chip key, emulated ms)
        let runs = [(true, false, true, 600), (true, false, false, 600), (false, false, true, 600), (false, false, false, 600)];
        for (jit, strict, sfc, ms) in runs.into_iter().chain([(false, true, true, 60)]) {
            let what = format!("jit {jit}, strict {strict}, sfc {sfc}");
            let mut m = Machine::from_package(&pkg, None, &Options { jit, strict, sfc, ..Options::default() }).unwrap();
            let (mut cpu, mut soc) = by_hand(&pkg, jit, strict, sfc);
            assert!(m.flash_base() == &flash[..] && m.changed_sectors().is_empty(), "{what}: the base is the package's chip");
            for s in [&mut m.soc, &mut soc] {
                let at = s.periph.cycle_of_ms(ms / 2);
                s.periph.script.push_back((at, Action::Key { id: 12, down: true }));
            }
            let (mut c1, mut tick) = (None, false);
            while soc.periph.ms() < ms {
                m.run(20_000).unwrap();
                let jit = cpu.jit_on;
                run_dual(&mut c1, &mut soc, 20_000, 1 << 20, jit, |s, k| run_blocks(&mut cpu, s, k, &mut tick)).unwrap();
            }
            assert_eq!(m.soc.periph.cycles, soc.periph.cycles, "{what}");
            assert_eq!((m.cpu.pc, m.cpu.regs, m.cpu.sp, m.cpu.insn_count), (cpu.pc, cpu.regs, cpu.sp, cpu.insn_count), "{what}");
            assert_eq!((m.cpu1.is_some(), m.halted(), m.soc.resets), (c1.is_some(), None, soc.resets), "{what}");
            assert!(m.soc.periph.audio == soc.periph.audio, "{what}: the audio");
            assert!(strict || m.soc.periph.audio.len() > 20_000, "{what}: {} samples", m.soc.periph.audio.len());
            assert!(m.soc.periph.keys_down[12] && soc.periph.keys_down[12], "{what}: the scripted key");
            assert!(m.soc.flash.mem == soc.flash.mem && m.soc.xip == soc.xip, "{what}: the chip, the window");
            assert!(m.soc.lcd.fb == soc.lcd.fb, "{what}: the screen");
            assert!((0..1 << 20).step_by(4).all(|a| m.soc.ram.read32(a) == soc.ram.read32(a)), "{what}: RAM");
        }
    }

    /// A machine whose chip (0xaa) and installed window (0x55) differ, as a scrambled package's
    /// do; its app area ends at flash 0x7000.
    fn blank(o: &Options) -> Machine {
        let st = Stock { xip: vec![0x55; 0x80000], flash: vec![0xaa; FLASH_SIZE], entry: XIP_BASE + 0x120, board: Board::Fm1,
                         identity: String::new(), key: 0x980f, app_end: 0x7000, usr: None, usr_layout: UsrLayout::FM1 };
        Machine::from_stock(st, o)
    }

    #[test]
    fn the_options_set_the_chip_and_cpu0() {
        let m = Machine::from_image(vec![0; 64], XIP_BASE, &Options::default());
        assert_eq!((m.soc.periph.hz, m.soc.periph.cycle_ticks, m.soc.periph.skip_idle), (240_000_000, 1, true));
        assert!(m.cpu.user && !m.cpu.jit_on && !m.cpu.cost_model && !m.soc.strict_align && m.soc.xip_cache.is_none());
        assert_eq!((m.cpu.pc, m.cpu.regs[0], m.soc.ram.read32(BOOT_PARAMS - RAM_BASE)), (XIP_BASE, BOOT_PARAMS, BOOT_PARAMS + 0x40));
        assert!(m.flash_base().iter().all(|&b| b == 0xff) && m.usr.is_none() && m.cpu1.is_none() && m.halted().is_none());
        assert_eq!((m.soc.ram.len(), m.soc.board, m.soc.flash.jedec), (RAM_SIZE, Board::Fm1, FlashPart::P25q80.jedec()));
        // the strict clock: eighths of a cycle and the interpreter only, whatever was asked
        let o = Options { mhz: 120, ram_size: 2 << 20, jit: true, skip_idle: false, user: false, strict: true, lcd_timed: true,
                          flash_part: FlashPart::Uc25hq80, cpu1_survives: true, flaky_core1: Some((0.5, 7)), sfc: false };
        let m = Machine::from_image(vec![0; 64], XIP_BASE, &o);
        assert_eq!((m.soc.periph.hz, m.soc.periph.cycle_ticks, m.soc.periph.skip_idle), (960_000_000, 8, false));
        assert!(!m.cpu.jit_on && m.cpu.cost_model && m.soc.strict_align && m.soc.xip_cache.is_some() && !m.soc.loop_skip.enabled);
        assert!(!m.cpu.user && m.soc.lcd_timed && m.soc.cpu1_survives && m.soc.flaky_core1.is_some());
        assert_eq!((m.soc.ram.len(), m.soc.flash.jedec), (2 << 20, FlashPart::Uc25hq80.jedec()));
        let m = Machine::from_image(vec![0; 64], XIP_BASE, &Options { jit: true, ..Options::default() });
        assert!(m.cpu.jit_on);
        // a package's boot: its chip, the SPL's parameters in place of the empty pair
        let m = blank(&Options::default());
        assert!(m.flash_base().iter().all(|&b| b == 0xaa) && m.soc.flash.mem == m.flash_base());
        assert_eq!((m.cpu.pc, m.cpu.regs[0], m.soc.ram.read32(BOOT_PARAMS - RAM_BASE)), (XIP_BASE + 0x120, BOOT_PARAMS, BOOT_PARAMS + 24));
    }

    #[test]
    fn a_run_that_cannot_go_on_halts_with_its_reason_and_runs_no_more() {
        let mut m = Machine::from_image(vec![0xff; 0x1000], XIP_BASE + 0x120, &Options::default()); // (no instruction)
        let why = m.run(100).unwrap_err();
        assert!(why.contains("UnknownInstruction") && why.contains("0x02000120"), "{why}");
        assert_eq!(m.halted(), Some(&why[..]));
        let was = (m.soc.periph.cycles, m.cpu.insn_count, m.cpu.pc);
        assert_eq!(m.run(100), Err(why.clone()));
        assert_eq!((m.soc.periph.cycles, m.cpu.insn_count, m.cpu.pc), was, "it ran no more");
        m.halt("a panic".into());
        assert_eq!(m.halted(), Some(&why[..]), "the first reason stays");
        // halted from outside (a front end that caught its own failure around a run)
        let mut m = Machine::from_image([0x20, 0x00].repeat(64), XIP_BASE, &Options::default()); // csync x 64
        m.run(8).unwrap();
        assert_eq!((m.cpu.insn_count, m.halted()), (8, None));
        m.halt("the emulator failed".into());
        assert_eq!(m.run(8), Err("the emulator failed".into()));
        assert_eq!(m.cpu.insn_count, 8);
    }

    #[test]
    fn a_tick_spent_on_the_next_instruction_is_carried_to_the_next_run() {
        // cpu0 stops short where it finds a lock core 1 holds (`Soc::waits`, set by hand here); a key
        // came due there (cycle 5), so the tick of the instruction it stopped before is spent
        let boot = || {
            let mut m = Machine::from_image([0x20, 0x00].repeat(64), XIP_BASE, &Options::default()); // csync x 64
            m.soc.periph.script.push_back((5, Action::Key { id: 12, down: true }));
            m.soc.waits[0] = Some(Wait::Lock);
            m
        };
        let (mut m, mut hand, mut tick) = (boot(), boot(), false);
        m.run(32).unwrap();
        run_dual(&mut None, &mut hand.soc, 32, RAM_SIZE, false, |s, k| run_blocks(&mut hand.cpu, s, k, &mut tick)).unwrap();
        assert!(m.pending_tick && tick);
        assert_eq!((m.cpu.insn_count, m.soc.periph.cycles), (4, 5));
        assert_eq!((hand.cpu.insn_count, hand.soc.periph.cycles), (4, 5));
        // the next run does not tick again for its first instruction: a cycle an instruction, as before
        (m.soc.waits[0], hand.soc.waits[0]) = (None, None);
        m.run(8).unwrap();
        run_dual(&mut None, &mut hand.soc, 8, RAM_SIZE, false, |s, k| run_blocks(&mut hand.cpu, s, k, &mut tick)).unwrap();
        assert_eq!((m.cpu.insn_count, m.soc.periph.cycles, m.pending_tick), (12, 12, false));
        assert_eq!((hand.cpu.insn_count, hand.soc.periph.cycles, tick), (12, 12, false));
        assert!(m.soc.periph.keys_down[12]);
    }

    #[test]
    fn a_restored_sector_replaces_its_whole_sector_in_the_window() {
        let mut m = blank(&Options::default());
        // one byte is not the chip's; the others are the chip's, which the window does not show
        let mut saved = vec![0xaa; SECTOR];
        saved[0] = 0x12;
        m.restore_sectors(&[(0x74000, &saved)]).unwrap();
        assert_eq!(m.soc.read32(0x0207_0000), 0xaaaa_aa12);
        assert_eq!(m.soc.read32(0x0207_0ffc), 0xaaaa_aaaa, "also where the saved byte is the chip's");
        assert_eq!(m.soc.read32(0x0207_1000), 0x5555_5555, "the next sector as installed");
        assert_eq!((m.soc.flash.mem[0x74000], m.flash_base()[0x74000], m.soc.flash.revision), (0x12, 0xaa, 1));
        // a sector that is all the chip's is written through as well, and is not a change
        m.restore_sectors(&[(0x75000, &[0xaa; SECTOR])]).unwrap();
        assert_eq!((m.soc.read32(0x0207_1000), m.soc.flash.revision), (0xaaaa_aaaa, 2));
        assert_eq!(m.changed_sectors(), vec![(0x74000, saved)]);
    }

    #[test]
    fn a_restored_sector_reads_as_the_firmwares_own_erase_and_program_left_it() {
        let (mut live, mut restored) = (blank(&Options::default()), blank(&Options::default()));
        // write enable, sector erase at 0x74000, write enable, page program of two bytes there
        for cmd in [&[0x06][..], &[0x20, 0x07, 0x40, 0x00], &[0x06], &[0x02, 0x07, 0x40, 0x00, 0x12, 0x34]] {
            live.soc.write32(0x500c0, 0); // PD0 low: chip select
            for &b in cmd {
                live.soc.write32(crate::spiflash::SPI0_BASE + 8, b as u32);
            }
            live.soc.write32(0x500c0, 1);
        }
        let saved = live.changed_sectors();
        assert_eq!((saved.len(), saved[0].0, &saved[0].1[..4]), (1, 0x74000, &[0x12, 0x34, 0xff, 0xff][..]));
        restored.restore_sectors(&[(saved[0].0, &saved[0].1)]).unwrap();
        assert!(restored.soc.flash.mem == live.soc.flash.mem && restored.changed_sectors() == saved);
        for a in (0x0207_0000..0x0207_2000).step_by(4) {
            assert_eq!(restored.soc.read32(a), live.soc.read32(a), "XIP at {a:#x}");
        }
        assert_eq!((live.soc.read32(0x0207_0000), live.soc.read32(0x0207_1000)), (0xffff_3412, 0x5555_5555));
    }

    #[test]
    fn an_invalid_restore_writes_nothing() {
        let mut m = blank(&Options::default());
        let (good, short) = (vec![0u8; SECTOR], vec![0u8; 10]);
        // not on a sector, past the chip, not a sector long, a sector twice (after one that is fine)
        for bad in [vec![(1, &good[..])], vec![(1 << 20, &good[..])], vec![(0, &short[..])], vec![(0x1000, &good[..]), (7, &good[..])],
                    vec![(0, &good[..]), (0x1000, &good[..]), (0, &good[..])]] {
            assert!(m.restore_sectors(&bad).is_err());
            assert!(m.soc.flash.mem == m.flash_base() && m.soc.flash.revision == 0 && m.changed_sectors().is_empty());
            assert_eq!(m.soc.read32(XIP_BASE), 0x5555_5555);
        }
        m.restore_sectors(&[(0, &good), (0xff000, &good)]).unwrap();
        assert_eq!(m.changed_sectors(), vec![(0, good.clone()), (0xff000, good)]);
    }

    #[test]
    fn a_bank_goes_over_the_flash_as_it_is_and_the_base_stays_the_booted_chip() {
        let mut m = blank(&Options::default());
        let bank = vec![0x23; 4096];
        assert!(m.load_syx(&bank).is_err() && m.changed_sectors().is_empty(), "no patch area in this package");
        m.usr = Some(0xe0000);
        m.restore_sectors(&[(0x74000, &[0x17; SECTOR])]).unwrap();
        let mut expected = m.soc.flash.mem.clone();
        fwsc::usr_load_syx(&mut expected, 0xe0000, UsrLayout::FM1, &bank, 0).unwrap();
        m.load_syx(&bank).unwrap();
        assert!(m.soc.flash.mem == expected && expected != m.flash_base());
        assert_eq!(m.soc.read32(XIP_BASE), 0x5555_5555, "the installed code stays in the window");
        assert_eq!(m.soc.flash.mem[0x74000], 0x17, "over the restored flash, not in its place");
        assert!(m.flash_base().iter().all(|&b| b == 0xaa));
        let changed: Vec<u32> = m.changed_sectors().iter().map(|s| s.0).collect();
        assert!(changed[0] == 0x74000 && changed.len() > 1 && changed[1..].iter().all(|&at| at >= 0xe0000), "{changed:x?}");
        assert!(m.load_syx(&[1, 2, 3]).is_err());
        assert!(m.soc.flash.mem == expected, "what is not a bank writes nothing");
    }

    #[test]
    fn with_the_chip_key_a_restored_sector_of_the_app_area_shows_decrypted() {
        // a sector as an updater stores it, scrambled for flash 0x5000 (fwsc::unscramble's cipher, its
        // own inverse): restored there, below the app area's end (`blank`: 0x7000), and at 0x9000
        let plain: Vec<u8> = (0..SECTOR as u32).map(|i| (i * 7 + 3) as u8).collect();
        let mut stored = plain.clone();
        for blk in (0..SECTOR).step_by(32) {
            fwsc::unmask(&mut stored[blk..blk + 32], 0x980f ^ ((0x5000 + blk - XIP_FLASH_OFFSET) >> 2) as u16);
        }
        for sfc in [true, false] {
            let mut m = blank(&Options { sfc, ..Options::default() });
            assert_eq!(m.soc.read8(0x40300), if sfc { 3 } else { 0 }, "SFCENC as the loader leaves it, or as the chip starts");
            m.restore_sectors(&[(0x5000, &stored), (0x9000, &stored)]).unwrap();
            assert_eq!(m.soc.xip[0x1000..0x2000] == plain[..], sfc, "the app area: decrypted with the key, as stored without");
            assert!(m.soc.xip[0x5000..0x6000] == stored[..], "past the app area: as stored");
            assert!(m.soc.flash.mem[0x5000..0x6000] == stored[..], "the chip holds what was saved");
        }
    }
}
