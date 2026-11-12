//! Any MIDI controller as the FM-1's front panel.
//!
//! The emulator's MIDI IN jack takes notes as the firmware's MIDI driver reads them, but most of
//! the FM-1's workflow is on its panel: hold a function button and the keys and knobs change
//! roles. A [`Profile`] binds a controller's notes, CCs and pads to the panel's controls (its 27
//! keys, 14 buttons, 7 encoders and the MASTER pot), or passes them through to the MIDI IN jack.
//! It maps the hardware, not a firmware, so one profile works for every firmware.
//!
//! [`Mapper`] turns MIDI messages into [`Out`]s the front ends (fm1-live, the plugin) apply to
//! the machine. It allocates nothing per message, so it can run on any thread.
//!
//! Profiles are JSON (see `profiles/*.json` and [`builtin`]); a user's own live in
//! [`user_dir`], one file each, and override a built-in of the same name.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// The panel's buttons, by their matrix key id (0..13).
pub const BUTTONS: [&str; 14] = [
    "OCT-", "OCT+", "FX", "SCL", "ENV", "LFO", "EDIT", "GLO", "HOME", "SAVE", "ARP", "SEQ", "PLAY", "REC",
];
/// The panel's encoders: (name, matrix encoder id).
pub const ENCODERS: [(&str, u8); 7] = [
    ("SELECT", 0),
    ("ALGORITHM", 1),
    ("KNOB1", 2),
    ("KNOB2", 3),
    ("KNOB3", 4),
    ("KNOB4", 5),
    ("PRESETS", 6),
];
/// The first note key's matrix id; the keys are F3..G5, 27 of them, a semitone apart.
pub const KEY0: u8 = 14;
pub const NKEYS: u8 = 27;
/// F3 as a MIDI note (C4 = 60): the panel's first key sounds this, before OCT- / OCT+.
pub const F3: u8 = 53;
/// The MASTER pot's ADC channel.
pub const POT_ADC: u8 = 4;
const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// The name of panel key `n` (0..26): F3, F#3, ..., G5.
pub fn key_name(n: u8) -> String {
    let midi = F3 + n;
    format!("{}{}", NAMES[(midi % 12) as usize], midi as i32 / 12 - 1)
}

/// A panel control, or something else a controller's control can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// A button: its matrix id (0..13).
    Button(u8),
    /// One note key: 0..26 from F3.
    Key(u8),
    /// A range of notes as the panel's keys (the binding's `base` and `fold`).
    Keys,
    /// An encoder: its matrix id.
    Enc(u8),
    /// The MASTER pot.
    Master,
    /// To the MIDI IN jack as it is.
    Midi,
    /// Dropped.
    Ignore,
    /// Cycle the key zones: panel keys, MIDI IN, auto (panel keys while a function button is held).
    KeysMode,
    /// The front end's recorder: start / stop writing a WAV.
    Record,
    /// The front end's overlay of what each panel control is bound to.
    Labels,
}

impl Target {
    /// Every target a user can bind, in the order an editor lists them.
    pub fn all() -> Vec<Target> {
        let mut v: Vec<Target> = ENCODERS.iter().map(|e| Target::Enc(e.1)).collect();
        v.push(Target::Master);
        v.extend((0..14).map(Target::Button));
        v.push(Target::Keys);
        v.extend((0..NKEYS).map(Target::Key));
        v.extend([Target::KeysMode, Target::Record, Target::Labels, Target::Midi, Target::Ignore]);
        v
    }
    fn is_function(&self) -> bool {
        // the buttons held for a layer (not OCT, PLAY, REC)
        matches!(self, Target::Button(2..=11))
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Target::Button(i) => f.write_str(BUTTONS.get(i as usize).copied().unwrap_or("?")),
            Target::Key(n) => write!(f, "key:{}", key_name(n)),
            Target::Keys => f.write_str("keys"),
            Target::Enc(e) => f.write_str(ENCODERS.iter().find(|x| x.1 == e).map_or("?", |x| x.0)),
            Target::Master => f.write_str("MASTER"),
            Target::Midi => f.write_str("midi"),
            Target::Ignore => f.write_str("ignore"),
            Target::KeysMode => f.write_str("keys-mode"),
            Target::Record => f.write_str("record"),
            Target::Labels => f.write_str("labels"),
        }
    }
}

