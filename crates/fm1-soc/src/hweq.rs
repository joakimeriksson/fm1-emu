//! The AC79's hardware EQ (JL_EQ at 0x1E000: CON0..3, DATAI_ADR, DATAO_ADR, DATA_LEN, FLT_ADR; its RAM at
//! 0x1E800), from JieLi's AC79 SDK driver (cpu.a eq.c.o, read with tools/sdk_mmio.py) and measured on an
//! FM-1 (fm1-dock's probes/hweq.c, tools/hwdsp.py: tests/hwdsp/fm1.txt's eq/ cases).
//!
//! A run filters `DATA_LEN + 1` frames of up to 15 channels through up to 64 biquad sections:
//!   CON0  bits 16-21 sections - 1, 8-11 out channels - 1, 6-7 out format, 4-5 in format (0 s16, 1 s32,
//!         2 f32), 2-3 run mode; writing bits 0-1 starts it (bit 0 reads 0 again when done, bit 1 stays),
//!         bit 31 reads done, writing bit 30 clears it, bit 15 enables its interrupt (not modelled).
//!   CON1  out channel step << 16 | in channel step (bytes); CON2 the same times (channels - 1)
//!   CON3  out sample step << 16 | in sample step (bytes)
//! A (channel, section) slot is 8 words: 5 f32 coefficients [g, c1, c2, c3, c4], then 3 words of state.
//! Slot j = channel * sections + section is at the EQ's RAM + 32 j for j < 20 (the FM-1: its RAM holds
//! 20, not the 26 its 208 words would), else at FLT_ADR + 32 (j - 20). Output channel o filters input
//! channel o % channels (run mode 1: one input to several outputs; 2: two to four).
//!
//! The section (measured): transposed direct form II, y = g x + s1, s1 = g c4 x + c3 y + s2,
//! s2 = g c2 x + c1 y, i.e. H(z) = g (1 + c4 z^-1 + c2 z^-2) / (1 - c3 z^-1 - c1 z^-2) (the SDK's
//! eq_design.c gives [b0/a0, -a2/a0, b2/b0, -a1/a0, b1/b0]). The chip computes in a wider format than
//! f32 and rounds once to the output: integers half away from zero, saturated; no scaling (a 16-bit
//! 1000 is 1000.0). NOT BIT-EXACT: here f64, then rounded; the chip lands within a few f32 ulp of it
//! (its own number format: -32.0 comes back as -31.999998). The state words: the chip keeps s2 in
//! word 5 and s1 in word 6 as fixed point (x 2^-32 while word 7 is 0); this model keeps its own f64
//! states, by the slot's address, and marks the slot with STATE_TAG in word 7, reading the chip's form
//! back when the tag is not there: a driver that zeroes the words starts the section from silence, one
//! that saves and restores them in place goes on; words copied to another slot do not carry the state. Cycles: ~100 + frames x channels x
//! (8 + 26 sections), the FM-1's (the EQ runs beside the CPU: it costs the CPU nothing).

use crate::hwdsp::Mem;

pub const BASE: u32 = 0x1e000;
pub const END: u32 = 0x1e01f;
pub const RAM: u32 = 0x1e800;
pub const RAM_WORDS: u32 = 208;
pub const RAM_END: u32 = RAM + 4 * RAM_WORDS - 1;
/// slots the chip reads from its RAM before FLT_ADR
pub const RAM_SLOTS: u32 = 20;
/// word 7 of a slot whose state is this model's (f64, in `HwEq::state`)
pub const STATE_TAG: u32 = 0x5145_4d31;

#[derive(Default, Clone, Debug)]
pub struct HwEq {
    pub regs: [u32; 8],
    pub ram: Vec<u32>,
    pending: bool,
    done_at: u64,
    /// (s1, s2) of a slot, by its address
    state: std::collections::HashMap<u32, (f64, f64)>,
    pub runs: u64,
}

impl HwEq {
    pub fn new() -> Self {
        HwEq { ram: vec![0; RAM_WORDS as usize], ..Default::default() }
    }

    pub fn read(&self, addr: u32, cycles: u64) -> u32 {
        if (RAM..=RAM_END).contains(&addr) {
            return self.ram.get(((addr - RAM) / 4) as usize).copied().unwrap_or(0);
        }
        let i = ((addr - BASE) / 4) as usize;
        if i == 0 {
            let busy = cycles < self.done_at;
            let mut v = self.regs[0];
            if self.pending && !busy {
                v = (v & !1) | 0x8000_0000;
            }
            return v;
        }
        self.regs[i]
    }

