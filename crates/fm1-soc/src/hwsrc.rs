//! The AC79's sample-rate converter (JL_SRC at 0x14300: CON0..3, IDAT_ADR, IDAT_LEN, ODAT_ADR, ODAT_LEN,
//! FLTB_ADR), from JieLi's AC79 SDK driver (cpu.a src.c.o, src_resample; tools/sdk_mmio.py) and measured
//! on an FM-1 (fm1-dock's probes/hwsrc.c, tools/hwdsp.py: tests/hwdsp/fm1.txt's src/ cases).
//!
//!   CON0  bits 0-2 channels - 1, 8-24 the phase, 26-30 the history's offset; bit 3 starts a run (reads 0
//!         again), bit 7 reads done, writing bit 6 clears it. A run leaves the phase and offset to go on
//!         from (the driver keeps them in src_fltb_t for the next call).
//!   CON1  out rate << 16 | in rate (Hz, or any ratio)
//!   CON2  in channel step << 16 | in sample step, CON3 the output's (in samples, not bytes)
//!   IDAT / ODAT_ADR, _LEN  16-bit samples, the lengths a channel's; a run ends when the last input is
//!         taken (not after an output with it) or the output is full; the lengths read back what is left
//!   FLTB_ADR  the history: channel k's last 24 inputs at FLTB_ADR + 48 k, a ring (the SDK's src_fltb_t
//!         has room for two channels; more go on past it)
//!
//! Measured (2026-10-28): the phase counts exactly: each output adds the in rate, each input taken
//! subtracts the out rate (inputs are taken while phase >= out rate), so the output's position between
//! inputs is phase / out rate. An input steps the offset back one (mod 24) and goes there. An output is
//! sum over t of hist[offset + t] h(t + phase / out rate) (t = 0 the newest input), rounded half up and
//! saturated: h a 24-tap low-pass (DC gain 0.999, 0.991 at its centre tap, so 1:1 is not quite a copy)
//! that no textbook window fits to the LSB; KERNEL (hwsrc_kernel.rs) is it at 256 phases, fitted to the
//! FM-1's outputs, interpolated linearly. NOT BIT-EXACT: ~97 % of mono outputs are, all within 1 LSB
//! (2 at full scale or in stereo). A starting phase at or over the out rate is not modelled exactly.
//! Cycles: ~240 an output + 440 an output and channel + 40 an input taken (the FM-1's; beside the CPU).

use crate::hwdsp::Mem;
use crate::hwsrc_kernel::KERNEL;

pub const BASE: u32 = 0x14300;
pub const END: u32 = 0x14323;
const TAPS: usize = 24;
const PHASES: usize = 256;

#[derive(Default, Clone, Debug)]
pub struct HwSrc {
    pub regs: [u32; 9],
    pending: bool,
    done_at: u64,
    pub runs: u64,
}

/// h(u) at u input samples after the newest (0..24)
fn h(u: f64) -> f64 {
    let q = u * PHASES as f64;
    let j = q.floor();
    let w = q - j;
    let at = |i: f64| -> f64 {
        let i = i as usize;
        if i < KERNEL.len() { KERNEL[i] as f64 / (1u64 << 30) as f64 } else { 0.0 }
    };
    if j < 0.0 {
        return 0.0;
    }
    at(j) * (1.0 - w) + at(j + 1.0) * w
}

impl HwSrc {
    pub fn read(&self, addr: u32, cycles: u64) -> u32 {
        let i = ((addr - BASE) / 4) as usize;
        let v = self.regs.get(i).copied().unwrap_or(0);
        if i == 0 && self.pending && cycles >= self.done_at {
            return v | 0x80;
        }
        v
    }

    pub fn write(&mut self, m: &mut impl Mem, addr: u32, v: u32, cycles: u64) {
        let i = ((addr - BASE) / 4) as usize;
        if i != 0 {
            if let Some(r) = self.regs.get_mut(i) {
                *r = v;
            }
            return;
        }
        if v & 0x40 != 0 {
            self.pending = false;
        }
        self.regs[0] = v & !0xc0;
        if v & 8 != 0 && v & 0x80 == 0 {
            let cost = self.run(m);
            self.regs[0] &= !8;
            self.runs += 1;
            self.pending = true;
            self.done_at = cycles + cost;
        }
    }