impl FromStr for Target {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let u = s.trim().to_ascii_uppercase();
        if let Some(i) = BUTTONS.iter().position(|b| *b == u) {
            return Ok(Target::Button(i as u8));
        }
        if let Some(e) = ENCODERS.iter().find(|e| e.0 == u) {
            return Ok(Target::Enc(e.1));
        }
        if let Some(k) = u.strip_prefix("KEY:") {
            return (0..NKEYS).find(|&n| key_name(n).to_ascii_uppercase() == k).map(Target::Key).ok_or(format!("no panel key {k}"));
        }
        Ok(match u.as_str() {
            "MASTER" => Target::Master,
            "KEYS" => Target::Keys,
            "MIDI" => Target::Midi,
            "IGNORE" => Target::Ignore,
            "KEYS-MODE" => Target::KeysMode,
            "RECORD" => Target::Record,
            "LABELS" => Target::Labels,
            _ => return Err(format!("unknown target {s:?}")),
        })
    }
}

impl Serialize for Target {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for Target {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

/// What a binding listens to. `ch` is 1..16; none: any channel.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Src {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ch: Option<u8>,
    /// One note (a pad, a key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<u8>,
    /// Notes lo..=hi.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<[u8; 2]>,
    /// A control change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cc: Option<u8>,
    /// The pitch bend.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bend: bool,
}

impl Src {
    fn ch_ok(&self, ch: u8) -> bool {
        self.ch.map_or(true, |c| c == ch + 1)
    }
    /// Short, for a label: "N41/10", "CC70", "N48-72", "bend".
    pub fn short(&self) -> String {
        let ch = self.ch.map_or(String::new(), |c| format!("/{c}"));
        if let Some(n) = self.note {
            format!("N{n}{ch}")
        } else if let Some([lo, hi]) = self.notes {
            format!("N{lo}-{hi}{ch}")
        } else if let Some(c) = self.cc {
            format!("CC{c}{ch}")
        } else if self.bend {
            format!("bend{ch}")
        } else {
            format!("any{ch}")
        }
    }
}

/// How a CC (or a note) drives its target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// An encoder: the default for its source (absolute for a CC, a press for a note).
    #[default]
    Auto,
    /// A CC's value is a position (0..127): an encoder turns by the change.
    Absolute,
    /// Two's complement: 1..63 clockwise, 127..65 counter-clockwise (65 = -63).
    Relative,
    /// Around 64: 65 is +1, 63 is -1.
    #[serde(rename = "relative-64")]
    Relative64,
    /// Bit 6 the sign: 1..63 clockwise, 65..127 counter-clockwise (65 = -1).
    #[serde(rename = "relative-signed")]
    RelativeSigned,
    /// A button held while the pad (or a CC at 64 or more) is.
    Momentary,
    /// A button that latches: each press flips it.
    Toggle,
    /// A note (or a CC at 64 or more) turns an encoder one way: `scale` clicks a press.
    Step,
}

fn one() -> f32 {
    1.0
}
fn is_one(v: &f32) -> bool {
    *v == 1.0
}
fn is_zero(v: &u8) -> bool {
    *v == 0
}

fn yes() -> bool {
    true
}
fn is_true(v: &bool) -> bool {
    *v
}
fn f3() -> u8 {
    F3
}
fn is_f3(v: &u8) -> bool {
    *v == F3
}

/// One control of the controller bound to one target.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub src: Src,
    pub to: Target,
    #[serde(default, skip_serializing_if = "is_auto")]
    pub mode: Mode,
    /// Encoder clicks per CC step (absolute, relative), per press (step; negative turns back).
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub scale: f32,
    /// `keys`: the MIDI note played as the panel's F3 key.
    #[serde(default = "f3", skip_serializing_if = "is_f3")]
    pub base: u8,
    /// `keys`: notes beyond the panel's 27 keys move by octaves onto them (else they are dropped).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub fold: bool,
    /// A name the overlay shows instead of the source ("Pad 3", "K1").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

