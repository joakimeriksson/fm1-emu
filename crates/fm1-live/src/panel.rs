//! The FM-1 front panel, drawn after Felucca's controls diagram
//! (reference/Felucca/docs/controls.jpg): MASTER / SELECT / PRESETS /
//! ALGORITHM top left with OCT- / OCT+ under them, the screen in the
//! middle, KNOB1-4 and two rows of six buttons on the right, and the 27
//! pill keys (F3..G5) across the bottom.

pub const W: usize = 1280;
pub const H: usize = 820;
pub const LCD_X: usize = 370;
pub const LCD_Y: usize = 42;

pub const ROOM: u32 = 0x18191b;
pub const AMBER: u32 = 0xf2a33a;
pub const GREEN: u32 = 0x62d36e;


pub struct Theme {
    pub body_top: u32,
    pub body_bot: u32,
    pub tray: u32,
    pub silk: u32,
    pub btn_top: u32,
    pub btn_bot: u32,
    pub btn_ink: u32,
    pub key_top: u32,
    pub key_bot: u32,
    pub key_line: u32,
}

const fn blend_toward_white(color: u32, t: u32) -> u32 {
    let r = ((color >> 16) & 0xFF) + ((255 - ((color >> 16) & 0xFF)) * t / 100);
    let g = ((color >> 8) & 0xFF) + ((255 - ((color >> 8) & 0xFF)) * t / 100);
    let b = (color & 0xFF) + ((255 - (color & 0xFF)) * t / 100);
    (r << 16) | (g << 8) | b
}

pub const THEMES: &[(&str, &str, Theme)] = &[
    ("default", "Grey (Emulator Classic)", Theme {
        body_top: 0x5E6166, body_bot: 0x4A4D52, tray: 0x2B2C2F, silk: 0xC9C6BD,
        btn_top: 0x4B4E54, btn_bot: 0x3B3D42, btn_ink: 0x9C9A94,
        key_top: 0x6A5557, key_bot: 0x4F3F42, key_line: 0x8F7C7E,
    }),
    ("cool-gray", "Cool Gray", Theme {
        body_top: 0xDAD1CC, body_bot: 0xCCC3BC, tray: 0x424649, silk: 0x6A6560,
        btn_top: 0x44484B, btn_bot: 0x363A3D, btn_ink: 0xA8A49E,
        key_top: 0x393D40, key_bot: 0x2C3033,
        key_line: blend_toward_white(0x393D40, 35),
    }),
    ("black", "Black", Theme {
        body_top: 0x3A3C3B, body_bot: 0x303131, tray: 0x181818, silk: 0x8A8A8A,
        btn_top: 0x2E2E2E, btn_bot: 0x222222, btn_ink: 0x787878,
        key_top: 0x1C1C1C, key_bot: 0x141414,
        key_line: blend_toward_white(0x1C1C1C, 35),
    }),
    ("orange", "Orange", Theme {
        body_top: 0xAE6246, body_bot: 0x9A5538, tray: 0x945B4C, silk: 0xE8CFC0,
        btn_top: 0xB06045, btn_bot: 0x985238, btn_ink: 0xE0C8B8,
        key_top: 0x8C5347, key_bot: 0x76443A,
        key_line: blend_toward_white(0x8C5347, 35),
    }),
    ("black-green", "Black & Green", Theme {
        body_top: 0x3A3A3D, body_bot: 0x2E2E30, tray: 0x75C0B7, silk: 0xA0D8D0,
        btn_top: 0x65C6B8, btn_bot: 0x52B0A0, btn_ink: 0x1E3830,
        key_top: 0x65C6B8, key_bot: 0x52B0A0,
        key_line: blend_toward_white(0x65C6B8, 35),
    }),
    ("purple", "Purple", Theme {
        body_top: 0xB58BC9, body_bot: 0xA078B5, tray: 0x7C72B3, silk: 0xE4D6F0,
        btn_top: 0x9070AE, btn_bot: 0x7C5E9A, btn_ink: 0xE0D0EC,
        key_top: 0x746AB1, key_bot: 0x5E5498,
        key_line: blend_toward_white(0x746AB1, 35),
    }),
    ("white-blue", "White & Blue", Theme {
        body_top: 0xEAE7E0, body_bot: 0xDCD8D0, tray: 0x5A7795, silk: 0x5A6878,
        btn_top: 0x516F8F, btn_bot: 0x3E5C7A, btn_ink: 0xD0D8E0,
        key_top: 0x516F8F, key_bot: 0x3E5C7A,
        key_line: blend_toward_white(0x516F8F, 35),
    }),
];

