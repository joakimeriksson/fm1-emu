//! Host controller profiles translate gestures to physical panel actions, never firmware patches.
use crate::Action;

pub fn is_minilab3(name: &str) -> bool {
    let name = name.to_lowercase().replace(' ', "");
    name.contains("minilab3") && name.ends_with("midi")
}

/// Factory Arturia/User CC layout, absolute encoders. First value establishes a baseline:
/// connecting or touching a control never jumps an unknown firmware parameter.
pub struct MiniLab3 { previous: [Option<u8>; 128] }
impl Default for MiniLab3 { fn default() -> Self { Self { previous: [None; 128] } } }
impl MiniLab3 {
    /// True means consumed: forwarding this CC as well would change two parameters.
    pub fn route(&mut self, msg: &[u8], mut emit: impl FnMut(Action)) -> bool {
        if msg.len() != 3 || msg[0] & 0xf0 != 0xb0 || msg[1] > 127 || msg[2] > 127 { return false; }
        let (cc, value) = (msg[1], msg[2]);
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
