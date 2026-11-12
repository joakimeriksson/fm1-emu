//! A MIDI controller as the panel (crates/fm1-map), the WAV recorder, and the labels overlay.
//!
//! The MIDI inputs' callbacks feed one [`Mapper`]; what it asks for goes to the machine as
//! [`Action`]s, and what the window shows (keys held, knobs turned, the recorder, learning) to a
//! [`View`] the window takes each frame.

use crate::panel::{self, ENCS};
use fm1_map::{KeysMode, Mapper, Out, Profile, Target};
use fm1_soc::Action;
use std::sync::{mpsc, Mutex};

/// What happened that the window shows or acts on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ui {
    Record,
    Labels,
    KeysMode(KeysMode),
    Learned,
}

pub struct View {
    /// keys and buttons the controller holds
    pub down: [bool; 41],
    /// encoder clicks since the window last took them, by panel::ENCS index
    pub enc: [i32; 7],
    pub pot: Option<u16>,
    pub events: Vec<Ui>,
}

impl Default for View {
    fn default() -> Self {
        View { down: [false; 41], enc: [0; 7], pot: None, events: Vec::new() }
    }
}

pub struct Ctl {
    pub mapper: Mutex<Mapper>,
    pub view: Mutex<View>,
    /// the MIDI inputs in use, for the title
    pub inputs: Mutex<Vec<String>>,
    /// false: --map none, MIDI goes to the jack as it is
    pub on: bool,
    /// --midi-ch: what passes to the jack moved to this channel (1..16; 0: as sent)
    pub midi_ch: u8,
    /// --midi-log: print each message and what it became
    pub log: bool,
}

impl Ctl {
    pub fn new(profile: Option<Profile>, midi_ch: u8) -> Ctl {
        let on = profile.is_some();
        Ctl {
            midi_ch,
            log: false,
            mapper: Mutex::new(Mapper::new(profile.unwrap_or_default())),
            view: Mutex::new(View::default()),
            inputs: Mutex::new(Vec::new()),
            on,
        }
    }

    /// One message from a MIDI input.
    pub fn feed(&self, msg: &[u8], tx: &mpsc::Sender<Action>) {
        let mut m = self.mapper.lock().unwrap();
        let mut v = self.view.lock().unwrap();
        let ch = self.midi_ch;
        if !self.log {
            m.feed(msg, &mut |o| apply(o, tx, &mut v, ch));
            return;
        }
        let mut outs = Vec::new();
        m.feed(msg, &mut |o| {
            outs.push(describe(&o));
            apply(o, tx, &mut v, ch)
        });
        if msg.first() != Some(&0xf8) && msg.first() != Some(&0xfe) {
            let bytes: Vec<String> = msg.iter().map(|b| format!("{b:02x}")).collect();
            let what = if outs.is_empty() { "(dropped)".to_string() } else { outs.join(", ") };
            eprintln!("midi in {:<9} {:<22} -> {what}", bytes.join(" "), name_of(msg));
        }
    }

    /// A target's tag for the overlay.
    pub fn tag(&self, t: Target) -> Option<String> {
        self.mapper.lock().unwrap().profile().tag_of(t)
    }

    /// The labels overlay, with the profile as it is now.
    pub fn draw_labels(&self, buf: &mut [u32]) {
        let m = self.mapper.lock().unwrap();
        crate::labels::draw(buf, m.profile(), m.learning());
    }
}

/// What the mapper asked for, to the machine and the view.
pub fn apply(o: Out, tx: &mpsc::Sender<Action>, v: &mut View, midi_ch: u8) {
    match o {
        Out::Key { id, down, vel } => {
            if down && id >= fm1_map::KEY0 {
                let _ = tx.send(Action::KeyVel { vel: vel.clamp(1, 127) });
            }
            let _ = tx.send(Action::Key { id, down });
            if let Some(d) = v.down.get_mut(id as usize) {
                *d = down;
            }
        }
        Out::Enc { enc, clicks } => {
            let _ = tx.send(Action::Enc { enc, clicks });
            if let Some(i) = ENCS.iter().position(|e| e.1 == enc) {
                v.enc[i] += clicks;
            }
        }
        Out::Adc { ch, value } => {
            let _ = tx.send(Action::Adc { ch, value });
            if ch == fm1_map::POT_ADC {
                v.pot = Some(value);
            }
        }
        Out::Midi { mut bytes, len } => {
            if midi_ch != 0 && (0x80..0xf0).contains(&bytes[0]) {
                bytes[0] = (bytes[0] & 0xf0) | (midi_ch - 1);
            }
            for &b in &bytes[..len as usize] {
                let _ = tx.send(Action::Midi { b });
            }
        }
        Out::Record => v.events.push(Ui::Record),
        Out::Labels => v.events.push(Ui::Labels),
        Out::KeysMode(k) => v.events.push(Ui::KeysMode(k)),
        Out::Learned => v.events.push(Ui::Learned),
    }
}

