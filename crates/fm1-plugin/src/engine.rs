//! The emulated FM-1 on a thread of its own, running a little ahead of the host.
//!
//! The firmware makes its audio in DMA halves, and how long a half takes to emulate varies, so the
//! machine does not run inside the host's audio callback: this thread keeps a ring of its 44.1 kHz
//! frames `lead` frames ahead of what the host has taken, and the host's MIDI reaches it stamped
//! with the frame it belongs to (`lead` frames after the frame the host was playing). The machine
//! takes each event once it has made that many frames, so notes keep their spacing and the delay
//! is a constant the plugin reports to the host as its latency.

use fm1_map::{KeysMode, Mapper, Out, Profile, Target};
use fm1_soc::{machine, Action, Board};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// The firmware's output rate.
pub const RATE: f64 = 44_100.0;
/// The shortest a panel key stays down or up on the emulated clock: a quick finger's press. A click
/// can come shorter (both of its events in one frame of the window, or while the machine waits for
/// the host), too short for the firmware's polling to see.
const KEY_MIN_MS: u64 = 100;

/// A MIDI message for the machine, with the frame (of its 44.1 kHz output) it is due at.
#[derive(Clone, Copy, Default)]
pub struct Midi {
    pub frame: u64,
    pub bytes: [u8; 3],
    pub len: u8,
}

/// What the editor and the plugin ask of the machine.
pub enum Cmd {
    /// Boot a package (.fwsc), with flash sectors saved from an earlier run written over it. A
    /// machine that runs this package with these sectors runs on.
    Boot { pkg: Arc<Vec<u8>>, sectors: Vec<(u32, Vec<u8>)> },
    /// The host initialized the plugin: `Boot` of the project's state (`Fm1Params::firmware` and
    /// `flash`) as it is when the machine's thread gets here, with every command before this one
    /// done. What the host's thread read would be the state from before a `Load` still in the queue.
    Init,
    /// Validate and replace a shared setup, including a different save of the same package.
    Load { firmware: crate::Firmware, sectors: Vec<crate::Sector>, reply: mpsc::Sender<Result<(), String>> },
    /// Capture live flash, rather than the host-state copy refreshed every two seconds.
    Snapshot { pkg: Arc<Vec<u8>>, reply: mpsc::Sender<Result<Vec<crate::Sector>, String>> },
    Key { id: u8, down: bool },
    Enc { enc: u8, clicks: i32 },
    /// SMK-37: an analog input (fm1_soc::smk's ANALOG_*), the next keys' velocity (µs)
    Analog { idx: u8, value: u16 },
    Vel(u32),
    /// The host's block in the machine's frames: the ring's lead is that plus the Lead parameter.
    Block(usize),
    /// A controller profile for the host's MIDI (none: to the MIDI IN jack as it comes).
    Map(Option<Profile>),
    /// The next note or CC binds to this target (none: stop learning).
    Learn(Option<Target>),
    /// Remove what is bound to a target.
    Unbind(Target),
    KeysMode(KeysMode),
    /// Start a WAV of the output, or finish the one being written.
    Record,
}

/// The controller's side of the machine's thread: the profile at work and what it holds.
struct Ctl {
    mapper: Option<Mapper>,
    outs: Vec<Out>,
    /// a control bound to Record was pressed: the run loop starts or stops the recorder
    record: bool,
    down: [bool; 41],
    enc: [i32; 7],
    labels: u32,
    msg: Option<String>,
    /// the profile changed: to the window
    changed: bool,
    /// it was learned in (or unbound): to the project and the user's profiles too
    learned: bool,
}

impl Ctl {
    fn new() -> Ctl {
        Ctl { mapper: None, outs: Vec::with_capacity(32), record: false, down: [false; 41], enc: [0; 7],
              labels: 0, msg: None, changed: true, learned: false }
    }

