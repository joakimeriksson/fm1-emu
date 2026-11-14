//! The SMK-37's front panel, drawn after the unit itself (M-VAVE's product photos) on the map of
//! fm1-to-smk37's panel (assets/smk37-panel.png): a near-black body; the pitch and mod wheels, the
//! M-VAVE logo and the six small buttons on the left; the screen (2x here, small on the unit);
//! K1..K8 over two rows of eight dark pads that glow their RGB colour from below; the four faders
//! and twelve buttons on the right; the 37 keys C3..C6 across the bottom, their shifted functions
//! printed above them. Ids are fm1_soc::smk's: keys 0..36, buttons 37..54.

use crate::panel::{canvas, disc, mix, put, rrect, text, AMBER, ROOM};

pub const W: usize = 1920;
pub const H: usize = 880;
pub const LCD_X: usize = 262;
pub const LCD_Y: usize = 44;
const NKEY: usize = 37;
const BTN0: u8 = 37;

const BODY_EDGE: u32 = 0x3a3c41;
const BODY_TOP: u32 = 0x222327;
const BODY_BOT: u32 = 0x18191c;
/// printing on the body
const INK: u32 = 0xb9bbbf;
const INK_DIM: u32 = 0x7d8085;
const CAP_TOP: u32 = 0x2b2c31;
const CAP_BOT: u32 = 0x1f2024;
/// the button LEDs' light (BT's is blue)
const LED: u32 = 0xe9eefc;
const LED_BT: u32 = 0x5b8cff;

/// What a point on the panel is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ctl {
    /// An encoder K1..K8.
    Enc(usize),
    /// A pad (hardware index 0..15: the top row 0..7).
    Pad(usize),
    /// A fader F1..F4.
    Fader(usize),
    Pitch,
    Mod,
    /// A button (id 37..54) or a key (0..36); `y` how far down the key, 0..1.
    Btn(u8),
    Key(u8, f32),
}

/// Encoders: centre x, y, radius (each over its column of pads).
pub fn encs() -> [(f32, f32, f32); 8] {
    std::array::from_fn(|i| (822.0 + i as f32 * 89.0, 158.0, 31.0))
}

/// Pads: x, y, w, h (pad p: the top row 0..7).
pub fn pads() -> [(usize, usize, usize, usize); 16] {
    std::array::from_fn(|p| (784 + (p % 8) * 89, 236 + (p / 8) * 98, 76, 76))
}

/// Faders: the slot's x, top y, length (the cap travels it).
pub fn faders() -> [(usize, usize, usize); 4] {
    std::array::from_fn(|i| (1562 + i * 92, 84, 240))
}

/// The wheels: x, y, w, h (pitch, mod).
pub const WHEELS: [(usize, usize, usize, usize); 2] = [(54, 70, 68, 214), (150, 70, 68, 214)];

/// Buttons: (label, id, x, y, w, h).
pub fn buttons() -> Vec<(&'static str, u8, usize, usize, usize, usize)> {
    let b = |i: u8| BTN0 + i;
    let mut v = Vec::new();
    // left, under the logo: KNOB BANK / FADER BANK, < / >, OCTAVE- / OCTAVE+
    for (r, pair) in [[(0, "KNOB BANK"), (1, "FADER BANK")], [(2, "<"), (3, ">")], [(4, "OCTAVE-"), (5, "OCTAVE+")]].iter().enumerate() {
        for (c, (i, name)) in pair.iter().enumerate() {
            v.push((*name, b(*i), 42 + c * 98, 384 + r * 52, 90, 42));
        }
    }
    // right, under the faders: ARP .. BT, PATCH .. SEQ REC
    let top = [(6, "ARP"), (7, "NOTE REPEAT"), (8, "SCALE"), (9, "CHORD"), (10, "GLOBE"), (11, "BT")];
    let bot = [(12, "PATCH"), (13, "PARA"), (14, "FX"), (15, "SEQ"), (16, "SEQ PLAY"), (17, "SEQ REC")];
    for (c, (i, name)) in top.iter().enumerate() {
        v.push((*name, b(*i), 1514 + c * 62, 410, 56, 44));
    }
    for (c, (i, name)) in bot.iter().enumerate() {
        v.push((*name, b(*i), 1514 + c * 62, 462, 56, 44));
    }
    v
}

