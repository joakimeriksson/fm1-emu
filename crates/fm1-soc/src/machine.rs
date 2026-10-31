//! Running the cores against the SoC: cpu0's block loop (fm1-live and the browser build share
//! it), and cpu1 next to it once the firmware releases it.
//!
//! cpu1 has no clock of its own. cpu0 runs in slices of `SLICE` instructions; after each,
//! cpu1 may run as many instructions as cycles passed (idle skips included, so it keeps the
//! hardware's pace, where both cores run at the CPU clock), through `Core1Bus`, whose tick
//! budget stops its interpreter and compiled code alike. It stops early once it is back at
//! the pc it started from with the same registers: a polling loop with nothing to do (the
//! stock firmware's job loop, X0X's), which would otherwise cost as much as cpu0.

use crate::{p33, Soc, RAM_BASE, VEC_BASE};
use fm1_core::{Bus, Cpu, PlainState};

/// cpu0 runs in slices of this many instructions once cpu1 is running, cpu1 after each.
pub const SLICE: u32 = 1024;

/// cpu1's view of the SoC: no clock, a budget of ticks instead.
pub struct Core1Bus<'a> {
    pub soc: &'a mut Soc,
    pub budget: u64,
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

impl Bus for Core1Bus<'_> {
    fn read8(&mut self, a: u32) -> u8 {
        self.soc.core1_read8(a)
    }
    fn read16(&mut self, a: u32) -> u16 {
        self.soc.core1_read16(a)
    }
    fn read32(&mut self, a: u32) -> u32 {
        self.soc.core1_read32(a)
    }
    fn fetch32(&mut self, a: u32) -> u32 {
        self.soc.read32(a)
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
        self.budget = self.budget.saturating_sub(1);
        self.budget == 0
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
        self.budget = self.budget.saturating_sub(n);
    }
    fn hw_lock(&mut self, core: u32, set: bool) -> bool {
        self.soc.hw_lock(core, set)
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
        self.0.read32(a)
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
        if self.load(a) { self.soc.read32(a) } else { 0 }
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

/// cpu0's block loop: interrupts taken between blocks, the SoC serviced when a tick is due.
/// `pending_tick` carries a tick already spent on the next instruction across calls. Returns
/// the instructions run: n, or fewer when cpu0 released cpu1 or the chip reset (it stops right
/// there; after a reset `soc.resets` has counted up, and cpu0 is back at its start).
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
        soc.periph.in_isr = !cpu.irq_levels.is_empty();
        if soc.periph.any_pending() {
            if let Some((irq, prio)) = soc.pending_irq() {
                if cpu.irq_ready(prio) {
                    let handler = soc.read32(VEC_BASE + 4 * irq as u32);
                    cpu.interrupt(handler, prio);
                    // the handler's first instruction may stay in compiled code (a micro-op now):
                    // TIMER4's idle skip must see "in an ISR" from that instruction on
                    soc.periph.in_isr = true;
                }
            }
        }
        let now = soc.periph.cycles;
        // not while a release is latched: run_fast's return must break below where it would
        // without the skip (lib.rs latches again on any later 0 -> 1 of 0x10008 bit 3)
        if soc.loop_skip.enabled && !soc.cpu1_release && n - done >= PROBE_MIN && !soc.periph.any_pending() && soc.loop_skip.may_probe(now) {
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
        match cpu.run_fast(soc, n - done) {
            Ok((0, _)) => {
                if let Err(e) = cpu.step(soc) {
                    return Err(format!("{e:?} at pc {:#010x}", cpu.pc));
                }
                done += 1;
            }
            Ok((k, due)) => {
                done += k;
                ticked = due;
            }
            Err(e) => return Err(format!("{e:?} at pc {:#010x}", cpu.pc)),
        }
        if soc.cpu1_release {
            break; // core 1 starts now (run_dual)
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
/// single steps; back where it started with nothing changed means it is idle.
pub fn run_cpu1(c1: &mut Cpu, soc: &mut Soc, cycles: u64) -> Result<(), String> {
    if soc.core_paused(1) || soc.reset_due() {
        return Ok(());
    }
    if soc.cpu1_idle {
        // asleep since its `idle`: on again only with an interrupt from its bank pending
        if !(soc.periph.any_pending() && soc.pending_irq_for(1).is_some()) {
            return Ok(());
        }
        soc.cpu1_idle = false;
    }
    let probe = (c1.pc, c1.regs, c1.sp);
    let mut left = cycles.min(1 << 20);
    // first a block at a time, looking for the way back to where it started (a polling loop
    // goes round in well under this); then, busy, in big stretches
    let mut probing = 512u64;
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
        let busy = if probing > 0 { 0 } else { left.min(256) };
        // (a stretch: its last call may end up to `STRETCH_ROOM` past what is left, and its ticks
        // never run out)
        let span = if stretch { probing.min(left) + fm1_core::STRETCH_ROOM as u64 } else { busy };
        let mut bus = Core1Bus { soc: &mut *soc, budget: span.max(64) + 1 };
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
        left = left.saturating_sub(k);
        probing = probing.saturating_sub(k);
        if soc.reset_due() {
            break; // (cpu1 wrote PR_PWR: `Core1Bus::stop_at_reset`)
        }
        if soc.cpu1_idle {
            break; // asleep (`Core1Bus::idle`)
        }
        if c1.pc == probe.0 && c1.sp == probe.2 && c1.regs == probe.1 {
            break; // polling with nothing to do
        }
    }
    Ok(())
}

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
    while cpu1.is_none() {
        *cpu1 = released_cpu1(soc, ram_size).map(|mut c| {
            c.jit_on = jit_on;
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
    while done < n {
        let k = (n - done).min(SLICE);
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
        done += k;
        let dt = soc.periph.cycles - c0;
        run_cpu1(c1, soc, dt)?;
    }
    Ok(done)
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
        let mut bus = Core1Bus { soc: &mut soc, budget: 1 << 20 };
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
    fn good_core1_reads_what_is_there() {
        let mut soc = soc_with_loads();
        let mut c1 = core_at_ram(1);
        soc.write32(RAM_BASE + 0x100, 0xdead_beef);
        c1.step(&mut Core1Loads(&mut soc)).unwrap();
        assert_eq!(c1.regs[3], 0xdead_beef);
        assert!(Core1Bus { soc: &mut soc, budget: 1 }.jit_mem().is_some());
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
}
