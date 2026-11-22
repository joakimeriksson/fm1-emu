//! Replays FM-1 hardware cases (pi32v2-isa hw/*.jsonl, prepared by tools/hw_replay.py) on the CPU core.
//! Each stdin line: `CODE STUB R0..R15 PSR SP RETS SCRATCH N W0..W(N-1)` (hex; CODE = the bytes placed at STUB:
//! the instruction, and a parallel twin's partner). The core steps once (and once more for a pending pair).
//! Output per line: `ok R0..R15 PSR SP RETS PC CC K (ADDR VALUE)*K` for the K scratch words that changed, or `err WHY`.
use fm1_core::{Bus, Cpu};
use fm1_soc::Soc;
use std::io::{BufRead, Write};

fn main() {
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let f: Vec<&str> = line.split_whitespace().collect();
        let h = |s: &str| u32::from_str_radix(s, 16).unwrap();
        let code: Vec<u8> = (0..f[0].len() / 2).map(|k| u8::from_str_radix(&f[0][2 * k..2 * k + 2], 16).unwrap()).collect();
        let stub = h(f[1]);
        let regs: Vec<u32> = f[2..18].iter().map(|s| h(s)).collect();
        let (psr, sp, rets, scratch, n) = (h(f[18]), h(f[19]), h(f[20]), h(f[21]), h(f[22]) as usize);
        let words: Vec<u32> = f[23..23 + n].iter().map(|s| h(s)).collect();
        let mut soc = Soc::new(256 * 1024, vec![0u8; 64]);
        for (k, b) in code.iter().enumerate() {
            soc.write8(stub + k as u32, *b);
        }
        // after the code: nops, so a block or a fall-through never runs into garbage
        for k in 0..32u32 {
            soc.write8(stub + code.len() as u32 + k, 0);
        }
        for (k, w) in words.iter().enumerate() {
            soc.write32(scratch + 4 * k as u32, *w);
        }
        let mut cpu = Cpu::new(stub);
        cpu.regs.copy_from_slice(&regs);
        cpu.flags = psr & 0xf;
        cpu.sp = sp;
        // the capture runner loads rets itself, and the chip keeps bits 26..1 of what is written to it
        cpu.rets = rets & 0x07ff_fffe;
        let mut r = cpu.step(&mut soc);
        if r.is_ok() && cpu.pair_pending.is_some() {
            r = cpu.step(&mut soc);
        }
        if let Err(e) = r {
            let s = format!("{e:?}");
            writeln!(out, "err {}", s.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("?")).unwrap();
            continue;
        }
        let mut changed = vec![];
        for (k, w) in words.iter().enumerate() {
            let v = soc.read32(scratch + 4 * k as u32);
            if v != *w {
                changed.push((scratch + 4 * k as u32, v));
            }
        }
        write!(out, "ok").unwrap();
        for v in cpu.regs.iter() {
            write!(out, " {v:x}").unwrap();
        }
        write!(out, " {:x} {:x} {:x} {:x} {} {}", cpu.flags, cpu.sp, cpu.rets, cpu.pc, 0, changed.len()).unwrap();
        for (a, v) in changed {
            write!(out, " {a:x} {v:x}").unwrap();
        }
        writeln!(out).unwrap();
    }
}
