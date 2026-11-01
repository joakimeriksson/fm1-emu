//! Play the emulated FM-1 live.
//!
//!   fm1-live [--bin work/felucca_xip.bin] [--entry 0x02000120] [--mhz 240]
//!   fm1-live --bench 3000          # headless: how fast does it run?
//!   fm1-live --fwsc FM-1.fwsc [--syx bank.syx]   # M-VAVE's stock firmware, from its package
//!   fm1-live ... --flash unit.bin  # the flash chip from this file (if it is there), back to it at the end
//!
//! The emulator runs on its own thread, paced to the wall clock: its
//! emulated time stays a little ahead of real time and the I2S output is
//! streamed to the default audio device. The window is the FM-1's front
//! panel (panel.rs): click or drag across the keys (lower on a key = harder: the velocity
//! its pill lands at; dragging down on a held key = pressing it harder), click buttons, drag a
//! knob up/down or scroll over it; MASTER is the volume pot. Button and key
//! LEDs follow the firmware's LED lines.
//!
//! Keyboard: A W S E D F T G Y H U J K O L P ; ' = C4..F5 (piano layout),
//! Z / X = OCT- / OCT+, Space = PLAY, R = REC, 1..0 = FX SCL ENV LFO EDIT
//! GLO HOME SAVE ARP SEQ, Left/Right = SELECT, Up/Down = PRESETS,
//! , / . = ALGORITHM, Esc quits.
//!
//! MIDI: every MIDI input of this computer plays the synth through its MIDI IN jack
//! (`--midi none` turns that off, `--midi NAME` takes the inputs whose name contains NAME);
//! `--midi-ch N` sends channel messages to channel N (M-VAVE's firmware listens on 2).
//!
//! USB (`--usb`): a cable to this computer (fm1_soc::usb): the emulated FM-1 enumerates, and a
//! USB-MIDI firmware shows up here as a MIDI device named after it, "<product> (emu)", which
//! any program can play and talk to (Felucca's tools/fm1_install.py, the web installer, a DAW).
//! It goes when the firmware drops off USB (the update mode), as the real one does.
//! Headless (`--shot`): FM1_USB_SEND='MS:HEX…;MS:HEX…' sends those MIDI messages over USB at
//! those times, and what comes back is printed. FM1_USB_WAV=FILE (`--bench` or `--shot`): the
//! computer records the device's USB audio into FILE (FM1_USB_RATE: at that rate).
//!
//! `--flaky-core1 RATE [--seed N]`: core 1 misreads RAM now and then, as on some units
//! (fm1_soc::flaky), for trying a firmware's fallback to one core.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use fm1_core::{Bus, Cpu};
use fm1_soc::{machine, Action, Soc};
use minifb::{Key, KeyRepeat, MouseButton, MouseMode, Window, WindowOptions};
use panel::{Ctl, ENCS, H, W};

#[path = "../../fm1-emu/src/buildinfo.rs"]
mod buildinfo;
mod panel;
mod panel_smk;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const RAM_BASE: u32 = 0x01c0_0000;
const RAM_SIZE: usize = 1 << 20;
/// How far emulated time runs ahead of the wall clock before audio starts.
const LEAD_MS: f64 = 60.0;
/// Once the firmware streams audio, the emulator runs whenever less than
/// this much sound is queued: the audio device sets the pace.
const AUDIO_TARGET_MS: f64 = 50.0;

struct Opts {
    bin: String,
    entry: u32,
    mhz: u64,
    bench: Option<u64>,
    /// Headless: run this many ms with a chord held, save the panel image.
    shot: Option<u64>,
    /// What runs, for the window title.
    name: String,
    /// M-VAVE's stock firmware from its update package (fm1_soc::stock_from_package)
    fwsc: Option<String>,
    /// a DX7 32-voice bank for the stock firmware's slots 1-32
    syx: Option<String>,
    /// MIDI inputs to play from: "all", "none", or part of an input's name
    midi: String,
    /// channel messages sent to this channel (1-16; 0 = as sent)
    midi_ch: u8,
    /// a flaky core 1 (fm1_soc::flaky): the chance that one of its RAM loads misreads
    flaky_core1: Option<f64>,
    /// its generator's seed (default 1)
    seed: Option<u64>,
    /// --flash FILE: the flash chip's contents from FILE at the start (if it exists; after the
    /// package is installed), written back to it at the end (bench, shot, or the window closed)
    flash: Option<String>,
    /// --usb: a USB cable to this computer (a host; USB-MIDI as a virtual MIDI device)
    usb: bool,
}

/// The firmware catalog web/firmwares.json as (id, name, version, image path): each image is
/// reference/firmwares/<id>.xip.bin (tools/fetch_firmwares.mjs). A small scan for the string
/// fields, enough for this file; no JSON dependency.
fn catalog() -> Vec<(String, String, String, String)> {
    let Ok(text) = std::fs::read_to_string("web/firmwares.json") else { return Vec::new() };
    let field = |obj: &str, key: &str| -> Option<String> {
        let at = obj.find(&format!("\"{key}\": \""))? + key.len() + 5;
        Some(obj[at..at + obj[at..].find('"')?].to_string())
    };
    text.split("{ \"id\"").skip(1).filter_map(|chunk| {
        let obj = format!("{{ \"id\"{chunk}");
        if obj.contains("\"byo\": true") {
            return None; // M-VAVE's own, brought by the user: --fwsc FM-1.fwsc
        }
        let id = field(&obj, "id")?;
        let path = format!("reference/firmwares/{id}.xip.bin");
        Some((id, field(&obj, "name")?, field(&obj, "version")?, path))
    }).collect()
}

fn parse() -> Option<Opts> {
    let a: Vec<String> = std::env::args().collect();
    let mut o = Opts { bin: "work/felucca_xip.bin".into(), entry: 0x0200_0120, mhz: 240, bench: None, shot: None,
                       name: "Felucca".into(), fwsc: None, syx: None, midi: "all".into(), midi_ch: 0,
                       flaky_core1: None, seed: None, flash: None, usb: false };
    let mut i = 1;
    while i < a.len() {
        if a[i] == "--usb" {
            o.usb = true;
            i += 1;
            continue;
        }
        match a[i].as_str() {
            "--bin" => {
                o.bin = a.get(i + 1)?.clone();
                o.name = o.bin.rsplit('/').next().unwrap_or("").trim_end_matches(".bin").trim_end_matches(".xip").to_string();
            }
            "--fw" => {
                // a firmware from web/firmwares.json by id (`--fw list` lists them)
                let want = a.get(i + 1)?;
                let cat = catalog();
                if want == "list" {
                    for (id, name, ver, path) in &cat {
                        let have = if std::path::Path::new(path).exists() { "" } else { "  (missing: node tools/fetch_firmwares.mjs)" };
                        println!("{id:14} {name} {ver}{have}");
                    }
                    std::process::exit(0);
                }
                let (_, name, ver, path) = cat.into_iter().find(|f| &f.0 == want)?;
                o.bin = path;
                o.name = format!("{name} {ver}");
            }
            "--fwsc" => {
                let path = a.get(i + 1)?;
                o.fwsc = Some(path.clone());
                // (the window's title: the package's name, `FM-1` for M-VAVE's own FM-1.fwsc)
                o.name = std::path::Path::new(path).file_stem().map_or("".into(), |s| s.to_string_lossy().into_owned());
            }
            "--syx" => o.syx = Some(a.get(i + 1)?.clone()),
            "--midi" => o.midi = a.get(i + 1)?.clone(),
            "--midi-ch" => o.midi_ch = a.get(i + 1)?.parse().ok().filter(|c| *c <= 16)?,
            "--entry" => o.entry = u32::from_str_radix(a.get(i + 1)?.trim_start_matches("0x"), 16).ok()?,
            "--mhz" => o.mhz = a.get(i + 1)?.parse().ok()?,
            "--bench" => o.bench = Some(a.get(i + 1)?.parse().ok()?),
            "--shot" => o.shot = Some(a.get(i + 1)?.parse().ok()?),
            "--flaky-core1" => o.flaky_core1 = Some(a.get(i + 1)?.parse::<f64>().ok().filter(|r| (0.0..=1.0).contains(r))?),
            "--seed" => o.seed = Some(a.get(i + 1)?.parse().ok()?),
            "--flash" => o.flash = Some(a.get(i + 1)?.clone()),
            _ => return None,
        }
        i += 2;
    }
    Some(o)
}

