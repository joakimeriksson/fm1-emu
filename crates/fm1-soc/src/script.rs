//! Shared benchmark input script, scheduled on the emulated clock.
use crate::{Action, Soc};

pub fn queue(soc: &mut Soc, keys: &str) {
    for k in keys.split(',') {
        if let Some((t, rest)) = k.split_once(':') {
            // `ms:mHEX` sends MIDI bytes into the MIDI IN jack (`ms:m903c64` = note on C4)
            if let Some(hex) = rest.trim().strip_prefix('m') {
                if let Ok(t) = t.trim().parse::<u64>() {
                    let at = soc.periph.cycle_of_ms(t);
                    for i in (0..hex.len() & !1).step_by(2) {
                        if let Ok(b) = u8::from_str_radix(&hex[i..i + 2], 16) {
                            soc.periph.script.push_back((at, Action::Midi { b }));
                        }
                    }
                }
                continue;
            }
            // `ms:pot:N` sets the MASTER pot (SARADC channel 4) to N (0..1023)
            if let Some(v) = rest.trim().strip_prefix("pot:") {
                if let (Ok(t), Ok(value)) = (t.trim().parse::<u64>(), v.trim().parse::<u16>()) {
                    let at = soc.periph.cycle_of_ms(t);
                    soc.periph.script.push_back((at, Action::Adc { ch: 4, value: value.min(1023) }));
                }
                continue;
            }
            // `ms:aN:V` sets analog input N to V (0..1023): the SMK-37's `ANALOG_*` (pads 0..15 by force,
            // faders 16..19, the pitch and mod wheels 20, 21, the pedal 22); on the FM-1 SARADC channel N
            if let Some((ch, v)) = rest.trim().strip_prefix('a').and_then(|r| r.split_once(':')) {
                if let (Ok(t), Ok(ch), Ok(value)) = (t.trim().parse::<u64>(), ch.parse::<u8>(), v.trim().parse::<u16>()) {
                    let at = soc.periph.cycle_of_ms(t);
                    soc.periph.script.push_back((at, Action::Adc { ch, value: value.min(1023) }));
                }
                continue;
            }
            // `ms:eN:clicks` turns matrix encoder N
            if let Some((e, c)) = rest.strip_prefix('e').and_then(|r| r.split_once(':')) {
                if let (Ok(t), Ok(enc), Ok(clicks)) = (t.trim().parse::<u64>(), e.parse::<u8>(), c.parse::<i32>()) {
                    let at = soc.periph.cycle_of_ms(t);
                    soc.periph.script.push_back((at, Action::Enc { enc, clicks }));
                }
                continue;
            }
            // `ms:ID@VEL` presses at velocity VEL (1..127; FM-1: how hard it lands on its pill),
            // `ms:ID~VEL` presses a held key to VEL's force (aftertouch)
            let (rest, vel, pressure) = match rest.trim().split_once(['@', '~']) {
                Some((i, v)) => (i, v.parse::<u8>().ok(), rest.contains('~')),
                None => (rest, None, false),
            };
            let id = rest;
            let (id, down) = match id.trim().strip_suffix('^') { Some(i) => (i, false), None => (id.trim(), true) };
            // a number, or a name on this board's panel (FM-1: `PLAY`, `C4`; SMK-37: `seqplay`, `k12`)
            let id = id.parse::<u8>().ok().or_else(|| match soc.board {
                crate::Board::Smk37 => crate::smk::key_id(id),
                crate::Board::Fm1 => crate::periph::key_id(id),
            });
            if let (Ok(t), Some(id)) = (t.trim().parse::<u64>(), id) {
                let at = soc.periph.cycle_of_ms(t);
                match (vel, pressure) {
                    (Some(vel), true) => soc.periph.script.push_back((at, Action::Pressure { id, vel })),
                    (Some(vel), false) => {
                        soc.periph.script.push_back((at, Action::KeyVel { vel }));
                        if soc.board == crate::Board::Smk37 {
                            // the SMK-37's velocity is the time between a key's two contacts: 80 ms (1) ..
                            // 2 ms (127), on fm1-live's panel's curve
                            soc.periph.script.push_back((at, Action::Vel { us: smk_contact_us(vel) }));
                        }
                        soc.periph.script.push_back((at, Action::Key { id, down }));
                    }
                    _ => soc.periph.script.push_back((at, Action::Key { id, down })),
                }
            }
        }
    }
    soc.periph.script.make_contiguous().sort_by_key(|(c, _)| *c);
}

/// The time between an SMK-37 key's two contacts for MIDI velocity `vel` (1..127): 80 ms .. 2 ms,
/// logarithmic (fm1-live's panel: lower on a key = harder).
pub fn smk_contact_us(vel: u8) -> u32 {
    let f = (vel.clamp(1, 127) - 1) as f32 / 126.0;
    (80_000f32.ln() + (2_000f32.ln() - 80_000f32.ln()) * f).exp() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedules_keys_encoders_and_midi_in_stable_time_order() {
        let mut soc = Soc::new(1 << 20, vec![]);
        queue(&mut soc, "100:21^,50:e2:-3,50:m903c64,0:21");
        let actions: Vec<_> = soc.periph.script.iter().map(|(_, a)| a.clone()).collect();
        assert_eq!(actions, vec![Action::Key { id: 21, down: true },
            Action::Enc { enc: 2, clicks: -3 }, Action::Midi { b: 0x90 },
            Action::Midi { b: 0x3c }, Action::Midi { b: 0x64 },
            Action::Key { id: 21, down: false }]);
        assert_eq!(soc.periph.script.back().unwrap().0, soc.periph.cycle_of_ms(100));
    }

    #[test]
    fn velocity_and_pressure() {
        let mut soc = Soc::new(1 << 20, vec![]);
        queue(&mut soc, "0:21@40,10:21~120,20:21^");
        let actions: Vec<_> = soc.periph.script.iter().map(|(_, a)| a.clone()).collect();
        assert_eq!(actions, vec![Action::KeyVel { vel: 40 }, Action::Key { id: 21, down: true },
            Action::Pressure { id: 21, vel: 120 }, Action::Key { id: 21, down: false }]);
    }

    #[test]
    fn smk_velocity_is_the_contact_time_and_analog_inputs_are_set() {
        let mut soc = Soc::new(1 << 20, vec![]);
        soc.set_board(crate::Board::Smk37);
        queue(&mut soc, "0:k12@127,10:k12^,20:a16:700,30:a20:900");
        let actions: Vec<_> = soc.periph.script.iter().map(|(_, a)| a.clone()).collect();
        assert_eq!(actions, vec![Action::KeyVel { vel: 127 }, Action::Vel { us: smk_contact_us(127) },
            Action::Key { id: 12, down: true }, Action::Key { id: 12, down: false },
            Action::Adc { ch: 16, value: 700 }, Action::Adc { ch: 20, value: 900 }]);
        assert!((1_990..=2_010).contains(&smk_contact_us(127)) && (79_000..=80_000).contains(&smk_contact_us(1)));
        assert!(smk_contact_us(20) > smk_contact_us(100));
    }
}
