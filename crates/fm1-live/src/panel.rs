//! The FM-1 front panel, shared by the page (fm1-web), fm1-live and the plugin.
//!
//! The controls sit as on the real unit: MASTER / SELECT / PRESETS / ALGORITHM top left with
//! OCT- / OCT+ under them, the screen in the middle, KNOB1-4 and two rows of six buttons on the
//! right, and the 27 pill keys (F3..G5) across the bottom. A [`Look`] picks how it is drawn:
//!
//! - [`Layout::Large`] (the default): that arrangement with a larger screen (the LCD at 2x),
//!   the screen centred between the top of the body and the keyboard.
//! - [`Layout::Real`]: the proportions of the real FM-1, measured from a photo (the LCD at 1x).
//! - [`Layout::Display`]: the LCD alone, at a whole multiple of its 240x240 pixels.
//!
//! The geometry is in "units" (the body is 1000 wide); [`Look::scale`] sets the pixels per unit
//! (1.0 draws the large layout's LCD at exactly 2x, 2.0 at 4x for a high-density screen). The
//! labels are Inter Tight (SIL OFL 1.1, assets/InterTight-OFL.txt), rasterized by ab_glyph.
//! Everything that never changes is drawn once in [`base`]; [`draw`] copies it and adds the LCD,
//! the knobs' pointers and whatever is pressed or lit.

use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont, VariableFont};

/// The page around the panel (the page's own background, so the two meet without a seam).
pub const ROOM: u32 = 0x111214;
pub const AMBER: u32 = 0xf2a33a;
/// A lit LED: the FM-1's are white.
pub const LIT: u32 = 0xffffff;

pub struct Theme {
    pub body_top: u32,
    pub body_bot: u32,
    pub tray: u32,
    pub silk: u32,
    #[allow(dead_code)]
    pub brand: u32,
    pub btn_top: u32,
    pub btn_bot: u32,
    pub btn_ink: u32,
    pub key_top: u32,
    pub key_bot: u32,
    /// the light bar in a key, unlit
    pub key_line: u32,
}

const fn blend_toward_white(color: u32, t: u32) -> u32 {
    let r = ((color >> 16) & 0xFF) + ((255 - ((color >> 16) & 0xFF)) * t / 100);
    let g = ((color >> 8) & 0xFF) + ((255 - ((color >> 8) & 0xFF)) * t / 100);
    let b = (color & 0xFF) + ((255 - (color & 0xFF)) * t / 100);
    (r << 16) | (g << 8) | b
}

/// (name, label, theme). The first is the default (and what an unknown name gets).
pub const THEMES: &[(&str, &str, Theme)] = &[
    ("graphite", "Graphite (FM-1 Black)", Theme {
        body_top: 0x333436, body_bot: 0x252628, tray: 0x1c1d1f, silk: 0xd6d6d3,
        brand: 0x8a8a87,
        btn_top: 0x1f2021, btn_bot: 0x151617, btn_ink: 0xd6d6d3,
        key_top: 0x232426, key_bot: 0x18191a, key_line: 0xe8e8e4,
    }),
    ("classic", "Grey (Emulator Classic)", Theme {
        body_top: 0x5E6166, body_bot: 0x4A4D52, tray: 0x2B2C2F, silk: 0xC9C6BD,
        brand: 0x8c8a85,
        btn_top: 0x4B4E54, btn_bot: 0x3B3D42, btn_ink: 0x9C9A94,
        key_top: 0x6A5557, key_bot: 0x4F3F42, key_line: 0x8F7C7E,
    }),
    ("cool-gray", "Cool Gray", Theme {
        body_top: 0xDAD1CC, body_bot: 0xCCC3BC, tray: 0x424649, silk: 0x6A6560,
        brand: 0x6A6560,
        btn_top: 0x44484B, btn_bot: 0x363A3D, btn_ink: 0xA8A49E,
        key_top: 0x393D40, key_bot: 0x2C3033,
        key_line: blend_toward_white(0x393D40, 35),
    }),
    ("black", "Black", Theme {
        body_top: 0x3A3C3B, body_bot: 0x303131, tray: 0x181818, silk: 0x8A8A8A,
        brand: 0x8A8A8A,
        btn_top: 0x2E2E2E, btn_bot: 0x222222, btn_ink: 0x787878,
        key_top: 0x1C1C1C, key_bot: 0x141414,
        key_line: blend_toward_white(0x1C1C1C, 35),
    }),
    ("orange", "Orange", Theme {
        body_top: 0xAE6246, body_bot: 0x9A5538, tray: 0x945B4C, silk: 0xE8CFC0,
        brand: 0xE8CFC0,
        btn_top: 0xB06045, btn_bot: 0x985238, btn_ink: 0xE0C8B8,
        key_top: 0x8C5347, key_bot: 0x76443A,
        key_line: blend_toward_white(0x8C5347, 35),
    }),
    ("black-green", "Black & Green", Theme {
        body_top: 0x3A3A3D, body_bot: 0x2E2E30, tray: 0x75C0B7, silk: 0xA0D8D0,
        brand: 0xA0D8D0,
        btn_top: 0x65C6B8, btn_bot: 0x52B0A0, btn_ink: 0x1E3830,
        key_top: 0x65C6B8, key_bot: 0x52B0A0,
        key_line: blend_toward_white(0x65C6B8, 35),
    }),
    ("purple", "Purple", Theme {
        body_top: 0xB58BC9, body_bot: 0xA078B5, tray: 0x7C72B3, silk: 0xE4D6F0,
        brand: 0xE4D6F0,
        btn_top: 0x9070AE, btn_bot: 0x7C5E9A, btn_ink: 0xE0D0EC,
        key_top: 0x746AB1, key_bot: 0x5E5498,
        key_line: blend_toward_white(0x746AB1, 35),
    }),
    ("white-blue", "White & Blue", Theme {
        body_top: 0xEAE7E0, body_bot: 0xDCD8D0, tray: 0x5A7795, silk: 0x5A6878,
        brand: 0x5A6878,
        btn_top: 0x516F8F, btn_bot: 0x3E5C7A, btn_ink: 0xD0D8E0,
        key_top: 0x516F8F, key_bot: 0x3E5C7A,
        key_line: blend_toward_white(0x516F8F, 35),
    }),
];

