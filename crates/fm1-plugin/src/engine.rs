//! The emulated FM-1 on a thread of its own, running a little ahead of the host.
//!
//! The firmware makes its audio in DMA halves, and how long a half takes to emulate varies, so the
//! machine does not run inside the host's audio callback: this thread keeps a ring of its 44.1 kHz
//! frames `lead` frames ahead of what the host has taken, and the host's MIDI reaches it stamped
//! with the frame it belongs to (`lead` frames after the frame the host was playing). The machine
//! takes each event once it has made that many frames, so notes keep their spacing and the delay
//! is a constant the plugin reports to the host as its latency.

use fm1_core::{Bus, Cpu};
use fm1_soc::{machine, Action, Board, Soc};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const RAM_BASE: u32 = 0x01c0_0000;
const RAM_SIZE: usize = 1 << 20;
/// The firmware's output rate.
pub const RATE: f64 = 44_100.0;
/// Flash sectors, for saving what the firmware wrote (its patches) with the host's project.
const SECTOR: usize = 4096;

/// A MIDI message for the machine, with the frame (of its 44.1 kHz output) it is due at.
#[derive(Clone, Copy, Default)]
pub struct Midi {
    pub frame: u64,
    pub bytes: [u8; 3],
    pub len: u8,
}

/// What the editor and the plugin ask of the machine.
pub enum Cmd {
    /// Boot a package (.fwsc), with flash sectors saved from an earlier run written over it.
    Boot { pkg: Arc<Vec<u8>>, sectors: Vec<(u32, Vec<u8>)> },
    /// Validate and replace a shared setup, including a different save of the same package.
    Load { firmware: crate::Firmware, sectors: Vec<crate::Sector>, reply: mpsc::Sender<Result<(), String>> },
    /// Capture live flash, rather than the host-state copy refreshed every two seconds.
    Snapshot { pkg: Arc<Vec<u8>>, reply: mpsc::Sender<Result<Vec<crate::Sector>, String>> },
    Key { id: u8, down: bool },
    Enc { enc: u8, clicks: i32 },
    /// SMK-37: an analog input (fm1_soc::smk's ANALOG_*), the next keys' velocity (µs)
    Analog { idx: u8, value: u16 },
    Vel(u32),
    /// The ring's lead in frames (from the host's block size).
    Lead(usize),
}

/// What the editor shows: the screen, the LEDs and how the machine is doing.
pub struct View {
    pub lcd: Vec<u16>,
    pub leds: [f32; 41],
    /// SMK-37: the pads' colours
    pub pads: [u32; 16],
    pub board: Board,
    pub booted: bool,
    pub halted: bool,
    /// emulated time over the time spent emulating it, over the last second: how much faster
    /// than real time the machine could run (it runs at the host's pace)
    pub speed: f32,
    /// frames the host found missing (the machine fell behind): all, and in the last second
    pub starved: u64,
    pub late: u64,
}

/// The flash sectors that differ from the package's.
type Sectors = Vec<(u32, Vec<u8>)>;

pub struct Shared {
    pub view: Mutex<View>,
    pub quit: AtomicBool,
}

struct Machine {
    cpu: Cpu,
    cpu1: Option<Cpu>,
    soc: Soc,
    pending_tick: bool,
    halted: bool,
    /// the flash as booted, to find the sectors the firmware has written since
    flash0: Vec<u8>,
}

impl Machine {
    /// `lcd_timed`: the LCD's transfers take their time, as on an FM-1 (fm1_soc's `Soc::lcd_timed`)
    fn boot(pkg: &[u8], sectors: &[(u32, Vec<u8>)], mhz: u64, lcd_timed: bool) -> Result<Self, String> {
        crate::savefile::validate_sectors(&sectors.iter().map(|(at, data)| crate::Sector {
            at: *at, data: Arc::new(data.clone()),
        }).collect::<Vec<_>>())?;
        let st = std::panic::catch_unwind(|| fm1_soc::stock_from_package(pkg, None))
            .map_err(|_| "malformed firmware package".to_owned())??;
        let mut soc = Soc::new(RAM_SIZE, st.xip);
        soc.periph.hz = mhz * 1_000_000;
        soc.periph.skip_idle = true;
        soc.lcd_timed = lcd_timed;
        let mut cpu = Cpu::new(st.entry);
        cpu.jit_on = true;
        cpu.user = true; // the SPL hands cpu0 over in user mode (fm1-core's Cpu::user)
        let params = RAM_BASE + 0x7fc00;
        soc.write32(params, params + 0x40);
        cpu.regs[0] = fm1_soc::stock_boot(&mut soc, &st.flash, params, st.board);
        let flash0 = soc.flash.mem.clone();
        restore_flash(&mut soc, sectors);
        Ok(Machine { cpu, cpu1: None, soc, pending_tick: false, halted: false, flash0 })
    }

