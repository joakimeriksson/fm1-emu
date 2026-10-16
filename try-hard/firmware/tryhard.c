/* SPDX-License-Identifier: GPL-3.0-only */
/* Try Hard: the FM-1 as a remote panel for the FM-1 emulator. Every key, button, encoder and the MASTER knob
 * goes out over USB-MIDI; the emulator page (joakimeriksson.github.io/fm1-emu) plays the firmware it runs
 * from them and sends its LEDs back. No sound here. The screen shows a clockwork of meshing gears that the
 * panel drives, over a big TRY HARD, with the link to the page under it. Protocol: ../PROTOCOL.md.
 *
 * Built on Dinghy (platform.c: boot, USB, the updater, the panel, the screen): the build drops this file in as
 * Dinghy's app (../build.sh). Integers only, no 64-bit division (Dinghy's docs/RULES.md). */
#define APP_NAME "TRY HARD"
#include "platform.c"

#define CH 0x0Fu                                /* MIDI channel 16: the panel's */
#define HELLO_CC 127u
#define HELLO_V 84u                             /* 'T' */

static void app_audio(int32_t *out, uint32_t n) { (void)out; (void)n; }   /* silent */

#ifdef DINGHY_HOST
static int32_t master_read(void) { return 700; }
#else
static int32_t master_read(void) { return fm1_adc_read(FM1_ADC_MASTER); }
#endif

/* ---------------------------------------------------------------- integer trig */
static int32_t sine(uint32_t ph)                /* a full cycle over 2^32 -> -32767..32767 (dinghy.c's) */
{
    int32_t x = (int32_t)ph >> 16, ax = x < 0 ? -x : x, y, ay;
    y = (x * (32768 - ax)) >> 13;
    ay = y < 0 ? -y : y;
    y += ((((y * ay) >> 15) - y) * 7373) >> 15;
    return y > 32767 ? 32767 : y < -32767 ? -32767 : y;
}
static int32_t sin16(uint32_t a) { return sine(a << 16); }              /* a: a turn is 65536 */
static int32_t cos16(uint32_t a) { return sine((a + 16384u) << 16); }

static const uint16_t ATAN64[65] = {           /* atan(i / 64) in 1/65536 turns */
    0, 163, 326, 489, 651, 813, 975, 1136, 1297, 1457, 1617, 1775, 1933, 2090, 2246, 2401, 2555, 2708, 2860,
    3010, 3159, 3307, 3453, 3599, 3742, 3884, 4025, 4164, 4302, 4438, 4572, 4705, 4836, 4966, 5094, 5220,
    5344, 5467, 5589, 5708, 5826, 5943, 6058, 6171, 6282, 6392, 6500, 6607, 6712, 6815, 6917, 7018, 7117,
    7214, 7310, 7405, 7498, 7589, 7679, 7768, 7856, 7942, 8026, 8110, 8192};
static uint32_t iatan2(int32_t y, int32_t x)   /* the angle of (x, y), 0..65535 (y down: clockwise) */
{
    uint32_t ax = (uint32_t)(x < 0 ? -x : x), ay = (uint32_t)(y < 0 ? -y : y), t, a;
    if (!ax && !ay)
        return 0;
    if (ax >= ay) {
        t = (ay << 10) / ax;                    /* 0..1024 */
        a = ATAN64[t >> 4] + (((ATAN64[(t >> 4) + (t < 1024u)] - ATAN64[t >> 4]) * (t & 15u)) >> 4);
    } else {
        t = (ax << 10) / ay;
        a = 16384u - (ATAN64[t >> 4] + (((ATAN64[(t >> 4) + (t < 1024u)] - ATAN64[t >> 4]) * (t & 15u)) >> 4));
    }
    if (x < 0) a = 32768u - a;
    if (y < 0) a = 65536u - a;
    return a & 0xFFFFu;
}
static uint32_t isqrt(uint32_t v)
{
    uint32_t r = 0, b = 1u << 30;
    while (b > v) b >>= 2;
    while (b) {
        if (v >= r + b) { v -= r + b; r = (r >> 1) + b; }
        else r >>= 1;
        b >>= 2;
    }
    return r;
}
static int32_t clamp256(int32_t v) { return v < 0 ? 0 : v > 256 ? 256 : v; }