impl Theme {
    pub fn by_name(name: &str) -> &'static Theme {
        THEMES.iter().find(|(n, _, _)| *n == name)
            .map(|(_, _, t)| t)
            .unwrap_or(&THEMES[0].2)
    }
}

/// What a point on the panel is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ctl {
    /// An encoder: index into `ENCS`.
    Enc(usize),
    /// The MASTER pot (SARADC ch 4).
    Pot,
    /// A button: its matrix key id (0..13).
    Btn(u8),
    /// A note key: its matrix key id (14..40).
    Key(u8),
}

/// Encoders: (label, matrix encoder, centre x, centre y, radius).
pub const ENCS: [(&str, u8, f32, f32, f32); 7] = [
    ("SELECT", 0, 250.0, 135.0, 44.0),
    ("PRESETS", 6, 110.0, 295.0, 44.0),
    ("ALGORITHM", 1, 250.0, 295.0, 44.0),
    ("KNOB1", 2, 935.0, 120.0, 36.0),
    ("KNOB2", 3, 1027.0, 120.0, 36.0),
    ("KNOB3", 4, 1119.0, 120.0, 36.0),
    ("KNOB4", 5, 1211.0, 120.0, 36.0),
];
pub const POT: (f32, f32, f32) = (110.0, 135.0, 44.0);

/// Buttons: (label, matrix id, x, y, w, h).
pub fn buttons() -> Vec<(&'static str, u8, usize, usize, usize, usize)> {
    let mut v = vec![("OCT-", 0, 62, 398, 112, 44), ("OCT+", 1, 186, 398, 112, 44)];
    let rows: [[(&str, u8); 6]; 2] = [
        [("FX", 2), ("SCL", 3), ("ENV", 4), ("LFO", 5), ("EDIT", 6), ("GLO", 7)],
        [("HOME", 8), ("SAVE", 9), ("ARP", 10), ("SEQ", 11), ("PLAY", 12), ("REC", 13)],
    ];
    for (r, row) in rows.iter().enumerate() {
        for (i, (name, id)) in row.iter().enumerate() {
            v.push((*name, *id, 906 + i * 58, 232 + r * 76, 50, 64));
        }
    }
    v
}

/// Note keys: (matrix id, x, y, w, h, black).
pub fn keys() -> Vec<(u8, usize, usize, usize, usize, bool)> {
    let (kw, gap, x0) = (62usize, 11usize, 61usize);
    let mut v = Vec::new();
    // F3..G5: the white keys are F G A B C D E F G A B C D E F G
    let whites = [0u8, 2, 4, 6, 7, 9, 11, 12, 14, 16, 18, 19, 21, 23, 24, 26];
    for (i, n) in whites.iter().enumerate() {
        v.push((14 + n, x0 + i * (kw + gap), 680, kw, 106, false));
    }
    // a black key sits over the gap after white key i
    let blacks = [(1u8, 0usize), (3, 1), (5, 2), (8, 4), (10, 5), (13, 7), (15, 8), (17, 9), (20, 11), (22, 12), (25, 14)];
    for (n, after) in blacks {
        let cx = x0 + (after + 1) * (kw + gap) - gap / 2;
        v.push((14 + n, cx - kw / 2, 574, kw, 94, true));
    }
    v
}

/// The velocity of a click on key `id` at height `y`: 1 at its top, 127 at its bottom.
pub fn key_vel(id: u8, y: f32) -> u8 {
    keys().iter().find(|k| k.0 == id).map_or(100, |&(_, _, py, _, h, _)| {
        (1.0 + 126.0 * ((y - py as f32) / h as f32).clamp(0.0, 1.0)).round() as u8
    })
}

pub fn hit(x: f32, y: f32) -> Option<Ctl> {
    for (i, (_, _, cx, cy, r)) in ENCS.iter().enumerate() {
        if (x - cx).powi(2) + (y - cy).powi(2) <= (r + 6.0).powi(2) {
            return Some(Ctl::Enc(i));
        }
    }
    if (x - POT.0).powi(2) + (y - POT.1).powi(2) <= (POT.2 + 6.0).powi(2) {
        return Some(Ctl::Pot);
    }
    let inside = |px: usize, py: usize, w: usize, h: usize| x >= px as f32 && x < (px + w) as f32 && y >= py as f32 && y < (py + h) as f32;
    for (_, id, px, py, w, h) in buttons() {
        if inside(px, py, w, h) {
            return Some(Ctl::Btn(id));
        }
    }
    // black keys first: they sit above the white row
    let mut ks = keys();
    ks.sort_by_key(|k| !k.5);
    for (id, px, py, w, h, _) in ks {
        if inside(px, py, w, h) {
            return Some(Ctl::Key(id));
        }
    }
    None
}

