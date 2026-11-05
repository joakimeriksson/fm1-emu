//! The AC79's FFT / vector unit (JL_FFT at 0x1EE0400: CON, CADR, CONST, TEST1), from JieLi's AC79 SDK
//! (`apps/common/jl_math/jl_fft.c`, `fftvec_math_drv.h`) and measured on an FM-1 (fm1-dock's
//! probes/hwdsp.c, `tools/hwdsp.py`: tests/hwdsp/fm1.txt is what the chip returned).
//!
//! A job is a descriptor in RAM that CADR points at; writing CON with bit 0 (en) runs it, bit 3 picks
//! the vector unit over the FFT, bits 8.. the number of descriptors in a row (FFT: 16 bytes each),
//! bit 6 clears done (the driver writes it with en, and again after); bit 7 reads done. When a job
//! is done en reads 0 and bit 16 reads 1 (it stays: CON reads 0x10100 between jobs).
//!   FFT descriptor    {con, iadr, oadr, 0}; con: bits 16-27 N, bit 1 real, bit 2 inverse, bit 0 in place,
//!                     bits 8-11 log2, 12 radix 3, 13 radix 5, 4-5 "fft4 /4" stages, 28-31 last division.
//!   vector descriptor {con, xadr, yadr, zadr, config0 = q << 24 | nloop << 12 | nlen,
//!                     config1 = z_inc << 20 | y_inc << 10 | x_inc (bytes, between loops), config2, 0};
//!                     CONST the constant.
//!
//! The FFT (measured, 2026-10-28): forward unscaled, inverse divided by 2^floor(log2 N) (so the SDK
//! header's "res x 1.875" for N = 120: 120 / 64), outputs in 29 bits (they wrap into +-2^28); the
//! real inverse takes bin 0's imaginary part as minus the Nyquist bin's real part and the Nyquist
//! bin's imaginary part as minus bin 0's real part (DC' = Re X0 - Im XN/2, Nyquist' = Re XN/2 - Im X0).
//! NOT BIT-EXACT: the chip's fixed-point stages land within 1-3 LSB of the exact transform rounded,
//! which is what this computes; a stage that overflows inside (outputs near 2^28) gives other values.
//! Cycles: the FM-1's, per size.
//!
//! The vector unit (measured, bit-exact on every case of tools/hwdsp.py's plan inside the SDK's rule
//! y width >= z width > x width): `/ 2^q` rounds half away from zero (q < 0 shifts left); add and
//! subtract wrap at 32 bits and saturate narrower; products saturate to z; dot products accumulate
//! in 64 bits (wrapping) before the rounding; elements are packed at their width. CONST: with the
//! multiply mode z = x +- sat32(y * CONST / 2^q) (wrapping at 32 bits: mac / msc_const), with the
//! square mode x * CONST / 2^q (mul_const); max / min compare x with CONST, then / 2^q. Unknown:
//! add with accumulate (the SDK's add_dot_product: it writes the unit's accumulator from an earlier
//! job, here 0) and 64-bit outputs of sums.

/// The memory a job reads and writes (the SoC's bus).
pub trait Mem {
    fn rd(&mut self, a: u32) -> u32;
    fn wr(&mut self, a: u32, v: u32);
}

pub const BASE: u32 = 0x1ee0400;
pub const END: u32 = 0x1ee040f;

#[derive(Default, Clone, Debug)]
pub struct HwDsp {
    pub con: u32,
    pub cadr: u32,
    pub cnst: u32,
    pub test1: u32,
    /// a job finished and done not yet cleared
    pending: bool,
    /// a job ran (CON then reads en 0, bit 16 1)
    ran: bool,
    /// the cycle the last job finishes at (done reads 0 until then)
    done_at: u64,
    pub jobs: u64,
}