/* ---------------------------------------------------------------- pixels (cv_px holds RGB565, byte-swapped) */
static inline void px_get(uint32_t i, int32_t *r, int32_t *g, int32_t *b)
{
    uint32_t c = swap16(cv_px[i]);
    *r = (int32_t)((c >> 11) << 3);
    *g = (int32_t)(((c >> 5) & 63u) << 2);
    *b = (int32_t)((c & 31u) << 3);
}
static inline void px_put(uint32_t i, int32_t r, int32_t g, int32_t b)
{
    r = r < 0 ? 0 : r > 255 ? 255 : r;
    g = g < 0 ? 0 : g > 255 ? 255 : g;
    b = b < 0 ? 0 : b > 255 ? 255 : b;
    cv_px[i] = swap16(RGB(r, g, b));
}

/* ---------------------------------------------------------------- the clockwork
 * Compound gears: a wheel and a pinion share an axle and turn together. Three heights (z 0..2): a pinion
 * drives the wheel of the next axle on its own height. All teeth have one module (M px), so meshing gears
 * sit their pitch radii apart (M * (N1 + N2) / 2) and their tooth phases are set so that a tooth always
 * meets a gap (gears_init). Axle a turns K[a] / 120 as fast as axle 0, the ratios of the teeth.
 *
 * Drawn flat and fast: the axles stay put, so each has a table of every pixel's distance and angle from
 * it, made once. A frame is then table lookups and compares per pixel: no square root, no arctangent. */
#define Q4 16                                   /* positions in 1/16 px */
#define M 4                                     /* module: px of pitch diameter per tooth */
#define NAX 5
#define NDISC 7
#define LIGHT 40960u                            /* light from the top left (225 degrees, y down) */
#define QWRAP (120 * 65536)                     /* the master angle wraps here: every axle turns whole */
#ifndef TIP_U
#define TIP_U 5900                              /* the tooth's tip land: 9 % of the pitch either side */
#endif
#ifndef ROOT_U
#define ROOT_U 21600                            /* its flank ends (the root begins) 33 % out */
#endif
#ifndef GAP
#define GAP 8                                   /* backlash: meshing centres this much further apart (Q4) */
#endif
#define FLANK_K ((256 << 18) / (ROOT_U - TIP_U))
#define CLOCK_H 144                             /* the clockwork and its shadows: rows 0..143 */

typedef struct {
    uint8_t ax, z, n, spokes;                   /* axle, height, teeth, spokes (0: solid) */
    uint8_t r, g, b;                            /* the metal */
    uint8_t top;                                /* the top disc of its axle: the jewel */
    int32_t rt, rr, rim, hub;                   /* tip, root, rim inside, hub outside (1/4 px) */
    uint16_t face, hubc, dark, lite;            /* colours, as cv_px holds them */
    uint8_t edge[256];                          /* the outline's radius (1/4 px) by place in a tooth */
    uint16_t win[192], wino[192];               /* a window starts past this place between spokes, by radius */
} disc_t;

static disc_t disc[NDISC] = {
    /* ax z  n  sp   metal */
    {0, 0, 18, 5, 200, 160, 76},                /* W0: brass, the big one */
    {0, 1, 8, 0, 182, 188, 198},                /* P0: steel pinion on W0 */
    {1, 1, 16, 5, 214, 176, 96},                /* W1: brass, driven by P0 */
    {1, 2, 7, 0, 182, 188, 198},                /* P1: steel pinion on W1 */
    {2, 2, 12, 4, 204, 124, 80},                /* W2: copper, driven by P1 */
    {3, 0, 10, 3, 156, 166, 186},               /* W3: blued steel, driven by W0 */
    {4, 2, 14, 6, 200, 160, 76},                /* W4: brass, driven by W2 */
};
static const int32_t K[NAX] = {120, -60, 35, -216, -30};
/* the meshes, in order from axle 0: disc i drives disc j, axle j's centre in direction PHI from axle i's */
static const struct { uint8_t i, j; uint16_t phi; } MESH[NAX - 1] = {
    {1, 2, 61895},                              /* P0 -> W1, -20 degrees */
    {3, 4, 6372},                               /* P1 -> W2, +35 */
    {0, 5, 27307},                              /* W0 -> W3, 150 */
    {4, 6, 59711},                              /* W2 -> W4, -32 */
};
static int32_t ax_x[NAX], ax_y[NAX];            /* axle centres (Q4) */
static uint32_t ax_c[NAX];                      /* phase of each axle */
static uint32_t ax_th[NAX];                     /* this frame: angles */
static uint8_t ZORD[NDISC];                     /* the discs, bottom up */