    fn run(&mut self, m: &mut impl Mem) -> u64 {
        let con = self.regs[0];
        let ch = (con & 7) + 1;
        let mut phase = (con >> 8) & 0x1ffff;
        let mut off = ((con >> 26) & 31) as usize % TAPS;
        let (in_rate, out_rate) = (self.regs[1] & 0xffff, (self.regs[1] >> 16).max(1));
        let (in_sp, in_ch) = (self.regs[2] & 0xffff, self.regs[2] >> 16);
        let (out_sp, out_ch) = (self.regs[3] & 0xffff, self.regs[3] >> 16);
        let (ia, mut ileft, oa, mut oleft) = (self.regs[4], self.regs[5], self.regs[6], self.regs[7]);
        let fltb = self.regs[8];
        let rd16 = |m: &mut dyn FnMut(u32) -> u32, a: u32| -> i16 { (m(a & !3) >> ((a & 2) * 8)) as u16 as i16 };
        // the history, channel by channel (24 halfwords each, from FLTB_ADR on)
        let mut hist: Vec<[i16; TAPS]> = (0..ch)
            .map(|k| {
                let mut r = [0i16; TAPS];
                for (t, s) in r.iter_mut().enumerate() {
                    *s = rd16(&mut |a| m.rd(a), fltb.wrapping_add(48 * k + 2 * t as u32));
                }
                r
            })
            .collect();
        let (mut s, mut o, mut taken) = (0u32, 0u32, 0u64);
        'out: while oleft > 0 {
            while phase >= out_rate {
                if ileft == 0 {
                    break 'out;
                }
                off = (off + TAPS - 1) % TAPS;
                for (k, r) in hist.iter_mut().enumerate() {
                    let a = ia.wrapping_add(2u32.wrapping_mul((k as u32).wrapping_mul(in_ch).wrapping_add(s.wrapping_mul(in_sp))));
                    r[off] = rd16(&mut |a| m.rd(a), a);
                }
                s += 1;
                ileft -= 1;
                taken += 1;
                phase -= out_rate;
            }
            // (the chip stops once the input is taken: the last input waits in the history for the
            // next call: 64 in at 1:1 make 64 out, 600 at 44100 -> 48000 make 654, phase 0xa1b8)
            if ileft == 0 && taken > 0 {
                break;
            }
            let f = phase as f64 / out_rate as f64;
            let c: Vec<f64> = (0..TAPS).map(|t| h(t as f64 + f)).collect();
            for (k, r) in hist.iter().enumerate() {
                let y: f64 = (0..TAPS).map(|t| r[(off + t) % TAPS] as f64 * c[t]).sum();
                let y = (y + 0.5).floor().clamp(-32768.0, 32767.0) as i32;
                let a = oa.wrapping_add(2u32.wrapping_mul((k as u32).wrapping_mul(out_ch).wrapping_add(o.wrapping_mul(out_sp))));
                let sh = (a & 2) * 8;
                let cur = m.rd(a & !3);
                m.wr(a & !3, (cur & !(0xffff << sh)) | ((y as u32 & 0xffff) << sh));
            }
            o += 1;
            oleft -= 1;
            phase += in_rate;
        }
        for (k, r) in hist.iter().enumerate() {
            for t in (0..TAPS).step_by(2) {
                let a = fltb.wrapping_add(48 * k as u32 + 2 * t as u32);
                m.wr(a, (r[t] as u16 as u32) | (r[t + 1] as u16 as u32) << 16);
            }
        }
        self.regs[0] = (self.regs[0] & !(0x1ffff << 8 | 31 << 26)) | (phase & 0x1ffff) << 8 | (off as u32) << 26;
        self.regs[5] = ileft;
        self.regs[7] = oleft;
        240 * o as u64 + 440 * o as u64 * ch as u64 + 40 * taken
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

    /// mono: x at 0x1000, y at 0x8000, history at 0x10000
    fn mono(u: &mut HwSrc, ram: &mut Ram, in_rate: u32, out_rate: u32, x: &[i16], nout: u32) -> Vec<i16> {
        for (k, v) in x.iter().enumerate() {
            let w = &mut ram.0[0x1000 / 4 + k / 2];
            let sh = (k % 2) * 16;
            *w = (*w & !(0xffff << sh)) | ((*v as u16 as u32) << sh);
        }
        let con = u.read(BASE, 1 << 40) & (0x1ffff << 8 | 31 << 26);
        for (r, v) in [(8, 0x10000), (1, out_rate << 16 | in_rate), (2, 1 << 16 | 1), (3, 1 << 16 | 1),
                       (7, nout), (6, 0x8000), (5, x.len() as u32), (4, 0x1000)] {
            u.write(ram, BASE + 4 * r, v, 0);
        }
        u.write(ram, BASE, con | 0x40, 0);
        u.write(ram, BASE, con | 8, 0);
        assert_ne!(u.read(BASE, 1 << 40) & 0x80, 0);
        (0..nout - u.regs[7]).map(|k| (ram.0[0x8000 / 4 + k as usize / 2] >> ((k % 2) * 16)) as u16 as i16).collect()
    }

    #[test]
    fn one_to_one_is_the_kernel_at_its_taps() {
        // the FM-1 (src/impulse_1to1): 10000 at input 20 comes out as ..., 86, 9912, 86, ... centred 13 later
        let mut ram = Ram(vec![0; 0x5000]);
        let mut x = [0i16; 64];
        x[20] = 10000;
        let y = mono(&mut HwSrc::default(), &mut ram, 48000, 48000, &x, 80);
        assert_eq!(y.len(), 64);
        // (within the model's 1 LSB)
        for (a, b) in y[31..36].iter().zip([-84, 86, 9912, 86, -84]) {
            assert!((*a as i32 - b).abs() <= 1, "{:?}", &y[31..36]);
        }
        assert_eq!(y[20], 0);
    }

    #[test]
    fn the_phase_counts_exactly_and_the_stream_goes_on() {
        // the FM-1: 600 inputs at 44100 -> 48000 give 654 outputs and leave phase 41400 (0xa1b8)
        let mut ram = Ram(vec![0; 0x5000]);
        let x: Vec<i16> = (0..600).map(|k| ((k * 7919) % 16001) as i16 - 8000).collect();
        let mut u = HwSrc::default();
        let whole = mono(&mut u, &mut ram, 44100, 48000, &x, 700);
        assert_eq!(whole.len(), 654);
        assert_eq!((u.regs[0] >> 8) & 0x1ffff, 0xa1b8);
        // in two calls, the second going on from the first's phase, offset and history (from silence:
        // a fresh RAM, the history in it zero)
        let mut ram = Ram(vec![0; 0x5000]);
        let mut u = HwSrc::default();
        let mut parts = mono(&mut u, &mut ram, 44100, 48000, &x[..250], 700);
        parts.extend(mono(&mut u, &mut ram, 44100, 48000, &x[250..], 700));
        assert_eq!(parts, whole);
    }
}
