//! The tags of what a controller profile binds, drawn over the FM-1's panel (fm1-live, the plugin).

use crate::panel::{self, ENCS};
use fm1_map::{Profile, Target};


/// The tags of what the controller is bound to, over each panel control.
pub fn draw(buf: &mut [u32], profile: &Profile, learning: Option<Target>) {
    let tag_color = 0x8fd3ff;
    let learn_color = 0xff6b6b;
    let col = |t: Target| if learning == Some(t) { learn_color } else { tag_color };
    let tag = |t: Target| -> Option<String> {
        if learning == Some(t) {
            return Some("LEARN".into());
        }
        profile.tag_of(t).map(|s| s.to_ascii_uppercase().chars().filter(|c| c.is_ascii_alphanumeric() || "+-/.".contains(*c)).take(7).collect())
    };
    for (i, (_, _, cx, cy, r)) in ENCS.iter().enumerate() {
        let t = Target::Enc(ENCS[i].1);
        if let Some(s) = tag(t) {
            panel::text(buf, *cx, cy + r + 6.0, &s, 2, col(t));
        }
    }
    if let Some(s) = tag(Target::Master) {
        panel::text(buf, panel::POT.0, panel::POT.1 + panel::POT.2 + 6.0, &s, 2, col(Target::Master));
    }
    for (_, id, x, y, w, _) in panel::buttons() {
        let t = Target::Button(id);
        if let Some(s) = tag(t) {
            panel::text(buf, (x + w / 2) as f32, y as f32 - 14.0, &s, 1, col(t));
        }
    }
    for (id, x, y, w, h, _) in panel::keys() {
        let t = Target::Key(id - fm1_map::KEY0);
        if let Some(s) = tag(t) {
            panel::text(buf, (x + w / 2) as f32, (y + h) as f32 - 14.0, &s, 1, col(t));
        }
    }
}