fn is_auto(m: &Mode) -> bool {
    *m == Mode::Auto
}

impl Binding {
    pub fn new(src: Src, to: Target) -> Binding {
        Binding { src, to, mode: Mode::Auto, scale: 1.0, base: F3, fold: true, label: None }
    }
    /// What the overlay shows for it.
    pub fn tag(&self) -> String {
        self.label.clone().unwrap_or_else(|| self.src.short())
    }
}

/// Where the key zones go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeysMode {
    /// The panel's keys, as if pressed: every firmware, every layer.
    #[default]
    Panel,
    /// The MIDI IN jack: the controller's own pitch, velocity and range.
    Midi,
    /// MIDI IN, but the panel's keys while a function button (FX..SEQ) is held.
    Auto,
}

impl KeysMode {
    pub fn next(self) -> KeysMode {
        match self {
            KeysMode::Panel => KeysMode::Midi,
            KeysMode::Midi => KeysMode::Auto,
            KeysMode::Auto => KeysMode::Panel,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            KeysMode::Panel => "panel keys",
            KeysMode::Midi => "MIDI IN",
            KeysMode::Auto => "auto",
        }
    }
}

/// A controller's bindings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    /// Part of the MIDI input's name this profile is for (case does not matter): picked by
    /// [`pick`] when that input is used.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device: String,
    /// Messages no binding takes go to the MIDI IN jack (else they are dropped).
    #[serde(default = "yes")]
    pub pass_unmapped: bool,
    #[serde(default)]
    pub keys_mode: KeysMode,
    /// The channel (1-16) what goes to MIDI IN is sent on, whatever channel the controller
    /// plays; 0: as played. Firmwares such as SLOOP play the selected track on a channel no
    /// track has (4-16), where channel 1 always plays track 1.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub midi_ch: u8,
    #[serde(default)]
    pub bindings: Vec<Binding>,
    /// Notes for people: how to set the controller up for it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

impl Profile {
    pub fn from_json(s: &str) -> Result<Profile, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
    /// The tag the overlay shows on target `t`, if something is bound to it.
    pub fn tag_of(&self, t: Target) -> Option<String> {
        if let Some(b) = self.bindings.iter().find(|b| b.to == t) {
            return Some(b.tag());
        }
        // a key inside a key zone: the note that plays it
        if let Target::Key(n) = t {
            for b in self.bindings.iter().filter(|b| b.to == Target::Keys) {
                let (lo, hi) = b.src.notes.map_or((0, 127), |[lo, hi]| (lo, hi));
                let want = b.base as i32 + n as i32;
                if (lo as i32..=hi as i32).contains(&want) {
                    return Some(format!("{want}"));
                }
            }
        }
        None
    }
    /// The file name it is saved as in [`user_dir`].
    pub fn file_name(&self) -> String {
        let s: String = self.name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
        let s = s.trim_matches('-').to_string();
        format!("{}.json", if s.is_empty() { "controller" } else { &s })
    }
}

/// The profiles that come with the emulator.
pub fn builtin() -> Vec<Profile> {
    [include_str!("../profiles/akai-mpk-mini-3.json"), include_str!("../profiles/generic.json")]
        .iter()
        .filter_map(|s| Profile::from_json(s).ok())
        .collect()
}

/// Where a user's profiles live: %APPDATA%\fm1-emu\controllers, ~/Library/Application
/// Support/fm1-emu/controllers, $XDG_CONFIG_HOME (or ~/.config)/fm1-emu/controllers.
pub fn user_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = if cfg!(windows) {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support"))
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))
    };
    base.map(|b| b.join("fm1-emu").join("controllers"))
}