/// The machine: cpu0 + SoC, stepped with interrupts and the peripheral clock; cpu1 once
/// the firmware releases it.
struct Machine {
    cpu: Cpu,
    /// The second core, from when cpu0 releases it (fm1_soc::machine).
    cpu1: Option<Cpu>,
    soc: Soc,
    /// FM1_SKIP_LOG: where idle skips happen (pc -> count).
    skip_log: Option<std::collections::HashMap<u32, u64>>,
    /// Run micro-ops in blocks (FM1_NO_BLOCKS=1: the reference per-step loop).
    blocks: bool,
    pending_tick: bool,
    /// FM1_WATCH_LEDPUT=after_ms: report led_put(…, 25, on) calls (debug)
    watch_call: Option<(u32, u64)>,
    watch_hits: u32,
}

impl Machine {
    fn new(o: &Opts) -> std::io::Result<Self> {
        let stock = match &o.fwsc {
            Some(path) => {
                let syx = o.syx.as_ref().map(std::fs::read).transpose()?;
                let pkg = std::fs::read(path)?;
                let st = fm1_soc::stock_from_package(&pkg, syx.as_deref())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{path}: {e}")))?;
                Some(st)
            }
            None => None,
        };
        let image = match &stock {
            Some(st) => st.xip.clone(),
            None => std::fs::read(&o.bin)?,
        };
        let mut soc = Soc::new(RAM_SIZE, image);
        let strict = std::env::var_os("FM1_STRICT").is_some();
        // FM1_STRICT: as an FM-1 is (misaligned RAM accesses fault, the LCD's transfers take their
        // time) and as it is timed: each instruction its measured cost (fm1_core's op_cost8) on a
        // clock in eighths of a cycle (the HW units' jobs too); the interpreter only, and no loop
        // skip (it counts instructions)
        soc.periph.cycle_ticks = if strict { 8 } else { 1 };
        soc.periph.hz = o.mhz * 1_000_000 * soc.periph.cycle_ticks;
        soc.strict_align = strict;
        // (FM1_STRICT or FM1_LCD_TIMED; FM1_LCD_INSTANT turns it off again)
        soc.lcd_timed = fm1_soc::lcd_timed_from_env();
        if strict {
            soc.loop_skip.enabled = false;
            soc.xip_cache = Some(fm1_soc::XipCache::default());
        }
        soc.periph.skip_idle = true;
        soc.flaky_core1 = o.flaky_core1.map(|rate| fm1_soc::flaky::FlakyCore1::new(rate, o.seed.unwrap_or(1)));
        let mut cpu = Cpu::new(stock.as_ref().map_or(o.entry, |st| st.entry));
        cpu.jit_on = std::env::var("FM1_NO_JIT").is_err() && !strict;
        cpu.cost_model = strict;
        if std::env::var_os("FM1_ISA_HIST").is_some() {
            cpu.isa_hist = Some(Default::default());
        }
        // the SPL hands cpu0 over in user mode (see fm1-emu)
        cpu.user = std::env::var_os("FM1_SUPERVISOR_START").is_none();
        // the SPL's boot-parameter struct (see fm1-emu): an all-zero pair
        let params = RAM_BASE + 0x7fc00;
        soc.write32(params, params + 0x40);
        cpu.regs[0] = params;
        if let Some(st) = &stock {
            cpu.regs[0] = fm1_soc::stock_boot(&mut soc, &st.flash, params, st.board);
            soc.sfc_boot(st.key, st.app_end);
        }
        if let Some(path) = o.flash.as_ref().filter(|p| std::path::Path::new(p).exists()) {
            let f = std::fs::read(path)?;
            if f.len() != fm1_soc::FLASH_SIZE {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{path}: not a {} byte flash image", fm1_soc::FLASH_SIZE)));
            }
            // (through the SFC as the loader leaves it, and again when the firmware sets its window)
            soc.restore_flash(&f);
        }
        if o.usb {
            soc.usb_plug();
        }
        let skip_log = std::env::var("FM1_SKIP_LOG").ok().map(|_| Default::default());
        let blocks = std::env::var("FM1_NO_BLOCKS").is_err();
        let watch_call = std::env::var("FM1_WATCH_LEDPUT").ok().and_then(|v| v.parse().ok()).map(|ms| (0x0201_ad60u32, ms));
        let blocks = blocks && watch_call.is_none();
        Ok(Self { cpu, cpu1: None, soc, skip_log, blocks, pending_tick: false, watch_call, watch_hits: 0 })
    }

    /// Run n instructions of cpu0, and of cpu1 alongside once it is running
    /// (fm1_soc::machine). FM1_NO_BLOCKS: the per-step reference loop, cpu0 only.
    fn run(&mut self, n: u32) -> Result<(), String> {
        if !self.blocks {
            return self.run_steps(n);
        }
        let jit = self.cpu.jit_on;
        let Machine { cpu, cpu1, soc, pending_tick, .. } = self;
        machine::run_dual(cpu1, soc, n, RAM_SIZE, jit, |soc, k| machine::run_blocks(cpu, soc, k, pending_tick))
    }

    /// The reference loop: one instruction per iteration.
    fn run_steps(&mut self, n: u32) -> Result<(), String> {
        self.soc.take_boot(&self.cpu);
        for _ in 0..n {
            if self.soc.periph.tick() {
                self.soc.advance(0);
            }
            if self.soc.reset_due() {
                self.soc.chip_reset(&mut self.cpu); // (as run_blocks: cpu0 from its start)
                self.cpu1 = None;
            }
            // (as run_blocks: the PC limits, the interrupt due, the exception taken once)
            machine::cpu0_between(&mut self.cpu, &mut self.soc);
            if let Some((bpc, after)) = self.watch_call {
                if self.cpu.pc == bpc && self.cpu.regs[2] != 0 && self.soc.periph.ms() >= after && self.watch_hits < 60 {
                    self.watch_hits += 1;
                    println!("led_put(arr {:#x}, id {}, on {}) from {:#010x}", self.cpu.regs[0], self.cpu.regs[1], self.cpu.regs[2], self.cpu.rets);
                }
            }
            let (pc, sk) = (self.cpu.pc, self.soc.periph.skipped);
            machine::cpu0_step(&mut self.cpu, &mut self.soc)?;
            if self.skip_log.is_some() && self.soc.periph.skipped != sk {
                *self.skip_log.as_mut().unwrap().entry(pc).or_insert(0) += 1;
            }
        }
        Ok(())
    }
}

/// Ask for a performance core: macOS runs default-QoS background threads on
/// the efficiency cores, which made the emulator ~40% slower than on main.
fn high_priority() {
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
        unsafe {
            pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);
        }
    }
}