/* the tables: per axle, a box of pixels around it, each with its angle and distance (1/4 px) */
#define TAB_MAX 26000
static uint16_t tab_a[TAB_MAX] __attribute__((section(".pool")));
static uint8_t tab_r[TAB_MAX] __attribute__((section(".pool")));
static uint16_t bg[CLOCK_H * 240] __attribute__((section(".pool")));   /* the ground with its shadows */
static struct { int32_t x0, y0, w, h; uint32_t at; } tab[NAX];

static uint16_t col(int32_t r, int32_t g, int32_t b)
{
    r = r < 0 ? 0 : r > 255 ? 255 : r;
    g = g < 0 ? 0 : g > 255 ? 255 : g;
    b = b < 0 ? 0 : b > 255 ? 255 : b;
    return swap16(RGB(r, g, b));
}

static void gears_init(void)
{
    uint32_t i, k, used = 0;
    int32_t z;
    ax_x[0] = 72 * Q4;
    ax_y[0] = 74 * Q4;
    ax_c[0] = 0;
    for (k = 0; k < NAX - 1u; k++) {
        const disc_t *a = &disc[MESH[k].i], *b = &disc[MESH[k].j];
        int32_t d = M * Q4 * (a->n + b->n) / 2 + GAP, num, mod = 65536 * (int32_t)b->n;
        int32_t phi = MESH[k].phi;
        ax_x[b->ax] = ax_x[a->ax] + ((d * cos16((uint32_t)phi)) >> 15);
        ax_y[b->ax] = ax_y[a->ax] + ((d * sin16((uint32_t)phi)) >> 15);
        /* tooth of a at phi meets a gap of b at phi + 180: (phi - c_a) n_a + (phi + 1/2 - c_b) n_b = 1/2 */
        num = phi * a->n + (phi + 32768) * b->n - (int32_t)ax_c[a->ax] * a->n - 32768;
        num %= mod;
        if (num < 0) num += mod;
        ax_c[b->ax] = (uint32_t)(num / b->n);
    }
    for (i = 0; i < NDISC; i++) {
        disc_t *d = &disc[i];
        int32_t pr = M * 4 * d->n / 2, j, u;   /* the pitch radius, 1/4 px */
        d->rt = pr + M * 4;
        d->rr = pr - (M * 4 * 115) / 100;
        d->rim = d->rr - 4 * 4;
        d->hub = pr * 3 / 10 < 6 * 4 ? 6 * 4 : pr * 3 / 10;
        d->top = 1;
        for (j = 0; j < NDISC; j++)
            if (j != (int32_t)i && disc[j].ax == d->ax && disc[j].z > d->z) d->top = 0;
        d->face = col(d->r, d->g, d->b);
        d->hubc = col(d->r * 5 / 4, d->g * 5 / 4, d->b * 5 / 4);
        d->dark = col(d->r * 2 / 5, d->g * 2 / 5, d->b * 2 / 5);
        d->lite = col(d->r * 3 / 2, d->g * 3 / 2, d->b * 3 / 2);
        for (j = 0; j < 256; j++) {             /* the teeth: a trapezoid with rounded corners */
            int32_t us = j << 7, prof;
            if (us < TIP_U) prof = 256;
            else if (us > ROOT_U) prof = 0;
            else {
                prof = ((ROOT_U - us) * FLANK_K) >> 18;
                prof = (prof * prof * (768 - 2 * prof)) >> 16;
            }
            d->edge[j] = (uint8_t)(d->rr + (((d->rt - d->rr) * prof) >> 8));
        }
        for (u = 0; u < 192; u++) {             /* the spokes: 2.2 px half wide, so wider in angle near the hub */
            int32_t a1 = u ? 10430 * 9 * d->spokes / u : 32767, a2 = u ? 10430 * 13 * d->spokes / u : 32767;
            d->win[u] = (uint16_t)(a2 > 32767 ? 32767 : a2);    /* past the spoke and its outline */
            d->wino[u] = (uint16_t)(a1 > 32767 ? 32767 : a1);   /* past the spoke */
        }
    }
    for (k = 0, z = 0; z < 3; z++)
        for (i = 0; i < NDISC; i++)
            if (disc[i].z == z) ZORD[k++] = (uint8_t)i;
    for (k = 0; k < NAX; k++) {                 /* the tables: each axle's box, as big as its biggest disc */
        int32_t rt = 0, x, y, cx = ax_x[k], cy = ax_y[k];
        for (i = 0; i < NDISC; i++)
            if (disc[i].ax == k && disc[i].rt > rt) rt = disc[i].rt;
        rt = rt / 4 + 1;
        tab[k].x0 = cx / Q4 - rt;
        tab[k].y0 = cy / Q4 - rt;
        tab[k].w = tab[k].h = 2 * rt + 1;
        tab[k].at = used;
        for (y = 0; y < tab[k].h; y++)
            for (x = 0; x < tab[k].w; x++) {
                int32_t dx = (tab[k].x0 + x) * Q4 + Q4 / 2 - cx, dy = (tab[k].y0 + y) * Q4 + Q4 / 2 - cy;
                uint32_t r = isqrt((uint32_t)(dx * dx + dy * dy)) / 4u;   /* 1/4 px */
                if (used >= TAB_MAX) break;
                tab_a[used] = (uint16_t)iatan2(dy, dx);
                tab_r[used] = (uint8_t)(r > 255u ? 255u : r);
                used++;
            }
    }
    /* the ground: deep blue to plum, a vignette, and a soft round shadow under each disc */
    {
        int32_t x, y;
        for (y = 0; y < CLOCK_H; y++)
            for (x = 0; x < 240; x++) {
                int32_t r = 16 + y * 22 / 240, g = 18 + y * 4 / 240, b = 34 + y * 22 / 240, v, s = 0;
                v = (x - 120) * (x - 120) / 900;
                for (i = 0; i < NDISC; i++) {
                    const disc_t *d = &disc[i];
                    int32_t h = 6 + d->z * 3, dx = x * 4 + 2 - (ax_x[d->ax] / 4 + h * 2), dy = y * 4 + 2 - (ax_y[d->ax] / 4 + h * 3);
                    int32_t rad = (d->rr + d->rt) / 2, dd = (int32_t)isqrt((uint32_t)(dx * dx + dy * dy)), c;
                    c = (rad - dd) * 256 / (4 * 4) + 128;          /* a 4 px soft edge */
                    c = c < 0 ? 0 : c > 256 ? 256 : c;
                    if (c > s) s = c;
                }
                r = (r - v) * (256 - s * 3 / 5) >> 8;
                g = (g - v) * (256 - s * 3 / 5) >> 8;
                b = (b - v) * (256 - s * 3 / 5) >> 8;
                bg[y * 240 + x] = col(r, g, b);
            }
    }
}