/// The profiles in `dir` (unreadable files are skipped, with their error).
pub fn load_dir(dir: &Path) -> (Vec<Profile>, Vec<String>) {
    let (mut v, mut errs) = (Vec::new(), Vec::new());
    let Ok(rd) = std::fs::read_dir(dir) else { return (v, errs) };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect();
    paths.sort();
    for p in paths {
        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| Profile::from_json(&s)) {
            Ok(pr) => v.push(pr),
            Err(e) => errs.push(format!("{}: {e}", p.display())),
        }
    }
    (v, errs)
}

/// The built-ins and the user's, a user's profile replacing a built-in of the same name.
pub fn all_profiles() -> (Vec<Profile>, Vec<String>) {
    let mut v = builtin();
    let (mine, errs) = user_dir().map_or((Vec::new(), Vec::new()), |d| load_dir(&d));
    for p in mine {
        match v.iter_mut().find(|q| q.name == p.name) {
            Some(q) => *q = p,
            None => v.push(p),
        }
    }
    (v, errs)
}

/// Write `p` to [`user_dir`]; the path written.
pub fn save_user(p: &Profile) -> Result<PathBuf, String> {
    let dir = user_dir().ok_or("no folder for settings (APPDATA / HOME not set)")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(p.file_name());
    std::fs::write(&path, p.to_json()).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// The profile for a MIDI input named `device`: the first whose `device` it contains, else the
/// one named "Generic", else the first.
pub fn pick(profiles: &[Profile], device: &str) -> Option<Profile> {
    let d = device.to_lowercase();
    profiles
        .iter()
        .find(|p| !p.device.is_empty() && d.contains(&p.device.to_lowercase()))
        .or_else(|| profiles.iter().find(|p| p.name.to_lowercase().starts_with("generic")))
        .or_else(|| profiles.first())
        .cloned()
}

/// What the mapper asks of the machine or the front end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Out {
    /// A panel key or button (`vel`: a key's velocity, 1..127, before a press).
    Key { id: u8, down: bool, vel: u8 },
    /// Turn an encoder.
    Enc { enc: u8, clicks: i32 },
    /// Set an ADC channel (0..1023): the MASTER pot.
    Adc { ch: u8, value: u16 },
    /// A message to the MIDI IN jack.
    Midi { bytes: [u8; 3], len: u8 },
    /// The front end's own: start / stop recording.
    Record,
    /// The front end's own: show / hide the labels.
    Labels,
    /// The key zones now go here.
    KeysMode(KeysMode),
    /// A binding was learned: the profile changed (save it if you keep profiles).
    Learned,
}

/// The length of a channel message from its status byte (0: not one).
pub fn msg_len(status: u8) -> u8 {
    match status & 0xf0 {
        0x80 | 0x90 | 0xa0 | 0xb0 | 0xe0 => 3,
        0xc0 | 0xd0 => 2,
        _ => 0,
    }
}

const NONE: u8 = 0xff;
/// A held note that went to the MIDI IN jack (its release goes there too).
const TO_MIDI: u8 = 0xfe;

/// A profile at work: the state its bindings need between messages.
pub struct Mapper {
    profile: Profile,
    keys_mode: KeysMode,
    /// The last absolute value of each channel's CCs (NONE: not seen).
    last_cc: Box<[[u8; 128]; 16]>,
    /// The panel key each held note pressed (NONE: none), so its release lets go of that key
    /// whatever changed since.
    held_note: Box<[[u8; 128]; 16]>,
    /// How many held notes are on each panel key (two notes fold onto one key).
    key_count: [u8; NKEYS as usize],
    /// The fraction of a click each binding has not turned yet.
    frac: Vec<f32>,
    /// Latched buttons, held buttons.
    latched: [bool; 14],
    held_btn: [u8; 14],
    pot: u16,
    learn: Option<Target>,
}