    /// A MIDI message that is due, through the profile (or straight to the jack, its channel
    /// moved by the parameter). A profile is the FM-1's panel: an SMK-37 firmware takes the
    /// host's MIDI as it comes, as in fm1-live.
    fn feed(&mut self, bytes: &[u8], m: &mut Machine, params: &crate::Fm1Params) {
        let fm1 = m.soc.board == Board::Fm1;
        let Some(mp) = self.mapper.as_mut().filter(|_| fm1) else {
            let ch = params.channel.value() as u8;
            for (i, &b) in bytes.iter().enumerate() {
                let b = if i == 0 && ch != 0 && (0x80..0xf0).contains(&b) { (b & 0xf0) | (ch - 1) } else { b };
                m.soc.periph.apply(&Action::Midi { b });
            }
            return;
        };
        let outs = &mut self.outs;
        mp.feed(bytes, &mut |o| outs.push(o));
        self.run(Some(m), params);
    }

    /// What the profile asked for: to the machine (if one runs, and is an FM-1), the window, the
    /// recorder.
    fn run(&mut self, m: Option<&mut Machine>, params: &crate::Fm1Params) {
        let mut m = m.filter(|m| m.soc.board == Board::Fm1);
        let ch = params.channel.value() as u8;
        let outs = std::mem::take(&mut self.outs);
        for o in &outs {
            match *o {
                Out::Key { id, down, vel } => {
                    if let Some(m) = m.as_deref_mut() {
                        if down && id >= fm1_map::KEY0 {
                            m.soc.periph.apply(&Action::KeyVel { vel: vel.clamp(1, 127) });
                        }
                        m.soc.periph.apply(&Action::Key { id, down });
                    }
                    if let Some(d) = self.down.get_mut(id as usize) {
                        *d = down;
                    }
                }
                Out::Enc { enc, clicks } => {
                    if let Some(m) = m.as_deref_mut() {
                        m.soc.periph.apply(&Action::Enc { enc, clicks });
                    }
                    if let Some(i) = crate::panel::ENCS.iter().position(|e| e.1 == enc) {
                        self.enc[i] += clicks;
                    }
                }
                // MASTER is the host's Master parameter: in the plugin it moves only through that
                // (automation, the host's own MIDI learn), never from the profile directly
                Out::Adc { .. } => {}
                Out::Midi { mut bytes, len } => {
                    if ch != 0 && (0x80..0xf0).contains(&bytes[0]) {
                        bytes[0] = (bytes[0] & 0xf0) | (ch - 1);
                    }
                    if let Some(m) = m.as_deref_mut() {
                        for &b in &bytes[..len as usize] {
                            m.soc.periph.apply(&Action::Midi { b });
                        }
                    }
                }
                Out::Record => self.record = true,
                Out::Labels => self.labels += 1,
                Out::KeysMode(k) => self.msg = Some(format!("Keys: {}", k.name())),
                Out::Learned => {
                    self.changed = true;
                    self.learned = true;
                    self.msg = Some("Learned".into());
                }
            }
        }
        self.outs = outs;
        self.outs.clear();
    }

    /// A new machine (a firmware loaded, booted again on another clock): what the profile held is
    /// let go of (in the window; the new machine holds nothing) and latched buttons are off.
    fn reset(&mut self, params: &crate::Fm1Params) {
        let outs = &mut self.outs;
        if let Some(mp) = self.mapper.as_mut() {
            mp.release_all(&mut |o| outs.push(o));
        }
        self.run(None, params);
        self.down = [false; 41];
        self.publish(params);
    }

    fn command(&mut self, c: Cmd, m: Option<&mut Machine>, params: &crate::Fm1Params) {
        match c {
            Cmd::Map(p) => {
                let outs = &mut self.outs;
                if let Some(mp) = self.mapper.as_mut() {
                    mp.release_all(&mut |o| outs.push(o));
                }
                self.run(m, params);
                self.mapper = p.map(Mapper::new);
                self.changed = true;
            }
            Cmd::Learn(t) => {
                if let Some(mp) = self.mapper.as_mut() {
                    mp.learn(t);
                }
            }
            Cmd::Unbind(t) => {
                if let Some(mp) = self.mapper.as_mut() {
                    mp.learn(None);
                    mp.unbind(t);
                    (self.changed, self.learned) = (true, true);
                    self.msg = Some(format!("{t}: unbound"));
                }
            }
            Cmd::KeysMode(k) => {
                let outs = &mut self.outs;
                if let Some(mp) = self.mapper.as_mut() {
                    mp.set_keys_mode(k, &mut |o| outs.push(o));
                }
                self.run(m, params);
            }
            _ => {}
        }
    }