// ---------------------------------------------------------------- drawing --

pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |s: u32| -> u32 {
        let x = ((a >> s) & 255) as f32;
        let y = ((b >> s) & 255) as f32;
        ((x + (y - x) * t).round() as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}

thread_local! {
    /// The width of the canvas being drawn (each panel's `base` / `draw` sets its own).
    static CANVAS_W: std::cell::Cell<usize> = const { std::cell::Cell::new(W) };
}

/// Draw on canvases `w` pixels wide from now on (the SMK-37 panel's is wider).
pub fn canvas(w: usize) {
    CANVAS_W.with(|c| c.set(w));
}

pub fn put(buf: &mut [u32], x: i32, y: i32, c: u32, a: f32) {
    let w = CANVAS_W.with(|c| c.get());
    if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < buf.len() / w && a > 0.0 {
        let p = &mut buf[y as usize * w + x as usize];
        *p = mix(*p, c, a);
    }
}

/// Rounded rectangle with a vertical gradient, anti-aliased edges.
pub fn rrect(buf: &mut [u32], x: f32, y: f32, w: f32, h: f32, r: f32, top: u32, bot: u32) {
    for py in (y.floor() as i32 - 1)..=((y + h).ceil() as i32) {
        for px in (x.floor() as i32 - 1)..=((x + w).ceil() as i32) {
            let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
            let qx = (fx - (x + w / 2.0)).abs() - (w / 2.0 - r);
            let qy = (fy - (y + h / 2.0)).abs() - (h / 2.0 - r);
            let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
            let a = (0.5 - d).clamp(0.0, 1.0);
            if a > 0.0 {
                put(buf, px, py, mix(top, bot, (fy - y) / h), a);
            }
        }
    }
}

pub fn disc(buf: &mut [u32], cx: f32, cy: f32, r: f32, f: impl Fn(f32, f32) -> u32) {
    for py in (cy - r - 1.0) as i32..=(cy + r + 1.0) as i32 {
        for px in (cx - r - 1.0) as i32..=(cx + r + 1.0) as i32 {
            let (dx, dy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
            let d = (dx * dx + dy * dy).sqrt();
            let a = (r - d + 0.5).clamp(0.0, 1.0);
            if a > 0.0 {
                put(buf, px, py, f(dx, dy), a);
            }
        }
    }
}

/// 5x7 glyphs, one byte per column, bit 0 at the top.
fn glyph(c: char) -> [u8; 5] {
    match c {
        '0' => [0x3E, 0x51, 0x49, 0x45, 0x3E], '1' => [0x00, 0x42, 0x7F, 0x40, 0x00],
        '2' => [0x42, 0x61, 0x51, 0x49, 0x46], '3' => [0x21, 0x41, 0x45, 0x4B, 0x31],
        '4' => [0x18, 0x14, 0x12, 0x7F, 0x10], '5' => [0x27, 0x45, 0x45, 0x45, 0x39],
        '6' => [0x3C, 0x4A, 0x49, 0x49, 0x30], '7' => [0x01, 0x71, 0x09, 0x05, 0x03],
        '8' => [0x36, 0x49, 0x49, 0x49, 0x36], '9' => [0x06, 0x49, 0x49, 0x29, 0x1E],
        'A' => [0x7E, 0x11, 0x11, 0x11, 0x7E], 'B' => [0x7F, 0x49, 0x49, 0x49, 0x36],
        'C' => [0x3E, 0x41, 0x41, 0x41, 0x22], 'D' => [0x7F, 0x41, 0x41, 0x22, 0x1C],
        'E' => [0x7F, 0x49, 0x49, 0x49, 0x41], 'F' => [0x7F, 0x09, 0x09, 0x09, 0x01],
        'G' => [0x3E, 0x41, 0x49, 0x49, 0x7A], 'H' => [0x7F, 0x08, 0x08, 0x08, 0x7F],
        'I' => [0x00, 0x41, 0x7F, 0x41, 0x00], 'J' => [0x20, 0x40, 0x41, 0x3F, 0x01],
        'K' => [0x7F, 0x08, 0x14, 0x22, 0x41], 'L' => [0x7F, 0x40, 0x40, 0x40, 0x40],
        'M' => [0x7F, 0x02, 0x0C, 0x02, 0x7F], 'N' => [0x7F, 0x04, 0x08, 0x10, 0x7F],
        'O' => [0x3E, 0x41, 0x41, 0x41, 0x3E], 'P' => [0x7F, 0x09, 0x09, 0x09, 0x06],
        'Q' => [0x3E, 0x41, 0x51, 0x21, 0x5E], 'R' => [0x7F, 0x09, 0x19, 0x29, 0x46],
        'S' => [0x46, 0x49, 0x49, 0x49, 0x31], 'T' => [0x01, 0x01, 0x7F, 0x01, 0x01],
        'U' => [0x3F, 0x40, 0x40, 0x40, 0x3F], 'V' => [0x1F, 0x20, 0x40, 0x20, 0x1F],
        'W' => [0x3F, 0x40, 0x38, 0x40, 0x3F], 'X' => [0x63, 0x14, 0x08, 0x14, 0x63],
        'Y' => [0x07, 0x08, 0x70, 0x08, 0x07], 'Z' => [0x61, 0x51, 0x49, 0x45, 0x43],
        '+' => [0x08, 0x08, 0x3E, 0x08, 0x08], '-' => [0x08, 0x08, 0x08, 0x08, 0x08],
        '/' => [0x20, 0x10, 0x08, 0x04, 0x02], '.' => [0x00, 0x60, 0x60, 0x00, 0x00],
        '<' => [0x08, 0x14, 0x22, 0x41, 0x00], '>' => [0x00, 0x41, 0x22, 0x14, 0x08],
        _ => [0; 5],
    }
}

/// Text centred on cx, `scale` pixels per font dot.
pub fn text(buf: &mut [u32], cx: f32, y: f32, s: &str, scale: usize, color: u32) {
    let adv = 6 * scale;
    let x0 = cx as i32 - (s.len() * adv) as i32 / 2 + scale as i32 / 2;
    for (i, c) in s.chars().enumerate() {
        for (gx, col) in glyph(c.to_ascii_uppercase()).iter().enumerate() {
            for gy in 0..7 {
                if col >> gy & 1 != 0 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let px = x0 + (i * adv + gx * scale + sx) as i32;
                            let py = y as i32 + (gy * scale + sy) as i32;
                            put(buf, px, py, color, 1.0);
                        }
                    }
                }
            }
        }
    }
}