impl Mapper {
    pub fn new(profile: Profile) -> Mapper {
        let mut m = Mapper {
            keys_mode: profile.keys_mode,
            frac: vec![0.0; profile.bindings.len()],
            profile,
            last_cc: Box::new([[NONE; 128]; 16]),
            held_note: Box::new([[NONE; 128]; 16]),
            key_count: [0; NKEYS as usize],
            latched: [false; 14],
            held_btn: [0; 14],
            pot: 900,
            learn: None,
        };
        m.frac.resize(m.profile.bindings.len(), 0.0);
        m
    }
    pub fn profile(&self) -> &Profile {
        &self.profile
    }
    pub fn keys_mode(&self) -> KeysMode {
        self.keys_mode
    }
    /// Use another profile; held keys and buttons are let go of through `out`.
    pub fn set_profile(&mut self, p: Profile, out: &mut dyn FnMut(Out)) {
        self.release_all(out);
        self.keys_mode = p.keys_mode;
        self.frac = vec![0.0; p.bindings.len()];
        self.profile = p;
        self.last_cc.iter_mut().for_each(|r| *r = [NONE; 128]);
    }
    /// The next note or CC binds to `t` (none: stop learning). A panel key, `Target::Key(n)`,
    /// learns the key zone so that the note pressed plays key n.
    pub fn learn(&mut self, t: Option<Target>) {
        self.learn = t;
    }
    pub fn learning(&self) -> Option<Target> {
        self.learn
    }
    /// Remove what is bound to `t`.
    pub fn unbind(&mut self, t: Target) {
        let keep: Vec<bool> = self.profile.bindings.iter().map(|b| b.to != t).collect();
        let mut i = 0;
        self.profile.bindings.retain(|_| { i += 1; keep[i - 1] });
        let mut i = 0;
        self.frac.retain(|_| { i += 1; keep[i - 1] });
    }
    pub fn set_keys_mode(&mut self, k: KeysMode, out: &mut dyn FnMut(Out)) {
        if k != self.keys_mode {
            self.release_keys(out);
            self.keys_mode = k;
            self.profile.keys_mode = k;
        }
        out(Out::KeysMode(k));
    }

    /// Let go of every key and button the controller holds.
    pub fn release_all(&mut self, out: &mut dyn FnMut(Out)) {
        self.release_keys(out);
        for i in 0..14 {
            if self.held_btn[i] > 0 || self.latched[i] {
                self.held_btn[i] = 0;
                self.latched[i] = false;
                out(Out::Key { id: i as u8, down: false, vel: 0 });
            }
        }
    }
    /// A channel message's status byte on the profile's MIDI IN channel.
    fn to_midi_ch(&self, st: u8) -> u8 {
        match self.profile.midi_ch {
            c @ 1..=16 if (0x80..0xf0).contains(&st) => (st & 0xf0) | (c - 1),
            _ => st,
        }
    }

    fn release_keys(&mut self, out: &mut dyn FnMut(Out)) {
        let off = self.to_midi_ch(0x80);
        for (ch, row) in self.held_note.iter_mut().enumerate() {
            for (note, k) in row.iter_mut().enumerate() {
                if *k == TO_MIDI {
                    let st = if off == 0x80 { 0x80 | ch as u8 } else { off };
                    out(Out::Midi { bytes: [st, note as u8, 0], len: 3 });
                }
                *k = NONE;
            }
        }
        for (n, c) in self.key_count.iter_mut().enumerate() {
            if *c > 0 {
                *c = 0;
                out(Out::Key { id: KEY0 + n as u8, down: false, vel: 0 });
            }
        }
    }

    fn function_held(&self) -> bool {
        (0..14u8).any(|i| Target::Button(i).is_function() && (self.held_btn[i as usize] > 0 || self.latched[i as usize]))
    }

