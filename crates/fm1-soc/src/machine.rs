//! Running the cores against the SoC: cpu0's block loop (fm1-live and the browser build share
//! it), and cpu1 next to it once the firmware releases it.
//!
//! cpu1 has no clock of its own. cpu0 runs in slices of `SLICE` instructions; after each,
//! cpu1 may run as many instructions as cycles passed (idle skips included, so it keeps the
//! hardware's pace, where both cores run at the CPU clock), through `Core1Bus`, whose tick
//! budget stops its interpreter and compiled code alike. It stops early once it is back at
//! the pc it started from with the same registers: a polling loop with nothing to do (the
//! stock firmware's job loop, X0X's), which would otherwise cost as much as cpu0.

use crate::{Soc, RAM_BASE, VEC_BASE};
use fm1_core::{Bus, Cpu};

/// cpu0 runs in slices of this many instructions once cpu1 is running, cpu1 after each.
pub const SLICE: u32 = 1024;

/// cpu1's view of the SoC: no clock, a budget of ticks instead.
pub struct Core1Bus<'a> {
    pub soc: &'a mut Soc,
    pub budget: u64,
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
        self.soc.write8(a, v)
    }
    fn write16(&mut self, a: u32, v: u16) {
        self.soc.write16(a, v)
    }
    fn write32(&mut self, a: u32, v: u32) {
        self.soc.write32(a, v)
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
    fn code_writes(&self) -> Option<u64> {
        self.0.code_writes()
    }
    fn code_mark(&mut self, lo: u32, hi: u32) {
        self.0.code_mark(lo, hi)
    }
}

/// cpu0's block loop: interrupts taken between blocks, the SoC serviced when a tick is due.
/// `pending_tick` carries a tick already spent on the next instruction across calls.
pub fn run_blocks(cpu: &mut Cpu, soc: &mut Soc, n: u32, pending_tick: &mut bool) -> Result<(), String> {
    let mut done = 0u32;
    let mut ticked = std::mem::take(pending_tick);
    while done < n {
        if ticked || soc.periph.tick() {
            soc.advance(0);
        }
        ticked = false;
        soc.periph.in_isr = !cpu.irq_levels.is_empty();
        if soc.periph.any_pending() {
            if let Some((irq, prio)) = soc.pending_irq() {
                if cpu.irq_ready(prio) {
                    let handler = soc.read32(VEC_BASE + 4 * irq as u32);
                    cpu.interrupt(handler, prio);
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
    Ok(())
}

/// cpu1 once cpu0 released it (`0x10008 |= 8`): its entry is the word at 0x01C7FFF8 (the
/// stock firmware's stub at 0x020001B8 and X0X's entry both put it there; older images
/// without it: the RAM job loop at 0x01C022B6). It starts in supervisor mode.
pub fn released_cpu1(soc: &mut Soc, ram_size: usize) -> Option<Cpu> {
    if !std::mem::take(&mut soc.cpu1_release) && soc.read32(0x10008) & 8 == 0 {
        return None;
    }
    let entry = soc.read32(0x01C7_FFF8);
    let in_code = (0x0200_0000..0x0300_0000).contains(&entry) || (RAM_BASE..RAM_BASE + ram_size as u32).contains(&entry);
    if std::env::var_os("FM1_DUAL_TRACE").is_some() {
        eprintln!("cpu1 released at {} ms (entry {entry:#010x})", soc.periph.ms());
    }
    let mut c1 = Cpu::new(if in_code { entry } else { RAM_BASE + 0x22b6 });
    c1.core_id = 1;
    c1.usp = RAM_BASE + 0x15eb4;
    c1.sp = RAM_BASE + 0x16eb4;
    Some(c1)
}

/// Run cpu1 for up to `cycles` instructions: interrupts from its own bank, then blocks or
/// single steps; back where it started with nothing changed means it is idle.
pub fn run_cpu1(c1: &mut Cpu, soc: &mut Soc, cycles: u64) -> Result<(), String> {
    if soc.core_paused(1) {
        return Ok(());
    }
    let probe = (c1.pc, c1.regs, c1.sp);
    let mut left = cycles.min(1 << 20);
    // first a block at a time, looking for the way back to where it started (a polling loop
    // goes round in well under this); then, busy, in big stretches
    let mut probing = 512u64;
    while left > 0 {
        if soc.periph.any_pending() {
            if let Some((irq, prio)) = soc.pending_irq_for(1) {
                if c1.irq_ready(prio) {
                    let handler = soc.read32(VEC_BASE + 4 * irq as u32);
                    c1.interrupt(handler, prio);
                }
            }
        }
        let chunk = if probing > 0 { 1 } else { left.min(256) };
        let mut bus = Core1Bus { soc: &mut *soc, budget: chunk.max(64) + 1 };
        let k = match c1.run_fast(&mut bus, chunk as u32) {
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
        if c1.pc == probe.0 && c1.sp == probe.2 && c1.regs == probe.1 {
            break; // polling with nothing to do
        }
    }
    Ok(())
}

/// Run n instructions of cpu0 through `run0` (the block loop, or a caller's own), with cpu1
/// alongside once it is released.
pub fn run_dual(
    cpu1: &mut Option<Cpu>,
    soc: &mut Soc,
    n: u32,
    ram_size: usize,
    jit_on: bool,
    mut run0: impl FnMut(&mut Soc, u32) -> Result<(), String>,
) -> Result<(), String> {
    let mut done = 0;
    while cpu1.is_none() {
        // cpu0 alone, in stretches short enough to see a release soon (X0X holds the release
        // bit only while it waits for core 1 to report in, then gives up and clears it)
        *cpu1 = released_cpu1(soc, ram_size).map(|mut c| {
            c.jit_on = jit_on;
            c
        });
        if cpu1.is_some() || done >= n {
            break;
        }
        let k = (n - done).min(4096);
        soc.periph.room_cap = k as u64;
        run0(soc, k)?;
        done += k;
    }
    let Some(c1) = cpu1.as_mut() else { return Ok(()) };
    soc.periph.room_cap = SLICE as u64;
    while done < n {
        let k = (n - done).min(SLICE);
        let c0 = soc.periph.cycles;
        if soc.core_paused(0) {
            soc.advance(k as u64); // held by cpu1 (C0_CON bit 2): time runs on
        } else {
            run0(soc, k)?;
        }
        done += k;
        let dt = soc.periph.cycles - c0;
        run_cpu1(c1, soc, dt)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flaky::{FlakyCore1, MISREAD_BIT};

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
}