impl Theme {
    pub fn try_by_name(name: &str) -> Option<&'static Theme> {
        // "default": the first, as the name was before the graphite theme
        let name = if name == "default" { THEMES[0].0 } else { name };
        THEMES.iter().find(|(n, _, _)| *n == name).map(|(_, _, t)| t)
    }

    pub fn by_name(name: &str) -> &'static Theme {
        Self::try_by_name(name).unwrap_or(&THEMES[0].2)
    }
}

// ---------------------------------------------------------------- the look --

/// How the panel is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The real arrangement with a larger screen (the default).
    Large,
    /// The real FM-1's proportions.
    Real,
    /// The LCD alone.
    Display,
}

impl Layout {
    pub const ALL: [Layout; 3] = [Layout::Large, Layout::Real, Layout::Display];

    pub fn name(self) -> &'static str {
        match self {
            Layout::Large => "large",
            Layout::Real => "real",
            Layout::Display => "display",
        }
    }

    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            Layout::Large => "Large screen",
            Layout::Real => "True to life",
            Layout::Display => "Display only",
        }
    }

    pub fn from_name(s: &str) -> Option<Layout> {
        Layout::ALL.into_iter().find(|l| l.name() == s)
    }

    /// The next one (a key that cycles them).
    pub fn next(self) -> Layout {
        match self {
            Layout::Large => Layout::Real,
            Layout::Real => Layout::Display,
            Layout::Display => Layout::Large,
        }
    }
}

/// The layout, whether the top keys carry the stock firmware's labels, and the resolution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub layout: Layout,
    /// OP1..OP6, PIT, GLO, MONO, POLY under the top keys, as printed on the real unit
    pub labels: bool,
    /// Pixels per unit / 1.25 (1.0: the large layout's LCD at 2x; 2.0 for a high-density
    /// screen). In the display layout: the LCD's whole multiple.
    pub scale: f32,
}

impl Default for Look {
    fn default() -> Self {
        Look { layout: Layout::Large, labels: false, scale: 1.0 }
    }
}

/// Room around the body for its shadow (units).
const MARGIN: f32 = 20.0;

impl Look {
    /// Pixels per unit.
    fn k(&self) -> f32 {
        1.25 * self.scale.max(0.25)
    }

    /// The display layout's multiple of the LCD.
    fn lcd_mult(&self) -> usize {
        (self.scale.round() as usize).max(1)
    }

    fn geo(&self) -> &'static Geo {
        if self.layout == Layout::Real { &REAL } else { &LARGE }
    }
}

/// The canvas the look draws, in pixels.
pub fn size(look: &Look) -> (usize, usize) {
    if look.layout == Layout::Display {
        let s = 240 * look.lcd_mult();
        return (s, s);
    }
    let (g, k) = (look.geo(), look.k());
    (((g.w + 2.0 * MARGIN) * k).round() as usize, ((g.h + 2.0 * MARGIN) * k).round() as usize)
}

/// The geometry of a layout, in units from the body's top left. Rectangles are
/// (x, y, w, h, corner radius); points are centres.
struct Geo {
    w: f32,
    h: f32,
    radius: f32,
    /// MASTER, SELECT, PRESETS, ALGORITHM, KNOB1..4
    knobs: [(f32, f32); 8],
    knob_r: f32,
    /// the knobs' labels: size, and how far above the knob their centre line is
    label: f32,
    label_gap: f32,
    oct_tray: [f32; 5],
    oct: [(f32, f32); 2],
    oct_wh: (f32, f32),
    btn_tray: [f32; 5],
    /// the first button's centre, the steps across and down, a button's size
    btn0: (f32, f32),
    btn_step: (f32, f32),
    btn_wh: (f32, f32),
    btn_font: f32,
    /// x, y, outer size, rim, radius: the LCD is the square inside the rim
    bezel: [f32; 5],
    key_tray: [f32; 5],
    /// the first bottom key's centre x and the step between bottom keys
    key_x0: f32,
    key_step: f32,
    key_w: f32,
    top_y: f32,
    top_h: f32,
    bot_y: f32,
    bot_h: f32,
}

/// The large screen: a 24-unit margin all round, the screen square and centred between the top
/// of the body and the keyboard tray. As on the real unit, PRESETS / ALGORITHM line up with the
/// first row of buttons and OCT with the second.
const LARGE: Geo = Geo {
    w: 1000.0,
    h: 720.0,
    radius: 34.0,
    knobs: [(68.0, 74.0), (160.0, 74.0), (68.0, 302.0), (160.0, 302.0), (689.0, 74.0), (771.0, 74.0), (853.0, 74.0), (935.0, 74.0)],
    knob_r: 28.0,
    label: 12.5,
    label_gap: 15.0,
    oct_tray: [24.0, 350.0, 180.0, 56.0, 14.0],
    oct: [(71.0, 378.0), (157.0, 378.0)],
    oct_wh: (78.0, 40.0),
    btn_tray: [648.0, 256.0, 328.0, 168.0, 16.0],
    btn0: (682.0, 302.0),
    btn_step: (52.0, 76.0),
    btn_wh: (44.0, 64.0),
    btn_font: 11.0,
    bezel: [224.0, 24.0, 400.0, 8.0, 16.0],
    key_tray: [24.0, 448.0, 952.0, 248.0, 24.0],
    key_x0: 76.25,
    key_step: 56.5,
    key_w: 44.0,
    top_y: 468.0,
    top_h: 98.0,
    bot_y: 578.0,
    bot_h: 98.0,
};