/* disc d at distance r (1/4 px) and angle a: 0 open, 1 the face, 2 the outline, 3 the hub, 4 the lit rim,
 * 5 the jewel. tests/mesh_test.c turns the train with this. */
static int disc_px(const disc_t *d, int32_t r, uint32_t a)
{
    uint32_t rel = a - ax_th[d->ax], u;
    int32_t us, edge, sv;
    if (r >= d->rt) return 0;
    u = (rel * d->n) & 0xFFFFu;
    us = (int16_t)u;
    if (us < 0) us = -us;
    edge = d->edge[(uint32_t)us >> 7 > 255u ? 255u : (uint32_t)us >> 7];
    if (r >= edge) return 0;
    if (edge - r <= 4) return 2;                /* the outline, 1 px */
    if (r < 3 * 4) return d->top ? 5 : 2;       /* the axle: a jewel on the top disc */
    if (r < d->hub) return d->hub - r <= 4 ? 2 : 3;
    if (d->spokes && r < d->rim + 4) {
        sv = (int16_t)((rel * d->spokes) & 0xFFFFu);
        if (sv < 0) sv = -sv;
        if (r < d->rim && r > d->hub + 4 && sv > d->win[r]) return 0;   /* a window */
        if (sv > d->wino[r]) return 2;          /* its edge */
    }
    if (edge - r <= 12 && (uint32_t)((a - LIGHT + 10000u) & 0xFFFFu) < 20000u) return 4;   /* the lit rim */
    return 1;
}

