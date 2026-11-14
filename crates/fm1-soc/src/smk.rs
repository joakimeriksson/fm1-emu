//! The SMK-37's panel (SMK-37 Pro / Elite, MKE-P37 and Starrykey 37 Play share it), after
//! fm1-to-smk37's docs/01-hardware.md and the reference port's firmware/hal/smk_input.h:
//!
//! * the matrix: 16 columns x 8 rows. Four 74HC595s take a 32-bit word by SPI2 (4 DMA bytes,
//!   little-endian in memory) and show it on a PA1 pulse: bits 31..24 select columns 0..7 and
//!   23..16 columns 8..15 (active low, one-hot), bits 15..0 are the button LED lines (active
//!   low) except 5..3, the analog muxes' address. The rows come back low = closed on PA2,
//!   PA5..PA7, PA9 (rows 0..4: the keybed), PH6, PH8 (5, 6: the buttons) and PH9 (7: the
//!   encoders). A position is `col << 4 | row`, in the hardware map's column numbering, which
//!   is the reference port's: it reads a column's rows before latching the next word, so the
//!   rows it files under column c come from the word selecting column c - 1 (mod 16). The
//!   contacts of code column c close while that word is latched.
//! * 37 keys with two contacts each: the first closes early in the stroke, the second near the
//!   bottom; the time between them is the velocity. Here the second closes `vel` cycles after
//!   the first.
//! * analog: three 8:1 muxes addressed by the latched word's bits 5..3, on ADC channels 1 (mux
//!   A: the top pads 0..7), 8 (mux B: the bottom pads 8..15) and 2 (mux C: the faders, the pitch
//!   and mod wheels, the pedal jack, the battery). `Action::Adc` sets them by `ANALOG_*` index.
//! * 19 button LEDs: the word's low 16 bits (active low), in two groups whose commons are bits
//!   17 and 18 of 0x16A0C (the second USB PHY's pads as GPIO; low = on), one group per frame.
//! * 16 RGB pad LEDs (WS2812, GRB) on SPI1 group B (IOMAP_CON1 bit 4 set; group A is the LCD):
//!   one SPI byte per LED bit, 0xF0 = 1, 0xC0 = 0, after a run of zero bytes.

/// `Action::Key` ids on this board: the keybed 0..36 (C3..C6), then the 18 buttons.
pub const NKEY: usize = 37;
pub const NBTN: usize = 18;
pub const NENC: usize = 8;
pub const NPAD: usize = 16;
pub const BTN0: u8 = NKEY as u8;

/// Each key's first and second contact, `col << 4 | row` (docs/01 5.2).
const KEY_C1: [u8; NKEY] = [
    0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0, 0xc0, 0xd0, 0xe0, 0xf0, 0xb0, 0x11, 0x21, 0x31, 0x41,
    0x51, 0x61, 0x71, 0x81, 0x91, 0xa1, 0xb1, 0xc1, 0xd1, 0xe1, 0xf1, 0x64, 0x74, 0x84, 0x94, 0xa4, 0xb4, 0xc4,
];
const KEY_C2: [u8; NKEY] = [
    0x52, 0x72, 0x92, 0xb2, 0xd2, 0xf2, 0xe2, 0xc2, 0xa2, 0x82, 0x62, 0x42, 0x22, 0x12, 0x32, 0x33, 0x43, 0x53, 0x63,
    0x73, 0x83, 0x93, 0xa3, 0xb3, 0xc3, 0xd3, 0xe3, 0x23, 0x13, 0x14, 0x24, 0x34, 0x44, 0x54, 0xd4, 0xf4, 0xe4,
];

/// The buttons in the port's order (docs/01 5.3): position and printed name.
pub const BUTTONS: [(u8, &str); NBTN] = [
    (0x26, "KNOB BANK"), (0x76, "FADER BANK"), (0x96, "<"), (0x56, ">"), (0x66, "OCTAVE-"), (0x36, "OCTAVE+"),
    (0x75, "ARP"), (0x65, "NOTE REPEAT"), (0x25, "SCALE"), (0x45, "CHORD"), (0x15, "GLOBE"), (0x86, "BT"),
    (0x95, "PATCH"), (0x55, "PARA"), (0x85, "FX"), (0x35, "SEQ"), (0x16, "SEQ PLAY"), (0x46, "SEQ REC"),
];

