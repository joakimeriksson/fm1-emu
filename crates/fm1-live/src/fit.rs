//! The window at a size that fits the screen, sharp at any display scaling.
//!
//! Windows stretches a program that does not say it handles display scaling: at 150 % the
//! 1280 × 820 panel became a blurry 1920 × 1230 window, taller than a 1920 × 1200 screen. So the
//! process says it is DPI aware, the window opens as large as the scaling asks but no larger
//! than the screen's work area, can be resized, and the panel is scaled into it here (smoothly,
//! keeping its shape, with bars at the sides if needed). The mouse is mapped back to the panel.

/// Before any window: draw at the screen's own pixels (Windows); nothing elsewhere.
pub fn dpi_aware() {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn SetProcessDpiAwarenessContext(ctx: isize) -> i32;
            fn SetProcessDPIAware() -> i32;
        }
        const PER_MONITOR_AWARE_V2: isize = -4;
        unsafe {
            if SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2) == 0 {
                SetProcessDPIAware();
            }
        }
    }
}

/// The work area (the screen without the taskbar) as left, top, width, height in pixels, and
/// the display scaling (1.0 = 100 %).
#[cfg(windows)]
fn work_area() -> Option<(i32, i32, i32, i32, f32)> {
    #[repr(C)]
    struct Rect {
        l: i32,
        t: i32,
        r: i32,
        b: i32,
    }
    #[link(name = "user32")]
    extern "system" {
        fn SystemParametersInfoW(action: u32, param: u32, out: *mut Rect, ini: u32) -> i32;
        fn GetDpiForSystem() -> u32;
    }
    const SPI_GETWORKAREA: u32 = 0x30;
    let mut r = Rect { l: 0, t: 0, r: 0, b: 0 };
    unsafe {
        if SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r, 0) == 0 || r.r <= r.l || r.b <= r.t {
            return None;
        }
        let dpi = GetDpiForSystem();
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        Some((r.l, r.t, r.r - r.l, r.b - r.t, scale))
    }
}

/// Where the window opens: its inner size, and its position (or none: the system's choice).
/// `zoom`: the user's own size (--zoom), else the display scaling; never past the work area.
pub fn start(w: usize, h: usize, zoom: Option<f32>) -> ((usize, usize), Option<(isize, isize)>) {
    #[cfg(windows)]
    if let Some((x, y, aw, ah, scale)) = work_area() {
        // room for the frame and the title bar
        let (fw, fh) = ((16.0 * scale) as i32, (40.0 * scale) as i32);
        let fit = ((aw - fw) as f32 / w as f32).min((ah - fh) as f32 / h as f32);
        let s = zoom.unwrap_or(scale).min(fit).max(0.25);
        let (ww, wh) = ((w as f32 * s).round() as usize, (h as f32 * s).round() as usize);
        let px = x + (aw - ww as i32 - fw).max(0) / 2;
        let py = y + (ah - wh as i32 - fh).max(0) / 2;
        return ((ww, wh), Some((px as isize, py as isize)));
    }
    let s = zoom.unwrap_or(1.0).max(0.25);
    (((w as f32 * s).round() as usize, (h as f32 * s).round() as usize), None)
}

/// The panel (w × h) scaled into a window of any size: keeps its shape, centred.
pub struct Fit {
    w: usize,
    h: usize,
    ww: usize,
    wh: usize,
    s: f32,
    ox: usize,
    oy: usize,
    out: Vec<u32>,
    // per window column / row: the panel's pixel to the left / above and the weight of the next
    cols: Vec<(u32, u32)>,
    rows: Vec<(u32, u32)>,
}

impl Fit {
    pub fn new(w: usize, h: usize) -> Fit {
        Fit { w, h, ww: 0, wh: 0, s: 1.0, ox: 0, oy: 0, out: Vec::new(), cols: Vec::new(), rows: Vec::new() }
    }

    fn layout(&mut self, ww: usize, wh: usize) {
        if (ww, wh) == (self.ww, self.wh) {
            return;
        }
        let (ww, wh) = (ww.max(1), wh.max(1));
        self.ww = ww;
        self.wh = wh;
        self.s = (ww as f32 / self.w as f32).min(wh as f32 / self.h as f32);
        let (iw, ih) = (((self.w as f32 * self.s) as usize).clamp(1, ww), ((self.h as f32 * self.s) as usize).clamp(1, wh));
        self.ox = (ww - iw) / 2;
        self.oy = (wh - ih) / 2;
        self.out = vec![0; ww * wh];
        let map = |n: usize, src: usize| -> Vec<(u32, u32)> {
            (0..n)
                .map(|i| {
                    let f = ((i as f32 + 0.5) * src as f32 / n as f32 - 0.5).clamp(0.0, (src - 1) as f32);
                    let i0 = (f as usize).min(src - 1);
                    (i0 as u32, ((f - i0 as f32) * 256.0) as u32)
                })
                .collect()
        };
        self.cols = map(iw, self.w);
        self.rows = map(ih, self.h);
    }

    /// The window's pixels for this frame of the panel. At the panel's own size, the panel.
    pub fn frame<'a>(&'a mut self, src: &'a [u32], ww: usize, wh: usize) -> (&'a [u32], usize, usize) {
        if (ww, wh) == (self.w, self.h) || ww == 0 || wh == 0 {
            // (1:1: the mouse needs no mapping either)
            self.ww = self.w;
            self.wh = self.h;
            return (src, self.w, self.h);
        }
        self.layout(ww, wh);
        let (w, h) = (self.w, self.h);
        let lerp = |a: u32, b: u32, t: u32| -> u32 {
            let mut o = 0;
            for sh in [0, 8, 16] {
                let (x, y) = ((a >> sh) & 0xff, (b >> sh) & 0xff);
                o |= ((x * (256 - t) + y * t) >> 8) << sh;
            }
            o
        };
        for (j, &(y0, fy)) in self.rows.iter().enumerate() {
            let y0 = y0 as usize;
            let y1 = (y0 + 1).min(h - 1);
            let (r0, r1) = (&src[y0 * w..y0 * w + w], &src[y1 * w..y1 * w + w]);
            let line = &mut self.out[(self.oy + j) * self.ww + self.ox..];
            for (i, &(x0, fx)) in self.cols.iter().enumerate() {
                let x0 = x0 as usize;
                let x1 = (x0 + 1).min(w - 1);
                let top = lerp(r0[x0], r0[x1], fx);
                let bot = lerp(r1[x0], r1[x1], fx);
                line[i] = lerp(top, bot, fy);
            }
        }
        (&self.out, self.ww, self.wh)
    }

    /// A mouse position in the window to the panel's coordinates.
    pub fn to_panel(&self, x: f32, y: f32) -> (f32, f32) {
        if self.ww == 0 || (self.ww, self.wh) == (self.w, self.h) {
            return (x, y);
        }
        (
            ((x - self.ox as f32) / self.s).clamp(0.0, self.w as f32 - 1.0),
            ((y - self.oy as f32) / self.s).clamp(0.0, self.h as f32 - 1.0),
        )
    }
}