/// The parts that never change: body, trays, bezel, labels.
pub fn base(theme: &Theme) -> Vec<u32> {
    canvas(W);
    let mut b = vec![ROOM; W * H];
    rrect(&mut b, 14.0, 14.0, (W - 28) as f32, (H - 28) as f32, 30.0, theme.body_top, theme.body_bot);
    // screen bezel
    rrect(&mut b, 352.0, 24.0, 516.0, 516.0, 22.0, 0x0e0e10, 0x0b0b0c);
    // OCT tray, button tray, keyboard tray
    rrect(&mut b, 50.0, 388.0, 260.0, 64.0, 14.0, theme.tray, theme.tray);
    rrect(&mut b, 896.0, 222.0, 360.0, 160.0, 16.0, theme.tray, theme.tray);
    rrect(&mut b, 34.0, 562.0, 1212.0, 236.0, 26.0, 0x2f2f33, 0x27272a);
    // knob labels
    text(&mut b, POT.0, POT.1 - POT.2 - 24.0, "MASTER", 2, theme.silk);
    for (name, _, cx, cy, r) in ENCS.iter() {
        text(&mut b, *cx, cy - r - 24.0, name, 2, theme.silk);
    }
    // PLAY / REC outlined, as on the panel
    rrect(&mut b, 1134.0, 302.0, 112.0, 72.0, 10.0, 0x3c6b43, 0x3c6b43);
    rrect(&mut b, 1136.0, 304.0, 108.0, 68.0, 9.0, theme.tray, theme.tray);
    text(&mut b, 640.0, 552.0 - 2.0, "FM-1  FELUCCA", 1, theme.silk);
    b
}