/// Encoders K1..K8: the positions of their A and B contacts (docs/01 5.4).
const ENC_AB: [[u8; 2]; NENC] =
    [[0x97, 0xa7], [0x37, 0x47], [0x17, 0x27], [0xb7, 0xc7], [0x57, 0x67], [0x77, 0x87], [0xd7, 0xe7], [0xf7, 0x07]];

/// `Action::Adc` indices on this board: pads 0..15 (force, 0 = untouched), faders 16..19,
/// then the pitch wheel, the mod wheel and the pedal.
pub const ANALOG_FADER0: u8 = 16;
pub const ANALOG_PITCH: u8 = 20;
pub const ANALOG_MOD: u8 = 21;
pub const ANALOG_PEDAL: u8 = 22;
/// Rest values on the tested unit (docs/01 6.5, 6.6): the pitch wheel centred, the mod wheel
/// down (up reads lower), no pedal plugged in, a full battery.
pub const PITCH_REST: u16 = 535;
pub const MOD_REST: u16 = 629;

/// Which mux address each pad / fader index sits on (docs/01 6.2).
const PAD_ADDR: [usize; 8] = [3, 0, 1, 2, 4, 6, 7, 5];
const FADER_ADDR: [usize; 4] = [3, 0, 1, 2];

/// The button LEDs (LED k = button k; 18 a spare): group, bit in the word (docs/01 5.5).
const LEDS: [(u8, u8); 19] = [
    (0, 12), (1, 2), (0, 0), (1, 0), (0, 13), (1, 13), (0, 2), (0, 1), (0, 9), (0, 8), (0, 11), (0, 10), (1, 1),
    (1, 9), (1, 8), (1, 10), (1, 12), (1, 14), (1, 11),
];
pub const NLED: usize = 19;

/// The pad LED chain: pad p is LED `PAD_SLOT[p]` in the chain.
const PAD_SLOT: [usize; NPAD] = [0, 15, 14, 13, 12, 11, 10, 9, 1, 2, 3, 4, 5, 6, 7, 8];

pub struct Panel {
    /// the 595 chain as shifted (the last 4 bytes) and as latched
    sr: u32,
    pub latched: u32,
    /// keys 0..36, then buttons
    pub down: [bool; NKEY + NBTN],
    /// cycle the second contact of each key closes at (u64::MAX: open)
    c2_at: [u64; NKEY],
    /// the pads' colours, 0xRRGGBB, from the last LED frame
    pub pads: [u32; NPAD],
    pub pad_frames: u64,
    /// what each mux (A, B, C) shows on each address
    analog: [[u16; 8]; 3],
    /// 0x16A0C (the LED groups' commons)
    pub commons: u32,
    /// latches with each LED lit, and with its group on, since `led_take`
    led_lit: [u32; NLED],
    group_on: [u32; 2],
}

impl Default for Panel {
    fn default() -> Self {
        Panel {
            sr: u32::MAX,
            latched: u32::MAX,
            down: [false; NKEY + NBTN],
            c2_at: [u64::MAX; NKEY],
            pads: [0; NPAD],
            pad_frames: 0,
            analog: [[0; 8], [0; 8], [0, 0, 0, 0, PITCH_REST, MOD_REST, 985, 640]],
            commons: u32::MAX,
            led_lit: [0; NLED],
            group_on: [0; 2],
        }
    }
}

impl Panel {
    /// A byte into the 595 chain (SPI2): four make the word, in memory order.
    pub fn spi2_byte(&mut self, b: u8) {
        self.sr = (self.sr >> 8) | (b as u32) << 24;
    }

    /// PA1's rising edge: the chain's outputs follow; the LEDs of the group whose common is on
    /// light where their bit is low.
    pub fn latch(&mut self) {
        self.latched = self.sr;
        for g in 0..2 {
            if self.commons >> (17 + g) & 1 == 0 {
                self.group_on[g] += 1;
                for (k, &(lg, bit)) in LEDS.iter().enumerate() {
                    if lg as usize == g && self.latched >> bit & 1 == 0 {
                        self.led_lit[k] += 1;
                    }
                }
            }
        }
    }