fn bench(o: &Opts, ms: u64) {
    let mut m = match Machine::new(o) {
        Ok(m) => m,
        Err(e) => { eprintln!("cannot read {}: {e}", o.bin); std::process::exit(1); },
    };
    usb_audio_arm(&mut m);
    if std::env::var("FM1_BENCH_NOKEYS").is_ok() {
        m.soc.periph.script.clear();
    } else if script_keys(&mut m) {
    } else {
    m.soc.periph.script.push_back((m.soc.periph.cycle_of_ms(800), Action::Key { id: 21, down: true }));
    m.soc.periph.script.push_back((m.soc.periph.cycle_of_ms(900), Action::Key { id: 25, down: true }));
    m.soc.periph.script.push_back((m.soc.periph.cycle_of_ms(1000), Action::Key { id: 28, down: true }));
    }
    m.cpu.profile = std::env::var("FM1_BENCH_ROUTES").is_ok();
    let warmup = std::env::var("FM1_WARMUP_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(1000).min(ms.saturating_sub(1));
    let mut steady = None;
    let t = Instant::now();
    let mut insns = 0u64;
    let mut pcs: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();
    let sample = std::env::var("FM1_BENCH_PCS").is_ok();
    let panel = std::env::var_os("FM1_BENCH_PANEL").is_some();
    let mut led_tail = false;
    while m.soc.periph.ms() < ms {
        if panel && !led_tail && m.soc.periph.ms() + 40 >= ms {
            // (the LEDs' on-fractions from here: the last 40 ms, as --shot shows them)
            let _ = m.soc.periph.led_take();
            led_tail = true;
        }
        if steady.is_none() && m.soc.periph.ms() >= warmup {
            steady = Some((Instant::now(), m.soc.periph.ms()));
        }
        if sample && m.soc.periph.ms() > ms / 2 {
            for _ in 0..1000 {
                if let Err(e) = m.run(97) {
                    usb_audio_save(&m);
                    halted(&e);
                    std::process::exit(1);
                }
                *pcs.entry(m.cpu.pc).or_insert(0) += 1;
            }
            insns += 97_000;
            continue;
        }
        if let Err(e) = m.run(100_000) {
            usb_audio_save(&m);
            halted(&e);
            std::process::exit(1);
        }
        insns += 100_000;
        if let Some(c1) = m.cpu1.as_mut() {
            c1.profile = m.cpu.profile;
        }
        if std::env::var_os("FM1_BENCH_TRACE").is_some() {
            // a checkpoint to diff between runs (JIT against the interpreter)
            eprintln!("cp {insns} cyc {} pc {:#010x} sp {:#x} r {:x?}", m.soc.periph.cycles, m.cpu.pc, m.cpu.sp, &m.cpu.regs[..8]);
        }
    }
    if sample {
        let mut v: Vec<_> = pcs.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        let tot: u64 = v.iter().map(|x| x.1).sum();
        for (pc, n) in v.iter().take(25) {
            println!("pc {pc:#010x} {:5.1}%", 100.0 * *n as f64 / tot as f64);
        }
    }
    let wall = t.elapsed().as_secs_f64();
    let steady_rt = steady.map(|(t, start)| (m.soc.periph.ms() - start) as f64 / 1000.0 / t.elapsed().as_secs_f64());
    let frames = m.soc.periph.audio.len() / 2;
    println!("steady: {:?}x real time; {frames} real audio frames", steady_rt);
    if let Ok(path) = std::env::var("FM1_BENCH_JSON") {
        let json = format!("{{\"emulatedMs\":{},\"wallSeconds\":{wall},\"realtime\":{},\"audioFrames\":{frames},\"steadyRealtime\":{}}}\n",
            m.soc.periph.ms(), m.soc.periph.ms() as f64 / 1000.0 / wall,
            steady_rt.map_or_else(|| "null".to_owned(), |v| v.to_string()));
        std::fs::write(path, json).expect("write benchmark JSON");
    }
    println!("icache: {} decodes for {} instructions", m.cpu.decodes, insns);
    if let Some(c1) = &m.cpu1 {
        println!("cpu1: {} instructions ({:.1} M per emulated s)", c1.insn_count, c1.insn_count as f64 / 1e3 / ms as f64);
    }
    let ls = &m.soc.loop_skip;
    if ls.enabled {
        println!("loop skip: {:.1}% of cycles ({} probes, {} found a loop)", 100.0 * ls.skipped as f64 / m.soc.periph.cycles.max(1) as f64, ls.probes, ls.hits);
    }
    if let Some(f) = &m.soc.flaky_core1 {
        println!("flaky core 1: {} misreads in {} RAM loads", f.misreads, f.loads);
    }
    if let Ok(path) = std::env::var("FM1_BENCH_WAV") {
        let bytes: Vec<u8> = m.soc.periph.audio.iter().flat_map(|s| s.to_le_bytes()).collect();
        let _ = std::fs::write(&path, bytes);
        println!("audio: {} samples -> {path}", m.soc.periph.audio.len());
    }
    usb_audio_save(&m);
    dump_ram(&mut m);
    if let Ok(path) = std::env::var("FM1_BENCH_PANEL") {
        let leds = m.soc.periph.led_take();
        match write_panel(&mut m, &[false; 41], &leds, &path) {
            Ok(()) => println!("panel at {} ms -> {path}", m.soc.periph.ms()),
            Err(e) => eprintln!("{path}: {e}"),
        }
    }
    save_flash(&m, o.flash.as_ref());
    if m.cpu.profile {
        println!("JIT cpu0: {}", m.cpu.jit_profile_json());
        if let Some(c1) = &m.cpu1 { println!("JIT cpu1: {}", c1.jit_profile_json()); }
        let tot: u64 = m.cpu.route_hist.iter().sum();
        println!("slow path: {} of {} instructions ({:.1}%)", tot, insns, 100.0 * tot as f64 / insns as f64);
        for (i, n) in m.cpu.route_hist.iter().enumerate().filter(|(_, n)| **n > 0) {
            println!("route {i:2}: {:5.1}%", 100.0 * *n as f64 / tot as f64);
        }
        let mut v: Vec<_> = m.cpu.slow_hist.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        for (name, n) in v.iter().take(40) {
            println!("slow {name:28} {:5.2}%", 100.0 * **n as f64 / tot as f64);
        }
        if let Some(c1) = &m.cpu1 {
            let t1: u64 = c1.route_hist.iter().sum();
            let mut v: Vec<_> = c1.slow_hist.iter().collect();
            v.sort_by(|a, b| b.1.cmp(a.1));
            println!("cpu1 slow path: {} of {} counted", t1 - c1.route_hist[31], t1);
            for (name, n) in v.iter().take(25) {
                println!("cpu1 slow {name:28} {:5.2}%", 100.0 * **n as f64 / t1.max(1) as f64);
            }
        }
    }
    let p = &m.soc.periph;
    println!(
        "{} ms emulated at {} MHz in {:.2} s wall: {:.2}x real time, {:.1} M instructions/s, {:.0}% of cycles skipped as idle",
        p.ms(),
        o.mhz,
        wall,
        p.ms() as f64 / 1000.0 / wall,
        insns as f64 / wall / 1e6,
        100.0 * p.skipped as f64 / p.cycles.max(1) as f64
    );
}

// ---------------------------------------------------------------- audio --

struct AudioRing {
    q: Mutex<VecDeque<i16>>,
    /// Set once the firmware has produced sound (silence before that is boot).
    started: AtomicBool,
    underruns: AtomicU64,
}

fn start_audio(ring: Arc<AudioRing>) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let dev = host.default_output_device().ok_or("no audio output device")?;
    let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
    if cfg.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!("output format {:?} not handled (f32 only)", cfg.sample_format()));
    }
    let channels = cfg.channels() as usize;
    let rate = cfg.sample_rate().0 as f64;
    let step = 44_100.0 / rate; // source frames per output frame
    let (mut pos, mut cur, mut next) = (0.0f64, (0.0f32, 0.0f32), (0.0f32, 0.0f32));
    let stream = dev
        .build_output_stream(
            &cfg.config(),
            move |out: &mut [f32], _| {
                let mut q = ring.q.lock().unwrap();
                let mut starved = false;
                for frame in out.chunks_mut(channels) {
                    pos += step;
                    while pos >= 1.0 {
                        pos -= 1.0;
                        cur = next;
                        next = match (q.pop_front(), q.pop_front()) {
                            (Some(l), Some(r)) => (l as f32 / 32768.0, r as f32 / 32768.0),
                            _ => {
                                starved = true;
                                (0.0, 0.0)
                            }
                        };
                    }
                    let f = pos as f32;
                    let (l, r) = (cur.0 + (next.0 - cur.0) * f, cur.1 + (next.1 - cur.1) * f);
                    for (c, s) in frame.iter_mut().enumerate() {
                        *s = if c % 2 == 0 { l } else { r };
                    }
                }
                if starved && ring.started.load(Ordering::Relaxed) {
                    ring.underruns.fetch_add(1, Ordering::Relaxed);
                }
            },
            |e| eprintln!("audio: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

/// Headless panel picture: C4-E4-G4 held from 0.8 s, PLAY pressed at 1.2 s,
/// the panel drawn at `ms` into work/panel.ppm.
/// FM1_BENCH_KEYS=ms:id[,ms:id^,ms:eN:clicks...]: queue key presses (`^`: release) and
/// encoder turns, `ms:mHEX` MIDI bytes into the MIDI IN jack (`@FILE`: the script from FILE).
/// False when the variable is unset.
/// --flash: the flash chip back to its file; and how many erases / programs it saw
fn save_flash(m: &Machine, path: Option<&String>) {
    let f = &m.soc.flash;
    if f.ops > 0 || path.is_some() {
        let cut = if f.cut() { format!(", power cut at op {}", f.cut_at.unwrap_or(0)) } else { String::new() };
        println!("flash: {} erases / programs{cut}", f.ops);
    }
    if let Some(path) = path {
        // (a new file renamed over the old: a run killed mid-save leaves the last save whole)
        let tmp = format!("{path}.tmp");
        if let Err(e) = std::fs::write(&tmp, &f.mem).and_then(|_| std::fs::rename(&tmp, path)) {
            eprintln!("{path}: {e}");
        }
    }
}

/// A halt, with the binary that halted: a stale one is the first suspect.
fn halted(e: &str) {
    eprintln!("halt: {e}");
    eprintln!("{}", buildinfo::halt_note("fm1-live"));
}

fn script_keys(m: &mut Machine) -> bool {
    let Ok(mut keys) = std::env::var("FM1_BENCH_KEYS") else { return false };
    if let Some(path) = keys.strip_prefix('@') {
        // (@FILE: the script from a file; a deploy of a whole firmware is megabytes of MIDI)
        keys = std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("FM1_BENCH_KEYS {path}: {e}");
            String::new()
        });
    }
    fm1_soc::script::queue(&mut m.soc, keys.trim());
    true
}