impl HwDsp {
    pub fn read(&self, addr: u32, cycles: u64) -> u32 {
        match addr {
            0x1ee0400 => {
                let busy = cycles < self.done_at;
                let con = if self.ran && !busy { (self.con & !1) | 0x1_0000 } else { self.con };
                (con & !0xc0) | if self.pending && !busy { 0x80 } else { 0 }
            }
            0x1ee0404 => self.cadr,
            0x1ee0408 => self.cnst,
            0x1ee040c => self.test1,
            _ => 0,
        }
    }

    pub fn write(&mut self, m: &mut impl Mem, addr: u32, v: u32, cycles: u64) {
        match addr {
            0x1ee0400 => {
                if v & 0x40 != 0 {
                    self.pending = false;
                }
                // en starts a job; the clear after one, `CON |= 1 << 6`, reads en back as 0
                let start = v & 1 != 0 && v & 0x80 == 0;
                self.con = v & !0xc0;
                if start {
                    let n = ((v >> 8) & 0xff).max(1);
                    let mut cost = 0u64;
                    for i in 0..n {
                        cost += if v & 8 != 0 {
                            vector(m, self.cadr.wrapping_add(32 * i), self.cnst)
                        } else {
                            fft_job(m, self.cadr.wrapping_add(16 * i))
                        };
                    }
                    self.jobs += n as u64;
                    self.pending = true;
                    self.ran = true;
                    self.done_at = cycles + cost;
                }
            }
            0x1ee0404 => self.cadr = v,
            0x1ee0408 => self.cnst = v,
            0x1ee040c => self.test1 = v,
            _ => {}
        }
    }
}

// ---- FFT ----------------------------------------------------------------------------------

/// (N, real, inverse) of an FFT con word
pub fn fft_shape(con: u32) -> (usize, bool, bool) {
    (((con >> 16) & 0xfff) as usize, con & 2 != 0, con & 4 != 0)
}

/// words read and written: complex 2N in and out; real forward N in, N + 2 out (bins 0..N/2);
/// real inverse N + 2 in, N out
pub fn fft_words(con: u32) -> (usize, usize) {
    let (n, real, inv) = fft_shape(con);
    match (real, inv) {
        (false, _) => (2 * n, 2 * n),
        (true, false) => (n, n + 2),
        (true, true) => (n + 2, n),
    }
}

/// cycles a job takes: the FM-1's (TIMER4 ticks x 10, the probe's poll included), else interpolated
pub fn fft_cycles(con: u32) -> u64 {
    let (n, real, _) = fft_shape(con);
    let known: &[(usize, bool, u64)] = &[
        (20, false, 187), (64, false, 620), (120, false, 1682), (128, false, 1628), (240, false, 3196),
        (256, false, 3114), (512, false, 7900), (1024, false, 15402),
        (20, true, 171), (64, true, 470), (120, true, 886), (128, true, 845), (240, true, 2091),
        (256, true, 2068), (512, true, 3980), (1024, true, 9620),
    ];
    if let Some(&(_, _, c)) = known.iter().find(|k| k.0 == n && k.1 == real) {
        return c;
    }
    // (about 1.5 cycles per point and stage complex, 0.9 real)
    let lg = (usize::BITS - n.max(2).leading_zeros()) as u64;
    let per = if real { 9 } else { 15 };
    (n as u64 * lg * per / 10).max(150)
}

/// a value as the unit's 29-bit datapath leaves it: rounded, wrapped into +-2^28
fn out29(x: f64) -> i32 {
    let r = x.round() as i64;
    (((r + (1 << 28)).rem_euclid(1 << 29)) - (1 << 28)) as i32
}