/* ---------------------------------------------------------------- the title and the status, when they change */
static void glyph_draw(int32_t x, int32_t y, uint32_t gi, int32_t s, int32_t y0, int32_t y1,
                       int32_t cr, int32_t cg, int32_t cb, int32_t alpha)
{   /* glyph gi of FONT_L (or FONT_S when s < 0, unscaled) at (x, y), scale s/256, into strip rows y0..y1 */
    const felucca_font_t *f = s < 0 ? &FONT_S : &FONT_L;
    int32_t sc = s < 0 ? 256 : s, inv = 65536 / sc;
    int32_t w = f->bw[gi], h = f->h, bpr = (w + 1) / 2, dw = (w * sc) >> 8, dh = (h * sc) >> 8, X, Y;
    const uint8_t *gd = f->data + f->off[gi];
    x -= (f->pad * sc) >> 8;
    for (Y = y < y0 ? y0 : y; Y < y + dh + 1 && Y < y1; Y++) {
        int32_t fy = (Y - y) * inv + inv / 2 - 128, gy = fy >> 8, ty = fy & 255;
        for (X = x < 0 ? 0 : x; X < x + dw + 1 && X < 240; X++) {
            int32_t fx = (X - x) * inv + inv / 2 - 128, gx = fx >> 8, tx = fx & 255, v[4], k, a;
            for (k = 0; k < 4; k++) {
                int32_t xx = gx + (k & 1), yy = gy + (k >> 1), b;
                if (xx < 0 || yy < 0 || xx >= w || yy >= h) { v[k] = 0; continue; }
                b = gd[yy * bpr + xx / 2];
                v[k] = (xx & 1) ? (b & 15) : (b >> 4);
            }
            if (fx < 0) tx = 0;
            if (fy < 0) ty = 0;
            a = ((v[0] * (256 - tx) + v[1] * tx) * (256 - ty) + (v[2] * (256 - tx) + v[3] * tx) * ty) >> 16;   /* 0..15 */
            a = (a * 273 >> 4) * alpha >> 8;
            if (a > 0) {
                uint32_t i = (uint32_t)(Y - y0) * 240u + (uint32_t)X;
                int32_t r, g, b;
                px_get(i, &r, &g, &b);
                px_put(i, r + (((cr - r) * a) >> 8), g + (((cg - g) * a) >> 8), b + (((cb - b) * a) >> 8));
            }
        }
    }
}
static void text_small(int32_t x, int32_t y, const char *s, int32_t cr, int32_t cg, int32_t cb, int32_t y0, int32_t y1)
{
    if (y >= y1 || y + 16 <= y0) return;
    for (; *s; s++) {
        uint32_t gi = glyph(&FONT_S, (uint8_t)*s);
        glyph_draw(x, y, gi, -1, y0, y1, cr, cg, cb, 256);
        x += FONT_S.adv[gi];
    }
}

#define STRIP 48
static const char TITLE[] = "TRY HARD";
static const uint8_t TCOL[8][3] = {
    {255, 214, 64}, {255, 156, 52}, {255, 96, 140}, {0, 0, 0}, {178, 124, 255}, {72, 212, 232},
    {124, 232, 124}, {255, 214, 64}};
