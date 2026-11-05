//! The FFT / vector unit's model (src/hwdsp.rs), the EQ's (src/hweq.rs) and the sample-rate converter's
//! (src/hwsrc.rs) against what an FM-1 returned for the same jobs
//! (tests/hwdsp/fm1.txt: tools/hwdsp.py run --on fm1, then export): every vector case inside the SDK's rule
//! (z no narrower than x) bit for bit, every FFT case whose outputs stay in range within 3 LSB.
//! The inputs are made again from the cases' seeds (fm1-dock's probes/hwdsp.c fill(), hwdsp.py gen()).
use fm1_soc::hwdsp::{HwDsp, Mem};
use fm1_soc::hweq::{self, HwEq};
use fm1_soc::hwsrc::{self, HwSrc};

const WORDS: usize = 2048 + 8;
const X: u32 = 0x1_0000;
const Y: u32 = 0x2_1000;
const Z: u32 = 0x3_2000;
const D: u32 = 0x100;

struct Ram(Vec<u32>);
impl Mem for Ram {
    fn rd(&mut self, a: u32) -> u32 {
        self.0[(a / 4) as usize]
    }
    fn wr(&mut self, a: u32, v: u32) {
        self.0[(a / 4) as usize] = v;
    }
}

fn gen(g: u32, amp: u32, p: u32) -> Vec<u32> {
    let mut x = if p != 0 { p } else { 1 };
    let amp = amp as i32;
    (0..WORDS)
        .map(|i| {
            let v: i32 = match g {
                1 => if i as u32 == p { amp } else { 0 },
                2 => amp,
                3 | 5 => {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    if g == 5 {
                        x as i32
                    } else if amp != 0 {
                        ((x >> 1) % (2 * amp as u32 + 1)) as i32 - amp
                    } else {
                        0
                    }
                }
                4 => (i as i32).wrapping_mul(amp).wrapping_add(p as i32),
                _ => 0,
            };
            v as u32
        })
        .collect()
}

/// (id, args, out) of each line of tests/hwdsp/fm1.txt (`id | args | ticks | con | out`)
fn cases(text: &str) -> Vec<(String, Vec<u32>, Vec<u32>)> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split(" | ").collect();
            let args = f[1].split(',').map(|t| t.trim().parse::<i64>().unwrap() as u32).collect();
            let out = f.get(4).map_or(vec![], |o| o.split_whitespace().map(|w| u32::from_str_radix(w, 16).unwrap()).collect());
            (f[0].to_string(), args, out)
        })
        .collect()
}

fn run(args: &[u32]) -> Vec<u32> {
    let mut ram = Ram(vec![0; 0x5_0000 / 4]);
    let mut u = HwDsp::default();
    let put = |ram: &mut Ram, at: u32, w: &[u32]| {
        for (k, v) in w.iter().enumerate() {
            ram.0[(at / 4) as usize + k] = *v;
        }
    };
    let (vector, off, n) = if args[0] == 1 {
        put(&mut ram, X, &gen(args[2], args[3], args[4]));
        let same = args[1] & 1 != 0;
        put(&mut ram, D, &[args[1], X, if same { X } else { Z }, 0]);
        (false, args[5], args[6])
    } else {
        put(&mut ram, X, &gen(args[6], args[7], args[8]));
        put(&mut ram, Y, &gen(args[9], args[10], args[11]));
        put(&mut ram, Z, &gen(args[12], args[13], args[14]));
        put(&mut ram, D, &[args[1], X, Y, Z, args[2], args[3], args[4], 0]);
        u.write(&mut ram, 0x1ee0408, args[5], 0);
        (true, args[15], args[16])
    };
    u.write(&mut ram, 0x1ee0404, D, 0);
    u.write(&mut ram, 0x1ee0400, (1 << 8) | (1 << 6) | ((vector as u32) << 3) | 1, 0);
    let at = if !vector && args[1] & 1 != 0 { X } else { Z };
    (0..n).map(|k| ram.rd(at + 4 * (off + k))).collect()
}