pub fn knob(buf: &mut [u32], cx: f32, cy: f32, r: f32, angle: f32, glow: f32) {
    // shadow, knurled skirt, cap, pointer
    disc(buf, cx + 2.0, cy + 4.0, r + 2.0, |_, _| 0x26272a);
    if glow > 0.0 {
        disc(buf, cx, cy, r + 4.0, |_, _| mix(0x2a2b2e, AMBER, glow * 0.8));
    }
    disc(buf, cx, cy, r, |dx, dy| {
        let a = dy.atan2(dx);
        let ridge = ((a * 18.0).sin() * 0.5 + 0.5) * 0.25;
        mix(0x2e3034, 0x50535a, ridge + (-dy / r) * 0.2 + 0.2)
    });
    disc(buf, cx, cy, r * 0.72, |dx, dy| {
        let l = (-(dx + dy) / (r * 1.2)).clamp(-1.0, 1.0) * 0.5 + 0.5;
        mix(0x3a3d43, 0x6d7178, l)
    });
    let (s, c) = angle.sin_cos();
    for t in 0..((r * 0.62) as i32) {
        let d = r * 0.12 + t as f32;
        let (px, py) = (cx + s * d, cy - c * d);
        disc(buf, px, py, 1.6, |_, _| 0xeeeae2);
    }
}

pub struct State<'a> {
    pub lcd: &'a [u16],
    pub enc_clicks: &'a [i32; 7],
    pub pot: u16,
    pub down: &'a [bool; 41],
    pub leds: &'a [f32; 41],
    pub turning: &'a [u32; 8],
}

/// LED fraction -> brightness: a fully lit LED is on ~1 tick in 11.
fn bright(f: f32) -> f32 {
    (f / 0.08).clamp(0.0, 1.0)
}