    /// Each button LED's lit fraction (of its group's time) since the last `led_take`, which
    /// goes on counting.
    pub fn led_peek(&self) -> [f32; NLED] {
        std::array::from_fn(|k| {
            let on = self.group_on[LEDS[k].0 as usize];
            if on == 0 { 0.0 } else { self.led_lit[k] as f32 / on as f32 }
        })
    }

    /// Each button LED's lit fraction (of its group's time) since the last call.
    pub fn led_take(&mut self) -> [f32; NLED] {
        let out = self.led_peek();
        self.led_lit = [0; NLED];
        self.group_on = [0; 2];
        out
    }

    /// The reading of analog input `idx` (`ANALOG_*`).
    pub fn analog(&self, idx: u8) -> u16 {
        match idx as usize {
            p @ 0..=15 => self.analog[p / 8][PAD_ADDR[p % 8]],
            f @ 16..=19 => self.analog[2][FADER_ADDR[f - 16]],
            20 => self.analog[2][4],
            21 => self.analog[2][5],
            22 => self.analog[2][6],
            _ => 0,
        }
    }

    /// Key or button `id` pressed or let go at `now`; a key's second contact closes `vel`
    /// cycles after its first.
    pub fn key(&mut self, id: u8, down: bool, now: u64, vel: u64) {
        let i = id as usize;
        if i >= NKEY + NBTN {
            return;
        }
        self.down[i] = down;
        if i < NKEY {
            self.c2_at[i] = if down { now + vel } else { u64::MAX };
        }
    }

    /// The rows (bit r = row r) a closed contact pulls low in the selected column(s) at `now`;
    /// `enc(i)` is encoder i's (A, B) contact state.
    pub fn rows(&self, now: u64, enc: impl Fn(usize) -> (bool, bool)) -> u32 {
        let w = self.latched;
        let sel = |code: u8| {
            let c = ((code >> 4) as u32 + 15) % 16; // the word's column (see the top)
            let bit = if c < 8 { 24 + c } else { 16 + c - 8 };
            w >> bit & 1 == 0
        };
        if w >> 16 == 0xffff {
            return 0;
        }
        let mut rows = 0u32;
        let mut close = |code: u8| {
            if sel(code) {
                rows |= 1 << (code & 15);
            }
        };
        for k in 0..NKEY {
            if self.down[k] {
                close(KEY_C1[k]);
                if now >= self.c2_at[k] {
                    close(KEY_C2[k]);
                }
            }
        }
        for (b, (code, _)) in BUTTONS.iter().enumerate() {
            if self.down[NKEY + b] {
                close(*code);
            }
        }
        for (i, ab) in ENC_AB.iter().enumerate() {
            let (a, b) = enc(i);
            if a {
                close(ab[0]);
            }
            if b {
                close(ab[1]);
            }
        }
        rows
    }

    /// The analog muxes' address (the latched word's bits 5..3).
    pub fn mux(&self) -> u32 {
        (self.latched >> 3) & 7
    }

    /// What ADC channel `ch` reads now: its mux at the latched address (None: not a mux).
    pub fn adc(&self, ch: u32) -> Option<u16> {
        let m = match ch {
            1 => 0,
            8 => 1,
            2 => 2,
            _ => return None,
        };
        Some(self.analog[m][self.mux() as usize])
    }

    /// Set analog input `idx` (`ANALOG_*`) to `value` (0..1023).
    pub fn set_analog(&mut self, idx: u8, value: u16) {
        let v = value.min(1023);
        match idx as usize {
            p @ 0..=15 => self.analog[p / 8][PAD_ADDR[p % 8]] = v,
            f @ 16..=19 => self.analog[2][FADER_ADDR[f - 16]] = v,
            20 => self.analog[2][4] = v,
            21 => self.analog[2][5] = v,
            22 => self.analog[2][6] = v,
            _ => {}
        }
    }