    pub fn write(&mut self, m: &mut impl Mem, addr: u32, v: u32, cycles: u64) {
        if (RAM..=RAM_END).contains(&addr) {
            if let Some(w) = self.ram.get_mut(((addr - RAM) / 4) as usize) {
                *w = v;
            }
            return;
        }
        let i = ((addr - BASE) / 4) as usize;
        if i != 0 {
            self.regs[i] = v;
            return;
        }
        // (bit 0 as it reads: 0 again once a run is done)
        let was = self.read(BASE, cycles);
        if v & 0x4000_0000 != 0 {
            self.pending = false;
        }
        self.regs[0] = v & !0xc000_0000;
        if v & 1 != 0 && was & 1 == 0 && v & 0x8000_0000 == 0 {
            let cost = self.run(m);
            self.runs += 1;
            self.pending = true;
            self.done_at = cycles + cost;
        }
    }

    fn slot_word(&self, m: &mut impl Mem, j: u32, w: u32) -> u32 {
        if j < RAM_SLOTS {
            self.ram[(8 * j + w) as usize]
        } else {
            m.rd(self.regs[7].wrapping_add(32 * (j - RAM_SLOTS) + 4 * w))
        }
    }

    fn slot_store(&mut self, m: &mut impl Mem, j: u32, w: u32, v: u32) {
        if j < RAM_SLOTS {
            self.ram[(8 * j + w) as usize] = v;
        } else {
            m.wr(self.regs[7].wrapping_add(32 * (j - RAM_SLOTS) + 4 * w), v);
        }
    }

    fn slot_addr(&self, j: u32) -> u32 {
        if j < RAM_SLOTS { RAM + 32 * j } else { self.regs[7].wrapping_add(32 * (j - RAM_SLOTS)) }
    }

    fn run(&mut self, m: &mut impl Mem) -> u64 {
        let con = self.regs[0];
        let nsec = ((con >> 16) & 63) + 1;
        let och = ((con >> 8) & 15) + 1;
        let (fin, fout) = ((con >> 4) & 3, (con >> 6) & 3);
        let n = self.regs[6].wrapping_add(1) & 0xffff;
        let (in_ch, out_ch) = (self.regs[1] & 0xffff, self.regs[1] >> 16);
        let (in_step, out_step) = (self.regs[3] & 0xffff, self.regs[3] >> 16);
        let (ia, oa) = (self.regs[4], self.regs[5]);
        // the input's channels: CON2's low half is the step to the last one
        let ch = if in_ch == 0 { 1 } else { (self.regs[2] & 0xffff) / in_ch + 1 };
        for o in 0..och {
            let k = o % ch;
            let at = |s: u32| ia.wrapping_add(k.wrapping_mul(in_ch)).wrapping_add(s.wrapping_mul(in_step));
            let mut x: Vec<f64> = (0..n).map(|s| load(m, at(s), fin)).collect();
            for sec in 0..nsec {
                let j = o * nsec + sec;
                let c: Vec<f64> = (0..5).map(|w| f32::from_bits(self.slot_word(m, j, w)) as f64).collect();
                let (g, c1, c2, c3, c4) = (c[0], c[1], c[2], c[3], c[4]);
                let at = self.slot_addr(j);
                let tag = self.slot_word(m, j, 7);
                let (mut s1, mut s2) = if tag == STATE_TAG {
                    self.state.get(&at).copied().unwrap_or((0.0, 0.0))
                } else {
                    let q = 2f64.powi(-32);
                    (self.slot_word(m, j, 6) as i32 as f64 * q, self.slot_word(m, j, 5) as i32 as f64 * q)
                };
                for v in x.iter_mut() {
                    let xi = *v;
                    let y = g * xi + s1;
                    s1 = g * c4 * xi + c3 * y + s2;
                    s2 = g * c2 * xi + c1 * y;
                    *v = y;
                }
                self.state.insert(at, (s1, s2));
                self.slot_store(m, j, 5, (s2 as f32).to_bits());
                self.slot_store(m, j, 6, (s1 as f32).to_bits());
                self.slot_store(m, j, 7, STATE_TAG);
            }
            for (s, v) in x.iter().enumerate() {
                store(m, oa.wrapping_add(o.wrapping_mul(out_ch)).wrapping_add((s as u32).wrapping_mul(out_step)), fout, *v);
            }
        }
        100 + (n as u64) * (och as u64) * (8 + 26 * nsec as u64)
    }
}

fn load(m: &mut impl Mem, a: u32, f: u32) -> f64 {
    match f {
        0 => (m.rd(a & !3) >> ((a & 2) * 8)) as u16 as i16 as f64,
        1 => m.rd(a) as i32 as f64,
        _ => f32::from_bits(m.rd(a)) as f64,
    }
}

/// rounded half away from zero, saturated
fn int(v: f64, bits: u32) -> i64 {
    let r = if v < 0.0 { -((-v) + 0.5).floor() } else { (v + 0.5).floor() };
    let lim = (1i64 << (bits - 1)) as f64;
    r.clamp(-lim, lim - 1.0) as i64
}