    /// To the window (and, for a learned profile, the project and the user's profiles).
    fn publish(&mut self, params: &crate::Fm1Params) {
        // a learned profile: to the project, and to a file, with no lock the window takes held
        let mut save = None;
        if std::mem::take(&mut self.learned) {
            if let Some(p) = self.mapper.as_ref().map(|mp| mp.profile().clone()) {
                let mut c = params.controller.lock().unwrap();
                c.chosen = true;
                c.profile = Some(p.clone());
                save = Some(p);
            }
        }
        let mut v = params.map.lock().unwrap();
        if save.is_some() {
            v.save = save;
        }
        if self.changed {
            self.changed = false;
            v.rev += 1;
            v.profile = self.mapper.as_ref().map(|mp| mp.profile().clone());
        }
        if let Some(mp) = self.mapper.as_ref() {
            v.keys_mode = mp.keys_mode();
            v.learning = mp.learning();
        } else {
            v.learning = None;
        }
        v.down = self.down;
        v.enc = self.enc;
        v.labels = self.labels;
        if let Some(msg) = self.msg.take() {
            v.msg = Some(msg);
        }
    }
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

/// The machine: fm1_soc::machine's (its `soc`, its halt and the flash sectors the firmware has written,
/// saved with the host's project, are reached through `Deref`), with the panel keys' hold around it.
struct Machine {
    m: machine::Machine,
    /// the cycle each panel key may next change at, one for every `u8` id
    key_free: [u64; 256],
    /// each panel key's state once its queued changes are done
    key_last: [bool; 256],
}

impl Machine {
    /// `lcd_timed`: the LCD's transfers take their time, as on an FM-1 (fm1_soc's `Soc::lcd_timed`)
    fn boot(pkg: &[u8], sectors: &[(u32, Vec<u8>)], mhz: u64, lcd_timed: bool) -> Result<Self, String> {
        crate::savefile::validate_sectors(&sectors.iter().map(|(at, data)| crate::Sector {
            at: *at, data: Arc::new(data.clone()),
        }).collect::<Vec<_>>())?;
        let st = std::panic::catch_unwind(|| fm1_soc::stock_from_package(pkg, None))
            .map_err(|_| "malformed firmware package".to_owned())??;
        // (no chip key, `Options::sfc`: the XIP window shows the chip as stored)
        let opts = machine::Options { mhz, jit: true, lcd_timed, sfc: false, ..Default::default() };
        let mut m = machine::Machine::from_stock(st, &opts);
        let sectors: Vec<(u32, &[u8])> = sectors.iter().map(|(at, data)| (*at, &data[..])).collect();
        m.restore_sectors(&sectors)?;
        Ok(Machine { m, key_free: [0; 256], key_last: [false; 256] })
    }

    /// A press that would wait more than two holds is dropped, and its release with it: fast clicks
    /// end at most three holds after the last one.
    fn key(&mut self, id: u8, down: bool) {
        let (i, now, hold) = (id as usize, self.soc.periph.cycles, self.soc.periph.cycle_of_ms(KEY_MIN_MS));
        let at = self.key_free[i].max(now);
        if down == self.key_last[i] || (down && at - now > 2 * hold) {
            return;
        }
        self.key_last[i] = down;
        self.key_free[i] = at + hold;
        if at > now {
            self.soc.periph.schedule(at, Action::Key { id, down });
        } else {
            self.soc.periph.apply(&Action::Key { id, down });
        }
    }

