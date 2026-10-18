//! The FM-1 emulator as an instrument plugin. One CLAP plugin written with nih-plug (its VST3
//! bindings left out: they are GPL); clap-wrapper makes the AU and the VST3 of it
//! (crates/fm1-plugin/wrap).
//!
//! The firmware is a `.fwsc` package the user loads in the plugin's window: its bytes are saved
//! with the host's project, and so is every flash sector the firmware writes (its patches), so a
//! project reopens with the same firmware and sounds. The host's MIDI goes to the MIDI IN jack
//! (channel messages optionally moved to one channel: M-VAVE's own firmware listens on 2), and
//! the machine's 44.1 kHz output is resampled to the host's rate.

use nih_plug::midi::MidiResult;
use nih_plug::prelude::*;
use nih_plug_egui::EguiState;
use serde::{Deserialize, Serialize};
use std::sync::{mpsc, Arc, Mutex};

mod editor;
pub mod engine;
pub mod savefile;
#[path = "../../fm1-live/src/panel.rs"]
#[allow(dead_code)]
mod panel;
#[path = "../../fm1-live/src/panel_smk.rs"]
#[allow(dead_code)]
mod panel_smk;

use engine::{Cmd, Midi, Shared, View, RATE};

/// The firmware package, saved with the project.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Firmware {
    pub name: String,
    #[serde(with = "b64")]
    pub pkg: Arc<Vec<u8>>,
}

/// A flash sector the firmware wrote, saved with the project.
#[derive(Clone, Serialize, Deserialize)]
pub struct Sector {
    pub at: u32,
    #[serde(with = "b64")]
    pub data: Arc<Vec<u8>>,
}

#[derive(Params)]
pub struct Fm1Params {
    #[persist = "editor-state"]
    editor_state: Arc<EguiState>,
    #[persist = "firmware"]
    pub firmware: Mutex<Firmware>,
    #[persist = "flash"]
    pub flash: Mutex<Vec<Sector>>,

    /// The MASTER pot.
    #[id = "master"]
    pub master: FloatParam,
    /// 0: channel messages as the host sends them; 1..16: moved to that channel.
    #[id = "channel"]
    pub channel: IntParam,
    /// The emulated CPU's clock. Changing it reboots the firmware (its flash is kept).
    #[id = "clock"]
    pub clock: EnumParam<Clock>,
}

/// The emulated clock. The emulator runs one instruction a cycle at the chip's nominal 240 MHz,
/// while the AC791N runs some instructions in dual-issue pairs, so a firmware that runs out of
/// time can be given more. A firmware that idles in loops the emulator cannot skip costs host
/// CPU in proportion to the clock, so a lower one lets a heavy firmware keep up on a slower
/// computer (on an M1 Pro, CS-1 runs at 1.2x real time at 240 MHz and 1.65x at 160; FM-106 at
/// 0.9x and 1.1x), as long as the firmware still has the time it needs.
#[derive(Enum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Clock {
    #[name = "120 MHz"]
    Mhz120,
    #[name = "160 MHz"]
    Mhz160,
    #[name = "240 MHz"]
    Mhz240,
    #[name = "320 MHz"]
    Mhz320,
    #[name = "400 MHz"]
    Mhz400,
}

impl Clock {
    pub fn mhz(self) -> u64 {
        match self {
            Clock::Mhz120 => 120,
            Clock::Mhz160 => 160,
            Clock::Mhz240 => 240,
            Clock::Mhz320 => 320,
            Clock::Mhz400 => 400,
        }
    }
}

impl Default for Fm1Params {
    fn default() -> Self {
        Self {
            editor_state: EguiState::from_size(960, 660),
            firmware: Mutex::new(Firmware::default()),
            flash: Mutex::new(Vec::new()),
            master: FloatParam::new("Master", 900.0 / 1023.0, FloatRange::Linear { min: 0.0, max: 1.0 })
                .with_unit(" %")
                .with_value_to_string(formatters::v2s_f32_percentage(0))
                .with_string_to_value(formatters::s2v_f32_percentage()),
            channel: IntParam::new("MIDI channel", 0, IntRange::Linear { min: 0, max: 16 })
                .with_value_to_string(Arc::new(|v| if v == 0 { "as sent".into() } else { v.to_string() })),
            clock: EnumParam::new("CPU clock", Clock::Mhz240).non_automatable(),
        }
    }
}

