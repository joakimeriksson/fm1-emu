//! Host controller profiles translate gestures to physical panel actions, never firmware patches.
use crate::Action;

pub fn is_minilab3(name: &str) -> bool {
    let name = name.to_lowercase().replace(' ', "");
    name.contains("minilab3") && name.ends_with("midi")
}

/// Factory Arturia/User CC layout, absolute encoders. First value establishes a baseline:
/// connecting or touching a control never jumps an unknown firmware parameter.
pub struct MiniLab3 { previous: [Option<u8>; 128], dedicated: bool, held: [bool; 17] }
impl Default for MiniLab3 { fn default() -> Self { Self { previous: [None; 128], dedicated: false, held: [false; 17] } } }
impl MiniLab3 {
    pub fn fm1() -> Self { Self { dedicated: true, ..Self::default() } }

    /// Release only buttons this profile owns, including duplicate HOME/SEL assignments.
    pub fn release(&mut self, mut emit: impl FnMut(Action)) {
        for id in 0..14 { if self.held.iter().enumerate().any(|(i, down)| *down && Self::button(i) == id) {
            emit(Action::Key { id, down: false });
        }}
        self.held.fill(false);
    }
    fn button(source: usize) -> u8 {
        [8,2,6,11,3,12,13,7, 4,5,10,9,0,1,8,3, 8][source]
    }
    fn key(&mut self, source: usize, down: bool, mut emit: impl FnMut(Action)) {
        let id = Self::button(source);
        let before = self.held.iter().enumerate().any(|(i, held)| *held && Self::button(i) == id);
        self.held[source] = down;
        let after = self.held.iter().enumerate().any(|(i, held)| *held && Self::button(i) == id);
        if before != after { emit(Action::Key { id, down: after }); }
    }
    /// True means consumed: forwarding this CC as well would change two parameters.
    pub fn route(&mut self, msg: &[u8], mut emit: impl FnMut(Action)) -> bool {
        if msg.len() != 3 || msg[0] & 0xf0 != 0xb0 || msg[1] > 127 || msg[2] > 127 { return false; }
        let (cc, value) = (msg[1], msg[2]);
        if self.dedicated {
            // Dedicated User program: CC74 absolute; seven Relative 1 encoders (64 +/- delta).
            if cc == 74 { emit(Action::Adc { ch: 4, value: ((value as u32 * 1023 + 63) / 127) as u16 }); return true; }
            let enc = match cc { 71 => 0, 76 => 6, 77 => 1, 93 => 2, 18 => 3, 19 => 4, 16 => 5,
                114 => 0, 112 => 6, _ => 255 };
            if enc != 255 {
                let clicks = value as i32 - 64;
                if clicks != 0 { emit(Action::Enc { enc, clicks }); }
                return true;
            }
            // Channel 16 isolates command pads from normal performance controllers.
            if msg[0] == 0xbf && (20..36).contains(&cc) {
                self.key((cc - 20) as usize, value >= 64, emit); return true;
            }
            if cc == 115 { self.key(16, value >= 64, emit); return true; }
            // MiniLab Shift belongs to its local functions, never to the FM-1 SEL modifier.
            if cc == 9 || cc == 113 { return true; }
            if [120,123].contains(&cc) { self.release(emit); return false; }
            return false;
        }
        // Four sound knobs, then navigation. The large encoder is SELECT too.
        let enc = match cc { 74 => 2, 71 => 3, 76 => 4, 77 => 5,
            93 => 0, 18 => 6, 19 => 1, _ => 255 };
        if enc != 255 {
            if let Some(last) = self.previous[cc as usize].replace(value) {
                let clicks = value as i32 - last as i32;
                if clicks != 0 { emit(Action::Enc { enc, clicks }); }
            }
            return true;
        }
        // Main encoder uses relative offset encoding (64 = stationary).
        if cc == 114 || cc == 112 {
            let clicks = value as i32 - 64;
            if clicks != 0 { emit(Action::Enc { enc: if cc == 112 { 6 } else { 0 }, clicks }); }
            return true;
        }
        // One dedicated master fader; other faders remain firmware MIDI controls.
        if cc == 16 || cc == 17 { emit(Action::Adc { ch: 4, value: ((value as u32 * 1023 + 63) / 127) as u16 }); return true; }
        // Main encoder click = HOME. Shift-click deliberately not SAVE (no surprise writes).
        if cc == 115 || cc == 9 { emit(Action::Key { id: if cc == 9 { 3 } else { 8 }, down: value >= 64 }); return true; }
        false
    }
}