    /// fm1-live's loop: cpu0's blocks, cpu1 beside once the firmware releases it.
    /// An error is why it halted, for good: an instruction the emulator cannot run, or a panic in the
    /// emulator's own code on what the firmware did, where it unwinds to here: in the interpreter and
    /// what it calls (a step, code not compiled yet, the peripherals' clock between blocks, the loop
    /// probe, core 1 outside compiled code), which would otherwise end the thread. Not under compiled
    /// code: that calls the bus and the interpreter's executor through `extern "C"` helpers (fm1-core's
    /// jit.rs), which a panic cannot leave, so a peripheral's model failing on an access made from a
    /// compiled block still aborts the process, the host with it.
    fn run(&mut self, n: u32) -> Result<(), String> {
        let m = &mut self.m;
        let ran = caught(std::panic::AssertUnwindSafe(|| m.run(n)));
        // (the machine keeps why it halted; a panic's reason it is told here)
        if let Err(why) = &ran {
            self.m.halt(why.clone());
        }
        ran
    }
}

// (its `cpu`, `cpu1` and `soc` are read through this. It is run through `run` above, which turns a
// panic into a halt: the shared machine's own `run`, reached past it as `self.m.run`, does not.)
impl std::ops::Deref for Machine {
    type Target = machine::Machine;
    fn deref(&self) -> &machine::Machine {
        &self.m
    }
}

impl std::ops::DerefMut for Machine {
    fn deref_mut(&mut self) -> &mut machine::Machine {
        &mut self.m
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

/// `f`, a panic in it as its error: the emulator's own code failing on a firmware or a package must
/// not end the machine's thread, which every later firmware needs. (A panic that unwinds to here:
/// one under compiled code does not, see `Machine::run`.)
fn caught<T>(f: impl FnOnce() -> Result<T, String> + std::panic::UnwindSafe) -> Result<T, String> {
    std::panic::catch_unwind(f).unwrap_or_else(|p| {
        let said = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned());
        Err(format!("the emulator failed ({})", said.as_deref().unwrap_or("a panic")))
    })
}

/// `Machine::boot`, with a panic anywhere in it as its error.
fn boot(pkg: &[u8], sectors: &[(u32, Vec<u8>)], mhz: u64, lcd_timed: bool) -> Result<Machine, String> {
    caught(|| Machine::boot(pkg, sectors, mhz, lcd_timed))
}

/// What the firmware has written to its flash, to the project's state when that changed.
fn save_sectors(m: &Machine, saved: &mut Sectors, params: &crate::Fm1Params) {
    let now = m.changed_sectors();
    if now != *saved {
        *params.flash.lock().unwrap() = project_sectors(&now);
        *saved = now;
    }
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
    let mut block = 1024usize;
    let mut ctl = Ctl::new();
    let mut made: u64 = 0; // frames pushed since boot
    let mut due: VecDeque<Midi> = VecDeque::new();
    let mut pot = u16::MAX;
    // the sectors last put in the project's state
    let mut saved: Sectors = Vec::new();
    let mut last_view = Instant::now();
    // when the host last took frames, and the free run's start (wall time, the machine's ms: none
    // again when another machine is booted, whose clock starts at 0)
    let mut last_take = Instant::now();
    let mut free: Option<(Instant, u64)> = None;
    let mut last_flash = Instant::now();
    let mut active_pkg: Option<Arc<Vec<u8>>> = None;
    let mut pending = Vec::new();
    let (mut speed_t, mut speed_ms, mut busy, mut starved0) = (Instant::now(), 0u64, Duration::ZERO, 0u64);
    let mut rec: Option<crate::wav::Wav> = None;
    // a run that ended in a halt: why, for the window
    let halt = |ran: Result<(), String>| {
        if let Err(e) = ran {
            *params.halt.lock().unwrap() = Some(format!("The firmware stopped: {e}"));
        }
    };
    while !shared.quit.load(Ordering::Relaxed) {
        while let Ok(c) = cmds.try_recv() {
            let (c, prepared, loaded) = match c {
                Cmd::Load { firmware, sectors, reply } => {
                    let sectors: Sectors = sectors.iter().map(|s| (s.at, s.data.to_vec())).collect();
                    match boot(&firmware.pkg, &sectors, params.clock.value().mhz(), lcd_timed) {
                        Ok(next) => (Cmd::Boot { pkg: firmware.pkg.clone(), sectors }, Some(next), Some((firmware, reply))),
                        Err(error) => { let _ = reply.send(Err(error)); continue; }
                    }
                }
                Cmd::Init => {
                    let pkg = params.firmware.lock().unwrap().pkg.clone();
                    if pkg.is_empty() {
                        continue;
                    }
                    let sectors = params.flash.lock().unwrap().iter().map(|s| (s.at, s.data.to_vec())).collect();
                    (Cmd::Boot { pkg, sectors }, None, None)
                }
                other => (other, None, None),
            };
            match c {
                Cmd::Boot { pkg, sectors } => {
                    // the host initializing again (a new sample rate) finds the package that runs with
                    // the sectors the project has from this machine, as last saved or as its flash is
                    // now: nothing to do. Other sectors are a state the host loaded (a preset, an
                    // undo), booted as another package is, so the project's flash and the machine's
                    // are the same again and what the firmware writes later is saved on top of that.
                    let runs = prepared.is_none() && active_pkg.as_ref().is_some_and(|a| Arc::ptr_eq(a, &pkg) || **a == *pkg);
                    if runs && m.as_ref().is_some_and(|mm| sectors == saved || sectors == mm.changed_sectors()) {
                        saved = sectors;
                        continue;
                    }
                    cancel_snapshots(&mut pending);
                    // (a project's package that does not boot: the window says why, as for a halt)
                    let booted = prepared.map_or_else(|| boot(&pkg, &sectors, params.clock.value().mhz(), lcd_timed), Ok);
                    *params.halt.lock().unwrap() = booted.as_ref().err().map(|e| format!("The firmware did not start: {e}"));
                    m = booted.ok();
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
                    drop(v);
                    ctl.reset(&params);
                    saved = sectors;
                    due.clear();
                    // (a free run is one machine's: its start is on that machine's clock)
                    free = None;
                    if let Some((firmware, reply)) = loaded {
                        *params.firmware.lock().unwrap() = firmware;
                        *params.flash.lock().unwrap() = project_sectors(&saved);
                        let _ = reply.send(Ok(()));
                    }
                    // (the frame count goes on: the host's stamps are against it)
                }
                Cmd::Init => unreachable!(), // (a Boot of what the project holds, above)
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
                Cmd::Key { id, down } => {
                    if let Some(m) = m.as_mut() {
                        m.key(id, down);
                    }
                }
                Cmd::Enc { enc, clicks } => apply(&mut m, Action::Enc { enc, clicks }),
                Cmd::Analog { idx, value } => apply(&mut m, Action::Adc { ch: idx, value: value.min(1023) }),
                Cmd::Vel(us) => {
                    if let Some(m) = m.as_mut() {
                        m.soc.periph.smk_vel_us = us;
                    }
                }
                Cmd::Block(b) => block = b,
                Cmd::Record => record(&mut rec, &params),
                c @ (Cmd::Map(_) | Cmd::Learn(_) | Cmd::Unbind(_) | Cmd::KeysMode(_)) => {
                    ctl.command(c, m.as_mut(), &params);
                    ctl.publish(&params);
                }
            }
        }
        // the ring's lead: the host's block and the Lead parameter, as the window sets it
        let lead = block + params.lead.value().frames();
        // a new clock: the machine's timers count its cycles, so boot it again on the new one,
        // with what the firmware wrote to its flash
        let mhz = params.clock.value().mhz();
        if let (Some(mm), Some(pkg)) = (m.as_ref(), active_pkg.as_ref()) {
            if mm.soc.periph.hz != mhz * 1_000_000 && pending.is_empty() {
                let sectors = mm.changed_sectors();
                if let Ok(next) = boot(pkg, &sectors, mhz, lcd_timed) {
                    m = Some(next);
                    *params.halt.lock().unwrap() = None;
                    pot = u16::MAX;
                    due.clear();
                    free = None;
                    ctl.reset(&params);
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
        let Some(mm) = m.as_mut().filter(|m| m.halted().is_none()) else {
            // nothing to run: silence, so the host's clock and ours stay together
            while out.slots() > 0 && rtrb_fill(&out) < lead {
                let _ = out.push([0.0, 0.0]);
                made += 1;
            }
            due.retain(|e| e.frame > made);
            // a machine that halted: the window is told, with the screen it stopped at, and what it
            // wrote to its flash before goes to the project (nothing changes after: the first look saves it)
            if let Some(mm) = m.as_ref() {
                if last_view.elapsed() >= Duration::from_millis(30) {
                    last_view = Instant::now();
                    let mut v = shared.view.lock().unwrap();
                    v.lcd.copy_from_slice(&mm.soc.lcd.fb);
                    v.halted = true;
                }
                if last_flash.elapsed() >= Duration::from_secs(2) {
                    last_flash = Instant::now();
                    save_sectors(mm, &mut saved, &params);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        // a control bound to Record (the profile's), as the button does
        if std::mem::take(&mut ctl.record) {
            record(&mut rec, &params);
        }
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
                halt(mm.run(20_000));
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
                ctl.feed(&e.bytes[..e.len as usize], mm, &params);
            }
            let t0 = Instant::now();
            halt(mm.run(20_000));
            busy += t0.elapsed();
            let a = std::mem::take(&mut mm.soc.periph.audio);
            if let Some(w) = rec.as_mut() {
                w.write(&a);
                // a failed write or the size limit: the take ends here, and the window says why
                if w.error.is_some() {
                    record(&mut rec, &params);
                }
            }
            for f in a.chunks_exact(2) {
                // (a full ring drops the frame: the host has stopped taking them)
                if out.push([f[0] as f32 / 32768.0, f[1] as f32 / 32768.0]).is_ok() {
                    made += 1;
                }
            }
        }
        if last_view.elapsed() >= Duration::from_millis(30) {
            last_view = Instant::now();
            ctl.publish(&params);
            params.rec.lock().unwrap().file = rec.as_ref().map(|w| (w.path.clone(), w.frames));
            let mut v = shared.view.lock().unwrap();
            v.lcd.copy_from_slice(&mm.soc.lcd.fb);
            v.leds = if mm.soc.board == Board::Smk37 {
                let mut l = [0.0; 41];
                l[..fm1_soc::smk::NLED].copy_from_slice(&mm.soc.periph.smk.led_take());
                l
            } else {
                mm.soc.periph.led_shown()
            };
            v.pads = mm.soc.periph.smk.pads;
            v.halted = mm.halted().is_some();
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
            save_sectors(mm, &mut saved, &params);
        }
    }
    // the plugin closed while recording: the file keeps what was played, with its length
    if let Some(w) = rec.take() {
        let _ = w.finish();
    }
}

/// Start a WAV of the output (in the Music folder's fm1-emu, named after the firmware), or
/// finish the one being written; what happened goes to the window.
fn record(rec: &mut Option<crate::wav::Wav>, params: &crate::Fm1Params) {
    let msg = match rec.take() {
        Some(w) => match w.finish() {
            Ok(p) => format!("Recorded {}", p.display()),
            Err(e) => format!("Recording: {e}"),
        },
        None => {
            let name = params.firmware.lock().unwrap().name.clone();
            match crate::wav::Wav::create(if name.is_empty() { "fm1" } else { &name }) {
                Ok(w) => {
                    let m = format!("Recording to {}", w.path.display());
                    *rec = Some(w);
                    m
                }
                Err(e) => format!("Recording: {e}"),
            }
        }
    };
    let mut r = params.rec.lock().unwrap();
    r.file = rec.as_ref().map(|w| (w.path.clone(), w.frames));
    r.msg = Some(msg);
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
    use fm1_core::Bus;
    fn machine() -> Machine {
        // (a bare image: cpu0 in supervisor mode, no idle skip, as a chip and a core start)
        let opts = machine::Options { skip_idle: false, user: false, ..Default::default() };
        Machine { m: machine::Machine::from_image(vec![0x55; 0x80000], 0x02000120, &opts), key_free: [0; 256], key_last: [false; 256] }
    }
    #[test]
    fn a_double_click_stays_two_presses_and_other_keys_do_not_wait() {
        let mut m = machine();
        let hold = m.soc.periph.cycle_of_ms(KEY_MIN_MS);
        for down in [true, false, true, false] { m.key(2, down); }
        m.key(3, true);
        assert!(m.soc.periph.keys_down[3]);
        for (at, down) in [(0, true), (hold - 1, true), (hold, false), (2 * hold - 1, false), (2 * hold, true), (3 * hold, false)] {
            let now = m.soc.periph.cycles;
            m.soc.periph.advance(at - now);
            assert_eq!(m.soc.periph.keys_down[2], down, "at cycle {at}");
        }
    }
    #[test]
    fn fast_clicks_stop_soon_after_the_last_one() {
        let mut m = machine();
        let hold = m.soc.periph.cycle_of_ms(KEY_MIN_MS);
        for _ in 0..50 {
            m.soc.periph.advance(hold / 2);
            m.key(2, true);
            m.key(2, false);
        }
        m.soc.periph.advance(3 * hold);
        assert!(m.soc.periph.script.is_empty(), "{} key changes still queued", m.soc.periph.script.len());
        assert!(!m.soc.periph.keys_down[2]);
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
    #[test]
    fn a_run_that_cannot_go_on_halts_with_its_reason() {
        let mut m = machine();
        m.soc.xip.fill(0xff); // (no instruction)
        let why = m.run(100).unwrap_err();
        assert!(m.halted().is_some() && why.contains("UnknownInstruction") && why.contains("0x02000120"), "{why}");
    }
    #[test]
    fn a_panic_is_an_error_with_its_text() {
        let said = caught(|| -> Result<(), String> { panic!("sector {} of 3", 7) });
        let other = caught(|| -> Result<(), String> { std::panic::panic_any(7u8) });
        assert_eq!(said.unwrap_err(), "the emulator failed (sector 7 of 3)");
        assert_eq!(other.unwrap_err(), "the emulator failed (a panic)");
        assert_eq!(caught(|| Ok::<_, String>(3)), Ok(3));
    }
    /// Felucca played as tools/catalog_check.py plays it in fm1-live (its script, 8 s at 240 MHz): the plugin's
    /// machine makes the audio tests/catalog.json has the hash of, where the package is the build that was
    /// hashed (a newer one of its author's is played, and its audio not compared). Needs
    /// reference/firmwares/felucca.fwsc (tools/fetch_firmwares.mjs): skipped without it, but not in CI, which
    /// fetches it and must not pass on nothing.
    #[test]
    fn felucca_sounds_as_the_catalog_has_it() {
        use sha2::{Digest, Sha256};
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../reference/firmwares/felucca.fwsc");
        let pkg = match std::fs::read(path) {
            Ok(p) => p,
            Err(_) if std::env::var_os("CI").is_none() => return eprintln!("no reference/firmwares/felucca.fwsc: skipped"),
            Err(e) => panic!("{path}: {e}"),
        };
        let hex = |b: &[u8]| format!("{:x}", Sha256::digest(b))[..16].to_owned();
        let catalog = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/catalog.json")).unwrap();
        let catalog: serde_json::Value = serde_json::from_slice(&catalog).unwrap();
        let want = &catalog["felucca"];
        let mut m = boot(&pkg, &[], 240, false).unwrap();
        fm1_soc::script::queue(&mut m.soc, "2000:C4,2000:m903c64,3500:C4^,3500:m803c00,5000:PLAY");
        while m.soc.periph.ms() < 8000 {
            m.run(20_000).unwrap();
        }
        let audio: Vec<u8> = m.soc.periph.audio.iter().flat_map(|s| s.to_le_bytes()).collect();
        assert!(audio.len() > 5 * 44_100 * 4, "{} bytes of audio in 8 s", audio.len());
        if want["package"] != hex(&pkg) {
            return eprintln!("felucca.fwsc is not the build tests/catalog.json hashed: played, its audio not compared");
        }
        let bytes = want["bytes"].as_u64().unwrap() as usize;
        assert!(audio.len() >= bytes, "the audio ends at byte {}, the catalog's at {bytes}", audio.len());
        assert_eq!(hex(&audio[..bytes]), want["audio"], "the audio of the first {bytes} bytes");
    }

    /// Needs reference/firmwares/felucca.fwsc (tools/fetch_firmwares.mjs).
    #[test]
    fn a_panic_under_the_interpreter_is_the_runs_halt() {
        let pkg = match std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../reference/firmwares/felucca.fwsc")) {
            Ok(p) => p,
            Err(_) => return eprintln!("no reference/firmwares/felucca.fwsc: skipped"),
        };
        let mut m = boot(&pkg, &[], 240, false).unwrap();
        // the interpreter alone (a panic under compiled code cannot be caught), and a flash chip cut
        // short: its model indexes past the end at the firmware's first read up there
        m.cpu.jit_on = false;
        m.soc.flash.mem.truncate(0x8000);
        let why = (0..100).find_map(|_| m.run(100_000).err()).expect("it ran on");
        assert!(m.halted() == Some(&why[..]) && why.starts_with("the emulator failed (index out of bounds"), "{why}");
    }
}