/// Keys: (id, x, y, w, h, black). 22 white keys C3..C6, the black ones over their gaps.
pub fn keys() -> Vec<(u8, usize, usize, usize, usize, bool)> {
    let (x0, y0, kw) = (47usize, 584usize, 83usize);
    let mut v = Vec::new();
    let mut white = 0usize;
    for k in 0..NKEY as u8 {
        let black = matches!(k % 12, 1 | 3 | 6 | 8 | 10);
        if black {
            let cx = x0 + white * kw;
            v.push((k, cx - 25, y0, 50, 172, true));
        } else {
            v.push((k, x0 + white * kw + 2, y0, kw - 4, 272, false));
            white += 1;
        }
    }
    v
}

pub fn hit(x: f32, y: f32) -> Option<Ctl> {
    for (i, (cx, cy, r)) in encs().iter().enumerate() {
        if (x - cx).powi(2) + (y - cy).powi(2) <= (r + 8.0).powi(2) {
            return Some(Ctl::Enc(i));
        }
    }
    let inside = |px: usize, py: usize, w: usize, h: usize| x >= px as f32 && x < (px + w) as f32 && y >= py as f32 && y < (py + h) as f32;
    for (p, (px, py, w, h)) in pads().iter().enumerate() {
        if inside(*px, *py, *w, *h) {
            return Some(Ctl::Pad(p));
        }
    }
    for (i, (fx, fy, len)) in faders().iter().enumerate() {
        if inside(fx - 30, fy - 20, 60, len + 40) {
            return Some(Ctl::Fader(i));
        }
    }
    if inside(WHEELS[0].0, WHEELS[0].1, WHEELS[0].2, WHEELS[0].3) {
        return Some(Ctl::Pitch);
    }
    if inside(WHEELS[1].0, WHEELS[1].1, WHEELS[1].2, WHEELS[1].3) {
        return Some(Ctl::Mod);
    }
    for (_, id, px, py, w, h) in buttons() {
        if inside(px, py, w, h) {
            return Some(Ctl::Btn(id));
        }
    }
    let mut ks = keys();
    ks.sort_by_key(|k| !k.5);
    for (id, px, py, w, h, _) in ks {
        if inside(px, py, w, h) {
            return Some(Ctl::Key(id, (y - py as f32) / h as f32));
        }
    }
    None
}

/// A fader's 0..1023 at panel y (the top is 1023).
pub fn fader_value(i: usize, y: f32) -> u16 {
    let (_, fy, len) = faders()[i];
    ((1.0 - (y - fy as f32) / len as f32).clamp(0.0, 1.0) * 1023.0).round() as u16
}

/// The wheels' raw reading at panel y: pitch 445..640 around its rest, mod 629 (down) .. 474 (up)
/// (fm1-to-smk37 docs/01 6.5).
pub fn wheel_value(pitch: bool, y: f32) -> u16 {
    let (_, wy, _, wh) = WHEELS[if pitch { 0 } else { 1 }];
    let t = ((y - wy as f32) / wh as f32).clamp(0.0, 1.0); // 0 top .. 1 bottom
    if pitch {
        (640.0 - t * (640.0 - 445.0)).round() as u16
    } else {
        (474.0 + t * (629.0 - 474.0)).round() as u16
    }
}

/// A filled triangle (4x4 samples a pixel).
fn tri(buf: &mut [u32], p: [(f32, f32); 3], c: u32) {
    let (x0, x1) = (p.iter().map(|q| q.0).fold(f32::MAX, f32::min), p.iter().map(|q| q.0).fold(f32::MIN, f32::max));
    let (y0, y1) = (p.iter().map(|q| q.1).fold(f32::MAX, f32::min), p.iter().map(|q| q.1).fold(f32::MIN, f32::max));
    let edge = |a: (f32, f32), b: (f32, f32), x: f32, y: f32| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
    let area = edge(p[0], p[1], p[2].0, p[2].1);
    for py in y0.floor() as i32..=y1.ceil() as i32 {
        for px in x0.floor() as i32..=x1.ceil() as i32 {
            let mut n = 0;
            for s in 0..16 {
                let (x, y) = (px as f32 + (s % 4) as f32 / 4.0 + 0.125, py as f32 + (s / 4) as f32 / 4.0 + 0.125);
                let (a, b, cc) = (edge(p[1], p[2], x, y), edge(p[2], p[0], x, y), edge(p[0], p[1], x, y));
                if (a * area >= 0.0) && (b * area >= 0.0) && (cc * area >= 0.0) {
                    n += 1;
                }
            }
            put(buf, px, py, c, n as f32 / 16.0);
        }
    }
}