    /// fm1-live's loop: cpu0's blocks, cpu1 beside once the firmware releases it.
    fn run(&mut self, n: u32) {
        let jit = self.cpu.jit_on;
        let Machine { cpu, cpu1, soc, pending_tick, .. } = self;
        if machine::run_dual(cpu1, soc, n, RAM_SIZE, jit, |soc, k| machine::run_blocks(cpu, soc, k, pending_tick)).is_err() {
            self.halted = true;
        }
    }

    fn changed_sectors(&self) -> Sectors {
        let (now, then) = (&self.soc.flash.mem, &self.flash0);
        (0..now.len().min(then.len()) / SECTOR)
            .filter(|s| now[s * SECTOR..(s + 1) * SECTOR] != then[s * SECTOR..(s + 1) * SECTOR])
            .map(|s| ((s * SECTOR) as u32, now[s * SECTOR..(s + 1) * SECTOR].to_vec()))
            .collect()
    }
}

fn restore_flash(soc: &mut Soc, sectors: &[(u32, Vec<u8>)]) {
    for (at, data) in sectors {
        let at = *at as usize;
        soc.flash.mem[at..at + data.len()].copy_from_slice(data);
        let from = at.max(fm1_soc::XIP_FLASH_OFFSET);
        let end = (at + data.len()).min(fm1_soc::XIP_FLASH_OFFSET + soc.xip.len());
        if from < end {
            soc.xip[from - fm1_soc::XIP_FLASH_OFFSET..end - fm1_soc::XIP_FLASH_OFFSET]
                .copy_from_slice(&data[from - at..end - at]);
        }
    }
}

struct Snapshot {
    pkg: Arc<Vec<u8>>,
    reply: mpsc::Sender<Result<Vec<crate::Sector>, String>>,
    flash: Vec<u8>,
    started: Instant,
    quiet_since: Instant,
    polled: Instant,
}

fn project_sectors(sectors: &Sectors) -> Vec<crate::Sector> {
    sectors.iter().map(|(at, data)| crate::Sector { at: *at, data: Arc::new(data.clone()) }).collect()
}

fn cancel_snapshots(pending: &mut Vec<Snapshot>) {
    for request in pending.drain(..) {
        let _ = request.reply.send(Err("firmware changed during export; try again".into()));
    }
}

fn snapshots(pending: &mut Vec<Snapshot>, machine: Option<&Machine>, params: &crate::Fm1Params, now: Instant) {
    pending.retain_mut(|request| {
        if now.duration_since(request.started) >= Duration::from_secs(5) {
            let _ = request.reply.send(Err("flash did not finish saving; try exporting again".into()));
            return false;
        }
        let Some(m) = machine else {
            let _ = request.reply.send(Err("no firmware is running".into()));
            return false;
        };
        if now.duration_since(request.polled) < Duration::from_millis(20) { return true; }
        request.polled = now;
        if request.flash != m.soc.flash.mem {
            request.flash.copy_from_slice(&m.soc.flash.mem);
            request.quiet_since = now;
        }
        // A firmware save may span several SPI commands and render batches.
        if m.soc.flash.busy() || now.duration_since(request.quiet_since) < Duration::from_millis(250) { return true; }
        let sectors = project_sectors(&m.changed_sectors());
        let fw = params.firmware.lock().unwrap();
        if *fw.pkg == *request.pkg { *params.flash.lock().unwrap() = sectors.clone(); }
        let _ = request.reply.send(Ok(sectors));
        false
    });
}