fn shot(o: &Opts, ms: u64) {
    let mut m = match Machine::new(o) {
        Ok(m) => m,
        Err(e) => return eprintln!("cannot read {}: {e}", o.bin),
    };
    usb_audio_arm(&mut m);
    if std::env::var("FM1_NO_SKIP").is_ok() {
        m.soc.periph.skip_idle = false;
    }
    let scripted = script_keys(&mut m);
    let held: &[u8] = if scripted { &[] } else { &[21u8, 25, 28] };
    let key_at: u64 = std::env::var("FM1_KEY_AT").ok().and_then(|v| v.parse().ok()).unwrap_or(800);
    let key_up: Option<u64> = std::env::var("FM1_KEY_UP").ok().and_then(|v| v.parse().ok());
    for &id in held {
        let at = m.soc.periph.cycle_of_ms(key_at);
        m.soc.periph.script.push_back((at, Action::Key { id, down: true }));
        if let Some(up) = key_up {
            let at = m.soc.periph.cycle_of_ms(up);
            m.soc.periph.script.push_back((at, Action::Key { id, down: false }));
        }
    }
    m.soc.periph.script.make_contiguous().sort_by_key(|(c, _)| *c);
    if std::env::var("FM1_NO_PLAY").is_err() && !scripted {
        let at = m.soc.periph.cycle_of_ms(1200);
        m.soc.periph.script.push_back((at, Action::Key { id: 12, down: true }));
        let at = m.soc.periph.cycle_of_ms(1350);
        m.soc.periph.script.push_back((at, Action::Key { id: 12, down: false }));
    }
    let skip_from: u64 = std::env::var("FM1_SKIP_FROM").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let want_skip = m.soc.periph.skip_idle;
    m.soc.periph.skip_idle = want_skip && skip_from == 0;
    let mut usb_sends = usb_script();
    // FM1_TPROF=PATH,FROM_MS,TO_MS: time-weighted PC samples of both cores (every 8 cycles,
    // one instruction at a time) and the flash cache's misses by line, into PATH
    let tprof: Option<(String, u64, u64)> = std::env::var("FM1_TPROF").ok().and_then(|v| {
        let mut it = v.split(',');
        Some((it.next()?.to_string(), it.next()?.parse().ok()?, it.next()?.parse().ok()?))
    });
    let mut tp0: std::collections::HashMap<u32, u64> = Default::default();
    let mut tp1: std::collections::HashMap<u32, u64> = Default::default();
    while m.soc.periph.ms() + 40 < ms {
        if want_skip && m.soc.periph.ms() >= skip_from {
            m.soc.periph.skip_idle = true;
        }
        if let Some((_, from, to)) = &tprof {
            let now = m.soc.periph.ms();
            if now >= *from && now < *to {
                if m.soc.miss_hist.is_none() {
                    m.soc.miss_hist = Some(Default::default());
                }
                const P: u64 = 64;
                for _ in 0..20000 {
                    let pc0 = m.cpu.pc;
                    let c0 = m.soc.periph.cycles;
                    if let Err(e) = m.run(1) {
                        return halted(&e);
                    }
                    let n = m.soc.periph.cycles / P - c0 / P;
                    if n > 0 {
                        *tp0.entry(pc0).or_insert(0) += n;
                        if let Some(c) = m.cpu1.as_ref() {
                            *tp1.entry(c.pc).or_insert(0) += n;
                        }
                    }
                }
                continue;
            }
            if now >= *to && m.soc.miss_hist.is_some() {
                let mut o = String::new();
                for (pc, n) in tp0.iter() { o += &format!("0 {pc:#010x} {n}\n"); }
                for (pc, n) in tp1.iter() { o += &format!("1 {pc:#010x} {n}\n"); }
                for (k, n) in m.soc.miss_hist.take().unwrap().iter() { o += &format!("m{} {:#010x} {n}\n", k >> 32, k & 0xffff_ffff); }
                let _ = std::fs::write(&tprof.as_ref().unwrap().0, o);
            }
        }
        if let Err(e) = m.run(100_000) {
            usb_audio_save(&m);
            return halted(&e);
        }
        usb_headless(&mut m, &mut usb_sends);
    }
    if o.usb {
        let u = &m.soc.usb;
        println!("usb: {} SIE accesses ({:.1} ms of CPU at {} cycles each), {} packets out, {} in",
                 u.sie_accesses, u.sie_accesses as f64 * u.sie_cycles as f64 * 1000.0 / m.soc.periph.hz as f64,
                 u.sie_cycles, u.out_packets, u.in_packets);
        usb_audio_save(&m);
    }
    println!("irq levels at the end: {:?}, ie {}, skipped {} cycles", m.cpu.irq_levels, m.cpu.ie, m.soc.periph.skipped);
    let _ = m.soc.periph.led_take();
    let (mut in_isr, mut total) = (0u64, 0u64);
    while m.soc.periph.ms() < ms {
        if let Err(e) = m.run(1) {
            usb_audio_save(&m);
            return halted(&e);
        }
        total += 1;
        if !m.cpu.irq_levels.is_empty() {
            in_isr += 1;
        }
    }
    println!(
        "last 40 ms: {:.0}% of instructions in ISRs = {:.1} M ISR instructions per second (clock {} MHz)",
        100.0 * in_isr as f64 / total.max(1) as f64,
        in_isr as f64 / 0.04 / 1e6,
        m.soc.periph.hz / 1_000_000
    );
    let leds = m.soc.periph.led_take();
    if let (Some(path), Some(h)) = (std::env::var_os("FM1_ISA_HIST"), m.cpu.isa_hist.as_ref()) {
        // executed instructions by ISA form, as JSON {syntax: count} (FM1_NO_BLOCKS: every step)
        let mut v: Vec<_> = h.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        let body: Vec<String> = v.iter().map(|(k, n)| format!("{:?}: {n}", k)).collect();
        let _ = std::fs::write(&path, format!("{{{}}}\n", body.join(", ")));
    }
    if m.soc.lcd_timed {
        // the screen's share of the run: a firmware whose main loop waits for whole frames gets that
        // little else done (FM-1 SID's silent tunes on the device, 2026-10-24)
        let p = &m.soc.periph;
        let share = 100.0 * p.spi1_wire as f64 / p.cycles.max(1) as f64;
        println!("LCD: on the wire {:.0}% of the run ({:.0} ms of {:.0} ms)", share,
            p.spi1_wire as f64 * 1000.0 / p.hz as f64, p.cycles as f64 * 1000.0 / p.hz as f64);
        if share > 50.0 {
            println!("warning: the LCD's transfers take {share:.0}% of the time: a main loop that waits for them \
                      has little left for anything else (send only what changed; FM1_LCD_INSTANT=1: the instant LCD)");
        }
    }
    if let Some(log) = &m.skip_log {
        let mut v: Vec<_> = log.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        for (pc, n) in v.iter().take(12) {
            println!("skip at {:#010x} x{n}", pc);
        }
    }
    let mut down = [false; 41];
    if key_up.is_none() {
        for &id in held {
            down[id as usize] = true;
        }
    }
    if let Some(a) = std::env::var("FM1_DUMP").ok().and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok()) {
        let w: Vec<String> = (0..4).map(|i| format!("{:08x}", m.soc.read32(a + 4 * i))).collect();
        println!("dump {a:#x}: {}", w.join(" "));
    }
    dump_ram(&mut m);
    let fr: Vec<String> = (0..41).filter(|&i| leds[i] > 0.0).map(|i| format!("{i}:{:.3}", leds[i])).collect();
    println!("LED on-fractions: {}", fr.join(" "));
    if m.soc.board == fm1_soc::Board::Smk37 {
        let p = &m.soc.periph.smk;
        let pads: Vec<String> = p.pads.iter().map(|c| format!("{c:06x}")).collect();
        println!("SMK-37 pads ({} frames): {}", p.pad_frames, pads.join(" "));
    }
    let lit: Vec<usize> = (0..41).filter(|&i| leds[i] > 0.01).collect();
    match write_panel(&mut m, &down, &leds, "work/panel.ppm") {
        Ok(()) => {
            let a = &m.soc.periph.audio;
            let tail = &a[a.len().saturating_sub(44_100)..];
            let peak = tail.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
            println!("work/panel.ppm at {} ms; LEDs lit: {lit:?}; audio peak over the last 0.5 s: {peak}", m.soc.periph.ms())
        }
        Err(e) => eprintln!("work/panel.ppm: {e}"),
    }
    save_flash(&m, o.flash.as_ref());
}

/// FM1_USB_SEND='MS:HEX…;MS:HEX…': MIDI messages to send over USB at those times (ms); `@PATH` reads them
/// from a file (a whole deploy is longer than an environment variable may be).
fn usb_script() -> VecDeque<(u64, Vec<u8>)> {
    let mut sends = std::env::var("FM1_USB_SEND").unwrap_or_default();
    if let Some(path) = sends.strip_prefix('@') {
        sends = std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("FM1_USB_SEND {path}: {e}");
            String::new()
        });
    }
    let mut q: Vec<(u64, Vec<u8>)> = sends.split(';').filter_map(|item| {
        let (t, hex) = item.trim().split_once(':')?;
        let hex: String = hex.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        let bytes = (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect::<Option<Vec<u8>>>()?;
        Some((t.trim().parse().ok()?, bytes))
    }).collect();
    q.sort_by_key(|(t, _)| *t);
    q.into()
}