fn store(m: &mut impl Mem, a: u32, f: u32, v: f64) {
    match f {
        0 => {
            let sh = (a & 2) * 8;
            let cur = m.rd(a & !3);
            m.wr(a & !3, (cur & !(0xffff << sh)) | ((int(v, 16) as u32 & 0xffff) << sh));
        }
        1 => m.wr(a, int(v, 32) as u32),
        _ => m.wr(a, (v as f32).to_bits()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ram(Vec<u32>);
    impl Mem for Ram {
        fn rd(&mut self, a: u32) -> u32 {
            self.0[(a / 4) as usize]
        }
        fn wr(&mut self, a: u32, v: u32) {
            self.0[(a / 4) as usize] = v;
        }
    }

    /// mono, f32 in and out, one section in the EQ's RAM; x at 0x1000, y at 0x2000
    fn mono(eq: &mut HwEq, ram: &mut Ram, c: [f32; 5], x: &[f32]) -> Vec<f32> {
        for (k, v) in c.iter().enumerate() {
            eq.write(ram, RAM + 4 * k as u32, v.to_bits(), 0);
        }
        for (k, v) in x.iter().enumerate() {
            ram.0[0x1000 / 4 + k] = v.to_bits();
        }
        let n = x.len() as u32;
        for (r, v) in [(1, 4 * n << 16 | 4 * n), (2, 0), (3, 4 << 16 | 4), (4, 0x1000), (5, 0x2000), (6, n - 1), (7, 0x3000)] {
            eq.write(ram, BASE + 4 * r, v, 0);
        }
        eq.write(ram, BASE, (2 << 4) | (2 << 6), 0);
        eq.write(ram, BASE, (2 << 4) | (2 << 6) | 0x4000_0000, 0);
        eq.write(ram, BASE, (2 << 4) | (2 << 6) | 3, 0);
        assert_eq!(eq.read(BASE, 1 << 30) & 0x8000_0003, 0x8000_0002, "done, en 0, bit 1 stays");
        (0..n).map(|k| f32::from_bits(ram.0[0x2000 / 4 + k as usize])).collect()
    }

    #[test]
    fn each_coefficient_does_what_the_fm1_showed() {
        // the FM-1's impulse responses (one coefficient set at a time)
        let mut ram = Ram(vec![0; 0x4000]);
        let mut imp = [0f32; 6];
        imp[0] = 1.0;
        let mut eq = HwEq::new();
        assert_eq!(mono(&mut eq, &mut ram, [1.0, 0.25, 0.0, 0.0, 0.0], &imp), [1.0, 0.0, 0.25, 0.0, 0.0625, 0.0]);
        let mut eq = HwEq::new();
        assert_eq!(mono(&mut eq, &mut ram, [1.0, 0.0, 0.25, 0.0, 0.0], &imp), [1.0, 0.0, 0.25, 0.0, 0.0, 0.0]);
        let mut eq = HwEq::new();
        assert_eq!(mono(&mut eq, &mut ram, [1.0, 0.0, 0.0, 0.25, 0.0], &imp)[..3], [1.0, 0.25, 0.0625]);
        let mut eq = HwEq::new();
        assert_eq!(mono(&mut eq, &mut ram, [0.5, 0.0, 0.0, 0.0, 0.25], &imp)[..3], [0.5, 0.125, 0.0]);
    }

    #[test]
    fn the_state_carries_on_into_the_next_run() {
        let mut ram = Ram(vec![0; 0x4000]);
        let c = [0.3, -0.2, 0.7, 0.6, -0.4];
        let x: Vec<f32> = (0..16).map(|k| ((k * 37) % 11) as f32 - 5.0).collect();
        let whole = mono(&mut HwEq::new(), &mut ram, c, &x);
        let mut eq = HwEq::new();
        let mut parts = mono(&mut eq, &mut ram, c, &x[..9]);
        parts.extend(mono(&mut eq, &mut ram, c, &x[9..]));
        assert_eq!(parts, whole);
    }

    #[test]
    fn the_chips_state_words_x_2_to_the_32() {
        // the FM-1, zero input: word 6 = 0x447A0000 (s1) gives 0.2675 first (eq/f32/state1)
        let mut ram = Ram(vec![0; 0x4000]);
        let mut eq = HwEq::new();
        eq.write(&mut ram, RAM + 24, 0x447a_0000, 0);
        let y = mono(&mut eq, &mut ram, [1.0, 0.25, 0.5, 0.5, 0.75], &[0.0; 4]);
        assert!((y[0] - 0.2675).abs() < 1e-4 && (y[1] - 0.1337).abs() < 1e-4, "{y:?}");
    }

    #[test]
    fn integers_round_half_away_and_saturate() {
        assert_eq!([int(-19.5, 16), int(19.5, 16), int(-9.75, 16), int(44560.0, 16)], [-20, 20, -10, 32767]);
    }
}