/// A label in one or two lines (split at its space when it is wider than `w`), centred at cx, cy.
fn label(buf: &mut [u32], cx: f32, cy: f32, w: f32, s: &str, color: u32) {
    match s.split_once(' ').filter(|_| (s.len() * 6) as f32 > w) {
        Some((a, b)) => {
            text(buf, cx, cy - 9.0, a, 1, color);
            text(buf, cx, cy + 2.0, b, 1, color);
        }
        None => text(buf, cx, cy - 3.0, s, 1, color),
    }
}

/// The transport symbols printed under the lower row of pads.
fn symbol(buf: &mut [u32], k: usize, cx: f32, cy: f32) {
    let c = INK_DIM;
    match k {
        0 => tri(buf, [(cx - 5.0, cy - 6.0), (cx - 5.0, cy + 6.0), (cx + 6.0, cy)], c),
        1 => {
            rrect(buf, cx - 5.0, cy - 6.0, 3.0, 12.0, 0.5, c, c);
            rrect(buf, cx + 2.0, cy - 6.0, 3.0, 12.0, 0.5, c, c);
        }
        2 => disc(buf, cx, cy, 5.5, |_, _| c),
        3 | 4 => {
            let s = if k == 3 { -1.0 } else { 1.0 };
            for o in [-5.0, 2.0] {
                let x = cx + o * s;
                tri(buf, [(x - 3.0 * s, cy - 6.0), (x - 3.0 * s, cy + 6.0), (x + 4.0 * s, cy)], c);
            }
        }
        5 | 6 => {
            let s = if k == 5 { -1.0 } else { 1.0 };
            tri(buf, [(cx - 6.0 * s, cy - 7.0), (cx - 6.0 * s, cy + 7.0), (cx + 6.0 * s, cy)], c);
            tri(buf, [(cx - 3.5 * s, cy - 3.5), (cx - 3.5 * s, cy + 3.5), (cx + 2.0 * s, cy)], BODY_TOP);
        }
        _ => text(buf, cx, cy - 3.0, "UNDO", 1, c),
    }
}

/// What the keys do with SHIFT, printed over them: MIDI channels, presets, the arpeggiator's
/// rates (the first line), and RESET on the last.
fn key_print(k: usize) -> &'static str {
    const P: [&str; 37] = [
        "CHN 1", "CHN 2", "CHN 3", "CHN 4", "CHN 5", "CHN 6", "CHN 7", "CHN 8", "CHN 9", "CHN 10", "CHN 11", "CHN 12",
        "CHN 13", "CHN 14", "CHN 15", "CHN 16", "PRE 1", "PRE 2", "PRE 3", "PRE 4", "PRE 5", "PRE 6", "PRE 7", "PRE 8",
        "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32", "1/32T", "", "", "", "", "RESET",
    ];
    P[k]
}

