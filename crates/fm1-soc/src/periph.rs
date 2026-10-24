//! Timed peripherals and the front panel, modelled from what Felucca's HAL
//! (reference/Felucca/firmware/hal/*.h) programs and from the RE docs:
//!
//! * a cycle counter (`hz`, one instruction per cycle; 240 MHz like the
//!   chip, lower for live play) paces everything, and the timers convert
//!   cycles to real time so the firmware's clock stays right at any `hz`;
//! * with `skip_idle`, a tight loop polling TIMER4 outside an ISR (a delay)
//!   jumps the clock to the next timed event instead of spinning;
//! * TIMER4 `0x10800`: free-running 24 MHz time base (CNT at +4);
//! * TIMER5 `0x10900`: 10 kHz tick (PRD at 6 MHz), pending CON bit 15,
//!   acknowledged by writing bit 14, IRQ 63;
//! * ALNK0 `0x12E00`: I2S out, double-buffered DMA of `LEN` int32 words per
//!   half (L/R, a 24-bit sample in the low bits of each word) at 44.1 kHz; a finished half sets
//!   CON2 bit 7 (cleared by writing bit 3), CON0 bit 15 = the half playing,
//!   IRQ 11;
//! * SARADC `0x13100`: CON (start = bit 6, done = bit 7, channel bits 8-11),
//!   RES = 10-bit value — MASTER pot on ch 4, battery on ch 3;
//! * the interrupt controller: enable/priority nibbles at `0x1EEF100` for cpu0 and
//!   `0x1EEF300` for cpu1 (one per source, bit 0 enable, bits 1-3 priority; the sources are
//!   shared, each core takes what its own bank enables), software latch set/clear at
//!   `0x1EEF1A0/1A4` (bit b = IRQ 120+b; the stock RTOS's context switch is bit 7 - cnum, its
//!   cross-core doorbell another bit), one RAM vector table at `0x01C7FE00` for both;
//! * the OS tick block at `0x1EEF0EC` (stock firmware): b[+0] bit 0 enable, bit 6 written =
//!   acknowledge, bit 7 read = pending; `+4` the count (written: restart), `+8` the period
//!   (`os_start`: clk/100, 10 ms); IRQ 3 (FM-1-RE docs/io/02-rtos.md);
//! * UART1 `0x12100` (MIDI IN, the TRS jack): bytes from `Action::Midi` arrive one per
//!   31250-baud byte time (320 us) and are DMA'd into the ring RXSADR..RXEADR. CON0 (the JieLi
//!   layout, SDK uart driver + Felucca hal/fm1_uart.h): bit 0 on, 2/3/5 TX/RX/timeout IE, 6 RX
//!   DMA, written bit 7 latches the bytes received since the last latch into HRXCNT (+0x28),
//!   written bits 10/12/13 clear the timeout / RX / TX pendings (bits 11/14/15). RX pending =
//!   RXCNT bytes since the last latch, timeout = OTCNT UART clocks (48 MHz) of silence after a
//!   byte; IRQ 20. A TX DMA (TXCNT written) finishes at once: no TX pin on the board;
//! * the key/encoder matrix: 11 columns from two 74HC595s (PA4 SER, PA3
//!   SRCLK, PA1 RCLK, MSB first, a low bit selects the column), rows PA0,
//!   PA5-PA8, PB7 with pull-ups (low = closed).
//! * the keys' carbon pills (as Velo, charlesvestal/fm1-velocity, and CS-1 sense them): rows PA7 / PA8 are also SARADC
//!   channels 0 / 1. A conversion there reads the key of that row in the latched column as a
//!   divider: the pill (its resistance falling with the force on it, ~100 ohm pressed firmly)
//!   in series with the matrix diode and the 595's low output (together 186 counts), against the
//!   row's 10k pull-up, or against the pin's own driver when the row is driven high (its strength
//!   from HD0 / HD: ~312 / 132 / 51 / 35 ohm, so that a firm press reads 389 / 547 / 740 / 805 as
//!   CS-1 measured on an FM-1). Open: 1023; driven low: 0. A press (Action::Key, at the velocity
//!   of Action::KeyVel) lands in 1.5 ms at a force of 0.33 + 0.67 vel / 127 (the pill: 100 ohm at
//!   1, ~3k at 0.33: the lightest press that seats, below CS-1's PRESS pulled up, ~990 driven)
//!   and stays there; Action::Pressure presses a held key to another velocity's force over 20 ms;
//!   a release lifts it in 8 ms. A count of noise. The digital scan still sees the key as
//!   pressed from Action::Key to its release. A row that is neither pulled up nor driven reads
//!   adc_value as before.

use std::collections::VecDeque;

/// `Periph::adc_state`: CON, done, IRQ, the sample, the noise generator
pub(crate) type AdcState = (u32, bool, bool, u16, u32);

pub const CPU_HZ: u64 = 240_000_000;
/// TIMER4 counts the 24 MHz crystal; TIMER5 runs from OSC/4.
const T4_HZ: u64 = 24_000_000;

/// a * b / c, as u32: in u64 while the product fits (TIMER4's count: 2^64 / 24 M cycles, 53
/// minutes at 240 MHz), else in u128, the same quotient (a u128 division is a library call;
/// firmware polls these counts)
#[inline]
fn mul_div(a: u64, b: u64, c: u64) -> u32 {
    match a.checked_mul(b) {
        Some(p) => (p / c) as u32,
        None => (a as u128 * b as u128 / c as u128) as u32,
    }
}
const T5_HZ: u64 = 6_000_000;
/// TIMER4 ticks per I2S frame (Felucca audio.c DAC_TICKS: 44.1 kHz).
const FRAME_T4: u64 = 544;

pub const IRQ_ALNK0: u8 = 11;
pub const IRQ_TIMER5: u8 = 63;
pub const IRQ_OS_TICK: u8 = 3;
pub const IRQ_SPI1: u8 = 16;
pub const IRQ_SARADC: u8 = 24;
pub const IRQ_SPI2: u8 = 37;
pub const IRQ_UART1: u8 = 20;
/// The UART's clock (CLK_CON1 selects PLL48M; OTCNT counts it).
const UART_HZ: u64 = 48_000_000;
/// Bits per MIDI byte on the wire (start, 8 data, stop) and the baud rate.
const MIDI_BYTE_BITS: u64 = 10;
const MIDI_BAUD: u64 = 31_250;
/// The most MIDI bytes waiting behind the jack (about a second at 31250 baud).
const MIDI_QUEUE_MAX: usize = 3125;

/// UART1's receive side (see the module notes).
#[derive(Default)]
struct Uart {
    con0: u32,
    rxsadr: u32,
    rxeadr: u32,
    rxcnt: u32,
    otcnt: u32,
    hrxcnt: u32,
    /// Bytes DMA'd since the last RDC latch.
    since: u32,
    /// The DMA's next write offset into the ring.
    wr: u32,
    rpnd: bool,
    otpnd: bool,
    tpnd: bool,
    /// Bytes on their way in, the cycle the next one has arrived, and when the timeout fires.
    q: VecDeque<u8>,
    next: u64,
    ot_next: u64,
    /// Bytes dropped: arrived with the UART off or without a ring, or past the queue's cap.
    dropped: u64,
}

/// The JL_TIMER prescaler table: CON bits 4-7 index it (the stock firmware's own copy at
/// 0x0204F8C0 reads the same).
const TMR_DIV: [u64; 16] = [1, 4, 16, 64, 2, 8, 32, 128, 256, 1024, 4096, 16384, 512, 2048, 8192, 32768];

/// TIMER0..3 (`0x10400 + 0x100 * n`, IRQ 4 + n): CON (bit 0 on; low nibble 9 = the 24 MHz
/// crystal, bits 4-7 the prescaler, bit 15 pending, written bit 14 clears it), CNT (where it
/// counts from), PRD (the count that raises the interrupt and restarts). The stock firmware
/// runs its usr_timer engine (the 2 ms ADC scan, key scan) on TIMER1.
#[derive(Clone, Copy, Default)]
struct Tmr {
    con: u32,
    prd: u32,
    cnt0: u32,
    start: u64,
    next: u64,
    pending: bool,
}
pub const VEC_BASE: u32 = 0x01C7_FE00;