static int32_t title_s, title_x;                /* the title's scale and left edge */
static int32_t th_kick;                         /* how far the knobs turned the gears since the last frame (Qm units) */
static uint32_t Qm;                             /* the master angle: axle 0 at Qm / 120 */
static char line1[40], line2[40];
static int linked;

/* the gears stand still until a knob turns: then they glide to where the knobs put them */
static void frame_setup(void)
{
    static int32_t target;
    int32_t diff;
    uint32_t a;
    target = (target + th_kick % QWRAP + QWRAP) % QWRAP;
    th_kick = 0;
    {   /* the short way round, in unsigned steps forward: -QWRAP / 2 as a constant looks like a ROM address */
        uint32_t fwd = ((uint32_t)target + (uint32_t)QWRAP - Qm) % (uint32_t)QWRAP;
        diff = fwd > (uint32_t)QWRAP / 2u ? (int32_t)fwd - QWRAP : (int32_t)fwd;
    }
    diff = diff / 3 ? diff / 3 : diff;          /* a third of the way each frame */
    Qm = (uint32_t)(((int32_t)Qm + diff) % QWRAP + QWRAP) % QWRAP;
    for (a = 0; a < NAX; a++)
        ax_th[a] = ((uint32_t)((int32_t)Qm / 120 * K[a] + (int32_t)(Qm % 120u) * K[a] / 120) + ax_c[a]) & 0xFFFFu;
}

/* strip s of the screen (48 rows): the clockwork every frame, the words below only when asked */
static void strip_draw(uint32_t s)
{
    int32_t y0 = (int32_t)s * STRIP, y1 = y0 + STRIP, x, y, i;
    cv_begin(240, STRIP, C_BLACK);
    for (y = y0; y < y1; y++) {
        uint16_t *row = cv_px + (uint32_t)(y - y0) * 240u;
        if (y < CLOCK_H) {
            const uint16_t *b = bg + y * 240;
            for (x = 0; x < 240; x++) row[x] = b[x];
        } else {                                /* (as the ground above, without shadows) */
            for (x = 0; x < 240; x++) {
                int32_t v = (x - 120) * (x - 120) / 900;
                row[x] = col(16 + y * 22 / 240 - v, 18 + y * 4 / 240 - v, 34 + y * 22 / 240 - v);
            }
        }
    }
    for (i = 0; i < NDISC; i++) {               /* bottom up: each disc paints over the ones below */
        const disc_t *d = &disc[ZORD[i]];
        int32_t k = d->ax, ty0 = y0 - tab[k].y0, ty1 = y1 - tab[k].y0, ty;
        if (ty0 < 0) ty0 = 0;
        if (ty1 > tab[k].h) ty1 = tab[k].h;
        for (ty = ty0; ty < ty1; ty++) {
            const uint16_t *ta = tab_a + tab[k].at + (uint32_t)(ty * tab[k].w);
            const uint8_t *tr = tab_r + tab[k].at + (uint32_t)(ty * tab[k].w);
            uint16_t *row = cv_px + (uint32_t)(tab[k].y0 + ty - y0) * 240u;
            int32_t tx, sx = tab[k].x0;
            for (tx = 0; tx < tab[k].w; tx++) {
                int32_t X = sx + tx, p;
                if ((uint32_t)X >= 240u || tr[tx] >= d->rt) continue;
                p = disc_px(d, tr[tx], ta[tx]);
                if (p == 1) row[X] = d->face;
                else if (p == 2) row[X] = d->dark;
                else if (p == 3) row[X] = d->hubc;
                else if (p == 4) row[X] = d->lite;
                else if (p == 5) row[X] = tr[tx] < 4 ? col(255, 200, 210) : col(200, 24, 52);
            }
        }
    }
    if (y1 > CLOCK_H) {                         /* TRY HARD, and the link with what went out last */
        int32_t px0 = title_x, k;
        for (k = 0; TITLE[k]; k++) {
            uint32_t gi = glyph(&FONT_L, (uint8_t)TITLE[k]);
            int32_t ly = 150 + ((k & 1) ? 3 : -3);             /* a playful up and down */
            if (TITLE[k] != ' ') {
                glyph_draw(px0 + 3, ly + 4, gi, title_s, y0, y1, 6, 4, 14, 170);
                glyph_draw(px0, ly, gi, title_s, y0, y1, TCOL[k][0], TCOL[k][1], TCOL[k][2], 256);
            }
            px0 += (FONT_L.adv[gi] * title_s) >> 8;
        }
        for (y = y0 < 206 ? 206 : y0; y < 221 && y < y1; y++)  /* the dot */
            for (x = 8; x < 21; x++)
                if ((x - 14) * (x - 14) + (y - 213) * (y - 213) <= 20)
                    px_put((uint32_t)(y - y0) * 240u + (uint32_t)x, linked ? 90 : 255, linked ? 230 : 170, linked ? 110 : 40);
        text_small(26, 205, line1, linked ? 200 : 230, linked ? 240 : 200, linked ? 210 : 150, y0, y1);
        text_small(26, 222, line2, 130, 120, 160, y0, y1);
    }
    cv_blit(0, (uint32_t)y0);
}