/// The parts that never change.
pub fn base() -> Vec<u32> {
    canvas(W);
    let mut b = vec![ROOM; W * H];
    // the body: a lighter edge, the matte top inside it
    rrect(&mut b, 8.0, 8.0, (W - 16) as f32, (H - 16) as f32, 30.0, BODY_EDGE, 0x2a2b2f);
    rrect(&mut b, 11.0, 11.0, (W - 22) as f32, (H - 22) as f32, 28.0, BODY_TOP, BODY_BOT);
    // the screen: a black bezel with a thin rim
    rrect(&mut b, (LCD_X - 16) as f32, (LCD_Y - 16) as f32, 512.0, 512.0, 16.0, 0x34363b, 0x2a2b2f);
    rrect(&mut b, (LCD_X - 14) as f32, (LCD_Y - 14) as f32, 508.0, 508.0, 15.0, 0x060607, 0x09090a);
    // the wheels' slots
    for (x, y, w, h) in WHEELS {
        rrect(&mut b, x as f32 - 8.0, y as f32 - 10.0, w as f32 + 16.0, h as f32 + 20.0, 12.0, 0x08080a, 0x0e0e10);
    }
    text(&mut b, WHEELS[0].0 as f32 + 34.0, 40.0, "PITCH", 2, INK);
    text(&mut b, WHEELS[1].0 as f32 + 34.0, 40.0, "MOD", 2, INK);
    // the logo, and PAD BANK over the left buttons
    text(&mut b, 138.0, 312.0, "M-VAVE", 4, 0xe4e5e8);
    text(&mut b, 138.0, 364.0, "PAD BANK", 1, INK);
    rrect(&mut b, 52.0, 367.0, 46.0, 1.0, 0.5, INK_DIM, INK_DIM);
    rrect(&mut b, 178.0, 367.0, 46.0, 1.0, 0.5, INK_DIM, INK_DIM);
    for (i, (cx, cy, r)) in encs().iter().enumerate() {
        text(&mut b, *cx, cy - r - 22.0, &format!("K{}", i + 1), 1, INK);
    }
    for (k, (x, y, w, h)) in pads().iter().enumerate().skip(8) {
        symbol(&mut b, k - 8, (x + w / 2) as f32, (y + h + 18) as f32);
    }
    for (i, (fx, fy, len)) in faders().iter().enumerate() {
        text(&mut b, *fx as f32, *fy as f32 - 48.0, &format!("FADER{}", i + 1), 1, INK);
        // the slot, with its scale beside it
        rrect(&mut b, *fx as f32 - 4.0, *fy as f32 - 6.0, 8.0, *len as f32 + 12.0, 4.0, 0x050506, 0x0a0a0b);
        for t in 0..=10 {
            let y = *fy as f32 + t as f32 * *len as f32 / 10.0;
            let w = if t % 5 == 0 { 10.0 } else { 6.0 };
            rrect(&mut b, *fx as f32 - 34.0, y - 0.5, w, 1.0, 0.5, INK_DIM, INK_DIM);
        }
    }
    // the keybed's well, the keys' printing over it
    rrect(&mut b, 38.0, 572.0, (W - 76) as f32, 294.0, 8.0, 0x060607, 0x0b0b0c);
    for (id, x, _, w, _, black) in keys() {
        let s = key_print(id as usize);
        if !s.is_empty() {
            text(&mut b, (x + w / 2) as f32, if black { 546.0 } else { 557.0 }, s, 1, INK_DIM);
        }
    }
    // the buttons' shadows
    for (_, _, x, y, w, h) in buttons() {
        rrect(&mut b, x as f32 + 1.0, y as f32 + 3.0, w as f32, h as f32, 7.0, 0x09090a, 0x09090a);
    }
    b
}

pub struct State<'a> {
    pub lcd: &'a [u16],
    pub enc_clicks: &'a [i32; 8],
    pub turning: &'a [u32; 8],
    /// keys 0..36, buttons 37..54
    pub down: &'a [bool; 64],
    /// button LEDs 0..18 (LED k = button k), lit fraction
    pub leds: &'a [f32; 19],
    /// the pads' colours (0xRRGGBB) and how hard each is pressed (0..1023)
    pub pads: &'a [u32; 16],
    pub pad_force: &'a [u16; 16],
    pub faders: &'a [u16; 4],
    pub pitch: u16,
    pub modw: u16,
}

/// A knurled knob: dark grey with fine flutes, a lighter domed cap, a white pointer line.
fn knob(buf: &mut [u32], cx: f32, cy: f32, r: f32, angle: f32, glow: f32) {
    disc(buf, cx + 1.5, cy + 4.0, r + 2.0, |_, _| 0x0c0c0e);
    if glow > 0.0 {
        disc(buf, cx, cy, r + 4.0, |_, _| mix(BODY_TOP, AMBER, glow * 0.7));
    }
    disc(buf, cx, cy, r, |dx, dy| {
        let a = dy.atan2(dx);
        let flute = ((a * 22.0).sin() * 0.5 + 0.5).powf(0.6);
        mix(0x2a2c30, 0x5e6168, flute * 0.55 + (-dy / r) * 0.25 + 0.15)
    });
    disc(buf, cx, cy, r * 0.74, |dx, dy| {
        let l = (-(dx * 0.4 + dy) / (r * 0.9)).clamp(-1.0, 1.0) * 0.5 + 0.5;
        mix(0x45484e, 0x6e727a, l)
    });
    let (s, c) = angle.sin_cos();
    for t in 0..((r * 0.42) as i32) {
        let d = r * 0.28 + t as f32;
        disc(buf, cx + s * d, cy - c * d, 1.4, |_, _| 0xf2f2ee);
    }
}