/// an EQ run as fm1-dock's probes/hweq.c programs it: its output words
fn run_eq(a: &[u32]) -> Vec<u32> {
    const IN: u32 = 0x1_0000;
    const OUT: u32 = 0x2_0000;
    const FLT: u32 = 0x3_8000;
    let mut ram = Ram(vec![0; 0x4_0000 / 4]);
    let mut eq = HwEq::new();
    let fmt = a[1];
    let (nsec, inm, outm, runm) = (fmt & 63, (fmt >> 8) & 3, (fmt >> 10) & 3, (fmt >> 12) & 3);
    let (dim, dom, cpu_only) = ((fmt >> 14) & 1, (fmt >> 15) & 1, (fmt >> 24) & 1);
    let ch = ((fmt >> 16) & 15).max(1);
    let och = if (fmt >> 20) & 15 == 0 { ch } else { (fmt >> 20) & 15 };
    let n = a[2];
    let (ib, ob) = (if inm != 0 { 4 } else { 2 }, if outm != 0 { 4 } else { 2 });
    let x = gen(a[3], a[4], a[5]);
    for i in 0..(n * ch) as usize {
        let v = x.get(i).copied().unwrap_or(0) as i32;
        match inm {
            0 => {
                let w = &mut ram.0[(IN / 4) as usize + i / 2];
                let sh = (i % 2) * 16;
                *w = (*w & !(0xffff << sh)) | ((v as u32 & 0xffff) << sh);
            }
            1 => ram.0[(IN / 4) as usize + i] = v as u32,
            _ => ram.0[(IN / 4) as usize + i] = (v as f32).to_bits(),
        }
    }
    let (nslots, given, marker) = (a[8] & 0xff, ((a[8] >> 8) & 0xff).max(1), (a[8] >> 16) & 0xff);
    for i in 0..8 * nslots {
        let g = if marker != 0 { (i / 8 == marker - 1) as u32 } else { i / 8 % given.min(nslots) };
        let w = a[9 + (g * 8 + i % 8) as usize];
        if cpu_only != 0 {
            ram.0[(FLT / 4 + i) as usize] = w;
        } else if i < hweq::RAM_WORDS {
            eq.write(&mut ram, hweq::RAM + 4 * i, w, 0);
        } else {
            ram.0[(FLT / 4 + i - hweq::RAM_WORDS) as usize] = w;
        }
    }
    let in_step = if dim != 0 { ch } else { 1 } * ib;
    let out_step = if dom != 0 { och } else { 1 } * ob;
    let chl_in = if runm == 1 { 0 } else if dim != 0 { ib } else { in_step * n };
    let chl_out = if dom != 0 { ob } else { out_step * n };
    let con0 = ((och - 1) << 8) | (runm << 2) | (inm << 4) | (outm << 6) | ((nsec - 1) << 16) | a[7];
    for (r, v) in [
        (0, 0), (7, FLT), (0, con0), (1, chl_out << 16 | chl_in),
        (2, ((chl_out << 16).wrapping_mul(och - 1)) | chl_in * (ch - 1)), (3, out_step << 16 | in_step),
        (4, IN), (5, OUT), (6, n - 1), (0, con0 | 1 << 30), (0, con0 | 3),
    ] {
        eq.write(&mut ram, hweq::BASE + 4 * r, v, 0);
    }
    (0..a[6]).map(|k| ram.rd(OUT + 4 * k)).collect()
}

/// the words as values: f32, or integers of the output format (s16: two a word)
fn eq_values(fmt: u32, w: &[u32]) -> Vec<f64> {
    match (fmt >> 10) & 3 {
        2 => w.iter().map(|v| f32::from_bits(*v) as f64).collect(),
        1 => w.iter().map(|v| *v as i32 as f64).collect(),
        _ => w.iter().flat_map(|v| [*v as u16 as i16 as f64, (*v >> 16) as u16 as i16 as f64]).collect(),
    }
}