/// --usb with FM1_USB_WAV=FILE (--bench, --shot): the computer records the device's USB audio,
/// asking for FM1_USB_RATE (default 44100).
fn usb_audio_arm(m: &mut Machine) {
    if std::env::var_os("FM1_USB_WAV").is_some() && m.soc.usb.cable {
        let rate = std::env::var("FM1_USB_RATE").ok().and_then(|v| v.parse().ok()).unwrap_or(44_100);
        m.soc.usb.audio_record(rate);
    }
}

/// --usb with FM1_USB_WAV=FILE: what the computer recorded from the device's USB audio (the
/// samples as sent, interleaved as its channels come, s16le, no header).
fn usb_audio_save(m: &Machine) {
    let Ok(path) = std::env::var("FM1_USB_WAV") else { return };
    let u = &m.soc.usb;
    if !u.cable {
        return println!("usb audio: no USB cable (--usb); nothing -> {path}");
    }
    let Some(a) = &u.audio_stream else {
        return println!("usb audio: the device has no audio streaming interface; nothing -> {path}");
    };
    if a.sample_bytes != 2 {
        println!("usb audio: {}-byte samples are not decoded (16-bit only); nothing recorded", a.sample_bytes);
    }
    let bytes: Vec<u8> = u.audio.iter().flat_map(|s| s.to_le_bytes()).collect();
    let _ = std::fs::write(&path, bytes);
    println!("usb audio: {} ch at {} Hz, {} samples in {} packets ({} empty, {} of them underruns) -> {path}",
             a.channels, u.audio_rate, u.audio.len(), u.audio_packets, u.audio_empty, u.audio_underruns);
}