/// What the plugin's three threads share: the machine's thread runs the firmware, the editor
/// asks it for things, the audio thread takes its frames.
pub struct Link {
    pub shared: Arc<Shared>,
    pub cmds: mpsc::Sender<Cmd>,
    /// the package the machine runs (to boot only when the project's is another)
    pub running: Mutex<Option<Arc<Vec<u8>>>>,
}

impl Link {
    /// Boot `fw` unless the machine already runs it; `sectors` are the project's saved flash.
    pub fn boot(&self, fw: &Firmware, sectors: &[Sector]) {
        let mut running = self.running.lock().unwrap();
        if fw.pkg.is_empty() || running.as_ref().is_some_and(|r| Arc::ptr_eq(r, &fw.pkg) || **r == *fw.pkg) {
            return;
        }
        *running = Some(fw.pkg.clone());
        let sectors = sectors.iter().map(|s| (s.at, s.data.to_vec())).collect();
        let _ = self.cmds.send(Cmd::Boot { pkg: fw.pkg.clone(), sectors });
    }
}

pub struct Fm1 {
    params: Arc<Fm1Params>,
    link: Option<Arc<Link>>,
    thread: Option<std::thread::JoinHandle<()>>,
    midi: Option<rtrb::Producer<Midi>>,
    audio: Option<rtrb::Consumer<[f32; 2]>>,
    /// machine frames per host sample
    ratio: f64,
    /// how far ahead of the host the machine runs, in its frames
    lead: usize,
    /// frames taken from the ring, and the resampler between the last two
    taken: u64,
    frac: f64,
    cur: [f32; 2],
    next: [f32; 2],
}

impl Default for Fm1 {
    fn default() -> Self {
        Self {
            params: Arc::new(Fm1Params::default()),
            link: None,
            thread: None,
            midi: None,
            audio: None,
            ratio: 1.0,
            lead: 2048,
            taken: 0,
            frac: 0.0,
            cur: [0.0; 2],
            next: [0.0; 2],
        }
    }
}

impl Fm1 {
    /// The machine's thread, once per plugin instance (hosts make instances only to scan them).
    fn start(&mut self) -> Arc<Link> {
        if let Some(l) = &self.link {
            return l.clone();
        }
        let shared = Arc::new(Shared {
            view: Mutex::new(View {
                lcd: vec![0; 240 * 240],
                leds: [0.0; 41],
                pads: [0; 16],
                board: fm1_soc::Board::Fm1,
                booted: false,
                halted: false,
                speed: 0.0,
                starved: 0,
                late: 0,
            }),
            quit: Default::default(),
        });
        let (tx, rx) = mpsc::channel();
        let (midi_tx, midi_rx) = rtrb::RingBuffer::new(4096);
        let (audio_tx, audio_rx) = rtrb::RingBuffer::new(16384);
        let (sh, params) = (shared.clone(), self.params.clone());
        self.thread = Some(
            std::thread::Builder::new()
                .name("fm1-machine".into())
                .spawn(move || engine::run(sh, params, rx, midi_rx, audio_tx))
                .expect("the machine's thread"),
        );
        self.midi = Some(midi_tx);
        self.audio = Some(audio_rx);
        let link = Arc::new(Link { shared, cmds: tx, running: Mutex::new(None) });
        self.link = Some(link.clone());
        link
    }
}