#[test]
fn the_eq_model_returns_what_the_fm1_did() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/hwdsp/fm1.txt");
    let text = std::fs::read_to_string(path).expect("tests/hwdsp/fm1.txt");
    let (mut exact, mut near, mut bad) = (0, 0, vec![]);
    for (id, args, hw) in cases(&text) {
        // (state2: word 7 of the chip's state is not known)
        if !id.starts_with("eq/") || args[0] != 1 || id.ends_with("state2") {
            continue;
        }
        let (m, h) = (eq_values(args[1], &run_eq(&args)), eq_values(args[1], &hw));
        let scale = h.iter().fold(1.0f64, |a, v| a.max(v.abs()));
        let d = m.iter().zip(&h).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        let float = (args[1] >> 10) & 3 == 2;
        if d == 0.0 {
            exact += 1;
        } else if (float && d <= 2e-7 * scale) || (!float && d <= 1.0) {
            near += 1;
        } else {
            bad.push(format!("{id}: off by {d} (of {scale})"));
        }
    }
    eprintln!("hweq vs the FM-1: {exact} runs bit-exact, {near} within a few f32 ulp (or 1 LSB)");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    assert!(exact + near >= 90, "too few cases: {exact} + {near}");
}

/// an SRC run (or runs, going on from each other) as fm1-dock's probes/hwsrc.c programs it: the output
/// window's halfwords, and each run's CON0 when done and what is left of the input and output
fn run_src(a: &[u32]) -> (Vec<i16>, Vec<(u32, u32, u32)>) {
    const IN: u32 = 0x1_0000;
    const OUT: u32 = 0x2_0000;
    const HIST: u32 = 0x3_0000;
    let mut ram = Ram(vec![0; 0x4_0000 / 4]);
    let mut u = HwSrc::default();
    let f = a[1];
    let (ch, inter, calls) = ((f & 15).max(1), (f >> 8) & 1, ((f >> 12) & 15).max(1));
    let (in_rate, out_rate, nin, nout) = (a[2], a[3], a[4], a[5]);
    let (mut phase, mut offset, out_n, out_off) = (a[9], a[10], a[11], a.get(12).copied().unwrap_or(0));
    let x = gen(a[6], a[7], a[8]);
    let x: Vec<u32> = if (nin * ch) as usize > x.len() {
        // (gen makes WORDS; the probe draws on, the same sequence)
        let mut v = x;
        let mut more = vec![];
        let (mut s, g, amp, p) = (if a[8] != 0 { a[8] } else { 1 }, a[6], a[7] as i32, a[8]);
        for i in 0..(nin * ch) as usize {
            let w: i32 = match g {
                1 => if i as u32 == p { amp } else { 0 },
                2 => amp,
                3 | 5 => {
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    if g == 5 { s as i32 } else if amp != 0 { ((s >> 1) % (2 * amp as u32 + 1)) as i32 - amp } else { 0 }
                }
                4 => (i as i32).wrapping_mul(amp).wrapping_add(p as i32),
                _ => 0,
            };
            more.push(w as u32);
        }
        v.clear();
        v.extend(more);
        v
    } else {
        x
    };
    for i in 0..(nin * ch) as usize {
        let w = &mut ram.0[(IN / 4) as usize + i / 2];
        let sh = (i % 2) * 16;
        *w = (*w & !(0xffff << sh)) | ((x[i] & 0xffff) << sh);
    }
    // (the probe's whole output buffer: 16000 + 64 halfwords)
    for i in 0..(16000 + 64) / 2 {
        ram.0[(OUT / 4) as usize + i] = 0x5a5a_5a5a;
    }
    let (in_sp, in_ch) = if inter != 0 { (ch, 1) } else { (1, nin) };
    let (out_sp, out_ch) = if inter != 0 { (ch, 1) } else { (1, nout) };
    let (mut in_at, mut out_at, mut runs) = (0u32, 0u32, vec![]);
    for k in 0..calls {
        let take = if k + 1 < calls { nin / calls } else { nin - in_at };
        let at_in = IN + 2 * if inter != 0 { in_at * ch } else { in_at };
        let at_out = OUT + 2 * if inter != 0 { out_at * ch } else { out_at };
        let con = phase << 8 | offset << 26 | (ch - 1);
        for (r, v) in [(0, 0), (8, HIST), (0, con), (1, out_rate << 16 | in_rate), (2, in_ch << 16 | in_sp),
                       (3, out_ch << 16 | out_sp), (7, nout - out_at), (6, at_out), (5, take), (4, at_in),
                       (0, con | 0x40), (0, con | 8)] {
            u.write(&mut ram, hwsrc::BASE + 4 * r, v, 0);
        }
        let done = u.read(hwsrc::BASE, 1 << 40);
        let (il, ol) = (u.read(hwsrc::BASE + 20, 0), u.read(hwsrc::BASE + 28, 0));
        offset = (done >> 26) & 31;
        phase = (done >> 8) & 0x1ffff;
        in_at += take - il;
        out_at = nout - ol;
        runs.push((done, il, ol));
    }
    let out = (out_off..out_off + out_n)
        .map(|i| (ram.rd(OUT + 2 * i & !3) >> ((2 * i & 2) * 8)) as u16 as i16)
        .collect();
    (out, runs)
}