pub fn draw(buf: &mut [u32], base: &[u32], st: &State, theme: &Theme) {
    canvas(W);
    buf.copy_from_slice(base);
    // the LCD at 2x
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
    // knobs: encoders turn 18 degrees a click; the pot sweeps 270 degrees
    for (i, (_, _, cx, cy, r)) in ENCS.iter().enumerate() {
        let glow = (st.turning[i] as f32 / 10.0).min(1.0);
        knob(buf, *cx, *cy, *r, (st.enc_clicks[i] as f32 * 18.0).to_radians(), glow);
    }
    let pa = (-135.0 + 270.0 * st.pot as f32 / 1023.0).to_radians();
    knob(buf, POT.0, POT.1, POT.2, pa, (st.turning[7] as f32 / 10.0).min(1.0));
    // buttons: pressed sinks, LED tints the cap
    for (name, id, x, y, w, h) in buttons() {
        let down = st.down[id as usize];
        let led = bright(st.leds[id as usize]);
        let tint = if id == 12 { GREEN } else { AMBER };
        let off = if down { 2.0 } else { 0.0 };
        rrect(buf, x as f32 + 1.0, y as f32 + 3.0, w as f32, h as f32, 9.0, 0x1c1d20, 0x1c1d20);
        let (top, bot) = (mix(theme.btn_top, tint, led * 0.85), mix(theme.btn_bot, tint, led * 0.7));
        rrect(buf, x as f32, y as f32 + off, w as f32, h as f32, 9.0, top, bot);
        let ink = if led > 0.5 { 0x2a1a03 } else { theme.btn_ink };
        text(buf, (x + w / 2) as f32, (y + h / 2) as f32 - 7.0 + off, name, 2, ink);
    }
    // note keys: pills with a ridge; pressed and lit keys glow
    for (id, x, y, w, h, _) in keys() {
        let down = st.down[id as usize];
        let led = bright(st.leds[id as usize]);
        let off = if down { 3.0 } else { 0.0 };
        rrect(buf, x as f32 + 1.0, y as f32 + 4.0, w as f32, h as f32, w as f32 / 2.0, 0x1a1a1c, 0x1a1a1c);
        let heat = if down { 0.55 } else { led * 0.6 };
        rrect(buf, x as f32, y as f32 + off, w as f32, h as f32, w as f32 / 2.0, mix(theme.key_top, AMBER, heat), mix(theme.key_bot, AMBER, heat * 0.8));
        let line = mix(theme.key_line, 0xffe2b0, if down { 1.0 } else { led });
        rrect(buf, (x + w / 2) as f32 - 2.5, y as f32 + 18.0 + off, 5.0, h as f32 - 36.0, 2.5, line, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_struct_has_all_color_fields() {
        let t = Theme::by_name("default");
        assert_eq!(t.body_top, 0x5e6166);
        assert_eq!(t.body_bot, 0x4a4d52);
        assert_eq!(t.tray, 0x2b2c2f);
        assert_eq!(t.silk, 0xc9c6bd);
        assert_eq!(t.btn_top, 0x4b4e54);
        assert_eq!(t.btn_bot, 0x3b3d42);
        assert_eq!(t.btn_ink, 0x9c9a94);
        assert_eq!(t.key_top, 0x6a5557);
        assert_eq!(t.key_bot, 0x4f3f42);
    }

    #[test]
    fn themes_array_has_seven_entries() {
        assert_eq!(THEMES.len(), 7);
    }

    #[test]
    fn themes_have_correct_names() {
        let names: Vec<&str> = THEMES.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(names, &["default", "cool-gray", "black", "orange", "black-green", "purple", "white-blue"]);
    }

    #[test]
    fn by_name_returns_matching_theme() {
        let t = Theme::by_name("cool-gray");
        assert_eq!(t.body_top, 0xDAD1CC);
    }

    #[test]
    fn by_name_falls_back_to_default() {
        let t = Theme::by_name("nonexistent");
        assert_eq!(t.body_top, 0x5e6166);
    }

    #[test]
    fn key_line_is_derived_lighter_than_key_top() {
        for (_, _, theme) in THEMES.iter() {
            let r = (theme.key_line >> 16) & 0xFF;
            let r_top = (theme.key_top >> 16) & 0xFF;
            assert!(r >= r_top, "key_line R channel should be >= key_top R channel");
        }
    }

    #[test]
    fn base_accepts_theme_and_produces_correct_size() {
        let t = Theme::by_name("default");
        let b = base(t);
        assert_eq!(b.len(), W * H);
    }

    #[test]
    fn base_uses_theme_body_color() {
        let t = Theme::by_name("cool-gray");
        let b = base(t);
        // (100, 200) is on the body, clear of bezels and trays
        let px = b[200 * W + 100];
        let r = (px >> 16) & 0xFF;
        // cool-gray body_top is 0xDAD1CC (R=0xDA=218), body_bot is 0xCCC3BC (R=0xCC=204)
        assert!(r >= 190 && r <= 230, "body pixel R={r:#x} should be in cool-gray range");
    }

    #[test]
    fn draw_accepts_theme() {
        let t = Theme::by_name("default");
        let b = base(t);
        let mut buf = vec![0u32; W * H];
        let lcd = vec![0u16; 240 * 240];
        let st = State {
            lcd: &lcd,
            enc_clicks: &[0; 7],
            pot: 512,
            down: &[false; 41],
            leds: &[0.0; 41],
            turning: &[0; 8],
        };
        draw(&mut buf, &b, &st, t);
        assert_eq!(buf.len(), W * H);
    }

    #[test]
    fn draw_uses_theme_button_colors() {
        let cool = Theme::by_name("cool-gray");
        let black = Theme::by_name("black");
        let b_cool = base(cool);
        let b_black = base(black);
        let mut buf_cool = vec![0u32; W * H];
        let mut buf_black = vec![0u32; W * H];
        let lcd = vec![0u16; 240 * 240];
        let st = State {
            lcd: &lcd,
            enc_clicks: &[0; 7],
            pot: 512,
            down: &[false; 41],
            leds: &[0.0; 41],
            turning: &[0; 8],
        };
        draw(&mut buf_cool, &b_cool, &st, cool);
        draw(&mut buf_black, &b_black, &st, black);
        // the center of the first button (FX at ~931, 264) should differ between themes
        let px_cool = buf_cool[264 * W + 931];
        let px_black = buf_black[264 * W + 931];
        assert_ne!(px_cool, px_black, "different themes should produce different button pixels");
    }

    #[test]
    fn default_theme_matches_original_constants() {
        let t = Theme::by_name("default");
        assert_eq!(t.body_top, 0x5e6166);
        assert_eq!(t.body_bot, 0x4a4d52);
        assert_eq!(t.tray, 0x2b2c2f);
        assert_eq!(t.silk, 0xc9c6bd);
        assert_eq!(t.btn_top, 0x4b4e54);
        assert_eq!(t.btn_bot, 0x3b3d42);
        assert_eq!(t.btn_ink, 0x9c9a94);
        assert_eq!(t.key_top, 0x6a5557);
        assert_eq!(t.key_bot, 0x4f3f42);
        assert_eq!(t.key_line, 0x8f7c7e);
    }
}