impl Drop for Fm1 {
    fn drop(&mut self) {
        if let Some(l) = &self.link {
            l.shared.quit.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The length of a channel message from its status byte (0: not one).
fn midi_len(status: u8) -> u8 {
    match status & 0xf0 {
        0x80 | 0x90 | 0xa0 | 0xb0 | 0xe0 => 3,
        0xc0 | 0xd0 => 2,
        _ => 0,
    }
}

impl Plugin for Fm1 {
    const NAME: &'static str = "FM-1";
    const VENDOR: &'static str = "fm1-emu";
    const URL: &'static str = "https://github.com/joakimeriksson/fm1-emu";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: None,
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];
    const MIDI_INPUT: MidiConfig = MidiConfig::MidiCCs;
    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        let link = self.start();
        editor::create(self.params.clone(), link)
    }

    fn initialize(&mut self, _layout: &AudioIOLayout, cfg: &BufferConfig, ctx: &mut impl InitContext<Self>) -> bool {
        let link = self.start();
        self.ratio = RATE / cfg.sample_rate as f64;
        // a host block's frames, and room for a DMA half and the emulation's unevenness
        self.lead = (cfg.max_buffer_size as f64 * self.ratio).ceil() as usize + 1024;
        let _ = link.cmds.send(Cmd::Lead(self.lead));
        ctx.set_latency_samples((self.lead as f64 / self.ratio).round() as u32);
        let fw = self.params.firmware.lock().unwrap().clone();
        let sectors = self.params.flash.lock().unwrap().clone();
        link.boot(&fw, &sectors);
        true
    }

    fn process(&mut self, buffer: &mut Buffer, _aux: &mut AuxiliaryBuffers, ctx: &mut impl ProcessContext<Self>) -> ProcessStatus {
        let start = self.taken;
        let ch = self.params.channel.value() as u8;
        while let Some(ev) = ctx.next_event() {
            let at = ev.timing();
            let Some(MidiResult::Basic(mut b)) = ev.as_midi() else { continue };
            let len = midi_len(b[0]);
            if len == 0 {
                continue;
            }
            if ch != 0 {
                b[0] = (b[0] & 0xf0) | (ch - 1);
            }
            let frame = start + (at as f64 * self.ratio) as u64 + self.lead as u64;
            if let Some(m) = self.midi.as_mut() {
                let _ = m.push(Midi { frame, bytes: b, len });
            }
        }
        let Some(rx) = self.audio.as_mut() else { return ProcessStatus::Normal };
        let out = buffer.as_slice();
        let n = out[0].len();
        let mut starved = 0u64;
        for i in 0..n {
            self.frac += self.ratio;
            while self.frac >= 1.0 {
                match rx.pop() {
                    Ok(f) => {
                        self.cur = self.next;
                        self.next = f;
                        self.taken += 1;
                        self.frac -= 1.0;
                    }
                    Err(_) => {
                        // the machine is behind: hold, and take the frame next time
                        self.frac = self.frac.min(1.0);
                        starved += 1;
                        break;
                    }
                }
            }
            let t = self.frac.min(1.0) as f32;
            for c in 0..2 {
                out[c][i] = self.cur[c] + (self.next[c] - self.cur[c]) * t;
            }
        }
        if starved > 0 {
            if let Some(l) = &self.link {
                if let Ok(mut v) = l.shared.view.try_lock() {
                    v.starved += starved;
                }
            }
        }
        ProcessStatus::KeepAlive
    }
}

impl ClapPlugin for Fm1 {
    const CLAP_ID: &'static str = "io.github.joakimeriksson.fm1-emu";
    const CLAP_DESCRIPTION: Option<&'static str> = Some("The M-VAVE FM-1 emulated: its community firmwares as an instrument");
    const CLAP_MANUAL_URL: Option<&'static str> = Some(Self::URL);
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[ClapFeature::Instrument, ClapFeature::Synthesizer, ClapFeature::Stereo];
}

nih_export_clap!(Fm1);

/// Bytes as base64 in the saved state (a package is ~600 KB).
mod b64 {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer>(v: &Arc<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(v.as_slice()))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<Vec<u8>>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD.decode(s).map(Arc::new).map_err(serde::de::Error::custom)
    }
}