/// The target of a panel control (a key: the key zone, learned so the note pressed plays it).
pub fn target_of(c: panel::Ctl) -> Target {
    match c {
        panel::Ctl::Enc(i) => Target::Enc(ENCS[i].1),
        panel::Ctl::Pot => Target::Master,
        panel::Ctl::Btn(id) => Target::Button(id),
        panel::Ctl::Key(id) => Target::Key(id - fm1_map::KEY0),
    }
}

/// The profile `--map` asks for: "auto" (by the MIDI inputs' names), "none", a profile's name,
/// or a JSON file.
pub fn choose(want: &str, inputs: &[String]) -> Result<Option<Profile>, String> {
    if want == "none" {
        return Ok(None);
    }
    let (all, errs) = fm1_map::all_profiles();
    for e in errs {
        eprintln!("controller profile: {e}");
    }
    if want.ends_with(".json") {
        let s = std::fs::read_to_string(want).map_err(|e| format!("{want}: {e}"))?;
        return Profile::from_json(&s).map(Some).map_err(|e| format!("{want}: {e}"));
    }
    if want != "auto" {
        return all.iter().find(|p| p.name.eq_ignore_ascii_case(want)).cloned().map(Some)
            .ok_or_else(|| format!("no controller profile {want:?} (have: {})", all.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")));
    }
    // the first input with a profile of its own, else the generic one
    for i in inputs {
        if let Some(p) = all.iter().find(|p| !p.device.is_empty() && i.to_lowercase().contains(&p.device.to_lowercase())) {
            return Ok(Some(p.clone()));
        }
    }
    Ok(fm1_map::pick(&all, ""))
}

/// A message, for people: "note on ch1 C3 (48) vel 90".
fn name_of(msg: &[u8]) -> String {
    const N: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let note = |n: u8| format!("{}{} ({n})", N[n as usize % 12], n as i32 / 12 - 1);
    let (Some(&st), d1, d2) = (msg.first(), msg.get(1).copied().unwrap_or(0), msg.get(2).copied().unwrap_or(0)) else {
        return String::new();
    };
    let ch = (st & 0x0f) + 1;
    match st & 0xf0 {
        0x90 if d2 > 0 => format!("on  ch{ch} {} v{d2}", note(d1)),
        0x80 | 0x90 => format!("off ch{ch} {}", note(d1)),
        0xb0 => format!("cc  ch{ch} {d1} = {d2}"),
        0xe0 => format!("bend ch{ch} {}", ((d2 as i32) << 7 | d1 as i32) - 8192),
        0xc0 => format!("prog ch{ch} {d1}"),
        0xd0 => format!("press ch{ch} {d1}"),
        _ => String::new(),
    }
}

/// What the mapper did, for the log: "panel key C4 down", "SELECT +1", "MIDI IN 9f 30 5a".
fn describe(o: &Out) -> String {
    const N: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    match *o {
        Out::Key { id, down, .. } if id >= fm1_map::KEY0 => {
            let n = 53 + (id - fm1_map::KEY0) as usize; // the panel's first key is F3
            format!("panel key {}{} {}", N[n % 12], n / 12 - 1, if down { "down" } else { "up" })
        }
        Out::Key { id, down, .. } => format!("button {} {}", Target::Button(id), if down { "down" } else { "up" }),
        Out::Enc { enc, clicks } => format!("{} {clicks:+}", Target::Enc(enc)),
        Out::Adc { value, .. } => format!("MASTER {value}"),
        Out::Midi { bytes, len } => {
            let b: Vec<String> = bytes[..len as usize].iter().map(|b| format!("{b:02x}")).collect();
            format!("MIDI IN {} ({})", b.join(" "), name_of(&bytes[..len as usize]))
        }
        Out::Record => "record".into(),
        Out::Labels => "labels".into(),
        Out::KeysMode(k) => format!("keys: {}", k.name()),
        Out::Learned => "learned".into(),
    }
}