/// --shot with --usb: the scripted MIDI out over USB, and what happened on the bus.
fn usb_headless(m: &mut Machine, sends: &mut VecDeque<(u64, Vec<u8>)>) {
    let now = m.soc.periph.ms();
    while sends.front().is_some_and(|(t, _)| *t <= now) {
        let (_, msg) = sends.pop_front().unwrap();
        println!("[{now:6} ms] usb out: {}", hex(&msg));
        m.soc.usb.midi_send(&msg);
    }
    for ev in m.soc.usb.events.drain(..) {
        println!("[{now:6} ms] usb: {ev:?}");
    }
    for msg in m.soc.usb.midi_take() {
        println!("[{now:6} ms] usb in:  {}", hex(&msg));
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

/// --usb, live: the device as a virtual MIDI device on this computer while it is on the bus.
#[derive(Default)]
struct UsbPorts {
    out: Option<midir::MidiOutputConnection>,
    _inp: Option<midir::MidiInputConnection<()>>,
}

impl UsbPorts {
    /// The bus's news, MIDI both ways.
    fn service(&mut self, m: &mut Machine, rx: &mpsc::Receiver<Vec<u8>>, tx: &mpsc::Sender<Vec<u8>>) {
        while let Ok(msg) = rx.try_recv() {
            m.soc.usb.midi_send(&msg);
        }
        for ev in m.soc.usb.events.drain(..) {
            match ev {
                fm1_soc::usb::UsbEvent::Configured { product, vid, pid } => {
                    let name = format!("{product} (emu)");
                    self.open(&name, tx);
                    let ok = self.out.is_some() && self._inp.is_some();
                    eprintln!("USB: \"{product}\" ({vid:04x}:{pid:04x}) on the bus{}", if ok { format!(", MIDI device \"{name}\"") } else { ", no MIDI device (virtual ports failed)".into() });
                }
                fm1_soc::usb::UsbEvent::Detached => {
                    eprintln!("USB: the device left the bus");
                    *self = UsbPorts::default();
                }
                fm1_soc::usb::UsbEvent::Failed(why) => eprintln!("USB: enumeration failed: {why}"),
            }
        }
        for msg in m.soc.usb.midi_take() {
            if let Some(out) = self.out.as_mut() {
                let _ = out.send(&msg);
            }
        }
    }
}

impl UsbPorts {
    /// The virtual MIDI device (CoreMIDI, ALSA; Windows has none: the bus runs without one).
    #[cfg(unix)]
    fn open(&mut self, name: &str, tx: &mpsc::Sender<Vec<u8>>) {
        use midir::os::unix::{VirtualInput, VirtualOutput};
        self.out = midir::MidiOutput::new("fm1-live").ok().and_then(|o| o.create_virtual(name).ok());
        self._inp = midir::MidiInput::new("fm1-live").ok().and_then(|mut i| {
            i.ignore(midir::Ignore::None);
            let tx = tx.clone();
            i.create_virtual(name, move |_, msg, _| { let _ = tx.send(msg.to_vec()); }, ()).ok()
        });
    }
    #[cfg(not(unix))]
    fn open(&mut self, _name: &str, _tx: &mpsc::Sender<Vec<u8>>) {}
}

/// FM1_DUMPRAM=FILE: the RAM as the run left it.
fn dump_ram(m: &mut Machine) {
    if let Ok(path) = std::env::var("FM1_DUMPRAM") {
        let bytes: Vec<u8> = (0..RAM_SIZE as u32 / 4).flat_map(|i| m.soc.read32(RAM_BASE + 4 * i).to_le_bytes()).collect();
        let _ = std::fs::write(&path, bytes);
    }
}

/// The panel as the machine shows it now, keys `down` held and `leds` lit, as a PPM file.
fn write_panel(m: &mut Machine, down: &[bool; 41], leds: &[f32; 41], path: &str) -> std::io::Result<()> {
    let st = panel::State {
        lcd: &m.soc.lcd.fb,
        enc_clicks: &[0, 0, 0, 3, -2, 0, 5],
        pot: m.soc.periph.adc_value[4],
        down,
        leds,
        turning: &[0, 0, 0, 0, 0, 0, 0, 0],
    };
    let (pw, ph) = if m.soc.board == fm1_soc::Board::Smk37 { (panel_smk::W, panel_smk::H) } else { (W, H) };
    let mut buf = vec![0u32; pw * ph];
    if m.soc.board == fm1_soc::Board::Smk37 {
        let p = &mut m.soc.periph.smk;
        let leds = p.led_take();
        let (pads, force, faders) = (p.pads, std::array::from_fn(|i| p.analog(i as u8)), std::array::from_fn(|i| p.analog(16 + i as u8)));
        let mut sdown = [false; 64];
        sdown[..55].copy_from_slice(&p.down);
        let st = panel_smk::State {
            lcd: &m.soc.lcd.fb,
            enc_clicks: &[0; 8],
            turning: &[0; 8],
            down: &sdown,
            leds: &leds,
            pads: &pads,
            pad_force: &force,
            faders: &faders,
            pitch: p.analog(fm1_soc::smk::ANALOG_PITCH),
            modw: p.analog(fm1_soc::smk::ANALOG_MOD),
        };
        println!("SMK-37 button LEDs: {}", leds.iter().enumerate().filter(|(_, &f)| f > 0.01).map(|(k, f)| format!("{k}:{f:.2}")).collect::<Vec<_>>().join(" "));
        panel_smk::draw(&mut buf, &panel_smk::base(), &st);
    } else {
        panel::draw(&mut buf, &panel::base(), &st);
    }
    let mut ppm = format!("P6\n{pw} {ph}\n255\n").into_bytes();
    for p in &buf {
        ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
    }
    std::fs::write(path, ppm)
}

// ---------------------------------------------------------------- input --

fn piano(k: Key) -> Option<u8> {
    // C4 = note key 7 = matrix id 21
    let semis = match k {
        Key::A => 0, Key::W => 1, Key::S => 2, Key::E => 3, Key::D => 4, Key::F => 5, Key::T => 6,
        Key::G => 7, Key::Y => 8, Key::H => 9, Key::U => 10, Key::J => 11, Key::K => 12, Key::O => 13,
        Key::L => 14, Key::P => 15, Key::Semicolon => 16, Key::Apostrophe => 17,
        _ => return None,
    };
    Some(21 + semis)
}

fn button(k: Key) -> Option<u8> {
    Some(match k {
        Key::Z => 0, Key::X => 1, Key::Space => 12, Key::R => 13,
        Key::Key1 => 2, Key::Key2 => 3, Key::Key3 => 4, Key::Key4 => 5, Key::Key5 => 6,
        Key::Key6 => 7, Key::Key7 => 8, Key::Key8 => 9, Key::Key9 => 10, Key::Key0 => 11,
        _ => return None,
    })
}

/// Connect the MIDI inputs `--midi` asks for: their bytes go to the MIDI IN jack. The
/// connections live as long as the returned value.
fn midi_inputs(o: &Opts, tx: &mpsc::Sender<Action>) -> Vec<midir::MidiInputConnection<()>> {
    let mut conns = Vec::new();
    if o.midi == "none" {
        return conns;
    }
    let probe = match midir::MidiInput::new("fm1-live") {
        Ok(m) => m,
        Err(e) => {
            eprintln!("MIDI in: {e}");
            return conns;
        }
    };
    for port in probe.ports() {
        let Ok(name) = probe.port_name(&port) else { continue };
        if o.midi != "all" && !name.to_lowercase().contains(&o.midi.to_lowercase()) {
            continue;
        }
        let Ok(mut input) = midir::MidiInput::new("fm1-live") else { continue };
        input.ignore(midir::Ignore::None);
        let (tx, ch) = (tx.clone(), o.midi_ch);
        let got = input.connect(&port, "fm1-live in", move |_, msg, _| {
            for (i, &b) in msg.iter().enumerate() {
                let b = if i == 0 && ch != 0 && (0x80..0xf0).contains(&b) { (b & 0xf0) | (ch - 1) } else { b };
                let _ = tx.send(Action::Midi { b });
            }
        }, ());
        match got {
            Ok(c) => {
                eprintln!("MIDI in: {name}");
                conns.push(c);
            }
            Err(e) => eprintln!("MIDI in: {name}: {e}"),
        }
    }
    if conns.is_empty() {
        eprintln!("MIDI in: no inputs{}", if o.midi == "all" { String::new() } else { format!(" matching {:?}", o.midi) });
    }
    conns
}

/// Pixels of vertical mouse drag per encoder click.
const DRAG_PER_CLICK: f32 = 12.0;

const USAGE: &str = "usage: fm1-live --fwsc FIRMWARE.fwsc   (a firmware package: a community firmware, or M-VAVE's own)
       fm1-live --fw ID | --fw list    (the page's list, in a checkout: node tools/fetch_firmwares.mjs)
       fm1-live --bin XIP.bin [--entry 0x02000120]
options: [--mhz 240] [--syx BANK.syx] [--midi all|none|NAME] [--midi-ch N] [--flash FILE] [--usb]
         [--flaky-core1 RATE [--seed N]] [--version] [--help]
headless: --bench MS   run MS emulated ms as fast as it goes, report the speed
          --shot MS    run MS ms, then write the panel to work/panel.ppm
env: FM1_BENCH_KEYS='T[-T2]:PLAY,T:n7,T:enc0:+3,T:pot:512,T:ms:mHEX'  scripted input (--bench, --shot;
                         times in ms, mHEX: MIDI bytes into the DIN jack)
     FM1_BENCH_WAV=FILE    --bench: the audio (44.1 kHz stereo s16le, no header)
     FM1_BENCH_PANEL=FILE  --bench: the panel at the end (PPM)
     FM1_DUMPRAM=FILE      --bench, --shot: the 1 MiB of RAM at the end
     FM1_BENCH_JSON=FILE   --bench: the speed as JSON
     FM1_BENCH_NOKEYS, FM1_WARMUP_MS=N (1000), FM1_BENCH_THREAD, FM1_BENCH_TRACE, FM1_BENCH_PCS,
     FM1_BENCH_ROUTES      --bench: no default keys, the steady-speed start, a thread, checkpoints, profiles
     FM1_KEY_AT, FM1_KEY_UP, FM1_NO_PLAY, FM1_NO_SKIP, FM1_SKIP_FROM, FM1_DUMP=ADDR  --shot
     FM1_NO_JIT            the interpreter (byte-identical, slower); FM1_NO_BLOCKS: a step at a time
     FM1_NO_LOOPSKIP       run polling loops round by round instead of skipping them (byte-identical, slower)
     FM1_FLASH_CUT=N       cut the power half way through the Nth flash erase / program (--flash keeps what is left)
     FM1_USB_SEND='MS:HEX;…'  --shot --usb: MIDI messages over USB at those times (@PATH: from a file); FM1_USB_TRACE: the bus
     FM1_USB_SIE_NS=N      --usb: each SIE register access takes N ns (default 1000)
     FM1_USB_WAV=FILE      --usb, --bench / --shot: record the USB audio as a computer would (s16le, no header);
                           FM1_USB_RATE=HZ asks for that rate if the device lists it (default 44100)
     FM1_LCD_TIMED=1       the LCD's transfers take their time at the SPI clock, as on an FM-1 (a 240 x 240
                           frame ~77 ms at 12 MHz; by default they finish at once). On with FM1_STRICT too;
                           FM1_LCD_INSTANT=1 turns it off again
     FM1_STRICT=1          as an FM-1 is: a misaligned 16- / 32-bit RAM access faults (EMU_MSG bit 0), the
                           LCD is timed (FM1_LCD_TIMED), and each instruction takes its measured cycles (a
                           taken branch ~5, a call ~5, ALU ~0.6), both cores, the flash cache's misses each
                           core's own; the interpreter, no JIT: slower. FM1_STRICT_LOG: list misaligned
                           accesses
     FM1_TPROF=PATH,FROM,TO  --shot: both cores' time-weighted PCs and the flash cache's misses by line from
                           FROM to TO ms, into PATH (with FM1_STRICT: where the cycles go)";

fn main() {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("{}", buildinfo::version("fm1-live"));
        if let Some(w) = buildinfo::stale() {
            println!("warning: {w}");
        }
        return;
    }
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    let Some(o) = parse() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    if o.seed.is_some() && o.flaky_core1.is_none() {
        eprintln!("error: --seed needs --flaky-core1 (it seeds its misreads)");
        std::process::exit(2);
    }
    if let Some(ms) = o.shot {
        return shot(&o, ms);
    }
    if let Some(ms) = o.bench {
        if std::env::var("FM1_BENCH_THREAD").is_ok() {
            let o2 = Opts { bin: o.bin.clone(), ..o };
            return std::thread::spawn(move || { high_priority(); bench(&o2, ms) }).join().unwrap();
        }
        return bench(&o, ms);
    }
    let mut m = match Machine::new(&o) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("cannot read {}: {e}", o.bin);
            if std::env::args().len() == 1 {
                eprintln!("(no firmware given)\n{USAGE}");
            }
            std::process::exit(1);
        }
    };
    let mut pot = m.soc.periph.adc_value[4];

    let ring = Arc::new(AudioRing { q: Mutex::new(VecDeque::new()), started: AtomicBool::new(false), underruns: AtomicU64::new(0) });
    let _stream = match start_audio(ring.clone()) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("audio off: {e}");
            None
        }
    };
    let lcd = Arc::new(Mutex::new(vec![0u16; 240 * 240]));
    let leds = Arc::new(Mutex::new([0f32; 41]));
    let pads = Arc::new(Mutex::new([0u32; 16]));
    let board = m.soc.board;
    let status = Arc::new(Mutex::new(String::from("booting")));
    let quit = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel::<Action>();
    let _midi = midi_inputs(&o, &tx);
    let (usb_tx, usb_rx) = mpsc::channel::<Vec<u8>>();
    let usb_on = o.usb;

    let audio_ok = _stream.is_some();
    let emu = {
        let (ring, lcd, leds, pads, status, quit) = (ring.clone(), lcd.clone(), leds.clone(), pads.clone(), status.clone(), quit.clone());
        let flash = o.flash.clone();
        std::thread::spawn(move || {
            high_priority();
            let start = Instant::now();
            let mut offset_ms = 0.0f64; // wall-clock time given up after falling behind
            let mut last_lcd = Instant::now();
            let mut last_stat = Instant::now();
            let (mut stat_emu, mut stat_wall) = (0u64, Instant::now());
            let mut usb_ports = UsbPorts::default();
            while !quit.load(Ordering::Relaxed) {
                while let Ok(a) = rx.try_recv() {
                    m.soc.periph.apply(&a);
                }
                if usb_on {
                    usb_ports.service(&mut m, &usb_rx, &usb_tx);
                }
                let real = start.elapsed().as_secs_f64() * 1000.0 - offset_ms;
                let emu_ms = m.soc.periph.cycles as f64 * 1000.0 / m.soc.periph.hz as f64;
                if real - emu_ms > 250.0 {
                    offset_ms += real - emu_ms - LEAD_MS; // too far behind: resync, do not race
                }
                let buffered = ring.q.lock().unwrap().len() as f64 / 88.2;
                let behind = if audio_ok && m.soc.periph.audio_halves > 0 {
                    buffered < AUDIO_TARGET_MS
                } else {
                    emu_ms < real + LEAD_MS
                };
                if behind {
                    if let Err(e) = m.run(20_000) {
                        *status.lock().unwrap() = format!("halted: {e}");
                        halted(&e);
                        break;
                    }
                } else {
                    std::thread::sleep(Duration::from_millis(1));
                }
                if !m.soc.periph.audio.is_empty() {
                    let mut q = ring.q.lock().unwrap();
                    q.extend(m.soc.periph.audio.drain(..));
                    ring.started.store(true, Ordering::Relaxed);
                    let cap = 44_100 * 2 / 4; // 250 ms
                    if q.len() > cap {
                        let extra = q.len() - cap;
                        q.drain(..extra);
                    }
                }
                if last_lcd.elapsed() >= Duration::from_millis(16) {
                    lcd.lock().unwrap().copy_from_slice(&m.soc.lcd.fb);
                    if m.soc.board == fm1_soc::Board::Smk37 {
                        let mut l = [0f32; 41];
                        l[..fm1_soc::smk::NLED].copy_from_slice(&m.soc.periph.smk.led_take());
                        *leds.lock().unwrap() = l;
                        *pads.lock().unwrap() = m.soc.periph.smk.pads;
                    } else {
                        *leds.lock().unwrap() = m.soc.periph.led_take();
                    }
                    last_lcd = Instant::now();
                }
                if last_stat.elapsed() >= Duration::from_millis(500) {
                    let e = m.soc.periph.ms();
                    let speed = (e - stat_emu) as f64 / stat_wall.elapsed().as_secs_f64() / 1000.0;
                    let buffered = ring.q.lock().unwrap().len() as f64 / 88.2;
                    *status.lock().unwrap() = format!(
                        "{:.2}x real time | {} MHz | audio {:.0} ms buffered, {} underruns",
                        speed, m.soc.periph.hz / 1_000_000, buffered, ring.underruns.load(Ordering::Relaxed)
                    );
                    if e / 2000 != stat_emu / 2000 {
                        eprintln!("[{:5.1} s] {}", e as f64 / 1000.0, status.lock().unwrap());
                    }
                    stat_emu = e;
                    stat_wall = Instant::now();
                    last_stat = Instant::now();
                }
            }
            save_flash(&m, flash.as_ref());
        })
    };

    if board == fm1_soc::Board::Smk37 {
        window_smk(&o, &tx, &lcd, &leds, &pads, &status);
        quit.store(true, Ordering::Relaxed);
        let _ = emu.join();
        return;
    }
    let mut win = match Window::new(&format!("FM-1 · {} (emulated)", o.name), W, H, WindowOptions::default()) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("window: {e}");
            quit.store(true, Ordering::Relaxed);
            let _ = emu.join();
            return;
        }
    };
    win.set_target_fps(60);
    let base = panel::base();
    let mut buf = vec![0u32; W * H];
    let mut down = [false; 41];
    let mut enc_clicks = [0i32; 7];
    let mut turning = [0u32; 8];
    let mut scroll_acc = [0f32; 8];
    let mut shown_leds = [0f32; 41];
    let mut grab: Option<Ctl> = None;
    let mut was_pressed = false;
    let mut mouse_vel = 0u8;                    // the key under the mouse: how hard (its height; lower = harder)
    let (mut last_y, mut drag_acc) = (0f32, 0f32);
    let mut last_title = Instant::now();
    eprintln!("mouse: click/drag the keys (lower = harder; drag down on a held key = aftertouch), click buttons, drag or scroll the knobs (MASTER = volume)");
    eprintln!("keys: A W S E D F T G Y H U J K O L P ; ' = C4..F5, Z/X OCT, Space PLAY, R REC, 1..0 buttons, arrows and , . for SELECT/PRESETS/ALGORITHM; Esc quits");

    while win.is_open() && !win.is_key_down(Key::Escape) {
        let turn = |i: usize, clicks: i32, enc_clicks: &mut [i32; 7], turning: &mut [u32; 8]| {
            enc_clicks[i] += clicks;
            turning[i] = 10;
            let _ = tx.send(Action::Enc { enc: ENCS[i].1, clicks });
        };

        // ---- mouse
        let pos = win.get_mouse_pos(MouseMode::Clamp);
        let pressed = win.get_mouse_down(MouseButton::Left);
        if let Some((mx, my)) = pos {
            if pressed && !was_pressed {
                grab = panel::hit(mx, my);
                last_y = my;
                drag_acc = 0.0;
                if let Some(Ctl::Key(k)) = grab {
                    mouse_vel = panel::key_vel(k, my);
                }
            } else if pressed {
                match grab {
                    // glissando: the held note follows the pointer across keys; on the same key,
                    // its height is the pressure (aftertouch)
                    Some(Ctl::Key(k0)) => {
                        if let Some(Ctl::Key(k)) = panel::hit(mx, my) {
                            let v = panel::key_vel(k, my);
                            if k != k0 {
                                grab = Some(Ctl::Key(k));
                                mouse_vel = v;
                            } else if v.abs_diff(mouse_vel) >= 4 {
                                mouse_vel = v;
                                let _ = tx.send(Action::Pressure { id: k, vel: v });
                            }
                        }
                    }
                    Some(Ctl::Enc(i)) => {
                        drag_acc += last_y - my;
                        while drag_acc >= DRAG_PER_CLICK {
                            drag_acc -= DRAG_PER_CLICK;
                            turn(i, 1, &mut enc_clicks, &mut turning);
                        }
                        while drag_acc <= -DRAG_PER_CLICK {
                            drag_acc += DRAG_PER_CLICK;
                            turn(i, -1, &mut enc_clicks, &mut turning);
                        }
                    }
                    Some(Ctl::Pot) => {
                        let v = (pot as f32 + (last_y - my) * 4.0).clamp(0.0, 1023.0) as u16;
                        if v != pot {
                            pot = v;
                            turning[7] = 10;
                            let _ = tx.send(Action::Adc { ch: 4, value: pot });
                        }
                    }
                    _ => {}
                }
                last_y = my;
            }
            if let Some((_, dy)) = win.get_scroll_wheel() {
                match panel::hit(mx, my) {
                    Some(Ctl::Enc(i)) => {
                        scroll_acc[i] += dy;
                        while scroll_acc[i] >= 1.0 {
                            scroll_acc[i] -= 1.0;
                            turn(i, 1, &mut enc_clicks, &mut turning);
                        }
                        while scroll_acc[i] <= -1.0 {
                            scroll_acc[i] += 1.0;
                            turn(i, -1, &mut enc_clicks, &mut turning);
                        }
                    }
                    Some(Ctl::Pot) => {
                        pot = (pot as f32 + dy * 24.0).clamp(0.0, 1023.0) as u16;
                        turning[7] = 10;
                        let _ = tx.send(Action::Adc { ch: 4, value: pot });
                    }
                    _ => {}
                }
            }
        }
        if !pressed {
            grab = None;
        }
        was_pressed = pressed;

        // ---- held keys and buttons: computer keyboard + mouse
        let mut want = [false; 41];
        for k in win.get_keys() {
            if let Some(id) = piano(k).or_else(|| button(k)) {
                want[id as usize] = true;
            }
        }
        if let Some(Ctl::Key(id) | Ctl::Btn(id)) = grab {
            want[id as usize] = true;
        }
        for id in 0..41 {
            if want[id] != down[id] {
                down[id] = want[id];
                if want[id] && id >= 14 {
                    // the mouse's key at its height, the computer keyboard's at 100
                    let mouse = matches!(grab, Some(Ctl::Key(k)) if k as usize == id);
                    let _ = tx.send(Action::KeyVel { vel: if mouse { mouse_vel } else { 100 } });
                }
                let _ = tx.send(Action::Key { id: id as u8, down: want[id] });
            }
        }
        for k in win.get_keys_pressed(KeyRepeat::Yes) {
            match k {
                Key::Left => turn(0, -1, &mut enc_clicks, &mut turning),
                Key::Right => turn(0, 1, &mut enc_clicks, &mut turning),
                Key::Down => turn(1, -1, &mut enc_clicks, &mut turning),
                Key::Up => turn(1, 1, &mut enc_clicks, &mut turning),
                Key::Comma => turn(2, -1, &mut enc_clicks, &mut turning),
                Key::Period => turn(2, 1, &mut enc_clicks, &mut turning),
                _ => {}
            }
        }
        for t in turning.iter_mut() {
            *t = t.saturating_sub(1);
        }

        // ---- draw: LEDs fade out over a few frames instead of flickering
        {
            let l = leds.lock().unwrap();
            for i in 0..41 {
                shown_leds[i] = l[i].max(shown_leds[i] * 0.8);
            }
        }
        {
            let fb = lcd.lock().unwrap();
            let st = panel::State { lcd: &fb, enc_clicks: &enc_clicks, pot, down: &down, leds: &shown_leds, turning: &turning };
            panel::draw(&mut buf, &base, &st);
        }
        if win.update_with_buffer(&buf, W, H).is_err() {
            break;
        }
        if last_title.elapsed() >= Duration::from_millis(500) {
            win.set_title(&format!("FM-1 · {} — {}", o.name, status.lock().unwrap()));
            last_title = Instant::now();
        }
    }
    quit.store(true, Ordering::Relaxed);
    let _ = emu.join();
}