#[cfg(test)]
mod tests {
 use super::*;
 #[test] fn all_four_knobs_are_panel_encoders_without_double_cc() {
   let mut p=MiniLab3::default();
   for (cc, enc) in [(74,2),(71,3),(76,4),(77,5),(93,0),(18,6),(19,1)] {
     let mut out=vec![];
     assert!(p.route(&[0xb0,cc,64],|a|out.push(a))); assert!(out.is_empty());
     assert!(p.route(&[0xb0,cc,65],|a|out.push(a)));
     assert!(matches!(out.as_slice(), [Action::Enc{enc:e,clicks:1}] if *e==enc));
     out.clear(); p.route(&[0xb0,cc,63],|a|out.push(a));
     assert!(matches!(out.as_slice(),[Action::Enc{clicks:-2,..}]));
   }
 }
 #[test] fn performance_and_hold_messages_pass_through() {
   let mut p=MiniLab3::default();
   for msg in [&[0x90,60,100][..], &[0x80,60,0], &[0xb0,64,127], &[0xb0,1,0]] {
     assert!(!p.route(msg, |_|panic!("performance message intercepted")));
   }
   assert!(!is_minilab3("Minilab3 MCU/HUI"));assert!(is_minilab3("MiniLab 3 MIDI"));
 }
 #[test] fn master_and_home_have_explicit_endpoints() {
   let mut p=MiniLab3::default(); let mut out=vec![];
   for v in [0,127] {p.route(&[0xb0,17,v],|a|out.push(a));}
   assert!(matches!(out.as_slice(),[Action::Adc{ch:4,value:0},Action::Adc{ch:4,value:1023}]));
   out.clear();for v in [127,0] {p.route(&[0xb0,115,v],|a|out.push(a));}
   assert!(matches!(out.as_slice(),[Action::Key{id:8,down:true},Action::Key{id:8,down:false}]));
 }
}

#[cfg(test)]
mod dedicated_tests {
    use super::*;
    #[test]
    fn relative_knobs_turn_on_first_message_and_repeat_without_endpoints() {
        let mut p = MiniLab3::fm1();
        for (cc, enc) in [(71,0),(76,6),(77,1),(93,2),(18,3),(19,4),(16,5),(114,0),(112,6)] {
            for (value, clicks) in [(65,1),(65,1),(63,-1),(67,3)] {
                let mut out = vec![];
                assert!(p.route(&[0xb0,cc,value], |a| out.push(a)));
                assert!(matches!(out.as_slice(), [Action::Enc {enc:e, clicks:c}] if *e==enc && *c==clicks));
            }
            p.route(&[0xb0,cc,64], |_| panic!("neutral encoder moved"));
        }
    }
    #[test]
    fn pads_cover_every_panel_button_and_preserve_holds() {
        let mut p = MiniLab3::fm1();
        for (cc,id) in (20..36).zip([8,2,6,11,3,12,13,7,4,5,10,9,0,1,8,3]) {
            for down in [true,false] {
                let mut out=vec![];
                assert!(p.route(&[0xbf,cc,if down {127} else {0}], |a|out.push(a)));
                assert!(matches!(out.as_slice(), [Action::Key {id:i,down:d}] if *i==id && *d==down));
            }
        }
        let mut out=vec![];
        for msg in [[0xbf,20,127],[0xbf,34,127],[0xb0,115,127],[0xbf,20,0],[0xbf,34,0]] {
            p.route(&msg, |a|out.push(a));
        }
        assert_eq!(out.len(),1,"duplicate HOME sources must not release one another");
        p.release(|a|out.push(a));
        assert!(matches!(out.last(),Some(Action::Key{id:8,down:false})));
        p.release(|_|panic!("already released"));
    }
    #[test]
    fn performance_faders_and_unassigned_channels_pass_through() {
        let mut p=MiniLab3::fm1();
        for msg in [[0x90,60,100],[0x80,60,0],[0x99,36,100],[0xb0,20,127],[0xb0,17,127],
            [0xb0,82,127],[0xb0,83,127],[0xb0,85,127],[0xb0,64,127],[0xe0,0,64],[0xb0,1,127]] {
            assert!(!p.route(&msg, |_|panic!("performance input intercepted")));
        }
        for cc in [9,113] { assert!(p.route(&[0xb0,cc,127], |_|panic!("local Shift action leaked"))); }
        for (v,expected) in [(0,0),(127,1023)] {
            p.route(&[0xb0,74,v], |a| assert!(matches!(a,Action::Adc{ch:4,value} if value==expected)));
        }
    }
}