/* ---------------------------------------------------------------- what the panel did, in words */
static const char *const BTN_NAME[14] = {"OCT-", "OCT+", "FX", "SEL", "ENV", "LFO", "EDIT", "GLO", "HOME", "SAVE",
                                         "ARP", "SEQ", "PLAY", "REC"};
static const char *const ENC_NAME[7] = {"SELECT", "ALGORITHM", "KNOB1", "KNOB2", "KNOB3", "KNOB4", "PRESETS"};
static char *put_s(char *p, const char *s) { while (*s) *p++ = *s++; *p = 0; return p; }
static char *put_u(char *p, uint32_t v)
{
    char t[10];
    int n = 0;
    do { t[n++] = (char)('0' + v % 10u); v /= 10u; } while (v);
    while (n) *p++ = t[--n];
    *p = 0;
    return p;
}
static uint32_t sent;
static void said(uint32_t id, int32_t v, int kind)  /* kind 0 key/button, 1 encoder, 2 MASTER */
{
    static const char *const NN[12] = {"C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"};
    char *p = line2;
    if (kind == 0 && id >= 14u) {
        uint32_t note = 53u + id - 14u;         /* key 0 = F3 */
        p = put_s(p, "KEY ");
        p = put_s(p, NN[note % 12u]);
        p = put_u(p, note / 12u - 1u);
    } else if (kind == 0) {
        p = put_s(p, BTN_NAME[id]);
    } else if (kind == 1) {
        p = put_s(p, ENC_NAME[id]);
        p = put_s(p, v < 0 ? " -" : " +");
        p = put_u(p, (uint32_t)(v < 0 ? -v : v));
    } else {
        p = put_s(p, "MASTER ");
        p = put_u(p, (uint32_t)v);
    }
    p = put_s(p, "  #");
    put_u(p, ++sent);
}

/* ---------------------------------------------------------------- the panel out, the LEDs in */
static uint8_t led_want[41], led_set[41];
static uint32_t last_page, last_hello;
static int32_t master_sent = -1000, master_avg = -1;

static void send_master(int32_t v)
{
    midi_send(0xB0u | CH, 7, (uint32_t)v >> 3);
    midi_send(0xB0u | CH, 39, ((uint32_t)v & 7u) << 4);
    master_sent = v;
}