    /// One MIDI message (status byte first; running status is not taken).
    pub fn feed(&mut self, msg: &[u8], out: &mut dyn FnMut(Out)) {
        let Some(&st) = msg.first() else { return };
        let len = msg_len(st) as usize;
        if len == 0 || msg.len() < len {
            // system messages (clock, start, stop) go to the firmware as they are
            if (0xf8..=0xfc).contains(&st) {
                out(Out::Midi { bytes: [st, 0, 0], len: 1 });
            }
            return;
        }
        let (kind, ch) = (st & 0xf0, st & 0x0f);
        let d1 = msg[1] & 0x7f;
        let d2 = if len == 3 { msg[2] & 0x7f } else { 0 };
        let bytes = [self.to_midi_ch(st), d1, d2];
        let pass = Out::Midi { bytes, len: len as u8 };
        let note_on = kind == 0x90 && d2 > 0;
        let note_off = kind == 0x80 || (kind == 0x90 && d2 == 0);

        if let Some(t) = self.learn {
            if note_on || (kind == 0xb0) || kind == 0xe0 {
                if kind == 0xb0 && self.profile.bindings.iter().any(|b| b.src.cc == Some(d1) && b.to == t) {
                    return; // the rest of a knob's turn
                }
                self.learn_from(t, kind, ch, d1);
                out(Out::Learned);
            }
            return;
        }

        // a note's release goes where its press went
        if note_off {
            let k = self.held_note[ch as usize][d1 as usize];
            if k == TO_MIDI {
                self.held_note[ch as usize][d1 as usize] = NONE;
                out(pass);
                return;
            }
            if k != NONE {
                self.held_note[ch as usize][d1 as usize] = NONE;
                let c = &mut self.key_count[k as usize];
                *c = c.saturating_sub(1);
                if *c == 0 {
                    out(Out::Key { id: KEY0 + k, down: false, vel: 0 });
                }
                return;
            }
        }

        for i in 0..self.profile.bindings.len() {
            let b = &self.profile.bindings[i];
            if !b.src.ch_ok(ch) {
                continue;
            }
            let hit = match kind {
                0x80 | 0x90 => b.src.note == Some(d1) || b.src.notes.is_some_and(|[lo, hi]| (lo..=hi).contains(&d1)),
                0xb0 => b.src.cc == Some(d1),
                0xe0 => b.src.bend,
                _ => false,
            };
            if !hit {
                continue;
            }
            let (to, mode, scale, base, fold) = (b.to, b.mode, b.scale, b.base, b.fold);
            // a CC's value; a note's press as 127, its release as 0
            let v = if kind == 0xb0 { d2 } else if note_on { 127 } else { 0 };
            let is_note = kind == 0x80 || kind == 0x90;
            match to {
                Target::Midi => out(pass),
                Target::Ignore => {}
                Target::Keys | Target::Key(_) if is_note => {
                    let to_midi = match self.keys_mode {
                        KeysMode::Panel => false,
                        KeysMode::Midi => to == Target::Keys,
                        KeysMode::Auto => to == Target::Keys && !self.function_held(),
                    };
                    if !note_on || self.held_note[ch as usize][d1 as usize] != NONE {
                        return; // (a release with nothing held, a second press without a release)
                    }
                    if to_midi {
                        self.held_note[ch as usize][d1 as usize] = TO_MIDI;
                        out(pass);
                        return;
                    }
                    let n = match to {
                        Target::Key(n) => Some(n as i32),
                        _ => {
                            let mut n = d1 as i32 - base as i32;
                            if fold {
                                while n < 0 { n += 12; }
                                while n >= NKEYS as i32 { n -= 12; }
                            }
                            (0..NKEYS as i32).contains(&n).then_some(n)
                        }
                    };
                    if let Some(n) = n {
                        let n = n as u8;
                        self.held_note[ch as usize][d1 as usize] = n;
                        self.key_count[n as usize] += 1;
                        if self.key_count[n as usize] == 1 {
                            out(Out::Key { id: KEY0 + n, down: true, vel: d2.max(1) });
                        }
                    }
                }
                Target::Button(id) => {
                    let id = id.min(13);
                    let down = v >= 64;
                    if mode == Mode::Toggle {
                        if down {
                            self.latched[id as usize] = !self.latched[id as usize];
                            out(Out::Key { id, down: self.latched[id as usize], vel: 0 });
                        }
                    } else {
                        let h = &mut self.held_btn[id as usize];
                        // (two sources may hold one button)
                        if down {
                            *h += 1;
                            if *h == 1 { out(Out::Key { id, down: true, vel: 0 }); }
                        } else if *h > 0 {
                            *h -= 1;
                            if *h == 0 { out(Out::Key { id, down: false, vel: 0 }); }
                        }
                    }
                }
                Target::Enc(enc) => {
                    let steps = self.steps(i, kind, ch, d1, v, mode, is_note);
                    let f = &mut self.frac[i];
                    *f += steps * scale;
                    let clicks = f.trunc();
                    *f -= clicks;
                    if clicks != 0.0 {
                        out(Out::Enc { enc, clicks: clicks as i32 });
                    }
                }
                Target::Master => {
                    let value = match mode {
                        Mode::Auto | Mode::Absolute if kind == 0xb0 => (v as u32 * 1023 / 127) as u16,
                        _ => {
                            let s = self.steps(i, kind, ch, d1, v, mode, is_note);
                            (self.pot as f32 + s * scale * 8.0).round().clamp(0.0, 1023.0) as u16
                        }
                    };
                    self.pot = value;
                    out(Out::Adc { ch: POT_ADC, value });
                }
                Target::KeysMode => {
                    if v >= 64 {
                        let k = self.keys_mode.next();
                        self.set_keys_mode(k, out);
                    }
                }
                Target::Record => if v >= 64 { out(Out::Record) },
                Target::Labels => if v >= 64 { out(Out::Labels) },
                Target::Keys | Target::Key(_) => {} // (a CC bound to keys: nothing)
            }
            return;
        }
        if self.profile.pass_unmapped {
            out(pass);
        }
    }