/// Key id at (packed row bit, physical column), -1 = none
/// (hal/fm1_input.h FM1_KEYMAP). Ids 0..13 are buttons, 14..40 note keys
/// (F3 .. G5).
const KEYMAP: [[i8; 11]; 6] = [
    [-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
    [5, 11, 4, 10, 3, 9, 2, 8, -1, -1, -1],
    [34, 35, 36, 37, 38, 40, 39, 13, 7, 6, 12],
    [23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33],
    [0, 1, 15, 14, 17, 16, 19, 18, 20, 21, 22],
    [-1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1],
];
/// Encoder i: contact A at (col, row), B at (col, row) (FM1_ENC).
const ENC: [[u8; 4]; 7] = [
    [0, 0, 1, 0],
    [2, 0, 3, 0],
    [8, 1, 9, 1],
    [8, 0, 9, 0],
    [6, 0, 7, 0],
    [4, 0, 5, 0],
    [0, 5, 1, 5],
];
/// Quadrature states in turning order: (A, B).
const GRAY: [(u8, u8); 4] = [(0, 0), (1, 0), (1, 1), (0, 1)];

pub struct Periph {
    pub cycles: u64,
    /// Emulated CPU clock (cycles per second of firmware time).
    pub hz: u64,
    /// Jump over TIMER4 polling loops (see the module notes).
    pub skip_idle: bool,
    /// Set by the runner while cpu0 is inside an ISR (no skipping there).
    pub in_isr: bool,
    last_cnt_read: u64,
    /// Consecutive close TIMER4 reads outside ISRs (a poll needs 3).
    cnt_streak: u32,
    /// Nothing timed happens before this cycle (0 = recompute): `advance`
    /// returns at once until then.
    pub(crate) next_due: u64,
    /// At most this many ticks per stretch of compiled code (`tick_room`): with two cores the
    /// JIT must hand back after a slice, or a loop waiting on cpu1 spins until the next event
    /// (up to 2^30 instructions) before cpu1 runs at all.
    pub room_cap: u64,
    /// Cycles jumped over by `skip_idle`.
    pub skipped: u64,
    // TIMER4
    t4_con: u32,
    t4_base: u64,
    // TIMER5
    t5_con: u32,
    t5_prd: u32,
    t5_next: u64,
    t5_pending: bool,
    // ALNK0
    alnk_con0: u32,
    alnk_con1: u32,
    alnk_con3: u32,
    alnk_pend: u32,
    alnk_adr: u32,
    alnk_len: u32,
    alnk_half: u32,
    alnk_next: u64,
    /// Interleaved L/R 16-bit samples played so far.
    pub audio: Vec<i16>,
    pub audio_halves: u64,
    // SARADC
    adc_con: u32,
    pub adc_value: [u16; 16],
    // interrupt controller
    pub ilat: u32,
    /// The CPU exception (interrupt 1) is pending: a guard tripped (`Soc::guard_trip`); taken
    /// once (the runner clears it as it enters the handler)
    pub exc_pending: bool,
    /// Enable/priority words at 0x1EEF100 (one nibble per source).
    pub icfg: [u32; 32],
    /// cpu1's enable/priority words at 0x1EEF300.
    pub icfg1: [u32; 32],
    /// The OS tick block at 0x1EEF0EC (enable byte, period, when it started, next tick).
    tick_ctl: u32,
    tick_prd: u32,
    tick_start: u64,
    tick_next: u64,
    tick_pending: bool,
    /// SPI1 finished a transfer with its interrupt enabled (CON bit 13); CON bit 14 clears.
    pub spi1_pending: bool,
    /// SMK-37: the cycle SPI1's transfer in progress ends at (CON's done bit reads set from
    /// then), and its interrupt's (u64::MAX: none). Its stock firmware redraws as often as the
    /// screen's DMA lets it: finishing at once, ~185 frames a second through the LCD model.
    pub spi1_done_at: u64,
    spi1_irq_at: u64,
    /// SPI1's transfers' time on the wire, all told (cycles; --shot reports it against the run)
    pub spi1_wire: u64,
    /// SARADC: a conversion finished (CON bit 7), and its IRQ 24 when CON bit 5 enabled it.
    adc_done: bool,
    adc_irq: bool,
    /// SMK-37: the cycle a conversion in progress finishes at (u64::MAX: none). Its stock
    /// firmware chains conversions through IRQ 24 (each handler starts the next): finishing at
    /// once, the next interrupt was due as the handler returned, ~2.4 M a second, and the main
    /// code never ran.
    adc_next: u64,
    /// SPI2 finished a transfer with its interrupt enabled (the stock key scan), and when
    /// the transfer under way finishes.
    pub spi2_pending: bool,
    spi2_next: u64,
    /// USB frames (usb.rs): the next, and cycles per millisecond; MAX = no cable.
    pub(crate) usb_next: u64,
    pub(crate) usb_period: u64,
    /// The cycle the watchdog runs out at (p33.rs; MAX: stopped).
    pub(crate) wdt_at: u64,
    tmr: [Tmr; 4],
    uart: Uart,
    /// The UART's interrupt condition (enabled and pending), kept for `any_pending`.
    uart_irq: bool,
    // key matrix
    sr_shift: u16,
    sr_latched: u16,
    pa_out: u32,
    ph_out: u32,
    /// Front-panel LEDs: cycles each key/button LED has been lit, and the
    /// cycle of the last change of the LED lines or the column latch.
    led_acc: [u64; 41],
    /// The LED state (lines, latched column) credited last and the time pending for it.
    led_key: (u32, u16),
    led_pend: u64,
    led_last: u64,
    led_since: u64,
    pub keys_down: [bool; 41],
    /// The keys' force (the module notes): a ramp from `from` to `to` over `dur` cycles from `t0`
    key_force: [Ramp; 41],
    /// FM-1: the velocity of the next presses (Action::KeyVel; 1..127)
    pub key_vel: u8,
    /// Port A's direction (1 = input), pull-ups and drive strength, for the rows' readings
    pa_dir: u32,
    pa_pu: u32,
    pa_hd0: u32,
    pa_hd: u32,
    adc_noise: u32,
    enc_state: [u8; 8],
    enc_pending: [i32; 8],
    enc_next: [u64; 8],
    /// The panel around the chip: the FM-1's (above), or the SMK-37's (`smk`).
    pub board: crate::Board,
    pub smk: crate::smk::Panel,
    /// SMK-37: cycles between a key's first and second contact (its velocity; less is harder).
    pub smk_vel: u64,
    /// SMK-37: the ADC's last conversion (taken when it starts, from the mux then latched).
    adc_sample: u16,
    /// Scheduled front-panel actions: (cycle, action).
    pub script: VecDeque<(u64, Action)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Key { id: u8, down: bool },
    /// Turn encoder `enc` by `clicks` detents (+ = clockwise).
    Enc { enc: u8, clicks: i32 },
    /// Set ADC channel `ch` to `value` (0..1023).
    Adc { ch: u8, value: u16 },
    /// One MIDI byte into the MIDI IN jack (UART1); it arrives a byte time after the one before.
    Midi { b: u8 },
    /// SMK-37: the next keys' velocity, as microseconds between their two contacts.
    Vel { us: u32 },
    /// FM-1: the next keys' velocity (1..127): how hard they land on their pills.
    KeyVel { vel: u8 },
    /// FM-1: a held key pressed to the force of velocity `vel` (aftertouch; 1..127).
    Pressure { id: u8, vel: u8 },
}

/// A key's force: from `from` to `to` (0..1) over `dur` cycles from cycle `t0`.
#[derive(Debug, Clone, Copy, Default)]
struct Ramp {
    from: f32,
    to: f32,
    t0: u64,
    dur: u64,
}

impl Ramp {
    fn at(&self, t: u64) -> f32 {
        if self.dur == 0 || t >= self.t0 + self.dur {
            return self.to;
        }
        let x = t.saturating_sub(self.t0) as f32 / self.dur as f32;
        self.from + (self.to - self.from) * x
    }
}

/// The force a key lands at for velocity `vel` (1..127)
fn vel_force(vel: u8) -> f32 {
    0.33 + 0.67 * vel.clamp(1, 127) as f32 / 127.0
}

/// The ADC reading of a row with a key of force `f` on it (0: open, buttons 1) against the
/// source resistance `rs` (the pull-up or the pin driver), in counts
fn pill_reading(f: f32, rs: f32) -> f32 {
    if f <= 0.001 {
        return 1023.0;
    }
    let r = 100.0 * f.min(1.0).powf(-3.06);
    186.0 + 837.0 * r / (r + rs)
}

impl Default for Periph {
    fn default() -> Self {
        let mut adc_value = [512u16; 16];
        adc_value[3] = 640; // battery: full (ui_draw.c: >= 591 is 3 bars)
        adc_value[4] = 900; // MASTER pot
        Self {
            cycles: 0,
            hz: CPU_HZ,
            skip_idle: false,
            in_isr: false,
            last_cnt_read: 0,
            cnt_streak: 0,
            next_due: 0,
            skipped: 0,
            room_cap: u64::MAX,
            t4_con: 0,
            t4_base: 0,
            t5_con: 0,
            t5_prd: 0,
            t5_next: u64::MAX,
            t5_pending: false,
            alnk_con0: 0,
            alnk_con1: 0,
            alnk_con3: 0,
            alnk_pend: 0,
            alnk_adr: 0,
            alnk_len: 0,
            alnk_half: 0,
            alnk_next: u64::MAX,
            audio: Vec::new(),
            audio_halves: 0,
            adc_con: 0,
            adc_value,
            ilat: 0,
            exc_pending: false,
            icfg: [0; 32],
            icfg1: [0; 32],
            tick_ctl: 0,
            tick_prd: 0,
            tick_start: 0,
            tick_next: u64::MAX,
            tick_pending: false,
            spi1_pending: false,
            spi1_done_at: 0,
            spi1_irq_at: u64::MAX,
            spi1_wire: 0,
            adc_done: false,
            adc_irq: false,
            adc_next: u64::MAX,
            spi2_pending: false,
            spi2_next: u64::MAX,
            usb_next: u64::MAX,
            usb_period: 0,
            wdt_at: u64::MAX,
            tmr: [Tmr { next: u64::MAX, ..Default::default() }; 4],
            uart: Uart { next: u64::MAX, ot_next: u64::MAX, ..Default::default() },
            uart_irq: false,
            sr_shift: 0xffff,
            sr_latched: 0xffff,
            pa_out: 0,
            ph_out: 0,
            led_acc: [0; 41],
            led_key: (0, 0),
            led_pend: 0,
            led_last: 0,
            led_since: 0,
            keys_down: [false; 41],
            key_force: [Ramp::default(); 41],
            key_vel: 100,
            pa_dir: 0,
            pa_pu: 0,
            pa_hd0: 0,
            pa_hd: 0,
            adc_noise: 1,
            enc_state: [0; 8],
            enc_pending: [0; 8],
            enc_next: [0; 8],
            board: crate::Board::Fm1,
            smk: crate::smk::Panel::default(),
            smk_vel: CPU_HZ / 1000 * 3,
            adc_sample: 0,
            script: VecDeque::new(),
        }
    }
}

/// What the cycle advance found due; the caller moves the audio half out
/// of RAM (and a received UART byte into it) because the peripheral block has no bus of its own.
#[derive(Debug, Default)]
pub struct Due {
    /// A half buffer finished playing: (RAM address, words).
    pub audio_half: Option<(u32, u32)>,
    /// A UART byte the RX DMA wrote: (address, byte).
    pub uart_rx: Option<(u32, u8)>,
    /// A USB frame is due (the SoC runs the host's millisecond: usb.rs).
    pub usb_frame: bool,
    /// The watchdog ran out (the SoC resets the chip).
    pub wdt: bool,
    pub actions: Vec<Action>,
}

impl Periph {
    pub fn ms(&self) -> u64 {
        self.cycles * 1000 / self.hz
    }

    pub fn cycle_of_ms(&self, ms: u64) -> u64 {
        ms * self.hz / 1000
    }

    fn t5_period(&self) -> u64 {
        (self.t5_prd.max(1) as u64 * self.hz / T5_HZ).max(1)
    }

    /// CPU cycles per count of TIMER0..3 `i` (the crystal through the prescaler).
    fn tmr_cycles_per_count(&self, i: usize) -> u64 {
        TMR_DIV[((self.tmr[i].con >> 4) & 15) as usize] * self.hz / T4_HZ
    }

    fn tmr_arm(&mut self, i: usize) {
        let t = self.tmr[i];
        if t.con & 3 == 0 || t.prd == 0 {
            self.tmr[i].next = u64::MAX;
            return;
        }
        let left = t.prd.saturating_sub(t.cnt0).max(1) as u64;
        self.tmr[i].start = self.cycles;
        self.tmr[i].next = self.cycles + left * self.tmr_cycles_per_count(i).max(1);
    }

    /// The OS tick: 10 ms however the period register is scaled (os_start writes clk/100).
    fn tick_period(&self) -> u64 {
        (self.hz / 100).max(1)
    }

    fn half_period(&self) -> u64 {
        (self.alnk_len.max(2) as u64 / 2 * FRAME_T4 * self.hz / T4_HZ).max(1)
    }

    fn enc_step(&self) -> u64 {
        // contacts move one quadrature state every 2 ms (SMK-37: 4 ms, the port's decoder takes a
        // state once it has held for two 1.6 ms scan frames)
        if self.board == crate::Board::Smk37 { self.hz / 250 } else { self.hz / 500 }
    }

    /// The next cycle at which something timed happens.
    fn next_event(&self) -> u64 {
        let mut n = self.t5_next.min(self.alnk_next).min(self.tick_next).min(self.spi2_next).min(self.adc_next).min(self.spi1_irq_at);
        n = n.min(self.uart.next).min(self.uart.ot_next).min(self.usb_next).min(self.wdt_at);
        for t in &self.tmr {
            n = n.min(t.next);
        }
        if let Some((at, _)) = self.script.front() {
            n = n.min(*at);
        }
        for i in 0..8 {
            if self.enc_pending[i] != 0 {
                n = n.min(self.enc_next[i]);
            }
        }
        n
    }

    /// Advance `n` cycles; returns what came due.
    pub fn advance(&mut self, n: u64) -> Due {
        self.cycles += n;
        let mut due = Due::default();
        if self.cycles < self.next_due {
            return due;
        }
        if self.cycles >= self.t5_next {
            self.t5_pending = true;
            let period = self.t5_period();
            self.t5_next += period;
            if self.t5_next <= self.cycles {
                self.t5_next = self.cycles + period;
            }
        }
        if self.cycles >= self.spi2_next {
            self.spi2_pending = true;
            self.spi2_next = u64::MAX;
        }
        if self.cycles >= self.spi1_irq_at {
            self.spi1_irq_at = u64::MAX;
            self.spi1_pending = true;
        }
        if self.cycles >= self.adc_next {
            self.adc_next = u64::MAX;
            self.adc_done = true;
            self.adc_irq = self.adc_con & 0x20 != 0;
        }
        for i in 0..4 {
            if self.cycles >= self.tmr[i].next {
                self.tmr[i].pending = true;
                self.tmr[i].cnt0 = 0;
                let per = self.tmr[i].prd.max(1) as u64 * self.tmr_cycles_per_count(i).max(1);
                self.tmr[i].start = self.tmr[i].next;
                self.tmr[i].next += per;
                if self.tmr[i].next <= self.cycles {
                    self.tmr[i].next = self.cycles + per;
                }
            }
        }
        if self.cycles >= self.tick_next {
            self.tick_pending = true;
            self.tick_start = self.tick_next;
            self.tick_next += self.tick_period();
            if self.tick_next <= self.cycles {
                self.tick_next = self.cycles + self.tick_period();
            }
        }
        if self.cycles >= self.alnk_next {
            let words = self.alnk_len.max(2);
            due.audio_half = Some((self.alnk_adr.wrapping_add(self.alnk_half * words * 4), words));
            self.alnk_half ^= 1;
            // every channel CON1 sets up pends with the block: FM1 Open's driver takes its
            // interrupt from the receive channel 0 (pending bit 4), the stock one from transmit
            // channel 3 (bit 7)
            for ch in 0..4 {
                if (self.alnk_con1 >> (4 * ch)) & 0xf != 0 {
                    self.alnk_pend |= 0x10 << ch;
                }
            }
            self.alnk_pend |= 0x80;
            self.alnk_next += self.half_period();
            self.audio_halves += 1;
        }
        if self.cycles >= self.uart.next {
            due.uart_rx = self.uart_receive();
        }
        if self.cycles >= self.usb_next {
            due.usb_frame = true;
            self.usb_next += self.usb_period;
            if self.usb_next <= self.cycles {
                self.usb_next = self.cycles + self.usb_period;
            }
        }
        if self.cycles >= self.wdt_at {
            self.wdt_at = u64::MAX;
            due.wdt = true;
        }
        if self.cycles >= self.uart.ot_next {
            self.uart.ot_next = u64::MAX;
            if self.uart.con0 & 1 != 0 {
                self.uart.otpnd = true;
                self.uart_update();
            }
        }
        for i in 0..8 {
            if self.enc_pending[i] != 0 && self.cycles >= self.enc_next[i] {
                let s = self.enc_state[i] as i32;
                self.enc_state[i] = ((s + self.enc_pending[i].signum()) & 3) as u8;
                self.enc_pending[i] -= self.enc_pending[i].signum();
                self.enc_next[i] = self.cycles + self.enc_step();
            }
        }
        while let Some((at, _)) = self.script.front() {
            if *at > self.cycles {
                break;
            }
            let (_, a) = self.script.pop_front().unwrap();
            self.apply(&a);
            due.actions.push(a);
        }
        self.next_due = self.next_event();
        due
    }

    /// A chip reset: the chip's own state as at power-on. What is outside it stays: the clock
    /// and the runner's settings, the audio played so far, the keys, encoders and pots, the key
    /// matrix's shift registers (74HC595s: the columns they hold), the SMK-37's panel, the LEDs'
    /// light so far, the script, the UART's bytes on their way in and a USB cable (its frames).
    /// A field added to `Periph` is reset unless it is named here.
    pub(crate) fn chip_reset(&mut self) {
        let mut uart = Uart { next: self.uart.next, ot_next: u64::MAX, ..Default::default() };
        (uart.q, uart.dropped) = (std::mem::take(&mut self.uart.q), self.uart.dropped);
        let old = std::mem::take(self);
        *self = Periph {
            cycles: old.cycles,
            hz: old.hz,
            skip_idle: old.skip_idle,
            room_cap: old.room_cap,
            skipped: old.skipped,
            audio: old.audio,
            audio_halves: old.audio_halves,
            adc_value: old.adc_value,
            usb_next: old.usb_next,
            usb_period: old.usb_period,
            uart,
            sr_shift: old.sr_shift,
            sr_latched: old.sr_latched,
            led_acc: old.led_acc,
            led_last: old.led_last,
            led_since: old.led_since,
            keys_down: old.keys_down,
            key_force: old.key_force,
            key_vel: old.key_vel,
            enc_state: old.enc_state,
            enc_pending: old.enc_pending,
            enc_next: old.enc_next,
            board: old.board,
            smk: old.smk,
            smk_vel: old.smk_vel,
            script: old.script,
            ..Default::default()
        };
    }

    pub fn apply(&mut self, a: &Action) {
        self.next_due = 0;
        match *a {
            Action::Key { id, down } => {
                if self.board == crate::Board::Smk37 {
                    self.smk.key(id, down, self.cycles, self.smk_vel);
                } else if (id as usize) < 41 {
                    let (i, t) = (id as usize, self.cycles);
                    if self.keys_down[i] != down {
                        let (to, ms) = if down { (if id < 14 { 1.0 } else { vel_force(self.key_vel) }, 1.5) } else { (0.0, 8.0) };
                        self.key_force[i] = Ramp { from: self.key_force[i].at(t), to, t0: t, dur: self.ms_cycles(ms) };
                    }
                    self.keys_down[i] = down;
                }
            }
            Action::KeyVel { vel } => self.key_vel = vel.clamp(1, 127),
            Action::Pressure { id, vel } => {
                let (i, t) = (id as usize, self.cycles);
                if i < 41 && self.keys_down[i] && id >= 14 {
                    self.key_force[i] = Ramp { from: self.key_force[i].at(t), to: vel_force(vel), t0: t, dur: self.ms_cycles(20.0) };
                }
            }
            Action::Enc { enc, clicks } => {
                let n = if self.board == crate::Board::Smk37 { crate::smk::NENC } else { 7 };
                if (enc as usize) < n {
                    // one detent = one full quadrature cycle (4 transitions);
                    // clockwise runs the Gray sequence backwards (Felucca
                    // flips the stock decoder's sign: + = clockwise)
                    self.enc_pending[enc as usize] -= clicks * 4;
                    self.enc_next[enc as usize] = self.cycles;
                }
            }
            Action::Adc { ch, value } if self.board == crate::Board::Smk37 => self.smk.set_analog(ch, value),
            Action::Vel { us } => self.smk_vel = (self.hz as u128 * us as u128 / 1_000_000) as u64,
            Action::Adc { ch, value } => {
                if (ch as usize) < 16 {
                    self.adc_value[ch as usize] = value.min(1023);
                }
            }
            Action::Midi { b } => {
                // a USB keyboard can send faster than the 31250-baud jack takes it: past a
                // second's worth of backlog the bytes are dropped rather than played late
                if self.uart.q.len() >= MIDI_QUEUE_MAX {
                    self.uart.dropped += 1;
                    return;
                }
                if self.uart.q.is_empty() {
                    self.uart.next = self.uart.next.min(self.cycles + self.midi_byte_time());
                }
                self.uart.q.push_back(b);
            }
        }
    }

    // ---- UART1 (MIDI IN) ------------------------------------------------------

    fn midi_byte_time(&self) -> u64 {
        (self.hz * MIDI_BYTE_BITS / MIDI_BAUD).max(1)
    }

    /// The next queued byte has arrived: where the RX DMA writes it (None: dropped).
    fn uart_receive(&mut self) -> Option<(u32, u8)> {
        let b = self.uart.q.pop_front()?;
        self.uart.next = if self.uart.q.is_empty() { u64::MAX } else { self.cycles + self.midi_byte_time() };
        let u = &mut self.uart;
        let len = u.rxeadr.wrapping_sub(u.rxsadr);
        if u.con0 & 0x41 != 0x41 || len == 0 || len > 0x10000 {
            u.dropped += 1;
            return None;
        }
        let at = u.rxsadr + u.wr % len;
        u.wr = (u.wr + 1) % len;
        u.since += 1;
        if u.rxcnt != 0 && u.since >= u.rxcnt {
            u.rpnd = true;
        }
        // the timeout: OTCNT UART clocks after the last byte (at least a byte time)
        let ot = (u.otcnt as u64 * self.hz / UART_HZ).max(self.midi_byte_time());
        self.uart.ot_next = self.cycles + ot;
        self.uart_update();
        Some((at, b))
    }

    fn uart_update(&mut self) {
        let u = &self.uart;
        self.uart_irq = (u.con0 & 0x08 != 0 && u.rpnd) || (u.con0 & 0x20 != 0 && u.otpnd) || (u.con0 & 0x04 != 0 && u.tpnd);
    }

    /// MIDI bytes waiting to arrive (the queue behind the jack).
    pub fn midi_queued(&self) -> usize {
        self.uart.q.len()
    }

    /// MIDI bytes dropped (no receiver set up: the UART off or no DMA ring; or a full queue).
    pub fn midi_dropped(&self) -> u64 {
        self.uart.dropped
    }

    /// The SARADC's state as a polling loop can change it (`machine`'s loop skip): CON, done,
    /// the IRQ line, and what a conversion start sets: the sample (a row's key force, which
    /// ramps) and the noise generator (it moves on every row conversion, so rounds that convert
    /// a row never repeat, and a failed probe's undo puts it back).
    pub(crate) fn adc_state(&self) -> AdcState {
        (self.adc_con, self.adc_done, self.adc_irq, self.adc_sample, self.adc_noise)
    }

    pub(crate) fn set_adc_state(&mut self, (con, done, irq, sample, noise): AdcState) {
        (self.adc_con, self.adc_done, self.adc_irq, self.adc_sample, self.adc_noise) = (con, done, irq, sample, noise);
    }

    /// One cycle; true when something timed is due (call `advance(0)`).
    #[inline(always)]
    pub fn tick(&mut self) -> bool {
        self.cycles += 1;
        self.cycles >= self.next_due
    }

    /// Ticks that can pass before one is due (a tick is due once cycles reach next_due).
    #[inline]
    pub fn tick_room(&self) -> u64 {
        self.next_due.saturating_sub(self.cycles + 1).min(self.room_cap)
    }

    #[inline]
    pub fn next_tick_due(&self) -> bool {
        self.cycles + 1 >= self.next_due
    }

    /// Any interrupt source pending at all (before enables and priorities).
    #[inline(always)]
    pub fn any_pending(&self) -> bool {
        self.t5_pending || self.alnk_pend & 0xf0 != 0 || self.ilat != 0 || self.tick_pending || self.spi1_pending || self.exc_pending
            || self.tmr.iter().any(|t| t.pending) || self.adc_irq || self.spi2_pending || self.uart_irq
    }

    /// The highest-priority source cpu0 has enabled and that is pending: (irq, prio).
    pub fn pending_irq(&self) -> Option<(u8, u8)> {
        self.pending_irq_for(0)
    }

    /// The same for core `core` (its own enable bank).
    pub fn pending_irq_for(&self, core: u32) -> Option<(u8, u8)> {
        let bank = if core == 0 { &self.icfg } else { &self.icfg1 };
        let icfg = |n: u8| bank[(n >> 3) as usize];
        if !self.any_pending() {
            return None;
        }
        let mut best: Option<(u8, u8)> = None;
        let mut consider = |n: u8| {
            let nib = (icfg(n) >> ((n & 7) * 4)) & 0xf;
            if nib & 1 != 0 {
                let prio = ((nib >> 1) & 7) as u8;
                if best.map_or(true, |(_, p)| prio > p) {
                    best = Some((n, prio));
                }
            }
        };
        if self.exc_pending && core == 0 {
            consider(1); // the CPU exception (a guard: `Soc::guard_trip`)
        }
        if self.t5_pending {
            consider(IRQ_TIMER5);
        }
        if self.tick_pending {
            consider(IRQ_OS_TICK);
        }
        if self.spi1_pending {
            consider(IRQ_SPI1);
        }
        if self.adc_irq {
            consider(IRQ_SARADC);
        }
        if self.spi2_pending {
            consider(IRQ_SPI2);
        }
        if self.uart_irq {
            consider(IRQ_UART1);
        }
        for i in 0..4 {
            if self.tmr[i].pending {
                consider(4 + i as u8);
            }
        }
        if self.alnk_pend & 0xf0 != 0 && self.alnk_con0 & 0x800 != 0 {
            consider(IRQ_ALNK0);
        }
        for b in 0..8u8 {
            if self.ilat & (1 << b) != 0 {
                consider(120 + b);
            }
        }
        best
    }

    // ---- registers ------------------------------------------------------

    /// Reads handled here; `None` = plain SFR storage.
    pub fn read(&mut self, addr: u32) -> Option<u32> {
        Some(match addr {
            0x10800 => self.t4_con,
            0x10804 => {
                if self.t4_con & 1 == 0 {
                    return Some(0);
                }
                // three reads close together outside ISRs: a polling loop.
                // (Reads inside an ISR neither count nor trigger: a single
                // main-loop read right after the tick ISR's own read once
                // skipped time in ui_input and the notes went silent.)
                if self.skip_idle && !self.in_isr {
                    if self.cycles - self.last_cnt_read < 64 {
                        self.cnt_streak += 1;
                    } else {
                        self.cnt_streak = 0;
                    }
                    if self.cnt_streak >= 2 {
                        // never past the next event, and at most 20 us at a
                        // time: the loop re-checks its own deadline (an
                        // uncapped jump once crossed a whole boot delay)
                        let next = self.next_event().min(self.cycles + self.hz / 50_000);
                        if next > self.cycles + 1 {
                            self.skipped += next - 1 - self.cycles;
                            self.cycles = next - 1;
                            self.next_due = 0;
                        }
                    }
                    self.last_cnt_read = self.cycles;
                }
                mul_div(self.cycles - self.t4_base, T4_HZ, self.hz)
            }
            0x10400 | 0x10500 | 0x10600 | 0x10700 => {
                let t = &self.tmr[((addr - 0x10400) >> 8) as usize];
                t.con | if t.pending { 0x8000 } else { 0 }
            }
            0x10404 | 0x10504 | 0x10604 | 0x10704 => {
                let i = ((addr - 0x10400) >> 8) as usize;
                let t = self.tmr[i];
                if t.next == u64::MAX {
                    t.cnt0
                } else {
                    t.cnt0 + ((self.cycles - t.start) / self.tmr_cycles_per_count(i).max(1)) as u32
                }
            }
            0x10408 | 0x10508 | 0x10608 | 0x10708 => self.tmr[((addr - 0x10400) >> 8) as usize].prd,
            0x10900 => self.t5_con | if self.t5_pending { 0x8000 } else { 0 },
            0x10904 => {
                if self.t5_con & 1 != 0 && self.t5_next != u64::MAX {
                    let period = self.t5_period();
                    let into = period - (self.t5_next - self.cycles).min(period);
                    ((into * T5_HZ / self.hz) % self.t5_prd.max(1) as u64) as u32
                } else {
                    0
                }
            }
            0x10908 => self.t5_prd,
            0x12e00 => (self.alnk_con0 & !0x8000) | (self.alnk_half << 15),
            0x12e04 => self.alnk_con1,
            0x12e08 => self.alnk_pend,
            0x12e0c => self.alnk_con3,
            0x12e1c => self.alnk_adr,
            0x12e20 => self.alnk_len,
            // conversions are instant: done (bit 7) once started. Bit 4 reads as written
            // (Felucca's `|= 0x40` read-modify-write must keep it); the stock scan's "skip
            // while bit 4" passes because its IRQ 24 handler writes CON = 0x40, then 0.
            0x13100 => (self.adc_con & !0x80) | if self.adc_done { 0x80 } else { 0 },
            // RES: the conversion's value, taken when it started (the FM-1's rows: their key then)
            0x13104 => self.adc_sample as u32,
            // the latch's set word reads the pending bits; its clear word reads 0: the stock
            // IRQ 122 handler acknowledges with `[0x1EEF1A4] |= 4`, and a read-back of every
            // pending bit there cleared a pending context switch too (a task ran on after
            // blocking and was queued on its wait list twice)
            0x1eef1a0 => self.ilat,
            0x1eef1a4 => 0,
            0x1eef100..=0x1eef17f => self.icfg[((addr - 0x1eef100) / 4) as usize],
            0x1eef300..=0x1eef37f => self.icfg1[((addr - 0x1eef300) / 4) as usize],
            0x1eef0ec => self.tick_ctl | if self.tick_pending { 0x80 } else { 0 },
            0x1eef0f0 => {
                // how far into the period, in the period register's units
                if self.tick_next == u64::MAX {
                    0
                } else {
                    let into = (self.cycles - self.tick_start).min(self.tick_period());
                    mul_div(into, self.tick_prd as u64, self.tick_period())
                }
            }
            0x1eef0f4 => self.tick_prd,
            0x12100 => {
                let u = &self.uart;
                u.con0 | (u.otpnd as u32) << 11 | (u.rpnd as u32) << 14 | (u.tpnd as u32) << 15
            }
            0x1211c => self.uart.rxsadr,
            0x12120 => self.uart.rxeadr,
            0x12124 => self.uart.rxcnt,
            0x12110 => self.uart.otcnt,
            0x12128 => self.uart.hrxcnt,
            0x50004 if self.board == crate::Board::Smk37 => self.smk_pa_in(),
            0x501c4 if self.board == crate::Board::Smk37 => self.smk_ph_in(),
            0x50004 => self.pa_in(),
            0x50044 => self.pb_in(),
            _ => return None,
        })
    }

    /// Writes handled here; returns false for plain SFR storage.
    pub fn write(&mut self, addr: u32, v: u32) -> bool {
        // a timer or DMA may have been (re)programmed: the runner services the SoC before the
        // next instruction (leaving compiled code). Not for the FM-1's ADC: its conversions are
        // instant, so a write there changes only whether IRQ 24 is raised (then it does).
        // OberVave converts ~1 M times a second, 5 writes each: every one left the JIT.
        // Nor for the port pins (the key matrix's shift registers, the LED lines: nothing timed;
        // Felucca and its forks scan the matrix with ~0.5 M writes a second) but PD0, the flash
        // chip select, nor for the counters' words that ignore writes (the stock firmware's core
        // 1 writes TIMER5's ~1.3 M times a second).
        let untimed = matches!(addr, 0x13100 | 0x13104) && self.board != crate::Board::Smk37
            || matches!(addr, 0x50000..=0x501ff) && addr != 0x500c0
            || matches!(addr, 0x10808 | 0x10904);
        if !untimed {
            self.next_due = 0;
        }
        match addr {
            0x10800 => {
                if self.t4_con & 1 == 0 && v & 1 != 0 {
                    self.t4_base = self.cycles;
                }
                self.t4_con = v & !0x4000;
            }
            0x10804 => self.t4_base = self.cycles,
            0x10808 => {}
            0x10900 => {
                if v & 0x4000 != 0 {
                    self.t5_pending = false;
                }
                let was = self.t5_con & 1;
                self.t5_con = v & !0xc000;
                if was == 0 && v & 1 != 0 {
                    self.t5_next = self.cycles + self.t5_period();
                } else if v & 1 == 0 {
                    self.t5_next = u64::MAX;
                }
            }
            0x10904 => {}
            0x10908 => self.t5_prd = v,
            0x10400 | 0x10500 | 0x10600 | 0x10700 => {
                let i = ((addr - 0x10400) >> 8) as usize;
                if v & 0x4000 != 0 {
                    self.tmr[i].pending = false;
                }
                self.tmr[i].con = v & !0xc000;
                self.tmr_arm(i);
            }
            0x10404 | 0x10504 | 0x10604 | 0x10704 => {
                let i = ((addr - 0x10400) >> 8) as usize;
                self.tmr[i].cnt0 = v;
                self.tmr_arm(i);
            }
            0x10408 | 0x10508 | 0x10608 | 0x10708 => {
                let i = ((addr - 0x10400) >> 8) as usize;
                self.tmr[i].prd = v;
                self.tmr_arm(i);
            }
            0x12e00 => {
                let was = self.alnk_con0 & 0x800;
                self.alnk_con0 = v & 0xffff & !0x8000;
                if was == 0 && v & 0x800 != 0 {
                    self.alnk_half = 0;
                    self.alnk_next = self.cycles + self.half_period();
                } else if v & 0x800 == 0 {
                    self.alnk_next = u64::MAX;
                }
            }
            0x12e04 => self.alnk_con1 = v & 0xffff,
            0x12e08 => self.alnk_pend &= !((v & 0xf) << 4), // bits 0-3 clear pendings 4-7
            0x12e0c => self.alnk_con3 = v & 0xff,
            0x12e1c => self.alnk_adr = v,
            0x12e20 => self.alnk_len = v & 0xffff,
            0x13100 => {
                // bit 6 with bit 4 (enable) starts a conversion, which finishes at once; with
                // bit 5 it raises IRQ 24 (the stock driver reads RES in that handler). Bit 6
                // without bit 4 acknowledges (`CON = 0x40`).
                if v & 0x40 != 0 {
                    if v & 0x10 != 0 {
                        if self.board == crate::Board::Smk37 {
                            // SMK-37: sampled now, from the mux address latched now; done ~40 us
                            // later (the hardware map's SMK timing), IRQ 24 then if enabled
                            let ch = (v >> 8) & 0xf;
                            self.adc_sample = self.smk.adc(ch).unwrap_or(self.adc_value[ch as usize]);
                            self.adc_done = false;
                            self.adc_irq = false;
                            self.adc_next = self.cycles + (self.hz / 25_000).max(1);
                            self.next_due = 0;
                        } else {
                            let ch = (v >> 8) & 0xf;
                            self.adc_sample = self.row_reading(ch).unwrap_or(self.adc_value[ch as usize]);
                            self.adc_done = true;
                            if self.adc_irq != (v & 0x20 != 0) {
                                self.next_due = 0;
                            }
                            self.adc_irq = v & 0x20 != 0;
                        }
                    } else {
                        if self.adc_irq {
                            self.next_due = 0;
                        }
                        self.adc_done = false;
                        self.adc_irq = false;
                    }
                }
                self.adc_con = v;
            }
            0x13104 => {}
            0x1eef100..=0x1eef17f => self.icfg[((addr - 0x1eef100) / 4) as usize] = v,
            0x1eef300..=0x1eef37f => self.icfg1[((addr - 0x1eef300) / 4) as usize] = v,
            0x1eef0ec => {
                if v & 0x40 != 0 {
                    self.tick_pending = false;
                }
                let was = self.tick_ctl & 1;
                self.tick_ctl = v & 0x3f;
                if was == 0 && v & 1 != 0 {
                    self.tick_start = self.cycles;
                    self.tick_next = self.cycles + self.tick_period();
                } else if v & 1 == 0 {
                    self.tick_next = u64::MAX;
                }
            }
            0x1eef0f0 => {
                self.tick_start = self.cycles;
                if self.tick_next != u64::MAX {
                    self.tick_next = self.cycles + self.tick_period();
                }
            }
            0x1eef0f4 => self.tick_prd = v,
            0x1eef1a0 => self.ilat |= v,
            0x1eef1a4 => self.ilat &= !v,
            0x12100 => {
                let u = &mut self.uart;
                if v & 0x80 != 0 {
                    u.hrxcnt = u.since;
                    u.since = 0;
                }
                if v & 0x400 != 0 {
                    u.otpnd = false;
                }
                if v & 0x1000 != 0 {
                    u.rpnd = false;
                }
                if v & 0x2000 != 0 {
                    u.tpnd = false;
                }
                u.con0 = v & 0xffff & !0xfc80;
                self.uart_update();
            }
            // the ring's start resets the DMA's write position
            0x1211c => {
                self.uart.rxsadr = v;
                self.uart.wr = 0;
            }
            0x12120 => self.uart.rxeadr = v,
            0x12124 => self.uart.rxcnt = v & 0xffff,
            0x12110 => self.uart.otcnt = v,
            // TX DMA count: sent at once (MIDI thru has no pin here)
            0x12118 => {
                self.uart.tpnd = true;
                self.uart_update();
                return false;
            }
            0x50000 if self.board == crate::Board::Smk37 => {
                // PA1's rising edge latches the 595 chain (SPI2 shifts it)
                if self.pa_out & 2 == 0 && v & 2 != 0 {
                    self.smk.latch();
                }
                self.pa_out = v;
            }
            0x501c0 if self.board == crate::Board::Smk37 => self.ph_out = v,
            0x50000 => {
                self.led_credit();
                self.pa_write(v);
            }
            // (also kept as plain SFRs: the rows' readings need them)
            0x50008 => {
                self.pa_dir = v;
                return false;
            }
            0x50010 => {
                self.pa_pu = v;
                return false;
            }
            0x50018 => {
                self.pa_hd0 = v;
                return false;
            }
            0x5001c => {
                self.pa_hd = v;
                return false;
            }
            0x501c0 => {
                self.led_credit();
                self.ph_out = v;
            }
            _ => return false,
        }
        true
    }

    // ---- LEDs ---------------------------------------------------------------
    // LED lines PA9, PA10, PH6, PH9 light the LED in the latched column on
    // row bit 1, 2, 3, 4 (hal/fm1_input.h fm1__led_lines); the LED belongs to
    // the key or button at that (row, column) of the matrix.

    fn led_credit(&mut self) {
        let dt = self.cycles - self.led_last;
        self.led_last = self.cycles;
        if dt == 0 {
            return;
        }
        let lines = ((self.pa_out >> 8) & 2) | ((self.pa_out >> 8) & 4) | ((self.ph_out >> 3) & 8) | ((self.ph_out >> 5) & 16);
        if lines == 0 {
            return;
        }
        // the same lines and column as last time: the time adds up, and the LEDs it lights are
        // charged when the state changes or the fractions are taken (Felucca's matrix scan writes
        // the port half a million times a second, most without a change)
        let key = (lines, self.sr_latched);
        if key == self.led_key {
            self.led_pend += dt;
            return;
        }
        self.led_flush();
        self.led_key = key;
        self.led_pend = dt;
    }

    /// Charge the time pending for the LED state last credited to its LEDs.
    fn led_flush(&mut self) {
        let (lines, latched) = self.led_key;
        let dt = std::mem::take(&mut self.led_pend);
        if dt == 0 {
            return;
        }
        for col in 0..11usize {
            if latched & (1 << col) != 0 {
                continue;
            }
            for row in 1..5usize {
                if lines & (1 << row) != 0 {
                    let id = KEYMAP[row][col];
                    if id >= 0 {
                        self.led_acc[id as usize] += dt;
                    }
                }
            }
        }
    }

    /// The fraction of time each LED was lit since the last call.
    pub fn led_take(&mut self) -> [f32; 41] {
        self.led_credit();
        self.led_flush();
        let span = (self.cycles - self.led_since).max(1) as f32;
        self.led_since = self.cycles;
        let mut out = [0f32; 41];
        for (o, a) in out.iter_mut().zip(self.led_acc.iter_mut()) {
            *o = *a as f32 / span;
            *a = 0;
        }
        out
    }

    // ---- key matrix -----------------------------------------------------

    /// cpu0 executed `idle` (it sleeps until an interrupt): with `skip_idle` and nothing
    /// pending, time jumps to just before the next event, where the interrupt that wakes it
    /// comes from. The stock firmware's idle task runs it on every pass.
    pub fn idle(&mut self) {
        if !self.skip_idle || self.any_pending() {
            return;
        }
        let next = self.next_event();
        if next != u64::MAX && next > self.cycles + 1 {
            self.skipped += next - 1 - self.cycles;
            self.cycles = next - 1;
            self.next_due = 0;
        }
    }

    /// An SPI1 transfer of `bytes` started at BAUD `baud` (the clock 60 MHz / (baud + 1): ~12 MHz at 4
    /// for the LCD, 6.67 MHz at 8 for the SMK-37's pad LEDs); with `irq` IRQ 16 at its end.
    pub fn spi1_start(&mut self, bytes: u32, baud: u32, irq: bool) {
        let clk = 60_000_000 / (baud as u64 + 1);
        let dur = (bytes.max(1) as u64 * 8 * self.hz / clk).max(1);
        self.spi1_done_at = self.cycles + dur;
        self.spi1_wire += dur;
        if irq {
            self.spi1_irq_at = self.spi1_done_at;
            self.next_due = 0;
        }
    }

    /// CON bit 13 cleared: IRQ 16 neither stays raised nor comes at the transfer's end.
    pub fn spi1_irq_off(&mut self) {
        self.spi1_pending = false;
        self.spi1_irq_at = u64::MAX;
    }

    /// A SPI2 transfer of `bytes` started (with its interrupt enabled when `irq`): it takes
    /// 16 * (baud + 1) peripheral clocks a byte (48 MHz assumed), so the stock firmware's
    /// interrupt-driven column scan runs at a hardware-like pace instead of back to back.
    pub fn spi2_start(&mut self, bytes: u32, baud: u32, irq: bool) {
        self.next_due = 0;
        if irq {
            let clocks = bytes.max(1) as u64 * 16 * (baud as u64 + 1);
            self.spi2_next = self.cycles + (clocks * self.hz / 48_000_000).max(1);
        }
    }

    /// A byte clocked into the 74HC595 chain by SPI2, MSB first (the stock firmware drives
    /// the matrix columns this way; Felucca bit-bangs SER/SRCLK on PA4/PA3). RCLK (PA1)
    /// still latches.
    pub fn sr_shift_byte(&mut self, b: u8) {
        if self.board == crate::Board::Smk37 {
            return self.smk.spi2_byte(b);
        }
        for bit in (0..8).rev() {
            self.sr_shift = (self.sr_shift << 1) | ((b >> bit) & 1) as u16;
        }
    }

    fn pa_write(&mut self, v: u32) {
        let old = self.pa_out;
        self.pa_out = v;
        if old & 8 == 0 && v & 8 != 0 {
            // SRCLK rising edge: shift SER (PA4) in, MSB first
            self.sr_shift = (self.sr_shift << 1) | ((v >> 4) & 1) as u16;
        }
        if old & 2 == 0 && v & 2 != 0 {
            // RCLK rising edge: outputs follow the shift register
            self.sr_latched = self.sr_shift;
        }
    }

    fn ms_cycles(&self, ms: f64) -> u64 {
        (self.hz as f64 * ms / 1000.0) as u64
    }

    /// ADC channel 0 / 1 (row PA7 / PA8): the key in the latched column, through the row's
    /// pull-up or its driver (the module notes). None: not a row, or a row neither pulled up nor
    /// driven (the channel's adc_value then, as before).
    fn row_reading(&mut self, ch: u32) -> Option<u16> {
        let (row, pin) = match ch { 0 => (3usize, 7u32), 1 => (4usize, 8u32), _ => return None };
        let m = 1u32 << pin;
        let rs = if self.pa_dir & m == 0 {
            if self.pa_out & m == 0 {
                return Some(0);
            }
            [312.0, 132.0, 51.0, 35.0][((self.pa_hd0 & m != 0) as usize) | ((self.pa_hd & m != 0) as usize) << 1]
        } else if self.pa_pu & m != 0 {
            10_000.0
        } else {
            return None;
        };
        let t = self.cycles;
        let mut v = 1023.0f32;
        for col in 0..11usize {
            let id = KEYMAP[row][col];
            if self.sr_latched & (1 << col) == 0 && id >= 0 {
                v = v.min(pill_reading(self.key_force[id as usize].at(t), rs));
            }
        }
        self.adc_noise = self.adc_noise.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let n = ((self.adc_noise >> 16) % 3) as f32 - 1.0;
        Some((v + if v < 1023.0 { n } else { 0.0 }).round().clamp(0.0, 1023.0) as u16)
    }

    /// Row bits (0..5) pulled low by a closed contact in a selected column.
    fn closed_rows(&self) -> u32 {
        let mut rows = 0u32;
        for col in 0..11usize {
            if self.sr_latched & (1 << col) != 0 {
                continue; // column not selected (active low)
            }
            for row in 0..6usize {
                let id = KEYMAP[row][col];
                if id >= 0 && self.keys_down[id as usize] {
                    rows |= 1 << row;
                }
            }
            for (i, e) in ENC.iter().enumerate() {
                let (a, b) = GRAY[self.enc_state[i] as usize];
                if e[0] as usize == col && a != 0 {
                    rows |= 1 << e[1];
                }
                if e[2] as usize == col && b != 0 {
                    rows |= 1 << e[3];
                }
            }
        }
        rows
    }

    fn pa_in(&self) -> u32 {
        let r = self.closed_rows();
        // row 0 = PA0, rows 1-4 = PA5..PA8; everything else reads as pulled up
        let low = (r & 1) | ((r & 0x1e) << 4);
        (self.pa_out | 0xffff) & !low
    }

    /// SMK-37: the matrix rows closed in the latched column(s).
    fn smk_rows(&self) -> u32 {
        self.smk.rows(self.cycles, |i| {
            let (a, b) = GRAY[self.enc_state[i] as usize];
            (a != 0, b != 0)
        })
    }

    /// SMK-37 rows 0..4 on PA2, PA5..PA7, PA9 (pulled up: low = closed).
    fn smk_pa_in(&self) -> u32 {
        let r = self.smk_rows();
        let low = (r & 1) << 2 | (r & 0xe) << 4 | (r & 0x10) << 5;
        (self.pa_out | 0xffff) & !low
    }

    /// SMK-37 rows 5..7 on PH6, PH8, PH9.
    fn smk_ph_in(&self) -> u32 {
        let r = self.smk_rows();
        let low = (r & 0x20) << 1 | (r & 0xc0) << 2;
        (self.ph_out | 0xffff) & !low
    }

    fn pb_in(&self) -> u32 {
        let r = self.closed_rows();
        0xffff & !(((r >> 5) & 1) << 7)
    }
}

/// Logical names from Felucca's panel.c mapped to matrix key ids.
pub fn key_id(name: &str) -> Option<u8> {
    let n = name.to_ascii_uppercase();
    let buttons = [
        ("OCTDN", 0), ("OCTUP", 1), ("FX", 2), ("SCL", 3), ("ENV", 4), ("LFO", 5), ("EDIT", 6),
        ("GLO", 7), ("HOME", 8), ("SAVE", 9), ("ARP", 10), ("SEQ", 11), ("PLAY", 12), ("REC", 13),
    ];
    if let Some((_, id)) = buttons.iter().find(|(b, _)| *b == n) {
        return Some(*id);
    }
    if let Some(k) = n.strip_prefix('K') {
        return k.parse::<u8>().ok().filter(|&k| k < 41);
    }
    if let Some(k) = note_name(&n) {
        return Some(14 + k);
    }
    if let Some(note) = n.strip_prefix('N') {
        // note keys: n0 = F3 .. n26 = G5
        return note.parse::<u8>().ok().filter(|&k| k < 27).map(|k| 14 + k);
    }
    None
}

/// Encoder names (panel.c PANEL_DEFAULT) to matrix encoder index.
pub fn enc_id(name: &str) -> Option<u8> {
    let n = name.to_ascii_uppercase();
    match n.as_str() {
        "SELECT" => Some(0),
        "ALGO" | "ALGORITHM" => Some(1),
        "PRESET" | "PRESETS" => Some(6),
        "K1" => Some(2),
        "K2" => Some(3),
        "K3" => Some(4),
        "K4" => Some(5),
        _ => n.strip_prefix("ENC").and_then(|e| e.parse::<u8>().ok()).filter(|&e| e < 7),
    }
}

/// `C4`, `F#3`, `Bb4` -> note-key index (0 = F3 .. 26 = G5).
fn note_name(n: &str) -> Option<u8> {
    let b = n.as_bytes();
    let base = match *b.first()? {
        b'C' => 0, b'D' => 2, b'E' => 4, b'F' => 5, b'G' => 7, b'A' => 9, b'B' => 11,
        _ => return None,
    };
    let (acc, rest) = match b.get(1) {
        Some(b'#') => (1, &n[2..]),
        Some(b'B') if b.len() == 3 => (-1, &n[2..]), // "BB4" after uppercasing = Bb4
        _ => (0, &n[1..]),
    };
    let oct: i32 = rest.parse().ok()?;
    let midi = (oct + 1) * 12 + base + acc;
    let k = midi - 53; // F3 = MIDI 53
    (0..27).contains(&k).then_some(k as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One conversion on ADC channel `ch`, as hal/fm1_adc.h starts and takes it.
    fn convert(p: &mut Periph, ch: u32) -> u32 {
        p.write(0x13100, 0x40 | ch << 8 | 0x0e);
        p.write(0x13100, 0x40 | 0x10 | ch << 8 | 0x0e);
        p.read(0x13104).unwrap()
    }

    #[test]
    fn the_rows_read_a_keys_pill() {
        let mut p = Periph::default();
        let ms = p.hz / 1000;
        // row PA7 an input with its pull-up, column 0 latched: key 9 (id 23)
        p.write(0x50008, 1 << 7);
        p.write(0x50010, 1 << 7);
        p.sr_latched = !1;
        assert_eq!(convert(&mut p, 0), 1023, "open");
        p.apply(&Action::KeyVel { vel: 127 });
        p.apply(&Action::Key { id: 23, down: true });
        p.cycles += 2 * ms;
        assert!((193..=196).contains(&convert(&mut p, 0)), "firmly pressed, pulled up: the diode and a little");
        // driven high at the strongest (HD0 + HD): ~806 firm
        p.write(0x50008, 0);
        p.pa_out |= 1 << 7;
        p.write(0x50018, 1 << 7);
        p.write(0x5001c, 1 << 7);
        let firm = convert(&mut p, 0);
        assert!((804..=808).contains(&firm), "{firm}");
        p.apply(&Action::Key { id: 23, down: false });
        p.cycles += 10 * ms;
        assert_eq!(convert(&mut p, 0), 1023, "lifted");
        p.apply(&Action::KeyVel { vel: 1 });
        p.apply(&Action::Key { id: 23, down: true });
        p.cycles += 2 * ms;
        let light = convert(&mut p, 0);
        assert!((1004..=1012).contains(&light), "a light press reads higher: {light}");
        p.apply(&Action::Pressure { id: 23, vel: 127 });
        p.cycles += 25 * ms;
        assert!(convert(&mut p, 0) < 810, "pressed harder");
        p.sr_latched = !2;
        assert_eq!(convert(&mut p, 0), 1023, "another column: its key is open");
        p.pa_out &= !(1 << 7);
        assert_eq!(convert(&mut p, 0), 0, "driven low");
        assert_eq!(convert(&mut p, 4), 900, "the MASTER pot as before");
    }

    /// Advance to `cycle`, collecting the bytes the UART's DMA wrote.
    fn run_to(p: &mut Periph, cycle: u64, got: &mut Vec<(u32, u8)>) {
        while p.cycles < cycle {
            let step = p.next_event().min(cycle).max(p.cycles + 1) - p.cycles;
            if let Some(w) = p.advance(step).uart_rx {
                got.push(w);
            }
        }
    }

    #[test]
    fn uart_midi_in_dma_latch_and_timeout() {
        let mut p = Periph::default();
        // Felucca's set-up (hal/fm1_uart.h) with a 4-byte ring, plus the RX and timeout IRQs
        p.write(0x1211c, 0x1000);
        p.write(0x12120, 0x1004);
        p.write(0x12124, 4);
        p.write(0x12110, 48_000); // 1 ms of silence
        p.write(0x12100, 0x40 | 0x08 | 0x20 | 1);
        for b in [0x90, 0x3c, 0x64, 0x80, 0x3c] {
            p.apply(&Action::Midi { b });
        }
        let byte = p.midi_byte_time();
        let mut got = Vec::new();
        run_to(&mut p, 3 * byte, &mut got);
        assert_eq!(got, [(0x1000, 0x90), (0x1001, 0x3c), (0x1002, 0x64)]); // a byte time apart
        assert!(p.pending_irq_for(0).is_none() && !p.any_pending()); // IRQ 20 not enabled yet
        p.icfg[(IRQ_UART1 >> 3) as usize] |= 1 << ((IRQ_UART1 & 7) * 4);
        run_to(&mut p, 5 * byte, &mut got);
        assert_eq!(got[3..], [(0x1003, 0x80), (0x1000, 0x3c)]); // the ring wraps
        assert_eq!(p.read(0x12100).unwrap() & 0x4000, 0x4000); // RXCNT (4) bytes: RX pending
        assert_eq!(p.pending_irq_for(0).map(|(n, _)| n), Some(IRQ_UART1));
        // the handler's acknowledge: latch the count, clear the pending
        let con = p.read(0x12100).unwrap();
        p.write(0x12100, con | 0x80);
        let con = p.read(0x12100).unwrap();
        p.write(0x12100, con | 0x1000);
        assert_eq!(p.read(0x12128), Some(5));
        assert!(p.pending_irq_for(0).is_none());
        // a millisecond of silence after the last byte: the timeout
        let ms = p.hz / 1000;
        run_to(&mut p, 5 * byte + ms + 1, &mut got);
        assert_eq!(p.read(0x12100).unwrap() & 0x800, 0x800);
        assert_eq!(p.pending_irq_for(0).map(|(n, _)| n), Some(IRQ_UART1));
        let con = p.read(0x12100).unwrap();
        p.write(0x12100, con | 0x80 | 0x400);
        assert_eq!(p.read(0x12128), Some(0));
        assert!(p.pending_irq_for(0).is_none());
        // with the UART off a byte is dropped
        p.write(0x12100, 0);
        p.apply(&Action::Midi { b: 0xf8 });
        run_to(&mut p, 7 * byte + ms, &mut got);
        assert_eq!((got.len(), p.midi_dropped()), (5, 1));
    }
}