    /// One SPI1 transfer on group B: a pad LED frame (zero bytes to latch, then 24 bit-bytes per
    /// LED, GRB, most significant first).
    pub fn pad_frame(&mut self, bytes: &[u8]) {
        let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
        let bits = &bytes[start..];
        if bits.len() < 24 {
            return;
        }
        for (pad, &slot) in PAD_SLOT.iter().enumerate() {
            let Some(led) = bits.get(slot * 24..slot * 24 + 24) else { continue };
            let v = led.iter().fold(0u32, |v, &b| v << 1 | (b & 0x30 != 0) as u32);
            let (g, r, b) = (v >> 16 & 0xff, v >> 8 & 0xff, v & 0xff);
            self.pads[pad] = r << 16 | g << 8 | b;
        }
        self.pad_frames += 1;
    }
}

/// A button's id by its printed name (case and spaces ignored), or a key by `k0`..`k36`.
pub fn key_id(name: &str) -> Option<u8> {
    let norm = |s: &str| s.to_ascii_uppercase().replace([' ', '_'], "");
    let n = norm(name);
    if let Some(b) = BUTTONS.iter().position(|(_, nm)| norm(nm) == n) {
        return Some(BTN0 + b as u8);
    }
    n.strip_prefix('K').and_then(|k| k.parse::<u8>().ok()).filter(|&k| (k as usize) < NKEY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(p: &mut Panel, w: u32) {
        for b in w.to_le_bytes() {
            p.spi2_byte(b);
        }
        p.latch();
    }

    #[test]
    fn keys_buttons_and_encoders_show_in_their_columns() {
        let mut p = Panel::default();
        p.key(0, true, 0, 100); // C3: contacts 0x10 and 0x52
        p.key(BTN0 + 16, true, 0, 0); // SEQ PLAY: 0x16
        // code column 1 = the word selecting column 0 (bit 24 low): C3's first contact (row 0)
        // and SEQ PLAY (row 6)
        word(&mut p, !0x0100_0000u32);
        assert_eq!(p.rows(0, |_| (false, false)), 1 | 1 << 6);
        // code column 5: the second contact closes only after `vel`
        word(&mut p, !(0x0100_0000u32 << 4));
        assert_eq!(p.rows(50, |_| (false, false)), 0);
        assert_eq!(p.rows(100, |_| (false, false)), 1 << 2);
        // code column 0 = the word selecting column 15 (bit 23): K8's B contact (0x07)
        word(&mut p, !(0x0001_0000u32 << 7));
        assert_eq!(p.rows(0, |i| (false, i == 7)), 1 << 7);
        // code column 9 = the word selecting column 8 (bit 16)
        word(&mut p, !0x0001_0000u32);
        assert_eq!(p.rows(0, |i| (i == 0, false)), 1 << 7, "K1's A contact is 0x97");
    }

    #[test]
    fn pad_frame_decodes_grb_in_chain_order() {
        let mut p = Panel::default();
        let mut f = vec![0u8; 36];
        for slot in 0..NPAD {
            // slot 15 (pad 1) red, slot 1 (pad 8) blue, the rest off
            let grb: u32 = match slot { 15 => 0x00ff00, 1 => 0x0000ff, _ => 0 };
            for i in (0..24).rev() {
                f.push(if grb >> i & 1 != 0 { 0xf0 } else { 0xc0 });
            }
        }
        p.pad_frame(&f);
        assert_eq!(p.pads[1], 0xff0000);
        assert_eq!(p.pads[8], 0x0000ff);
        assert_eq!(p.pads[0], 0);
    }

    #[test]
    fn names() {
        assert_eq!(key_id("seq play"), Some(BTN0 + 16));
        assert_eq!(key_id("k36"), Some(36));
        assert_eq!(key_id("k37"), None);
    }

    #[test]
    fn led_peek_reads_the_fractions_without_taking_them() {
        let mut p = Panel::default();
        let (g, bit) = LEDS[0];
        p.commons = u32::MAX & !(1 << (17 + g as u32)); // its group's common on
        word(&mut p, !(1u32 << bit)); // lit (active low)
        word(&mut p, !0u32); // dark
        assert_eq!(p.led_peek()[0], 0.5);
        assert_eq!(p.led_peek()[0], 0.5, "a peek leaves the counts alone");
        assert_eq!(p.led_take()[0], 0.5, "the take still covers all of them");
        assert_eq!(p.led_take()[0], 0.0, "the take did reset them");
    }
}