/// The real FM-1, measured from a photo from above (its screen a little larger than measured,
/// so the LCD is drawn at a whole 1x).
const REAL: Geo = Geo {
    w: 1000.0,
    h: 593.0,
    radius: 34.0,
    knobs: [(81.0, 93.0), (187.0, 93.0), (81.0, 197.0), (187.0, 197.0), (564.0, 93.0), (678.0, 93.0), (792.0, 93.0), (906.0, 93.0)],
    knob_r: 22.0,
    label: 11.0,
    label_gap: 13.0,
    oct_tray: [54.0, 257.0, 158.0, 46.0, 12.0],
    oct: [(98.0, 280.0), (168.0, 280.0)],
    oct_wh: (62.0, 32.0),
    btn_tray: [538.0, 166.0, 402.0, 130.0, 16.0],
    btn0: (580.0, 201.0),
    btn_step: (63.5, 61.0),
    btn_wh: (50.0, 46.0),
    btn_font: 10.0,
    bezel: [262.0, 40.0, 236.0, 22.0, 20.0],
    key_tray: [35.0, 336.0, 930.0, 240.0, 26.0],
    key_x0: 72.0,
    key_step: 57.0,
    key_w: 46.0,
    top_y: 354.0,
    top_h: 96.0,
    bot_y: 462.0,
    bot_h: 96.0,
};

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

/// Encoders: (label, matrix encoder). Encoder i sits at the layout's knob i + 1 (knob 0 is MASTER).
pub const ENCS: [(&str, u8); 7] = [
    ("SELECT", 0),
    ("PRESETS", 6),
    ("ALGORITHM", 1),
    ("KNOB1", 2),
    ("KNOB2", 3),
    ("KNOB3", 4),
    ("KNOB4", 5),
];

/// The top keys' labels on the real unit, left to right (the last has none).
const TOP_LABELS: [&str; 11] = ["OP1", "OP2", "OP3", "OP4", "OP5", "OP6", "PIT", "GLO", "MONO", "POLY", ""];

/// Buttons: (label, matrix id, x, y, w, h) in units.
fn buttons(g: &Geo) -> Vec<(&'static str, u8, f32, f32, f32, f32)> {
    let (bw, bh) = g.oct_wh;
    let mut v = vec![
        ("OCT-", 0, g.oct[0].0 - bw / 2.0, g.oct[0].1 - bh / 2.0, bw, bh),
        ("OCT+", 1, g.oct[1].0 - bw / 2.0, g.oct[1].1 - bh / 2.0, bw, bh),
    ];
    let rows: [[(&str, u8); 6]; 2] = [
        [("FX", 2), ("SCL", 3), ("ENV", 4), ("LFO", 5), ("EDIT", 6), ("GLO", 7)],
        [("HOME", 8), ("SAVE", 9), ("ARP", 10), ("SEQ", 11), ("PLAY\nSTOP", 12), ("REC", 13)],
    ];
    let (w, h) = g.btn_wh;
    for (r, row) in rows.iter().enumerate() {
        for (i, (name, id)) in row.iter().enumerate() {
            let (cx, cy) = (g.btn0.0 + i as f32 * g.btn_step.0, g.btn0.1 + r as f32 * g.btn_step.1);
            v.push((*name, *id, cx - w / 2.0, cy - h / 2.0, w, h));
        }
    }
    v
}

/// Note keys: (matrix id, x, y, w, h, top row) in units, the top row's left to right last.
fn keys(g: &Geo) -> Vec<(u8, f32, f32, f32, f32, bool)> {
    let mut v = Vec::new();
    // F3..G5: the bottom row is F G A B C D E F G A B C D E F G
    let whites = [0u8, 2, 4, 6, 7, 9, 11, 12, 14, 16, 18, 19, 21, 23, 24, 26];
    for (i, n) in whites.iter().enumerate() {
        let cx = g.key_x0 + i as f32 * g.key_step;
        v.push((14 + n, cx - g.key_w / 2.0, g.bot_y, g.key_w, g.bot_h, false));
    }
    // a top key sits over the gap after bottom key i, as a piano's black keys
    let blacks = [(1u8, 0usize), (3, 1), (5, 2), (8, 4), (10, 5), (13, 7), (15, 8), (17, 9), (20, 11), (22, 12), (25, 14)];
    for (n, after) in blacks {
        let cx = g.key_x0 + (after as f32 + 0.5) * g.key_step;
        v.push((14 + n, cx - g.key_w / 2.0, g.top_y, g.key_w, g.top_h, true));
    }
    v
}

/// A point on the canvas (pixels) in units, from the body's top left.
fn to_units(look: &Look, x: f32, y: f32) -> (f32, f32) {
    let k = look.k();
    (x / k - MARGIN, y / k - MARGIN)
}

/// The velocity of a click on key `id` at height `y` (pixels): 1 at its top, 127 at its bottom.
pub fn key_vel(look: &Look, id: u8, y: f32) -> u8 {
    let (_, uy) = to_units(look, 0.0, y);
    keys(look.geo()).iter().find(|k| k.0 == id).map_or(100, |&(_, _, ky, _, h, _)| {
        (1.0 + 126.0 * ((uy - ky) / h).clamp(0.0, 1.0)).round() as u8
    })
}