/// the DFT of any N with small prime factors (mixed radix, recursive), sign -1 forward
pub fn dft(x: &[(f64, f64)], inverse: bool) -> Vec<(f64, f64)> {
    let n = x.len();
    if n <= 1 {
        return x.to_vec();
    }
    let p = (2..=n).find(|p| n % p == 0).unwrap();
    let m = n / p;
    let subs: Vec<Vec<(f64, f64)>> = (0..p)
        .map(|r| dft(&(0..m).map(|k| x[k * p + r]).collect::<Vec<_>>(), inverse))
        .collect();
    let s = if inverse { 1.0 } else { -1.0 };
    let mut out = vec![(0.0, 0.0); n];
    for (k, o) in out.iter_mut().enumerate() {
        let (mut re, mut im) = (0.0, 0.0);
        for (r, sub) in subs.iter().enumerate() {
            let a = s * 2.0 * std::f64::consts::PI * (r * k) as f64 / n as f64;
            let (c, sn) = (a.cos(), a.sin());
            let (fr, fi) = sub[k % m];
            re += fr * c - fi * sn;
            im += fr * sn + fi * c;
        }
        *o = (re, im);
    }
    out
}

/// the transform of a con word on its input words (fft_words' lengths)
pub fn fft(con: u32, input: &[i32]) -> Vec<i32> {
    let (n, real, inv) = fft_shape(con);
    if n == 0 {
        return vec![];
    }
    // the inverse divides by the power of two at or under N
    let scale = if inv { 1.0 / (1u64 << (usize::BITS - 1 - n.leading_zeros())) as f64 } else { 1.0 };
    let x: Vec<(f64, f64)> = match (real, inv) {
        (false, _) => (0..n).map(|k| (input[2 * k] as f64, input[2 * k + 1] as f64)).collect(),
        (true, false) => (0..n).map(|k| (input[k] as f64, 0.0)).collect(),
        (true, true) => {
            let h = n / 2;
            let (dc, ny) = (
                input[0] as f64 - input[2 * h + 1] as f64,
                input[2 * h] as f64 - input[1] as f64,
            );
            (0..n)
                .map(|k| match k {
                    0 => (dc, 0.0),
                    _ if k == h && n % 2 == 0 => (ny, 0.0),
                    _ if k <= h => (input[2 * k] as f64, input[2 * k + 1] as f64),
                    _ => (input[2 * (n - k)] as f64, -(input[2 * (n - k) + 1] as f64)),
                })
                .collect()
        }
    };
    let y = dft(&x, inv);
    match (real, inv) {
        (false, _) => y.iter().flat_map(|&(r, i)| [out29(r * scale), out29(i * scale)]).collect(),
        (true, false) => y[..=n / 2].iter().flat_map(|&(r, i)| [out29(r), out29(i)]).collect(),
        (true, true) => y.iter().map(|&(r, _)| out29(r * scale)).collect(),
    }
}

fn fft_job(m: &mut impl Mem, d: u32) -> u64 {
    let con = m.rd(d);
    let (iadr, oadr) = (m.rd(d.wrapping_add(4)), m.rd(d.wrapping_add(8)));
    let (ni, no) = fft_words(con);
    let input: Vec<i32> = (0..ni).map(|k| m.rd(iadr.wrapping_add(4 * k as u32)) as i32).collect();
    let out = fft(con, &input);
    let at = if con & 1 != 0 { iadr } else { oadr };
    for (k, v) in out.iter().enumerate().take(no) {
        m.wr(at.wrapping_add(4 * k as u32), *v as u32);
    }
    fft_cycles(con)
}

// ---- the vector unit ----------------------------------------------------------------------

pub const VEC_SUB: u32 = 1 << 4;
pub const VEC_ACC: u32 = 1 << 5;
pub const VECX_CONJ: u32 = 1 << 12;
pub const VECY_CONJ: u32 = 1 << 13;
pub const VEC_QDT: u32 = 1 << 14;
pub const VEC_MAC32: u32 = 1 << 15;
pub const VEC_RES_NEG_ZERO: u32 = 1 << 16;
pub const VEC_CONST_MUL: u32 = 1 << 17;
pub const VEC_CPX_QDT: u32 = 1 << 18;
pub const VEC_MAX: u32 = 1 << 19;
pub const VEC_MIN: u32 = 1 << 20;

/// bytes of an element of width code w (0 s8, 1 s16, 2 s32, 3 s64)
fn wbytes(w: u32) -> u32 {
    1 << w
}