    /// The steps (+ clockwise) a message turns binding `i` by.
    fn steps(&mut self, _i: usize, kind: u8, ch: u8, d1: u8, v: u8, mode: Mode, is_note: bool) -> f32 {
        if is_note || mode == Mode::Step {
            return if v >= 64 { 1.0 } else { 0.0 };
        }
        if kind != 0xb0 {
            return 0.0;
        }
        match mode {
            Mode::Relative => match v {
                1..=63 => v as f32,
                65..=127 => v as f32 - 128.0,
                _ => 0.0,
            },
            Mode::Relative64 => v as f32 - 64.0,
            Mode::RelativeSigned => match v {
                1..=63 => v as f32,
                65..=127 => -(v as f32 - 64.0),
                _ => 0.0,
            },
            _ => {
                // absolute: the change since the last value; the first one only sets it
                let last = &mut self.last_cc[ch as usize][d1 as usize];
                let s = if *last == NONE { 0.0 } else { v as f32 - *last as f32 };
                *last = v;
                s
            }
        }
    }

    fn learn_from(&mut self, t: Target, kind: u8, ch: u8, d1: u8) {
        // learning one panel key learns the key zone: the note pressed plays that key
        let (t, at) = match t {
            Target::Key(n) => (Target::Keys, n),
            t => (t, 0),
        };
        let mut src = Src { ch: Some(ch + 1), ..Src::default() };
        let mut mode = Mode::Auto;
        match kind {
            0x90 if t == Target::Keys => src.notes = Some([0, 127]),
            0x90 => src.note = Some(d1),
            0xb0 => {
                src.cc = Some(d1);
                if matches!(t, Target::Button(_)) {
                    mode = Mode::Momentary;
                }
            }
            _ => src.bend = true,
        }
        // one binding per single target, and one target per source
        let single = !matches!(t, Target::Midi | Target::Ignore);
        let keep: Vec<bool> = self.profile.bindings.iter().map(|b| !(b.src == src || (single && b.to == t))).collect();
        let mut j = 0;
        self.profile.bindings.retain(|_| { j += 1; keep[j - 1] });
        let mut b = Binding::new(src, t);
        b.mode = mode;
        if t == Target::Keys {
            b.base = d1.saturating_sub(at); // the note pressed plays the key learned
        }
        // first, so it wins over a key zone that covers the same note
        self.profile.bindings.insert(0, b);
        self.frac = vec![0.0; self.profile.bindings.len()];
        self.learn = None;
    }
}

#[cfg(test)]
mod tests;