/// What is at canvas pixel (x, y).
pub fn hit(look: &Look, x: f32, y: f32) -> Option<Ctl> {
    if look.layout == Layout::Display {
        return None;
    }
    let g = look.geo();
    let (x, y) = to_units(look, x, y);
    let reach = g.knob_r + 6.0;
    for (i, (cx, cy)) in g.knobs.iter().enumerate() {
        if (x - cx).powi(2) + (y - cy).powi(2) <= reach * reach {
            return Some(if i == 0 { Ctl::Pot } else { Ctl::Enc(i - 1) });
        }
    }
    let inside = |px: f32, py: f32, w: f32, h: f32| x >= px && x < px + w && y >= py && y < py + h;
    for (_, id, px, py, w, h) in buttons(g) {
        if inside(px, py, w, h) {
            return Some(Ctl::Btn(id));
        }
    }
    // keys: a little wider than drawn, so the gaps between them play too
    for (id, px, py, w, h, _) in keys(g) {
        let pad = (g.key_step - w) / 2.0;
        if inside(px - pad, py, w + 2.0 * pad, h) {
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

/// `c` lighter (t > 0, toward white) or darker (t < 0, toward black).
fn tone(c: u32, t: f32) -> u32 {
    if t >= 0.0 { mix(c, 0xffffff, t) } else { mix(c, 0, -t) }
}

thread_local! {
    /// The width of the canvas being drawn (each panel's `base` / `draw` sets its own).
    static CANVAS_W: std::cell::Cell<usize> = const { std::cell::Cell::new(1280) };
}

/// Draw on canvases `w` pixels wide from now on.
pub fn canvas(w: usize) {
    CANVAS_W.with(|c| c.set(w));
}

pub fn put(buf: &mut [u32], x: i32, y: i32, c: u32, a: f32) {
    let w = CANVAS_W.with(|c| c.get());
    if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < buf.len() / w && a > 0.0 {
        let p = &mut buf[y as usize * w + x as usize];
        *p = if a >= 1.0 { c } else { mix(*p, c, a) };
    }
}

/// Rounded rectangle with a vertical gradient, anti-aliased edges (the SMK-37's panel's).
pub fn rrect(buf: &mut [u32], x: f32, y: f32, w: f32, h: f32, r: f32, top: u32, bot: u32) {
    let cw = CANVAS_W.with(|c| c.get()) as i32;
    let (x0, x1) = (x.floor() as i32 - 1, (x + w).ceil() as i32);
    for py in (y.floor() as i32 - 1)..=((y + h).ceil() as i32) {
        let fy = py as f32 + 0.5;
        let c = mix(top, bot, (fy - y) / h);
        let qy = (fy - (y + h / 2.0)).abs() - (h / 2.0 - r);
        let a = |px: i32| {
            let qx = (px as f32 + 0.5 - (x + w / 2.0)).abs() - (w / 2.0 - r);
            let d = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
            (0.5 - d).clamp(0.0, 1.0)
        };
        // the shape is convex, so a row's fully covered pixels are one run: only its ends blend
        let mut l = x0;
        while l <= x1 && a(l) < 1.0 {
            put(buf, l, py, c, a(l));
            l += 1;
        }
        let mut rt = x1;
        while rt >= l && a(rt) < 1.0 {
            put(buf, rt, py, c, a(rt));
            rt -= 1;
        }
        let (l, rt) = (l.max(0), rt.min(cw - 1));
        if py >= 0 && py < buf.len() as i32 / cw && l <= rt {
            let row = (py * cw) as usize;
            buf[row + l as usize..=row + rt as usize].fill(c);
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

/// The signed distance from (px, py) to a rounded rectangle's edge (negative inside).
fn sdf(px: f32, py: f32, x: f32, y: f32, w: f32, h: f32, r: f32) -> f32 {
    let r = r.min(w / 2.0).min(h / 2.0);
    let qx = (px - (x + w / 2.0)).abs() - (w / 2.0 - r);
    let qy = (py - (y + h / 2.0)).abs() - (h / 2.0 - r);
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
}

/// A rounded rectangle (pixels), anti-aliased, each pixel's colour from `f(u, v)`, its place in
/// the rectangle (0..1 across, 0..1 down).
fn shape(buf: &mut [u32], x: f32, y: f32, w: f32, h: f32, r: f32, f: impl Fn(f32, f32) -> u32) {
    for py in (y.floor() as i32)..=((y + h).ceil() as i32) {
        let fy = py as f32 + 0.5;
        for px in (x.floor() as i32)..=((x + w).ceil() as i32) {
            let fx = px as f32 + 0.5;
            let a = (0.5 - sdf(fx, fy, x, y, w, h, r)).clamp(0.0, 1.0);
            if a > 0.0 {
                put(buf, px, py, f((fx - x) / w, (fy - y) / h), a);
            }
        }
    }
}

/// A soft halo of `color` around a rounded rectangle (pixels), outside it only: shadows (black)
/// and glows (white). `alpha` at its edge, fading out over `blur`.
fn halo(buf: &mut [u32], x: f32, y: f32, w: f32, h: f32, r: f32, blur: f32, color: u32, alpha: f32) {
    let blur = blur.max(1.0);
    for py in ((y - blur).floor() as i32)..=((y + h + blur).ceil() as i32) {
        let fy = py as f32 + 0.5;
        for px in ((x - blur).floor() as i32)..=((x + w + blur).ceil() as i32) {
            let d = sdf(px as f32 + 0.5, fy, x, y, w, h, r);
            if d > -1.0 && d < blur {
                let t = 1.0 - (d.max(0.0) / blur);
                put(buf, px, py, color, alpha * t * t);
            }
        }
    }
}

/// A line with round ends (pixels), anti-aliased.
fn line(buf: &mut [u32], x0: f32, y0: f32, x1: f32, y1: f32, w: f32, color: u32, alpha: f32) {
    let r = w / 2.0;
    let (dx, dy) = (x1 - x0, y1 - y0);
    let len2 = (dx * dx + dy * dy).max(1e-6);
    for py in ((y0.min(y1) - r - 1.0) as i32)..=((y0.max(y1) + r + 1.0) as i32) {
        for px in ((x0.min(x1) - r - 1.0) as i32)..=((x0.max(x1) + r + 1.0) as i32) {
            let (fx, fy) = (px as f32 + 0.5 - x0, py as f32 + 0.5 - y0);
            let t = ((fx * dx + fy * dy) / len2).clamp(0.0, 1.0);
            let d = ((fx - t * dx).powi(2) + (fy - t * dy).powi(2)).sqrt() - r;
            let a = (0.5 - d).clamp(0.0, 1.0) * alpha;
            if a > 0.0 {
                put(buf, px, py, color, a);
            }
        }
    }
}

/// 5x7 glyphs, one byte per column, bit 0 at the top (the SMK-37's panel and overlays).
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
        ':' => [0x00, 0x36, 0x36, 0x00, 0x00],
        _ => [0; 5],
    }
}

/// Bitmap text centred on cx, `scale` pixels per font dot.
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

/// The labels' face: Inter Tight at semibold.
fn font() -> &'static FontRef<'static> {
    static FONT: std::sync::OnceLock<FontRef<'static>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        let mut f = FontRef::try_from_slice(include_bytes!("../assets/InterTight.ttf")).expect("InterTight.ttf");
        f.set_variation(b"wght", 600.0);
        f
    })
}

/// The width of `s` at `size` pixels, with `track` pixels between letters.
fn label_width(s: &str, size: f32, track: f32) -> f32 {
    let f = font().as_scaled(PxScale::from(size));
    let mut w = 0.0;
    let mut prev = None;
    for c in s.chars() {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            w += f.kern(p, id) + track;
        }
        w += f.h_advance(id);
        prev = Some(id);
    }
    w
}

/// Label text centred on (cx, cy) (its capitals' middle), `size` pixels, `track` em of letter
/// spacing. A newline starts a second line under the first.
fn label(buf: &mut [u32], cx: f32, cy: f32, s: &str, size: f32, track: f32, color: u32, alpha: f32) {
    let lines: Vec<&str> = s.split('\n').collect();
    let lead = size * 1.12;
    let cap = size * 0.727;
    let fnt = font();
    let f = fnt.as_scaled(PxScale::from(size));
    let track = track * size;
    for (n, l) in lines.iter().enumerate() {
        let base = cy + cap / 2.0 + (n as f32 - (lines.len() - 1) as f32 / 2.0) * lead;
        let mut x = cx - label_width(l, size, track) / 2.0;
        let mut prev = None;
        for c in l.chars() {
            let id = f.glyph_id(c);
            if let Some(p) = prev {
                x += f.kern(p, id) + track;
            }
            let g = id.with_scale_and_position(PxScale::from(size), point(x, base));
            if let Some(o) = fnt.outline_glyph(g) {
                let bb = o.px_bounds();
                o.draw(|gx, gy, a| put(buf, bb.min.x as i32 + gx as i32, bb.min.y as i32 + gy as i32, color, a * alpha));
            }
            x += f.h_advance(id);
            prev = Some(id);
        }
    }
}

/// Unit -> pixel mapping for one look.
struct Pen {
    k: f32,
    o: f32,
}

impl Pen {
    fn new(look: &Look) -> Pen {
        let k = look.k();
        Pen { k, o: MARGIN * k }
    }
    fn x(&self, v: f32) -> f32 {
        self.o + v * self.k
    }
    fn y(&self, v: f32) -> f32 {
        self.o + v * self.k
    }
    fn s(&self, v: f32) -> f32 {
        v * self.k
    }
    fn rect(&self, r: [f32; 5]) -> (f32, f32, f32, f32, f32) {
        (self.x(r[0]), self.y(r[1]), self.s(r[2]), self.s(r[3]), self.s(r[4]))
    }
}

/// A recessed tray: darker inside at its top edge, a light lip under its bottom edge.
fn tray(buf: &mut [u32], p: &Pen, r: [f32; 5], theme: &Theme) {
    let (x, y, w, h, rad) = p.rect(r);
    let lip = tone(mix(theme.body_top, theme.body_bot, 0.6), 0.08);
    shape(buf, x, y + p.s(1.5), w, h, rad, |_, _| lip);
    let (t, inner) = (theme.tray, p.s(4.0));
    shape(buf, x, y, w, h, rad, |_, v| {
        let d = v * h;
        if d < inner { mix(tone(t, -0.45), t, d / inner) } else { t }
    });
}

/// A button's cap (pixels): a soft vertical gradient and a light top edge; `lit` 0..1.
fn button_cap(buf: &mut [u32], p: &Pen, x: f32, y: f32, w: f32, h: f32, name: &str, font: f32, theme: &Theme, lit: f32, down: bool) {
    let rad = p.s(8.0);
    let (top, bot) = if down { (tone(theme.btn_bot, -0.1), theme.btn_bot) } else { (theme.btn_top, theme.btn_bot) };
    let (top, bot) = (mix(top, LIT, lit * 0.12), mix(bot, LIT, lit * 0.06));
    let hl = tone(theme.btn_top, 0.07);
    let edge = p.s(1.4);
    shape(buf, x, y, w, h, rad, |_, v| if v * h < edge && !down { hl } else { mix(top, bot, v) });
    let ink = mix(theme.btn_ink, LIT, lit);
    let size = p.s(font);
    label(buf, x + w / 2.0, y + h / 2.0, name, size, 0.06, ink, 1.0);
}

/// A note key's cap (pixels): a pill, lighter down its middle, with its light bar.
fn key_cap(buf: &mut [u32], p: &Pen, x: f32, y: f32, w: f32, h: f32, lab: &str, theme: &Theme, lit: f32, down: bool) {
    let (kt, kb) = if down { (tone(theme.key_top, -0.12), tone(theme.key_bot, -0.1)) } else { (theme.key_top, theme.key_bot) };
    let hl = tone(theme.key_top, 0.07);
    let edge = p.s(1.4);
    shape(buf, x, y, w, h, w / 2.0, |u, v| {
        if v * h < edge && !down {
            return hl;
        }
        let across = 1.0 - ((u - 0.5).abs() * 2.0).powf(1.6);
        mix(mix(kb, kt, across), kb, v * 0.35)
    });
    let bar_w = p.s(4.0);
    let (b0, b1) = if lab.is_empty() { (y + h * 0.15, y + h * 0.85) } else { (y + h * 0.13, y + h * 0.56) };
    let cx = x + w / 2.0;
    if lit > 0.0 {
        halo(buf, cx - bar_w / 2.0, b0, bar_w, b1 - b0, bar_w / 2.0, p.s(9.0), LIT, 0.55 * lit);
    }
    let idle = mix(kt, theme.key_line, if down { 0.95 } else { 0.82 });
    line(buf, cx, b0 + bar_w / 2.0, cx, b1 - bar_w / 2.0, bar_w, mix(idle, LIT, lit), 1.0);
    if !lab.is_empty() {
        label(buf, cx, y + h * 0.77, lab, p.s(9.5), 0.04, theme.silk, 0.9);
    }
}

/// A knob without its pointer (pixels): shadow, dark skirt, a softly lit cap.
fn knob_body(buf: &mut [u32], p: &Pen, cx: f32, cy: f32, r: f32, theme: &Theme) {
    halo(buf, cx - r, cy - r + p.s(3.0), 2.0 * r, 2.0 * r, r, p.s(6.0), 0, 0.55);
    let skirt = tone(theme.btn_bot, -0.35);
    shape(buf, cx - r, cy - r, 2.0 * r, 2.0 * r, r, |_, _| skirt);
    let ri = r - p.s(1.5);
    let (light, dark) = (tone(theme.btn_top, 0.1), theme.btn_bot);
    let rim = tone(theme.btn_top, 0.16);
    shape(buf, cx - ri, cy - ri, 2.0 * ri, 2.0 * ri, ri, |u, v| {
        let (dx, dy) = ((u - 0.5) * 2.0, (v - 0.5) * 2.0);
        let d = (dx * dx + dy * dy).sqrt();
        // the top rim catches the light
        if d > 1.0 - p.s(1.6) / ri && dy < 0.0 {
            return mix(dark, rim, -dy);
        }
        let l = ((dx + 0.25).powi(2) + (dy + 0.45).powi(2)).sqrt() / 1.5;
        mix(light, dark, l)
    });
}

/// The parts that never change: body, trays, bezel, labels, and every control at rest.
pub fn base(theme: &Theme, look: &Look) -> Vec<u32> {
    let (w, h) = size(look);
    canvas(w);
    if look.layout == Layout::Display {
        return vec![0; w * h];
    }
    let mut b = vec![ROOM; w * h];
    let (g, p) = (look.geo(), Pen::new(look));
    // body: a soft shadow under it, a gradient, a light top edge
    let (bx, by, bw, bh, br) = p.rect([0.0, 0.0, g.w, g.h, g.radius]);
    halo(&mut b, bx, by + p.s(8.0), bw, bh, br, p.s(16.0), 0, 0.5);
    let edge = p.s(1.6);
    let hl = tone(theme.body_top, 0.12);
    shape(&mut b, bx, by, bw, bh, br, |_, v| {
        let d = v * bh;
        if d < edge { hl } else if v < 0.4 { mix(theme.body_top, mix(theme.body_top, theme.body_bot, 0.45), v / 0.4) } else { mix(mix(theme.body_top, theme.body_bot, 0.45), theme.body_bot, (v - 0.4) / 0.6) }
    });
    tray(&mut b, &p, g.oct_tray, theme);
    tray(&mut b, &p, g.btn_tray, theme);
    tray(&mut b, &p, g.key_tray, theme);
    // the screen's bezel: a thin dark frame with a faint outer line
    let (zx, zy, zs, rim, zr) = (p.x(g.bezel[0]), p.y(g.bezel[1]), p.s(g.bezel[2]), p.s(g.bezel[3]), p.s(g.bezel[4]));
    shape(&mut b, zx - p.s(1.5), zy - p.s(1.5), zs + p.s(3.0), zs + p.s(3.0), zr + p.s(1.5), |_, _| tone(theme.body_bot, -0.35));
    shape(&mut b, zx, zy, zs, zs, zr, |_, v| mix(0x111213, 0x0b0b0c, v));
    let (lx, ly, ls) = (zx + rim, zy + rim, zs - 2.0 * rim);
    shape(&mut b, lx, ly, ls, ls, p.s(2.0), |_, _| 0x050506);
    // knobs and their labels
    let names = ["MASTER", "SELECT", "PRESETS", "ALGORITHM", "KNOB1", "KNOB2", "KNOB3", "KNOB4"];
    for (i, (cx, cy)) in g.knobs.iter().enumerate() {
        label(&mut b, p.x(*cx), p.y(cy - g.knob_r - g.label_gap), names[i], p.s(g.label), 0.08, theme.silk, 1.0);
        knob_body(&mut b, &p, p.x(*cx), p.y(*cy), p.s(g.knob_r), theme);
    }
    // buttons and keys at rest
    for (name, _, x, y, w, h) in buttons(g) {
        let (x, y, w, h) = (p.x(x), p.y(y), p.s(w), p.s(h));
        halo(&mut b, x, y + p.s(2.0), w, h, p.s(8.0), p.s(3.5), 0, 0.7);
        button_cap(&mut b, &p, x, y, w, h, name, g.btn_font, theme, 0.0, false);
    }
    let mut top = 0;
    for (_, x, y, w, h, is_top) in keys(g) {
        let (x, y, w, h) = (p.x(x), p.y(y), p.s(w), p.s(h));
        halo(&mut b, x, y + p.s(2.5), w, h, w / 2.0, p.s(4.0), 0, 0.7);
        let lab = if is_top && look.labels { TOP_LABELS[top] } else { "" };
        top += is_top as usize;
        key_cap(&mut b, &p, x, y, w, h, lab, theme, 0.0, false);
    }
    b
}

pub struct State<'a> {
    pub lcd: &'a [u16],
    pub enc_clicks: &'a [i32; 7],
    /// Visual encoder position in detents; firmware still receives integer physical detents.
    pub positions: Option<&'a [f32; 7]>,
    pub pot: u16,
    pub down: &'a [bool; 41],
    pub leds: &'a [f32; 41],
    pub turning: &'a [u32; 8],
}