fn load(m: &mut impl Mem, base: u32, w: u32, i: u32) -> i64 {
    let a = base.wrapping_add(i.wrapping_mul(wbytes(w)));
    match w {
        0 => (m.rd(a & !3) >> ((a & 3) * 8)) as u8 as i8 as i64,
        1 => (m.rd(a & !3) >> ((a & 2) * 8)) as u16 as i16 as i64,
        2 => m.rd(a) as i32 as i64,
        _ => ((m.rd(a.wrapping_add(4)) as u64) << 32 | m.rd(a) as u64) as i64,
    }
}

fn store(m: &mut impl Mem, base: u32, w: u32, i: u32, v: i64) {
    let a = base.wrapping_add(i.wrapping_mul(wbytes(w)));
    match w {
        0 | 1 => {
            let sh = (a & 3) * 8;
            let mask = if w == 0 { 0xffu32 } else { 0xffff } << sh;
            let cur = m.rd(a & !3);
            m.wr(a & !3, (cur & !mask) | ((v as u32) << sh) & mask);
        }
        2 => m.wr(a, v as u32),
        _ => {
            m.wr(a, v as u32);
            m.wr(a.wrapping_add(4), (v >> 32) as u32);
        }
    }
}

/// saturated to width w
fn sat(v: i128, w: u32) -> i64 {
    let b = 8u32 << w;
    let (lo, hi) = (-(1i128 << (b - 1)), (1i128 << (b - 1)) - 1);
    v.clamp(lo, hi) as i64
}

/// wrapped to width w
fn wrap(v: i128, w: u32) -> i64 {
    let b = 8u32 << w;
    if b >= 64 {
        return v as i64;
    }
    let m = 1i128 << b;
    ((v + m / 2).rem_euclid(m) - m / 2) as i64
}

/// x / 2^q as the unit does it: rounded half away from zero; q < 0 a left shift
fn rnd(x: i128, q: i32) -> i128 {
    if q <= 0 {
        return x << (-q).min(64);
    }
    let a = (x.abs() + (1i128 << (q - 1))) >> q.min(120);
    if x < 0 { -a } else { a }
}