/// The SMK-37's window: its panel (panel_smk.rs), the mouse on its keys (velocity: how far down
/// a key is hit), pads, faders, wheels and knobs; the computer keyboard as on the page.
fn window_smk(o: &Opts, tx: &mpsc::Sender<Action>, lcd: &Mutex<Vec<u16>>, leds: &Mutex<[f32; 41]>, pads: &Mutex<[u32; 16]>, status: &Mutex<String>) {
    use fm1_soc::smk::{ANALOG_MOD, ANALOG_PITCH, MOD_REST, PITCH_REST};
    use panel_smk::Ctl as S;
    let (w, h) = (panel_smk::W, panel_smk::H);
    let mut win = match Window::new(&format!("SMK-37 · {} (emulated)", o.name), w, h, WindowOptions { resize: true, ..WindowOptions::default() }) {
        Ok(w) => w,
        Err(e) => return eprintln!("window: {e}"),
    };
    win.set_target_fps(60);
    let base = panel_smk::base();
    let mut buf = vec![0u32; w * h];
    let btn0 = fm1_soc::smk::BTN0;
    let (mut down, mut enc_clicks, mut turning) = ([false; 64], [0i32; 8], [0u32; 8]);
    let mut analog = [0u16; 23];
    analog[ANALOG_PITCH as usize] = PITCH_REST;
    analog[ANALOG_MOD as usize] = MOD_REST;
    let mut set_analog = |analog: &mut [u16; 23], i: usize, v: u16| {
        if analog[i] != v {
            analog[i] = v;
            let _ = tx.send(Action::Adc { ch: i as u8, value: v });
        }
    };
    let turn = |i: usize, clicks: i32, enc_clicks: &mut [i32; 8], turning: &mut [u32; 8]| {
        enc_clicks[i] += clicks;
        turning[i] = 10;
        let _ = tx.send(Action::Enc { enc: i as u8, clicks });
    };
    // 0 (softest) .. 1 (hardest) -> 80 ms .. 2 ms between a key's two contacts
    let vel = |f: f32| Action::Vel { us: (80_000f32.ln() + (2_000f32.ln() - 80_000f32.ln()) * f.clamp(0.0, 1.0)).exp() as u32 };
    let (mut grab, mut was_pressed, mut last_y, mut drag_acc, mut scroll_acc) = (None, false, 0f32, 0f32, [0f32; 8]);
    let mut last_title = Instant::now();
    eprintln!("SMK-37: click the keys (lower = harder), pads, buttons; drag faders, wheels, knobs. A..' C4..F5, Z/X OCTAVE, Space SEQ PLAY, R SEQ REC, 1..8 pads, arrows K1/K2; Esc quits");
    while win.is_open() && !win.is_key_down(Key::Escape) {
        let pressed = win.get_mouse_down(MouseButton::Left);
        if let Some((mx, my)) = win.get_mouse_pos(MouseMode::Clamp) {
            // (the window may be resized: back to panel coordinates)
            let (ww, wh) = win.get_size();
            let (mx, my) = (mx * w as f32 / ww.max(1) as f32, my * h as f32 / wh.max(1) as f32);
            if pressed && !was_pressed {
                grab = panel_smk::hit(mx, my);
                last_y = my;
                drag_acc = 0.0;
                match grab {
                    Some(S::Key(_, f)) => { let _ = tx.send(vel(f)); }
                    Some(S::Pad(p)) => set_analog(&mut analog, p, 700),
                    _ => {}
                }
            }
            if pressed {
                match grab {
                    Some(S::Key(k, _)) => {
                        if let Some(S::Key(k2, f)) = panel_smk::hit(mx, my) {
                            if k2 != k {
                                let _ = tx.send(vel(f));
                                grab = Some(S::Key(k2, f));
                            }
                        }
                    }
                    Some(S::Enc(i)) => {
                        drag_acc += last_y - my;
                        while drag_acc >= DRAG_PER_CLICK { drag_acc -= DRAG_PER_CLICK; turn(i, 1, &mut enc_clicks, &mut turning); }
                        while drag_acc <= -DRAG_PER_CLICK { drag_acc += DRAG_PER_CLICK; turn(i, -1, &mut enc_clicks, &mut turning); }
                    }
                    Some(S::Fader(f)) => set_analog(&mut analog, 16 + f, panel_smk::fader_value(f, my)),
                    Some(S::Pitch) => set_analog(&mut analog, ANALOG_PITCH as usize, panel_smk::wheel_value(true, my)),
                    Some(S::Mod) => set_analog(&mut analog, ANALOG_MOD as usize, panel_smk::wheel_value(false, my)),
                    _ => {}
                }
                last_y = my;
            }
            if let Some((_, dy)) = win.get_scroll_wheel() {
                if let Some(S::Enc(i)) = panel_smk::hit(mx, my) {
                    scroll_acc[i] += dy;
                    while scroll_acc[i] >= 1.0 { scroll_acc[i] -= 1.0; turn(i, 1, &mut enc_clicks, &mut turning); }
                    while scroll_acc[i] <= -1.0 { scroll_acc[i] += 1.0; turn(i, -1, &mut enc_clicks, &mut turning); }
                }
            }
        }
        if !pressed {
            match grab {
                Some(S::Pad(p)) => set_analog(&mut analog, p, 0),
                Some(S::Pitch) => set_analog(&mut analog, ANALOG_PITCH as usize, PITCH_REST),
                _ => {}
            }
            grab = None;
        }
        was_pressed = pressed;

        // held keys and buttons: the computer keyboard (as on the page) and the mouse
        let mut want = [false; 64];
        let mut pad_keys = [false; 8];
        for k in win.get_keys() {
            if let Some(id) = piano(k) {
                want[(id - 21 + 12) as usize] = true; // (piano() gives the FM-1's ids, C4 = 21)
            }
            let b = match k { Key::Z => Some(4), Key::X => Some(5), Key::Space => Some(16), Key::R => Some(17), _ => None };
            if let Some(b) = b {
                want[(btn0 + b) as usize] = true;
            }
            let pad = [Key::Key1, Key::Key2, Key::Key3, Key::Key4, Key::Key5, Key::Key6, Key::Key7, Key::Key8].iter().position(|&p| p == k);
            if let Some(p) = pad {
                pad_keys[p] = true;
            }
        }
        for (p, &on) in pad_keys.iter().enumerate() {
            if grab != Some(S::Pad(p)) {
                set_analog(&mut analog, p, if on { 700 } else { 0 });
            }
        }
        match grab {
            Some(S::Key(id, _)) | Some(S::Btn(id)) => want[id as usize] = true,
            _ => {}
        }
        for id in 0..55 {
            if want[id] != down[id] {
                if want[id] && id < 37 && !matches!(grab, Some(S::Key(k, _)) if k as usize == id) {
                    let _ = tx.send(vel(0.75));
                }
                down[id] = want[id];
                let _ = tx.send(Action::Key { id: id as u8, down: want[id] });
            }
        }
        for k in win.get_keys_pressed(KeyRepeat::Yes) {
            match k {
                Key::Left => turn(0, -1, &mut enc_clicks, &mut turning),
                Key::Right => turn(0, 1, &mut enc_clicks, &mut turning),
                Key::Down => turn(1, -1, &mut enc_clicks, &mut turning),
                Key::Up => turn(1, 1, &mut enc_clicks, &mut turning),
                Key::Comma => turn(2, -1, &mut enc_clicks, &mut turning),
                Key::Period => turn(2, 1, &mut enc_clicks, &mut turning),
                _ => {}
            }
        }
        for t in turning.iter_mut() {
            *t = t.saturating_sub(1);
        }
        {
            let fb = lcd.lock().unwrap();
            let l = *leds.lock().unwrap();
            let pc = *pads.lock().unwrap();
            let st = panel_smk::State {
                lcd: &fb,
                enc_clicks: &enc_clicks,
                turning: &turning,
                down: &down,
                leds: &std::array::from_fn(|k| l[k]),
                pads: &pc,
                pad_force: &std::array::from_fn(|p| analog[p]),
                faders: &std::array::from_fn(|f| analog[16 + f]),
                pitch: analog[ANALOG_PITCH as usize],
                modw: analog[ANALOG_MOD as usize],
            };
            panel_smk::draw(&mut buf, &base, &st);
        }
        if win.update_with_buffer(&buf, w, h).is_err() {
            break;
        }
        if last_title.elapsed() >= Duration::from_millis(500) {
            win.set_title(&format!("SMK-37 · {} — {}", o.name, status.lock().unwrap()));
            last_title = Instant::now();
        }
    }
}