#[test]
fn the_src_model_returns_what_the_fm1_did() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/hwdsp/fm1.txt");
    let text = std::fs::read_to_string(path).expect("tests/hwdsp/fm1.txt");
    let (mut exact, mut near, mut bad) = (0, 0, vec![]);
    for line in text.lines().filter(|l| l.starts_with("src/")) {
        let f: Vec<&str> = line.split(" | ").collect();
        let args: Vec<u32> = f[1].split(',').map(|t| t.trim().parse::<i64>().unwrap() as u32).collect();
        if args[0] != 1 {
            continue;
        }
        let done: Vec<u32> = f[3].split_whitespace().map(|w| u32::from_str_radix(w.trim_start_matches("0x"), 16).unwrap()).collect();
        let hw: Vec<i16> = f.get(4).map_or(vec![], |o| {
            o.split_whitespace()
                .flat_map(|w| { let v = u32::from_str_radix(w, 16).unwrap(); [v as u16 as i16, (v >> 16) as u16 as i16] })
                .collect()
        });
        let (model, runs) = run_src(&args);
        let id = f[0];
        // the run's bookkeeping exactly: the phase and offset it leaves (CON0's fields), done
        let model_done: Vec<u32> = runs.iter().map(|r| r.0 & !0xff | 0x80).collect();
        let hw_done: Vec<u32> = done.iter().map(|d| d & !0x7f).collect();
        if model_done != hw_done {
            bad.push(format!("{id}: CON0 when done {model_done:x?}, the FM-1 {hw_done:x?}"));
            continue;
        }
        let d = model.iter().zip(&hw).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap_or(0);
        if d == 0 { exact += 1 } else if d <= 3 { near += 1 } else { bad.push(format!("{id}: off by {d}")) }
    }
    eprintln!("hwsrc vs the FM-1: {exact} runs bit-exact, {near} within 3 LSB (phase, offset, counts exact)");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    assert!(exact + near >= 35, "too few cases: {exact} + {near}");
}

#[test]
fn the_model_returns_what_the_fm1_did() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/hwdsp/fm1.txt");
    let text = std::fs::read_to_string(path).expect("tests/hwdsp/fm1.txt");
    let (mut exact, mut near, mut skipped) = (0, 0, 0);
    let mut bad = vec![];
    for (id, args, hw) in cases(&text) {
        if args[0] == 0 || id.starts_with("eq/") || id.starts_with("src/") {
            continue;
        }
        let model = run(&args);
        if args[0] == 2 {
            let con = args[1];
            let (wx, wz, mode) = ((con >> 6) & 3, (con >> 10) & 3, con & 3);
            // outside what is known: add with accumulate, 64-bit sums, z narrower than x,
            // loops whose increments break the element alignment
            let inc = args[3];
            if (mode == 0 && con & 0x20 != 0) || (wz == 3 && con & 0x20 != 0) || (wz < wx && (mode == 1 || mode == 2))
                || ((inc >> 20) & 0x3ff) % (1 << wz) != 0
            {
                skipped += 1;
                continue;
            }
            if model == hw { exact += 1 } else { bad.push(format!("{id}: not bit-exact")) }
        } else {
            // in range: the inputs that keep every stage under 2^28 (the plan's impulses and 2^14 randoms)
            if !(id.contains("/imp") || id.contains("/rnd14")) {
                skipped += 1;
                continue;
            }
            let d = model.iter().zip(&hw).map(|(a, b)| (*a as i32 as i64 - *b as i32 as i64).abs()).max().unwrap_or(0);
            if d <= 3 { near += 1 } else { bad.push(format!("{id}: off by {d}")) }
        }
    }
    eprintln!("hwdsp vs the FM-1: {exact} vector cases bit-exact, {near} FFT cases within 3, {skipped} outside the model");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
    assert!(exact >= 58 && near >= 102, "too few cases: {exact} / {near}");
}