/// The thread: commands in, MIDI in (stamped), frames out.
pub fn run(
    shared: Arc<Shared>,
    params: Arc<crate::Fm1Params>,
    cmds: mpsc::Receiver<Cmd>,
    mut midi_in: rtrb::Consumer<Midi>,
    mut out: rtrb::Producer<[f32; 2]>,
) {
    let mut m: Option<Machine> = None;
    // the instant LCD unless the host's environment asks for the timed one (FM1_LCD_TIMED)
    let lcd_timed = fm1_soc::lcd_timed_from_env();
    let mut lead = 2048usize;
    let mut made: u64 = 0; // frames pushed since boot
    let mut due: VecDeque<Midi> = VecDeque::new();
    let mut pot = u16::MAX;
    // the sectors last put in the project's state
    let mut saved: Sectors = Vec::new();
    let mut last_view = Instant::now();
    // when the host last took frames, and the free run's start (wall time, emulated ms)
    let mut last_take = Instant::now();
    let mut free: Option<(Instant, u64)> = None;
    let mut last_flash = Instant::now();
    let mut active_pkg: Option<Arc<Vec<u8>>> = None;
    let mut pending = Vec::new();
    let (mut speed_t, mut speed_ms, mut busy, mut starved0) = (Instant::now(), 0u64, Duration::ZERO, 0u64);
    while !shared.quit.load(Ordering::Relaxed) {
        while let Ok(c) = cmds.try_recv() {
            let (c, prepared, loaded) = match c {
                Cmd::Load { firmware, sectors, reply } => {
                    let sectors: Sectors = sectors.iter().map(|s| (s.at, s.data.to_vec())).collect();
                    match Machine::boot(&firmware.pkg, &sectors, params.clock.value().mhz(), lcd_timed) {
                        Ok(next) => (Cmd::Boot { pkg: firmware.pkg.clone(), sectors }, Some(next), Some((firmware, reply))),
                        Err(error) => { let _ = reply.send(Err(error)); continue; }
                    }
                }
                other => (other, None, None),
            };
            match c {
                Cmd::Boot { pkg, sectors } => {
                    cancel_snapshots(&mut pending);
                    m = prepared.or_else(|| Machine::boot(&pkg, &sectors, params.clock.value().mhz(), lcd_timed).ok());
                    active_pkg = m.as_ref().map(|_| pkg);
                    pot = u16::MAX;
                    let mut v = shared.view.lock().unwrap();
                    v.booted = m.is_some();
                    v.halted = false;
                    v.speed = 0.0;
                    // (frames missed before a machine runs do not count)
                    starved0 = v.starved;
                    speed_t = Instant::now();
                    speed_ms = 0;
                    busy = Duration::ZERO;
                    v.board = m.as_ref().map_or(Board::Fm1, |m| m.soc.board);
                    saved = sectors;
                    due.clear();
                    if let Some((firmware, reply)) = loaded {
                        *params.firmware.lock().unwrap() = firmware;
                        *params.flash.lock().unwrap() = project_sectors(&saved);
                        let _ = reply.send(Ok(()));
                    }
                    // (the frame count goes on: the host's stamps are against it)
                }
                Cmd::Snapshot { pkg, reply } => {
                    if active_pkg.as_ref().is_some_and(|active| **active == *pkg) {
                        if let Some(machine) = &m {
                            let now = Instant::now();
                            pending.push(Snapshot { pkg, reply, flash: machine.soc.flash.mem.clone(),
                                started: now, quiet_since: now, polled: now });
                        }
                    } else { let _ = reply.send(Err("firmware changed before export; try again".into())); }
                }
                Cmd::Load { .. } => unreachable!(), // normalized above, after validation
                Cmd::Key { id, down } => apply(&mut m, Action::Key { id, down }),
                Cmd::Enc { enc, clicks } => apply(&mut m, Action::Enc { enc, clicks }),
                Cmd::Analog { idx, value } => apply(&mut m, Action::Adc { ch: idx, value: value.min(1023) }),
                Cmd::Vel(us) => {
                    if let Some(m) = m.as_mut() {
                        m.soc.periph.smk_vel_us = us;
                    }
                }
                Cmd::Lead(l) => lead = l,
            }
        }
        // a new clock: the machine's timers count its cycles, so boot it again on the new one,
        // with what the firmware wrote to its flash
        let mhz = params.clock.value().mhz();
        if let (Some(mm), Some(pkg)) = (m.as_ref(), active_pkg.as_ref()) {
            if mm.soc.periph.hz != mhz * 1_000_000 && pending.is_empty() {
                let sectors = mm.changed_sectors();
                if let Ok(next) = Machine::boot(pkg, &sectors, mhz, lcd_timed) {
                    m = Some(next);
                    pot = u16::MAX;
                    due.clear();
                    let mut v = shared.view.lock().unwrap();
                    (v.halted, v.speed) = (false, 0.0);
                    starved0 = v.starved;
                    (speed_t, speed_ms, busy) = (Instant::now(), 0, Duration::ZERO);
                }
            }
        }
        snapshots(&mut pending, m.as_ref(), &params, Instant::now());
        // the MASTER pot, a host parameter
        let p = (params.master.value() * 1023.0).round().clamp(0.0, 1023.0) as u16;
        if p != pot && m.as_ref().is_some_and(|m| m.soc.board == Board::Fm1) {
            pot = p;
            apply(&mut m, Action::Adc { ch: 4, value: p });
        }
        while let Ok(e) = midi_in.pop() {
            due.push_back(e);
        }
        let Some(mm) = m.as_mut().filter(|m| !m.halted) else {
            // nothing to run: silence, so the host's clock and ours stay together
            while out.slots() > 0 && rtrb_fill(&out) < lead {
                let _ = out.push([0.0, 0.0]);
                made += 1;
            }
            due.retain(|e| e.frame > made);
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        let fill = rtrb_fill(&out);
        if fill < lead {
            last_take = Instant::now();
            free = None;
        }
        if fill >= lead && last_take.elapsed() >= Duration::from_millis(50) {
            // The host takes no audio (a host that only shows the window, a track not playing):
            // the machine runs on in real time, as the device would, its audio dropped and not
            // counted, so the host's stamps still line up when it takes frames again.
            let (t0, ms0) = *free.get_or_insert((Instant::now(), mm.soc.periph.ms()));
            if mm.soc.periph.ms() - ms0 < t0.elapsed().as_millis() as u64 {
                let t1 = Instant::now();
                mm.run(20_000);
                busy += t1.elapsed();
                mm.soc.periph.audio.clear();
            } else {
                std::thread::sleep(Duration::from_millis(1));
            }
        } else if fill >= lead {
            std::thread::sleep(Duration::from_micros(500));
        } else {
            while due.front().is_some_and(|e| e.frame <= made) {
                let e = due.pop_front().unwrap();
                for &b in &e.bytes[..e.len as usize] {
                    mm.soc.periph.apply(&Action::Midi { b });
                }
            }
            let t0 = Instant::now();
            mm.run(20_000);
            busy += t0.elapsed();
            let a = std::mem::take(&mut mm.soc.periph.audio);
            for f in a.chunks_exact(2) {
                // (a full ring drops the frame: the host has stopped taking them)
                if out.push([f[0] as f32 / 32768.0, f[1] as f32 / 32768.0]).is_ok() {
                    made += 1;
                }
            }
        }
        if last_view.elapsed() >= Duration::from_millis(30) {
            last_view = Instant::now();
            let mut v = shared.view.lock().unwrap();
            v.lcd.copy_from_slice(&mm.soc.lcd.fb);
            v.leds = if mm.soc.board == Board::Smk37 {
                let mut l = [0.0; 41];
                l[..fm1_soc::smk::NLED].copy_from_slice(&mm.soc.periph.smk.led_take());
                l
            } else {
                mm.soc.periph.led_take()
            };
            v.pads = mm.soc.periph.smk.pads;
            v.halted = mm.halted;
            if speed_t.elapsed() >= Duration::from_secs(1) {
                let ms = mm.soc.periph.ms();
                // (the first second is the boot's: it runs flat out to fill the ring)
                if speed_ms > 0 && busy > Duration::ZERO {
                    v.speed = ms.saturating_sub(speed_ms) as f32 / busy.as_secs_f32() / 1000.0;
                    v.late = v.starved - starved0;
                }
                starved0 = v.starved;
                (speed_t, speed_ms, busy) = (Instant::now(), ms.max(1), Duration::ZERO);
            }
        }
        if last_flash.elapsed() >= Duration::from_secs(2) {
            last_flash = Instant::now();
            let now = mm.changed_sectors();
            if now != saved {
                *params.flash.lock().unwrap() =
                    now.iter().map(|(at, d)| crate::Sector { at: *at, data: Arc::new(d.clone()) }).collect();
                saved = now;
            }
        }
    }
}

fn apply(m: &mut Option<Machine>, a: Action) {
    if let Some(m) = m.as_mut() {
        m.soc.periph.apply(&a);
    }
}

/// Frames waiting in the ring.
fn rtrb_fill(p: &rtrb::Producer<[f32; 2]>) -> usize {
    p.buffer().capacity() - p.slots()
}

#[cfg(test)]
mod portable_tests {
    use super::*;
    fn machine() -> Machine {
        let soc = Soc::new(RAM_SIZE, vec![0x55; 0x80000]);
        let flash0 = soc.flash.mem.clone();
        Machine { cpu: Cpu::new(0x02000120), cpu1: None, soc, pending_tick: false, halted: false, flash0 }
    }
    #[test]
    fn restored_sectors_update_the_entire_xip_sector() {
        let mut m = machine();
        let mut sector = vec![0xff; 4096]; sector[0] = 0x12;
        restore_flash(&mut m.soc, &[(0x74000, sector)]);
        assert_eq!(m.soc.read32(0x02070000), 0xffffff12);
        assert_eq!(m.soc.read32(0x02070010), 0xffffffff);
        assert_eq!(m.changed_sectors()[0].0, 0x74000);
    }
    #[test]
    fn snapshot_captures_live_spi_writes_and_waits_for_idle() {
        let mut m = machine();
        let pkg = Arc::new(vec![42; 960]);
        let params = crate::Fm1Params::default();
        *params.firmware.lock().unwrap() = crate::Firmware { name: "live".into(), pkg: pkg.clone() };
        let (tx, rx) = mpsc::channel();
        let start = Instant::now();
        let mut pending = vec![Snapshot { pkg, reply: tx, flash: m.soc.flash.mem.clone(),
            started: start, quiet_since: start, polled: start }];
        for cmd in [&[0x06][..], &[0x02, 0x0f, 0x90, 0x00, 0x12]] {
            m.soc.write32(0x500c0, 0);
            for &byte in cmd { m.soc.write32(0x11c08, byte as u32); }
            m.soc.write32(0x500c0, 1);
        }
        assert!(params.flash.lock().unwrap().is_empty(), "the periodic project-state copy is still old");
        snapshots(&mut pending, Some(&m), &params, start + Duration::from_millis(300));
        assert!(rx.try_recv().is_err());
        m.soc.write32(0x500c0, 0); // a subsequent SPI command is still selected
        snapshots(&mut pending, Some(&m), &params, start + Duration::from_millis(600));
        assert!(rx.try_recv().is_err());
        m.soc.write32(0x500c0, 1);
        snapshots(&mut pending, Some(&m), &params, start + Duration::from_millis(650));
        let sectors = rx.try_recv().unwrap().unwrap();
        assert_eq!(sectors[0].at, 0xf9000);
        assert_eq!(sectors[0].data[0], 0x12);
        assert_eq!(params.flash.lock().unwrap()[0].data[0], 0x12);
        assert!(pending.is_empty());
    }
    #[test]
    fn an_unfinished_flash_command_times_out_without_exporting() {
        let m = machine();
        let params = crate::Fm1Params::default();
        let (tx, rx) = mpsc::channel();
        let start = Instant::now();
        let mut pending = vec![Snapshot { pkg: Arc::new(vec![]), reply: tx,
            flash: m.soc.flash.mem.clone(), started: start, quiet_since: start, polled: start }];
        snapshots(&mut pending, Some(&m), &params, start + Duration::from_secs(6));
        assert!(rx.try_recv().unwrap().is_err());
        assert!(pending.is_empty());
    }
}