/// A rolling wheel in its slot, its ribs turned to the reading `t` (0 top .. 1 bottom).
fn wheel(buf: &mut [u32], x: f32, y: f32, w: f32, h: f32, t: f32) {
    let (cy, rr) = (y + h / 2.0, h / 2.0);
    let phase = t * 5.0;
    for py in y as i32..(y + h) as i32 {
        let v = ((py as f32 + 0.5 - cy) / rr).clamp(-0.999, 0.999);
        let th = v.asin();
        let shade = (th.cos() * 0.85 - v * 0.25).clamp(0.0, 1.0);
        let rib = ((th * 18.0 + phase).sin() * 0.5 + 0.5).powf(6.0);
        for px in x as i32..(x + w) as i32 {
            let e = ((px as f32 + 0.5 - x).min(x + w - px as f32 - 0.5) / 5.0).clamp(0.0, 1.0);
            let c = mix(0x0b0b0d, mix(0x34363b, 0x5c5f66, rib), shade * (0.45 + 0.55 * e));
            put(buf, px, py, c, 1.0);
        }
    }
    // the reading, marked on the slot's edge
    let my = y + t.clamp(0.0, 1.0) * h;
    rrect(buf, x - 7.0, my - 1.5, 5.0, 3.0, 1.0, INK, INK);
}