fn vector(m: &mut impl Mem, d: u32, cnst: u32) -> u64 {
    let con = m.rd(d);
    let (xa, ya, za) = (m.rd(d.wrapping_add(4)), m.rd(d.wrapping_add(8)), m.rd(d.wrapping_add(12)));
    let (c0, c1) = (m.rd(d.wrapping_add(16)), m.rd(d.wrapping_add(20)));
    let q = (c0 >> 24) as i8 as i32;
    let nloop = ((c0 >> 12) & 0xfff).max(1);
    let nlen = c0 & 0xfff;
    let (xi, yi, zi) = (c1 & 0x3ff, (c1 >> 10) & 0x3ff, (c1 >> 20) & 0x3ff);
    let (wx, wy, wz) = ((con >> 6) & 3, (con >> 8) & 3, (con >> 10) & 3);
    let mode = con & 3;
    let k = cnst as i32 as i128;
    let sub = con & VEC_SUB != 0;
    let accum = con & VEC_ACC != 0;
    let cpx = mode == 2 || (mode == 3 && con & VEC_CPX_QDT != 0);
    let count = if mode == 2 { nlen } else if cpx { nlen / 2 } else { nlen };
    for l in 0..nloop {
        let (x0, y0, z0) = (xa.wrapping_add(l * xi), ya.wrapping_add(l * yi), za.wrapping_add(l * zi));   // (l < 4096, inc < 1024)
        let (mut acc, mut acc_i) = (0i128, 0i128);
        for i in 0..count {
            if mode == 2 {
                let (xr, mut xim) = (load(m, x0, wx, 2 * i) as i128, load(m, x0, wx, 2 * i + 1) as i128);
                let (yr, mut yim) = (load(m, y0, wy, 2 * i) as i128, load(m, y0, wy, 2 * i + 1) as i128);
                if con & VECX_CONJ != 0 {
                    xim = -xim;
                }
                if con & VECY_CONJ != 0 {
                    yim = -yim;
                }
                let (re, im) = (yr * xr - yim * xim, yr * xim + yim * xr);
                if accum {
                    acc += re;
                    acc_i += im;
                    continue;
                }
                let (mut r, mut s) = (rnd(re, q), rnd(im, q));
                if con & VEC_MAC32 != 0 {
                    let (zr, zim) = (load(m, z0, wz, 2 * i) as i128, load(m, z0, wz, 2 * i + 1) as i128);
                    (r, s) = if sub { (zr - r, zim - s) } else { (zr + r, zim + s) };
                }
                store(m, z0, wz, 2 * i, sat(r, wz));
                store(m, z0, wz, 2 * i + 1, sat(s, wz));
                continue;
            }
            let x = load(m, x0, wx, if cpx { 2 * i } else { i }) as i128;
            let r: i128 = match mode {
                0 => {
                    let y = load(m, y0, wy, i) as i128;
                    if accum {
                        continue; // (the SDK's add_dot_product: unknown, see the head)
                    }
                    let v = if sub { y - x } else { y + x };
                    (if wz == 2 { wrap(v, wz) } else { sat(v, wz) }) as i128
                }
                1 => {
                    let y = load(m, y0, wy, i) as i128;
                    if con & VEC_CONST_MUL != 0 {
                        let t = sat(rnd(y * k, q), 2) as i128;
                        let v = if sub { x - t } else { x + t };
                        (if wz == 2 { wrap(v, wz) } else { sat(v, wz) }) as i128
                    } else if accum {
                        acc += y * x;
                        continue;
                    } else {
                        let mut r = rnd(y * x, q);
                        if con & VEC_MAC32 != 0 {
                            let z = load(m, z0, wz, i) as i128;
                            r = if sub { z - r } else { z + r };
                        }
                        sat(r, wz) as i128
                    }
                }
                _ => {
                    let p = if con & (VEC_MAX | VEC_MIN) != 0 {
                        if con & VEC_MAX != 0 { x.max(k) } else { x.min(k) }
                    } else if cpx {
                        let xim = load(m, x0, wx, 2 * i + 1) as i128;
                        x * x + xim * xim
                    } else if con & VEC_QDT != 0 {
                        if con & VEC_CONST_MUL != 0 { x * k } else { x * x }
                    } else {
                        x
                    };
                    if accum {
                        acc += p;
                        continue;
                    }
                    sat(rnd(p, q), wz) as i128
                }
            };
            let r = if con & VEC_RES_NEG_ZERO != 0 && r < 0 { 0 } else { r };
            store(m, z0, wz, i, r as i64);
        }
        if accum {
            if mode == 2 {
                // (64 bits, wrapping, as the real dot product's: measured there; the complex one's
                // cases never came near overflowing)
                store(m, z0, wz, 0, sat(rnd(wrap(acc, 3) as i128, q), wz));
                store(m, z0, wz, 1, sat(rnd(wrap(acc_i, 3) as i128, q), wz));
            } else if mode == 0 {
                store(m, z0, wz, 0, 0);
            } else {
                store(m, z0, wz, 0, sat(rnd(wrap(acc, 3) as i128, q), wz));
            }
        }
    }
    let per = if con & VEC_MAC32 != 0 { 45 } else { 33 };
    30 + (nlen as u64) * (nloop as u64) * per / 10
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

    fn con(n: u32, real: bool, inv: bool) -> u32 {
        (n << 16) | (real as u32) << 1 | (inv as u32) << 2
    }

    #[test]
    fn real_fft_of_an_impulse_is_flat_and_inverts() {
        let mut x = vec![0i32; 256];
        x[0] = 1000;
        let y = fft(con(256, true, false), &x);
        assert_eq!(y.len(), 258);
        assert!(y.chunks(2).all(|b| b == [1000, 0]));
        let back = fft(con(256, true, true), &y);
        assert_eq!(back, x);
    }

    #[test]
    fn the_inverse_divides_by_a_power_of_two() {
        // N = 120: by 64, the SDK header's "res x 1.875" (measured)
        let x: Vec<i32> = (0..240).map(|k| ((k * 7919) % 2001) as i32 - 1000).collect();
        let back = fft(con(120, false, true), &fft(con(120, false, false), &x));
        for (a, b) in back.iter().zip(&x) {
            assert!((*a as f64 - *b as f64 * 120.0 / 64.0).abs() <= 1.0, "{a} {b}");
        }
    }

    #[test]
    fn the_real_inverse_takes_im_x0_as_the_nyquist_bin() {
        // the FM-1: an impulse of 2^20 at word 1 (bin 0's imaginary part) of REAL_IFFT64
        let mut x = vec![0i32; 66];
        x[1] = 1 << 20;
        let y = fft(con(64, true, true), &x);
        assert_eq!(&y[..4], &[-16384, 16384, -16384, 16384]);
    }

    #[test]
    fn outputs_wrap_into_29_bits() {
        let mut x = vec![0i32; 64];
        x[0] = 300_000_000;
        assert_eq!(fft(con(64, true, false), &x)[0], 300_000_000 - (1 << 29));
    }

    #[test]
    fn a_job_runs_from_its_descriptor_and_reads_done() {
        let mut ram = Ram(vec![0; 4096]);
        let d = 0x100;
        ram.0[d / 4] = con(64, true, false);
        ram.0[d / 4 + 1] = 0x1000;
        ram.0[d / 4 + 2] = 0x2000;
        ram.0[0x1000 / 4] = 5;
        let mut u = HwDsp::default();
        u.write(&mut ram, 0x1ee0404, d as u32, 0);
        u.write(&mut ram, 0x1ee0400, (1 << 8) | (1 << 6) | 1, 0);
        assert_eq!(u.read(0x1ee0400, 0) & 0x80, 0, "busy for its cycles");
        assert_eq!(u.read(0x1ee0400, 100_000), 0x10180, "done: en 0, bit 16, as the FM-1 reads it");
        assert_eq!(ram.0[0x2000 / 4], 5);
        assert_eq!(ram.0[0x2000 / 4 + 64], 5, "bin 32");
        u.write(&mut ram, 0x1ee0400, u.read(0x1ee0400, 100_000) | 0x40, 100_000);
        assert_eq!(u.read(0x1ee0400, 100_000), 0x10100, "cleared, nothing started");
    }

    fn vjob(ram: &mut Ram, u: &mut HwDsp, words: [u32; 8]) {
        for (i, w) in words.iter().enumerate() {
            ram.0[0x40 + i] = *w;
        }
        u.write(ram, 0x1ee0404, 0x100, 0);
        u.write(ram, 0x1ee0400, (1 << 8) | (1 << 6) | (1 << 3) | 1, 0);
    }

    #[test]
    fn vector_rounds_half_away_from_zero_and_wraps_adds() {
        let mut ram = Ram(vec![0; 4096]);
        let mut u = HwDsp::default();
        let w32 = (2 << 6) | (2 << 8) | (2 << 10);
        // the FM-1: -56 * 5 / 16 = -17.5 -> -18 (vec/real_mul_round)
        ram.0[0x400 / 4] = -56i32 as u32;
        ram.0[0x800 / 4] = 5;
        vjob(&mut ram, &mut u, [w32 | 1, 0x400, 0x800, 0xc00, (4 << 24) | (1 << 12) | 1, 0, 0, 0]);
        assert_eq!(ram.0[0xc00 / 4] as i32, -18);
        // 32-bit adds wrap (vec/real_add_sat)
        ram.0[0x400 / 4] = -2146266987i32 as u32;
        ram.0[0x800 / 4] = -2144644789i32 as u32;
        vjob(&mut ram, &mut u, [w32, 0x400, 0x800, 0xc00, (1 << 12) | 1, 0, 0, 0]);
        assert_eq!(ram.0[0xc00 / 4] as i32, 4055520);
    }
}