static void panel_out(void)
{
    static uint32_t prev, t_master;
    uint32_t keys = fm1_in.notes, pressed = fm1_input_note_edges(), b, rel, k;
    int32_t v;
    for (k = 0; k < 27u; k++) {
        uint32_t m = 1u << k, id = 14u + k;
        if (pressed & m) {
            if (prev & m) midi_send(0x80u | CH, id, 0);
            midi_send(0x90u | CH, id, 127);
            said(id, 0, 0);
        } else if ((keys & m) && !(prev & m)) {
            midi_send(0x90u | CH, id, 127);
            said(id, 0, 0);
        }
        if (!(keys & m) && ((prev & m) || (pressed & m)))
            midi_send(0x80u | CH, id, 0);
    }
    prev = keys;
    b = fm1_input_edges(&rel);
    for (k = 0; k < 14u; k++) {
        uint32_t m = 1u << k;
        if (b & m) { midi_send(0x90u | CH, k, 127); said(k, 0, 0); }
        if (rel & m) {
            midi_send(0x80u | CH, k, 0);
            if (fm1_in.buttons & m) midi_send(0x90u | CH, k, 127);
        }
    }
    for (k = 0; k < 7u; k++) {
        int32_t d = fm1_enc_take(k);
        if (!d) continue;
        v = 64 + d;
        midi_send(0xB0u | CH, 16u + k, (uint32_t)(v < 1 ? 1 : v > 127 ? 127 : v));
        said(k, d, 1);
        th_kick += d * (QWRAP / 36);             /* a detent: half a tooth of the big wheel, either way */
    }
    if ((v = master_read()) >= 0) {
        master_avg = master_avg < 0 ? v * 16 : master_avg + (v * 16 - master_avg) / 4;
        v = master_avg / 16;
        if ((v - master_sent > 3 || master_sent - v > 3 || (v != master_sent && (v == 0 || v == 1023)))
            && fm1_ms - t_master >= 10u) {
            t_master = fm1_ms;
            if (master_sent >= 0) th_kick += (v - master_sent) * 3000;   /* MASTER turns them too */
            send_master(v);
            said(0, v, 2);
        }
    }
    if (fm1_ms - last_hello >= 1000u) {          /* here I am */
        last_hello = fm1_ms;
        midi_send(0xB0u | CH, HELLO_CC, HELLO_V);
    }
}

static void midi_in(void)
{
    uint8_t st, d1, d2;
    uint32_t k;
    while (midi_get(&st, &d1, &d2)) {
        if ((st & 0x0Fu) != CH) continue;
        if ((st & 0xF0u) == 0x90u && d1 < 41u) led_want[d1] = d2 ? 1u : 0u;
        else if ((st & 0xF0u) == 0x80u && d1 < 41u) led_want[d1] = 0;
        else if ((st & 0xF0u) == 0xB0u && d1 == HELLO_CC) {
            if (!linked) {                      /* a page came: tell it where MASTER is */
                linked = 1;
                if (master_sent >= 0) send_master(master_sent);
            }
            last_page = fm1_ms;
        } else if ((st & 0xF0u) == 0xB0u && d1 == 123u)
            for (k = 0; k < 41u; k++) led_want[k] = 0;
    }
    if (linked && fm1_ms - last_page > 3000u) {  /* the page went away */
        linked = 0;
        for (k = 0; k < 41u; k++) led_want[k] = 0;
    }
}

static void leds(void)
{
    uint32_t k, chase = (fm1_ms / 90u) % 40u;
    for (k = 0; k < 41u; k++) {
        uint8_t on = led_want[k];
        if (!linked) on = k >= 14u && (k - 14u == chase || k - 14u + 1u == chase);   /* waiting: a chase */
        if (on != led_set[k]) { led_set[k] = on; fm1_led_key(k, on); }
    }
}

static int same(const char *a, const char *b) { while (*a && *a == *b) { a++; b++; } return *a == *b; }

static void app_main(void)
{
    uint32_t s = 0, words = 1;
    char drawn1[40] = "", drawn2[40] = "";
    gears_init();
    title_s = (226 * 256) / text_w(&FONT_L, TITLE);
    if (title_s > 2 * 256) title_s = 2 * 256;
    title_x = (240 - ((text_w(&FONT_L, TITLE) * title_s) >> 8)) / 2;
    put_s(line2, "usb-midi channel 16");
    for (;;) {
        platform_poll();
        panel_out();
        midi_in();
        leds();
        put_s(line1, linked ? "linked to the emulator" : "waiting for the page");
        if (s == 0) {
            frame_setup();
            if (!same(line1, drawn1) || !same(line2, drawn2)) {   /* the words changed: draw them too */
                put_s(drawn1, line1);
                put_s(drawn2, line2);
                words = 1;
            }
        }
        strip_draw(s);
        s++;
        if (s * STRIP >= (words ? 240u : (uint32_t)CLOCK_H)) { s = 0; words = 0; }
    }
}
