//! A MIDI controller as the panel in the plugin (crates/fm1-map): the host's MIDI goes through
//! the chosen profile on the machine's thread, at the frame it is due, to the panel's keys,
//! buttons and encoders or to the MIDI IN jack. The window picks the profile, learns bindings,
//! shows them over the panel; a control bound to Record starts the recorder.

use fm1_map::{KeysMode, Profile, Target};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The project's controller: the profile chosen (with what was learned in it), or none.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Controller {
    /// false: never chosen in this project, so the last choice in any project is used
    #[serde(default)]
    pub chosen: bool,
    #[serde(default)]
    pub profile: Option<Profile>,
}

impl Controller {
    /// What a project saved before controller profiles has: none, as chosen.
    pub fn before_profiles() -> Controller {
        Controller { chosen: true, profile: None }
    }
    /// As nih-plug keeps a persisted field: JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Fm1;
    use nih_plug::prelude::{Plugin, PluginState};

    #[test]
    fn an_old_project_gets_no_profile_and_a_new_one_keeps_its_own() {
        // a state saved before profiles: no "controller"
        let mut old = PluginState { version: "0.4.0".into(), params: Default::default(), fields: Default::default() };
        old.fields.insert("firmware".into(), "{}".into());
        Fm1::filter_state(&mut old);
        let c: Controller = serde_json::from_str(&old.fields["controller"]).unwrap();
        assert!(c.chosen && c.profile.is_none());
        // a state saved with one keeps it
        let mine = Controller { chosen: true, profile: Some(Profile { name: "Mine".into(), ..Profile::default() }) };
        let mut new = PluginState { version: "0.5.0".into(), params: Default::default(), fields: Default::default() };
        new.fields.insert("controller".into(), mine.to_json());
        Fm1::filter_state(&mut new);
        let c: Controller = serde_json::from_str(&new.fields["controller"]).unwrap();
        assert_eq!(c.profile.map(|p| p.name).as_deref(), Some("Mine"));
    }
}

/// What the machine's thread tells the window about the controller.
pub struct MapView {
    pub profile: Option<Profile>,
    /// counts changes of the profile (learned, unbound, another chosen)
    pub rev: u64,
    pub keys_mode: KeysMode,
    pub learning: Option<Target>,
    /// keys and buttons the controller holds
    pub down: [bool; 41],
    /// encoder clicks the controller turned, all told, by panel::ENCS index
    pub enc: [i32; 7],
    /// the MASTER pot as the controller set it last
    pub pot: Option<u16>,
    /// counts the controller's "labels" presses
    pub labels: u32,
    /// a line for the status bar
    pub msg: Option<String>,
}

impl Default for MapView {
    fn default() -> Self {
        MapView {
            profile: None,
            rev: 0,
            keys_mode: KeysMode::Panel,
            learning: None,
            down: [false; 41],
            enc: [0; 7],
            pot: None,
            labels: 0,
            msg: None,
        }
    }
}

/// The file that remembers the last controller chosen, for new projects.
fn last_file() -> Option<PathBuf> {
    fm1_map::user_dir().and_then(|d| d.parent().map(|p| p.join("plugin-controller.txt")))
}

/// The profile the last project chose ("" for none), if one did.
pub fn last_choice() -> Option<Option<Profile>> {
    let name = std::fs::read_to_string(last_file()?).ok()?;
    let name = name.trim();
    if name.is_empty() {
        return Some(None);
    }
    let (all, _) = fm1_map::all_profiles();
    all.into_iter().find(|p| p.name == name).map(Some)
}

pub fn remember(p: Option<&Profile>) {
    if let Some(f) = last_file() {
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, p.map_or("", |p| p.name.as_str()));
    }
}

/// The target of a panel hit (the editor's codes: 0..6 an encoder, 7 MASTER, 100 + id a button
/// or a key); a key learns the key zone so the note pressed plays it.
pub fn target_of_hit(code: i32) -> Option<Target> {
    match code {
        0..=6 => Some(Target::Enc(crate::panel::ENCS[code as usize].1)),
        7 => Some(Target::Master),
        100..=113 => Some(Target::Button((code - 100) as u8)),
        114..=140 => Some(Target::Key((code - 114) as u8)),
        _ => None,
    }
}