pub fn draw(buf: &mut [u32], base: &[u32], st: &State) {
    canvas(W);
    buf.copy_from_slice(base);
    for y in 0..240 {
        for x in 0..240 {
            let p = st.lcd[y * 240 + x] as u32;
            let c = ((p >> 11) * 255 / 31) << 16 | (((p >> 5) & 63) * 255 / 63) << 8 | (p & 31) * 255 / 31;
            let o = (LCD_Y + 2 * y) * W + LCD_X + 2 * x;
            buf[o] = c;
            buf[o + 1] = c;
            buf[o + W] = c;
            buf[o + W + 1] = c;
        }
    }
    for (i, (cx, cy, r)) in encs().iter().enumerate() {
        let glow = (st.turning[i] as f32 / 10.0).min(1.0);
        knob(buf, *cx, *cy, *r, (st.enc_clicks[i] as f32 * 18.0).to_radians(), glow);
    }
    // pads: dark rubber, their LED glowing out from under the lower edge; a pressed one sinks and
    // lets more light through
    for (p, (x, y, w, h)) in pads().iter().enumerate() {
        let (x, y, w, h) = (*x as f32, *y as f32, *w as f32, *h as f32);
        let c = st.pads[p];
        let force = st.pad_force[p] as f32 / 1023.0;
        let lit = |s: u32| ((((c >> s) & 255) as f32 / 255.0).powf(0.6) * 255.0) as u32;
        let rgb = lit(16) << 16 | lit(8) << 8 | lit(0);
        let on = c != 0;
        let off = if force > 0.0 { 2.0 } else { 0.0 };
        if on {
            // the glow on the body around the pad, strongest under its lower edge
            for py in (y - 8.0) as i32..(y + h + 14.0) as i32 {
                for px in (x - 10.0) as i32..(x + w + 10.0) as i32 {
                    let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                    let dx = (x - fx).max(fx - (x + w)).max(0.0);
                    let dy = (y - fy).max(fy - (y + h)).max(0.0);
                    let d = (dx * dx + dy * dy).sqrt();
                    let below = ((fy - y) / h).clamp(0.0, 1.0);
                    let a = (1.0 - d / 11.0).clamp(0.0, 1.0).powi(2) * (0.25 + 0.6 * below);
                    if d > 0.0 {
                        put(buf, px, py, rgb, a);
                    }
                }
            }
        } else {
            rrect(buf, x + 1.0, y + 3.0, w, h, 7.0, 0x0c0c0e, 0x0c0c0e);
        }
        let face = mix(0x2a2b30, 0x1d1e22, 0.5);
        let bot = if on { mix(0x1d1e22, rgb, 0.22 + 0.25 * force) } else { 0x1b1c20 };
        rrect(buf, x, y + off, w, h, 7.0, mix(0x2c2d32, rgb, if on { 0.05 + 0.15 * force } else { 0.0 }), mix(face, bot, 1.0));
        if on {
            // the lit lower edge
            rrect(buf, x + 3.0, y + h - 7.0 + off, w - 6.0, 5.0, 2.5, mix(rgb, 0xffffff, 0.15), rgb);
        }
    }
    // faders: a black cap on the slot, a white line across it
    for (i, (fx, fy, len)) in faders().iter().enumerate() {
        let t = st.faders[i] as f32 / 1023.0;
        let (fx, cy) = (*fx as f32, *fy as f32 + (1.0 - t) * *len as f32);
        rrect(buf, fx - 21.0, cy - 17.0 + 4.0, 42.0, 34.0, 5.0, 0x060607, 0x060607);
        rrect(buf, fx - 21.0, cy - 17.0, 42.0, 34.0, 5.0, 0x3a3c42, 0x16171a);
        rrect(buf, fx - 19.0, cy - 15.0, 38.0, 8.0, 3.0, 0x4a4d54, 0x33353a);
        rrect(buf, fx - 17.0, cy - 1.0, 34.0, 2.0, 1.0, 0xe6e6e2, 0xe6e6e2);
    }
    for (i, (x, y, w, h)) in WHEELS.iter().enumerate() {
        let t = if i == 0 { (640.0 - st.pitch as f32) / (640.0 - 445.0) } else { (st.modw as f32 - 474.0) / (629.0 - 474.0) };
        wheel(buf, *x as f32, *y as f32, *w as f32, *h as f32, t);
    }
    // buttons: dark caps with their names printed on; a lit LED lights the name and rims the cap
    for (name, id, x, y, w, h) in buttons() {
        let (x, y, w, h) = (x as f32, y as f32, w as f32, h as f32);
        let down = st.down[id as usize];
        // (one latch a frame of 16 carries the other group's bits: a ~6% ghost, kept dark)
        let led = ((st.leds[(id - BTN0) as usize] - 0.15) / 0.35).clamp(0.0, 1.0);
        let lc = if name == "BT" { LED_BT } else { LED };
        let off = if down { 2.0 } else { 0.0 };
        if led > 0.0 {
            rrect(buf, x - 2.0, y - 2.0 + off, w + 4.0, h + 4.0, 9.0, mix(BODY_TOP, lc, led * 0.45), mix(BODY_BOT, lc, led * 0.35));
        }
        let (top, bot) = if down { (0x232428, 0x1c1d20) } else { (CAP_TOP, CAP_BOT) };
        rrect(buf, x, y + off, w, h, 7.0, mix(top, lc, led * 0.12), mix(bot, lc, led * 0.08));
        label(buf, x + w / 2.0, y + h / 2.0 + off, w - 8.0, name, mix(0x9fa2a8, lc, led));
    }
    // keys: light grey whites with a front lip, then the glossy blacks over them; held ones sink
    let mut ks = keys();
    ks.sort_by_key(|k| k.5);
    for (id, x, y, w, h, black) in ks {
        let (x, y, w, h) = (x as f32, y as f32, w as f32, h as f32);
        let down = st.down[id as usize];
        if black {
            rrect(buf, x + 3.0, y, w, h + 8.0, 4.0, 0x030303, 0x050506);
            let (top, bot) = if down { (0x101113, 0x1d1e21) } else { (0x16171a, 0x2e3035) };
            let hh = if down { h - 2.0 } else { h - 16.0 };
            rrect(buf, x, y, w, h, 4.0, top, bot);
            // the top face, a gloss streak, and the front bevel below it
            rrect(buf, x + 4.0, y, w - 8.0, hh, 3.0, 0x0e0f11, if down { 0x15161a } else { 0x1b1c20 });
            if !down {
                rrect(buf, x + 8.0, y + 6.0, 3.0, hh - 14.0, 1.5, 0x34363c, 0x26282c);
            }
        } else {
            let (top, bot) = if down { (0xbcbdc0, 0xcfd0d3) } else { (0xd9dadd, 0xeeeff1) };
            let lip = if down { 6.0 } else { 14.0 };
            rrect(buf, x, y, w, h, 5.0, top, bot);
            rrect(buf, x, y + h - lip, w, lip, 5.0, 0xc3c4c8, 0xa9aaae);
            if down {
                rrect(buf, x, y, w, 10.0, 2.0, 0x8e8f93, 0xbcbdc0);
            }
        }
    }
}