/// LED fraction -> brightness: a fully lit LED is on ~1 tick in 11.
fn bright(f: f32) -> f32 {
    (f / 0.08).clamp(0.0, 1.0)
}

/// The LCD (240x240 RGB565) into the square at (x, y), `s` pixels a side, nearest neighbour.
fn blit_lcd(buf: &mut [u32], lcd: &[u16], x: usize, y: usize, s: usize, cw: usize) {
    if lcd.len() < 240 * 240 || s == 0 {
        return;
    }
    let cols: Vec<usize> = (0..s).map(|i| i * 240 / s).collect();
    let mut row = vec![0u32; s];
    let mut last = usize::MAX;
    for j in 0..s {
        let sy = j * 240 / s;
        if sy != last {
            for (o, &sx) in row.iter_mut().zip(cols.iter()) {
                let p = lcd[sy * 240 + sx] as u32;
                *o = ((p >> 11) * 255 / 31) << 16 | (((p >> 5) & 63) * 255 / 63) << 8 | (p & 31) * 255 / 31;
            }
            last = sy;
        }
        let o = (y + j) * cw + x;
        if o + s <= buf.len() {
            buf[o..o + s].copy_from_slice(&row);
        }
    }
}

pub fn draw(buf: &mut [u32], base: &[u32], st: &State, theme: &Theme, look: &Look) {
    let (cw, ch) = size(look);
    canvas(cw);
    buf.copy_from_slice(base);
    if look.layout == Layout::Display {
        blit_lcd(buf, st.lcd, 0, 0, cw.min(ch), cw);
        return;
    }
    let (g, p) = (look.geo(), Pen::new(look));
    let (lx, ly) = (p.x(g.bezel[0] + g.bezel[3]).round() as usize, p.y(g.bezel[1] + g.bezel[3]).round() as usize);
    blit_lcd(buf, st.lcd, lx, ly, p.s(g.bezel[2] - 2.0 * g.bezel[3]).round() as usize, cw);
    // knobs: encoders turn 6 degrees a click; the pot sweeps 270 degrees
    for (i, (cx, cy)) in g.knobs.iter().enumerate() {
        let (angle, turning) = if i == 0 {
            ((-135.0 + 270.0 * st.pot as f32 / 1023.0).to_radians(), st.turning[7])
        } else {
            ((st.positions.map_or(st.enc_clicks[i - 1] as f32, |p| p[i - 1]) * 6.0).to_radians(), st.turning[i - 1])
        };
        let (cx, cy, r) = (p.x(*cx), p.y(*cy), p.s(g.knob_r));
        let glow = (turning as f32 / 10.0).min(1.0);
        if glow > 0.0 {
            halo(buf, cx - r, cy - r, 2.0 * r, 2.0 * r, r, p.s(7.0), LIT, 0.22 * glow);
        }
        let (s, c) = angle.sin_cos();
        let (d0, d1) = (r * 0.3, r * 0.78);
        line(buf, cx + s * d0, cy - c * d0, cx + s * d1, cy - c * d1, p.s(3.0).max(1.5), theme.key_line, 0.95);
    }
    // buttons and keys pressed or lit: drawn again over their resting selves
    for (name, id, x, y, w, h) in buttons(g) {
        let (down, lit) = (st.down[id as usize], bright(st.leds[id as usize]));
        if !down && lit == 0.0 {
            continue;
        }
        let (x, y, w, h) = (p.x(x), p.y(y), p.s(w), p.s(h));
        let off = if down { p.s(1.5) } else { 0.0 };
        if down {
            shape(buf, x - 1.0, y - 1.0, w + 2.0, h + off + 4.0, p.s(8.0), |_, _| theme.tray);
        }
        button_cap(buf, &p, x, y + off, w, h, name, g.btn_font, theme, lit, down);
    }
    let mut top = 0;
    for (id, x, y, w, h, is_top) in keys(g) {
        let lab = if is_top && look.labels { TOP_LABELS[top] } else { "" };
        top += is_top as usize;
        let (down, lit) = (st.down[id as usize], bright(st.leds[id as usize]));
        if !down && lit == 0.0 {
            continue;
        }
        let (x, y, w, h) = (p.x(x), p.y(y), p.s(w), p.s(h));
        let off = if down { p.s(2.0) } else { 0.0 };
        if down {
            shape(buf, x - 1.0, y - 1.0, w + 2.0, h + off + 5.0, w / 2.0, |_, _| theme.tray);
        }
        key_cap(buf, &p, x, y + off, w, h, lab, theme, lit, down);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn looks() -> Vec<Look> {
        let mut v = Vec::new();
        for layout in [Layout::Large, Layout::Real] {
            for scale in [1.0, 2.0] {
                for labels in [false, true] {
                    v.push(Look { layout, labels, scale });
                }
            }
        }
        v
    }

    #[test]
    fn themes_have_correct_names() {
        let names: Vec<&str> = THEMES.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(names, &["graphite", "classic", "cool-gray", "black", "orange", "black-green", "purple", "white-blue"]);
    }

    #[test]
    fn default_and_unknown_names_get_graphite() {
        assert_eq!(Theme::by_name("default").body_top, 0x333436);
        assert_eq!(Theme::by_name("nonexistent").body_top, 0x333436);
        assert_eq!(Theme::by_name("cool-gray").body_top, 0xDAD1CC);
    }

    #[test]
    fn layouts_round_trip_their_names() {
        for l in Layout::ALL {
            assert_eq!(Layout::from_name(l.name()), Some(l));
        }
        assert_eq!(Layout::Large.next().next().next(), Layout::Large);
    }

    #[test]
    fn the_font_has_every_label() {
        let f = font();
        for s in ["MASTER", "SELECT", "PRESETS", "ALGORITHM", "KNOB1234", "OCT-+", "FX SCL ENV LFO EDIT GLO HOME SAVE ARP SEQ PLAY STOP REC", "OP56 PIT MONO POLY"] {
            for c in s.chars().filter(|c| *c != ' ') {
                assert_ne!(f.glyph_id(c).0, 0, "no glyph for {c:?}");
            }
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(size(&Look::default()), (1300, 950));
        assert_eq!(size(&Look { layout: Layout::Real, ..Look::default() }), (1300, 791));
        assert_eq!(size(&Look { layout: Layout::Display, labels: false, scale: 3.0 }), (720, 720));
    }

    #[test]
    fn the_lcd_lands_on_whole_pixels() {
        // large at 2x, real at 1x (and twice that at scale 2)
        for (layout, scale, px) in [(Layout::Large, 1.0, 480.0), (Layout::Real, 1.0, 240.0), (Layout::Large, 2.0, 960.0), (Layout::Real, 2.0, 480.0)] {
            let look = Look { layout, labels: false, scale };
            let g = look.geo();
            assert_eq!((g.bezel[2] - 2.0 * g.bezel[3]) * look.k(), px);
        }
    }

    #[test]
    fn every_control_is_hit_at_its_centre() {
        for look in looks() {
            let (g, p) = (look.geo(), Pen::new(&look));
            for (i, (cx, cy)) in g.knobs.iter().enumerate() {
                let want = if i == 0 { Ctl::Pot } else { Ctl::Enc(i - 1) };
                assert_eq!(hit(&look, p.x(*cx), p.y(*cy)), Some(want), "{look:?} knob {i}");
            }
            for (_, id, x, y, w, h) in buttons(g) {
                assert_eq!(hit(&look, p.x(x + w / 2.0), p.y(y + h / 2.0)), Some(Ctl::Btn(id)), "{look:?} button {id}");
            }
            for (id, x, y, w, h, _) in keys(g) {
                assert_eq!(hit(&look, p.x(x + w / 2.0), p.y(y + h / 2.0)), Some(Ctl::Key(id)), "{look:?} key {id}");
            }
            assert_eq!(hit(&look, 2.0, 2.0), None);
        }
        assert_eq!(hit(&Look { layout: Layout::Display, ..Look::default() }, 100.0, 100.0), None);
    }

    #[test]
    fn controls_stay_inside_their_trays_and_the_body() {
        for look in [Look::default(), Look { layout: Layout::Real, ..Look::default() }] {
            let g = look.geo();
            let within = |r: [f32; 5], x: f32, y: f32, w: f32, h: f32| x >= r[0] && y >= r[1] && x + w <= r[0] + r[2] && y + h <= r[1] + r[3];
            for (name, id, x, y, w, h) in buttons(g) {
                let t = if id < 2 { g.oct_tray } else { g.btn_tray };
                assert!(within(t, x, y, w, h), "{:?} {name}", look.layout);
            }
            for (id, x, y, w, h, _) in keys(g) {
                assert!(within(g.key_tray, x, y, w, h), "{:?} key {id}", look.layout);
            }
            for r in [g.oct_tray, g.btn_tray, g.key_tray, [g.bezel[0], g.bezel[1], g.bezel[2], g.bezel[2], 0.0]] {
                assert!(within([0.0, 0.0, g.w, g.h, 0.0], r[0], r[1], r[2], r[3]));
            }
        }
    }

    #[test]
    fn velocity_grows_down_a_key() {
        let look = Look::default();
        let p = Pen::new(&look);
        let (id, _, y, _, h, _) = keys(look.geo())[0];
        assert!(key_vel(&look, id, p.y(y + 1.0)) < 10);
        assert!(key_vel(&look, id, p.y(y + h - 1.0)) > 120);
    }

    #[test]
    fn base_and_draw_fill_the_canvas() {
        let lcd = vec![0xffffu16; 240 * 240];
        for look in looks().into_iter().chain([Look { layout: Layout::Display, labels: false, scale: 2.0 }]) {
            let t = Theme::by_name("graphite");
            let b = base(t, &look);
            let (w, h) = size(&look);
            assert_eq!(b.len(), w * h);
            let mut buf = vec![0; b.len()];
            let mut down = [false; 41];
            down[2] = true;
            down[20] = true;
            let mut leds = [0.0; 41];
            leds[14] = 1.0;
            let st = State { lcd: &lcd, positions: None, enc_clicks: &[1; 7], pot: 512, down: &down, leds: &leds, turning: &[5; 8] };
            draw(&mut buf, &b, &st, t, &look);
            if look.layout == Layout::Display {
                assert!(buf.iter().all(|&p| p == 0xffffff));
            } else {
                // the room in a corner, the LCD (all white) in the screen
                assert_eq!(buf[0], ROOM);
                let (g, p) = (look.geo(), Pen::new(&look));
                let cx = p.x(g.bezel[0] + g.bezel[2] / 2.0) as usize;
                let cy = p.y(g.bezel[1] + g.bezel[2] / 2.0) as usize;
                assert_eq!(buf[cy * w + cx], 0xffffff);
            }
        }
    }

    #[test]
    fn put_blends_as_mix_does() {
        canvas(4);
        let mut b = vec![0x102030u32; 16];
        put(&mut b, 1, 1, 0xf0e0d0, 0.25);
        assert_eq!(b[5], mix(0x102030, 0xf0e0d0, 0.25));
        put(&mut b, 2, 1, 0xf0e0d0, 1.0);
        assert_eq!(b[6], 0xf0e0d0);
    }
}

/// `FM1_PANEL_OUT=dir cargo test -p fm1-live render_samples -- --ignored`: the looks as PPM files,
/// to look at the panel without a firmware.
#[cfg(test)]
#[test]
#[ignore]
fn render_samples() {
    let dir = std::env::var("FM1_PANEL_OUT").unwrap_or_else(|_| "work".into());
    std::fs::create_dir_all(&dir).unwrap();
    // an LCD test card: grey ramp and a white frame
    let mut lcd = vec![0u16; 240 * 240];
    for y in 0..240 {
        for x in 0..240 {
            let v = if x < 4 || y < 4 || x > 235 || y > 235 { 31 } else if (40..200).contains(&y) && (20..220).contains(&x) { (x as u16 - 20) * 31 / 200 } else { 2 };
            lcd[y * 240 + x] = v << 11 | (v * 2) << 5 | v;
        }
    }
    let mut down = [false; 41];
    down[9] = true;
    down[30] = true;
    let mut leds = [0.0; 41];
    leds[2] = 1.0;
    leds[12] = 1.0;
    leds[22] = 1.0;
    leds[17] = 1.0;
    for (name, look) in [
        ("large", Look::default()),
        ("large-labels", Look { labels: true, ..Look::default() }),
        ("real", Look { layout: Layout::Real, ..Look::default() }),
        ("large-2x", Look { scale: 2.0, ..Look::default() }),
    ] {
        let t = Theme::by_name("graphite");
        let b = base(t, &look);
        let (w, h) = size(&look);
        let mut buf = vec![0; b.len()];
        let st = State { lcd: &lcd, positions: None, enc_clicks: &[2, 0, -3, 1, 4, -2, 6], pot: 700, down: &down, leds: &leds, turning: &[0, 0, 8, 0, 0, 0, 0, 0] };
        draw(&mut buf, &b, &st, t, &look);
        let mut ppm = format!("P6\n{w} {h}\n255\n").into_bytes();
        for p in buf {
            ppm.extend([(p >> 16) as u8, (p >> 8) as u8, p as u8]);
        }
        std::fs::write(format!("{dir}/{name}.ppm"), ppm).unwrap();
    }
}
