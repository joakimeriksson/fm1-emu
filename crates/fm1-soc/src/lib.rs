//! FM-1 SoC: memory map + peripherals.
//!
//! Memory map (recon + Felucca HAL headers + corpus literals):
//!   0x0200_0000  XIP flash (app window) — code runs from here
//!   0x01c0_0000  data RAM (≥512 KiB; BOOT_STATE at 0x01C7FD80 observed)
//!   0x5000_0-    GPIO ports A..H (0x40 stride: OUT+0, DIR+8, DIE+C, PU+10, PD+14)
//!   0x5100_|     IOMAP
//!   0x1000_-0x1002  clock
//!   0x1080_0/0x1090_0  TIMER4/TIMER5 (CNT reads = synthetic progress)
//!   0x11c0_0-    SPI0 = external NOR flash (model in spiflash.rs; CS = PD0)
//!   0x11d0_0-    SPI1 = LCD (ST7789-class 240×240, model in lcd.rs)
//!   0x1180_0-    USB
//!   0x1210_0-    UART1 (MIDI IN)
//!   0x1310_0-    ADC
//!   0x1eee000-   interrupt controller (0x1eee008 bit14 = ready, polled by the
//!                RAM early-init; the rest stored bits)
//!   0x1eee240    DBG_MSG debug box (console)
//!   0x01f0_0000  SRAM banks (256 KiB; cleared by the early init, config block at 0x1f28000)
//!   0x01c0xxxx 'negative' long-call targets are RAM-resident .data code
//!   (copied from flash 0x02084820 by the CRT), not a mask ROM
//!
//! Policy: unknown MMIO accesses are logged and return a benign default —
//! never silently swallowed. The access log is the reverse-engineering
//! roadmap.

use fm1_core::Bus;

pub mod script;
pub mod flaky;
pub mod hwdsp;
pub mod hweq;
pub mod hwsrc;
mod hwsrc_kernel;
pub mod fwsc;
mod lcd;
pub mod machine;
pub mod p33;
pub mod periph;
mod ram;
pub mod smk;
mod spiflash;
pub mod usb;

pub use lcd::{write_bmp, Lcd};
pub use periph::{Action, Periph, CPU_HZ, VEC_BASE};
pub use ram::Ram;
pub use spiflash::{SpiFlash, FLASH_SIZE};

pub const XIP_BASE: u32 = 0x0200_0000;
pub const RAM_BASE: u32 = 0x01c0_0000;
pub const OVL_BASE: u32 = 0x0400_0000;
pub const SRAM2_BASE: u32 = 0x01f0_0000;
/// The stock V13 firmware's .bd_base: what the BT baseband's window register 0x2FC44 reads
/// before a firmware writes it (each stock image writes its own .bd_base there: V13 this,
/// the SMK-37's v016 0x01C09A18).
pub const BT_WINDOW_BASE: u32 = 0x01c0_9e7c;
/// The flash offset behind XIP 0x02000000 (the stock layout: the app area starts at 0x4000).
pub const XIP_FLASH_OFFSET: usize = 0x4000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownAccess {
    pub addr: u32,
    pub width: u8,
    pub is_write: bool,
    pub value: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Region {
    Xip,
    Ram,
    Ovl,
    Gpio,
    Iomap,
    Clk,
    Timer,
    Spi0,
    Spi1,
    Usb,
    Uart1,
    Adc,
    Ana,
    /// The power domain's register port (p33.rs).
    P33,
    Bt,
    Spi2,
    Crc,
    Rand,
    XipFlash,
    /// SFCENC: the SFC's decryption and its plain window.
    Sfcenc,
    Dbg,
    /// The CPU's guards (0x1EEE250..0x1EEE3FF: write limits, DBG_EN, PC limits): stored, and the PC
    /// limits watched (`Soc::pc_windows`)
    Guard,
    Irqc,
    /// Interrupt controller enable/priority words and software latch.
    Irqc2,
    /// ALNK0 I2S out.
    Alnk,
    /// JL_FFT, the FFT / vector unit (hwdsp.rs).
    HwDsp,
    /// JL_EQ and its RAM (hweq.rs).
    HwEq,
    /// JL_SRC, the sample-rate converter (hwsrc.rs).
    HwSrc,
    Sram2,
    Unknown,
}

// The peripheral window, by table: `Soc::region` was a chain of some 30 compares with P33 near
// its end, and OberVave, ZP-12 and FM1-DOOM touch P33 tens of millions of times a second (a
// watchdog kick per round of their main loop; a quarter of FM1-DOOM's time was the chain).
const SFR_LO: u32 = 0x10000;
/// The FM-1's flash cache as harbor's cache probe measured it on a real FM-1 (2026-10-24,
/// probes/cache.c): 32 KB (a 32 KB footprint hits, 64 KB misses), 64-byte lines (the cost doubles
/// with the stride up to 64), ~300 cycles a miss (1.29 us a load at a 64-byte stride). 4-way with LRU
/// is assumed (not measured). Shared by data reads from XIP and code fetched from XIP. Its costs
/// are in the strict clock's eighths of a cycle.
#[derive(Clone)]
pub struct XipCache {
    tags: [[u32; 4]; 128],
    age: [[u8; 4]; 128],
    pub misses: u64,
}

impl Default for XipCache {
    fn default() -> Self {
        XipCache { tags: [[u32::MAX; 4]; 128], age: [[0, 1, 2, 3]; 128], misses: 0 }
    }
}

impl XipCache {
    /// a miss, in the strict clock's ticks (eighths of a 240 MHz cycle): ~300 cycles
    pub const MISS_TICKS: u64 = 300 * 8;

    /// The line of addr is used: true when it was not in the cache (it is now, in the way least
    /// recently used)
    #[inline]
    pub fn access(&mut self, addr: u32) -> bool {
        let line = addr >> 6;
        let set = (line & 127) as usize;
        let (tags, age) = (&mut self.tags[set], &mut self.age[set]);
        let (way, miss) = match tags.iter().position(|&t| t == line) {
            Some(w) => (w, false),
            None => (age.iter().position(|&a| a == 3).unwrap_or(0), true),
        };
        if miss {
            tags[way] = line;
            self.misses += 1;
        }
        let was = age[way];
        for a in age.iter_mut() {
            if *a < was {
                *a += 1;
            }
        }
        age[way] = 0;
        miss
    }
}

/// Below this nothing answers: an access is bus-invalid (the guards' "NULL page")
const NULL_PAGE: u32 = 0x1000;
const SFR_HI: u32 = 0x14000;

/// The region of an address in the peripheral window (8-byte cells: every block there starts
/// and ends on one; `sfr_table_matches_ranges` checks it).
const fn sfr_region(a: u32) -> Region {
    match a {
        0x12e00..=0x12e3f => Region::Alnk,
        0x10000..=0x1001f => Region::Clk,
        0x10400..=0x1091f => Region::Timer,
        0x11c00..=0x11c1f => Region::Spi0,
        0x11d00..=0x11d1f => Region::Spi1,
        0x11e00..=0x11e1f => Region::Spi2,
        0x11800..=0x1183f => Region::Usb,
        0x12100..=0x1212f => Region::Uart1,
        0x13100..=0x1311f => Region::Adc,
        0x13500..=0x1350f => Region::Crc,
        0x13b00..=0x13b07 => Region::Rand,
        0x11900..=0x119ff => Region::Ana,
        0x13e08..=0x13e0f => Region::P33,
        _ => Region::Unknown,
    }
}

static SFR_MAP: [Region; ((SFR_HI - SFR_LO) / 8) as usize] = {
    let mut t = [Region::Unknown; ((SFR_HI - SFR_LO) / 8) as usize];
    let mut i = 0;
    while i < t.len() {
        t[i] = sfr_region(SFR_LO + 8 * i as u32);
        i += 1;
    }
    t
};

/// The SoC bus: XIP flash + data RAM + modeled peripherals + a logged stub.
/// A cheap hasher for register addresses (the SoC's register map): `std`'s SipHash cost the
/// SMK-37's stock firmware, which touches registers about a million times a second, a few
/// percent of its time.
#[derive(Default)]
pub struct AddrHasher(u64);

/// SFR words at aligned addresses below this live in `Soc::sfr_lo`.
const SFR_LO_END: u32 = 0x6_0000;

impl std::hash::Hasher for AddrHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    fn write_u32(&mut self, v: u32) {
        self.0 = (v as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

/// The board around the chip: the FM-1's panel, or the SMK-37's (SMK-37 Pro / Elite, MKE-P37
/// and Starrykey 37 Play share one: fm1-to-smk37 docs/01-hardware.md). Same chip, flash
/// layout, audio and update path; the panel's wiring differs (smk.rs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Board {
    #[default]
    Fm1,
    Smk37,
}

impl Board {
    /// The board a package is for, from its identity (`FM-1_015`, `SMK-37 Pro_016`, ...).
    pub fn of_identity(id: &str) -> Board {
        let u = id.to_ascii_uppercase();
        if ["SMK", "MKE", "STARRYKEY"].iter().any(|p| u.starts_with(p)) {
            Board::Smk37
        } else {
            Board::Fm1
        }
    }
}

pub struct Soc {
    pub board: Board,
    /// The BT baseband's window base (0x2FC44), as the firmware set it.
    bt_window: u32,
    pub xip: Vec<u8>,
    pub ram: Ram,
    pub lcd: Lcd,
    /// SPI0 + the external NOR flash (1 MiB; `--flash` image loaded at 0).
    pub flash: SpiFlash,
    /// Console bytes written through DBG_MSG / UART-style debug boxes.
    pub console: Vec<u8>,
    /// The guards' causes in DBG_MSG (bit 12 a PC outside its limits, 4 / 5 a read / write of
    /// unmapped space): set as a guard trips, cleared by DBG_MSG_CLR
    pub dbg_cause: u32,
    /// The PC limits were written: `machine::run_blocks` gives the core `pc_windows` again
    pub pc_guard_dirty: bool,
    /// EMU_MSG's causes (0x1EEF0D4, write 1 to clear): bit 0 a misaligned access (`strict_align`)
    pub emu_cause: u32,
    /// The FM-1's LCD transfers take their time on the wire, as the SMK-37's (SPI1 at 60 MHz /
    /// (BAUD + 1): Felucca's and Dinghy's BAUD 4 is 12 MHz, a whole 240 x 240 frame ~77 ms). A
    /// firmware that waits for whole frames then gets that little done in between, as on an FM-1
    /// (FM-1 SID 2026-10-24: its tunes went silent on the device and played here, the transfers
    /// having finished at once). Off (the default): the FM-1's transfers finish at once, unless
    /// their interrupt is on. Set by the front end before the run (`lcd_timed_from_env`: fm1-live's
    /// and fm1-emu's FM1_STRICT / FM1_LCD_TIMED; fm1-web's `fm1_lcd_timed`).
    pub lcd_timed: bool,
    /// A 16- or 32-bit RAM access that is not aligned raises the CPU exception (EMU_MSG bit 0), as
    /// on an FM-1 (a probe, 2026-10-24: every misaligned load or store to RAM faulted; XIP reads
    /// across). Only where loads and stores go through the bus: fm1-live's FM1_STRICT=1 runs
    /// without the JIT for it.
    pub strict_align: bool,
    /// The flash cache (FM1_STRICT): XIP reads and code fetched from XIP that miss it cost a line's
    /// fill (`XipCache`)
    pub xip_cache: Option<XipCache>,
    /// Core 1 is running: a flash cache miss stalls core 1, not the clock; its ticks gather in
    /// `core1_ticks` for `machine::Core1Bus` to take from core 1's budget, and what an instruction
    /// ran past a slice's end is `core1_debt`, owed from the next slice
    pub core1_charging: bool,
    pub core1_ticks: u64,
    pub core1_debt: u64,
    /// FM1_TPROF: flash cache misses by (core << 32 | line address)
    pub miss_hist: Option<Box<std::collections::HashMap<u64, u64>>>,
    /// Overlay RAM (second bank, 0x04000000), written by the app CRT.
    pub ovl: Ram,
    /// SRAM banks at 0x01f00000 (cleared by the RAM early init).
    pub sram2: Ram,
    /// monotonic "time" for peripheral counters
    pub ticks: u64,
    /// Timers, audio DMA, ADC, interrupt controller and the front panel.
    pub periph: Periph,
    /// Plain SFR words: the aligned ones below `SFR_LO_END` in an array (the GPIO and clock
    /// windows, written at every port write), the rest in the map.
    sfr_lo: Vec<u32>,
    sfr: std::collections::HashMap<u32, u32, std::hash::BuildHasherDefault<AddrHasher>>,
    /// Core 0 / 1 paused by the other (C0_CON / C1_CON bit 2 pulses, bit 3 resumes).
    paused: [bool; 2],
    /// The XIP window continues into the flash chip past the image (the stock layout, which
    /// the updater gives every firmware: April OS keeps its samples at flash 0x74000, read
    /// through XIP 0x02070000, and waits at boot until it finds them or a blank chip's 0xff).
    pub xip_flash: bool,
    /// The SFC decrypts the app area on the fly with the chip key (packages are installed
    /// scrambled): with a key (`sfc_boot`), what the window shows from the chip (a write, a
    /// restored flash file) is read through SFCENC: CON bit 0 decrypts, bit 1 leaves the plain
    /// window UNENC_L..=UNENC_H (XIP addresses) as stored. No key (a bare `--bin`): the chip as
    /// stored.
    sfc_key: Option<u16>,
    sfc_con: u32,
    sfc_unenc: (u32, u32),
    /// The window's 32-byte blocks that show the chip rather than the installed package: read
    /// again when the firmware sets SFCENC.
    sfc_from_flash: Vec<bool>,
    /// cpu0 released core 1: 0x10008 bit 3 (the stock firmware), or the first `|= 8` (run) on
    /// core 1's CON register 0x1EEE004 (M-VAVE's FM1 Open, after the newer AC79 SDK); latched
    /// until the runner starts cpu1, as firmwares hold the bit only while they wait for core 1
    /// to report in.
    pub cpu1_release: bool,
    /// core 1 was released once: a later run on 0x1EEE004 is the stock IPC's resume after a
    /// pause, not a release, and a later 0 -> 1 of 0x10008 bit 3 latches nothing (core 1 runs on).
    cpu1_released_once: bool,
    /// core 1 ran `idle`: it sleeps until an interrupt from its own bank is pending, as the
    /// chip's core does (`machine::run_cpu1` leaves it be meanwhile; before this its `idle`
    /// did nothing, and a core 1 that idles and feeds the watchdog each round, Bubba
    /// Machine's, cost as much as a busy one).
    pub cpu1_idle: bool,
    /// A fault on core 1 (an instruction it could not run: a jump into erased flash, COSMOS
    /// 0.23's package has its core 1 entry in a hole of 0xFF) stops that core alone and cpu0
    /// goes on, as the chip's would (felucca-bsp's `cpu2_fallback` is built for a core 1 that
    /// fails). Off, the default, the machine halts there as before: `run_cpu1`'s errors are
    /// mostly the emulator's own gaps (Decode, Unsupported, MissingSlot), which a halt shows
    /// and a surviving core 0 would hide. `--cpu1-survives` in the runners, `fm1_cpu1_survives`
    /// for a page.
    pub cpu1_survives: bool,
    /// core 1 stopped on a fault (`cpu1_survives`): held until a chip reset; the message says
    /// where.
    pub cpu1_fault: Option<String>,
    /// FM1_AUDIO_TRACE: log each audio half-buffer (debugging the DMA's pace).
    audio_trace: bool,
    /// The CRC-16 unit's register (0x13504).
    crc: u16,
    /// The random number generator's state (JL_RAND, 0x13B00: R64L / R64H).
    rand: u64,
    /// The cores' hardware lock (`lockset` / `lockclr`): which core holds it.
    lock_owner: Option<u32>,
    /// FM1_GPIO_TRACE: GPIO register writes by address (debugging port use).
    pub gpio_writes: Option<std::collections::BTreeMap<u32, u64>>,
    unknown_log: Vec<UnknownAccess>,
    /// `--flaky-core1`: core 1's RAM loads misread now and then (flaky.rs); None = a good unit.
    pub flaky_core1: Option<flaky::FlakyCore1>,
    /// The power domain's analog registers.
    pub p33: p33::P33,
    /// The FFT / vector unit (JL_FFT, 0x1EE0400; hwdsp.rs).
    pub hwdsp: hwdsp::HwDsp,
    /// The EQ (JL_EQ, 0x1E000, its RAM at 0x1E800; hweq.rs).
    pub hweq: hweq::HwEq,
    /// The sample-rate converter (JL_SRC, 0x14300; hwsrc.rs).
    pub hwsrc: hwsrc::HwSrc,
    /// USB0 and, with a cable (`usb_plug`), a host on the other end (usb.rs).
    pub usb: usb::Usb,
    /// Exact skipping of cpu0's polling loops (`machine::run_blocks`).
    pub loop_skip: machine::LoopSkip,
    /// Chip resets so far (the watchdog's, PR_PWR's: p33.rs), and the cause of one to come
    /// (`RST_SRC`'s bit), which `machine::run_blocks` carries out.
    pub resets: u32,
    reset_req: Option<u8>,
    /// How the loader left the chip, taken as cpu0 starts: what a reset goes back to.
    boot: Option<Box<Boot>>,
}

/// The chip as the loader hands it to the app (`Soc::chip_reset`).
struct Boot {
    cpu: fm1_core::PlainState,
    sfr_lo: Vec<u32>,
    sfr: std::collections::HashMap<u32, u32, std::hash::BuildHasherDefault<AddrHasher>>,
    sfc: (u32, (u32, u32)),
    bt_window: u32,
    /// The SPL's boot parameters (r0 into RAM, `write_boot_info`): where, and the bytes.
    info: Option<(u32, Vec<u8>)>,
}

fn dbg_line(addr: u32) -> &'static str {
    match addr & !3 {
        0x1eee240 => "wr_en",
        0x1eee244 => "msg",
        0x1eee248 => "msg_clr",
        _ => "dbg",
    }
}

impl Soc {
    /// After an erase or page program: the XIP window shows the flash chip, also where the
    /// window's contents come from the package (`xip`, the app area as installed). A booted
    /// package's window reaches flash 0x93000, and April OS writes its samples from 0x74000
    /// and reads them back through XIP.
    fn sync_xip(&mut self) {
        let Some((a, b)) = self.flash.changed.take() else { return };
        if !self.xip_flash {
            return;
        }
        if self.sfc_key.is_some() {
            // whole blocks, as the SFC decrypts them
            let mut blk = a.max(XIP_FLASH_OFFSET) & !31;
            while blk < b && blk - XIP_FLASH_OFFSET < self.xip.len() {
                let i = (blk - XIP_FLASH_OFFSET) / 32;
                if self.sfc_from_flash.len() <= i {
                    self.sfc_from_flash.resize(i + 1, false);
                }
                self.sfc_from_flash[i] = true;
                self.xip_block(blk);
                blk += 32;
            }
            return;
        }
        for o in a.max(XIP_FLASH_OFFSET)..b {
            match self.xip.get_mut(o - XIP_FLASH_OFFSET) {
                Some(x) => *x = self.flash.mem[o],
                None => break,
            }
        }
    }

    /// Flash offset `o` is read through the SFC's decryption: CON bit 0, outside the plain
    /// window when bit 1 sets one.
    fn sfc_scrambled(&self, o: usize) -> bool {
        let x = XIP_BASE + (o - XIP_FLASH_OFFSET) as u32;
        self.sfc_con & 1 != 0 && !(self.sfc_con & 2 != 0 && (self.sfc_unenc.0..=self.sfc_unenc.1).contains(&x))
    }

    /// The 32-byte block at flash offset `blk` into the XIP window, decrypted where the SFC
    /// decrypts (as fwsc::unscramble: the key mixed with the block's place in the app area).
    fn xip_block(&mut self, blk: usize) {
        let i = blk - XIP_FLASH_OFFSET;
        let n = 32.min(self.xip.len() - i).min(self.flash.mem.len() - blk);
        let mut b = [0u8; 32];
        b[..n].copy_from_slice(&self.flash.mem[blk..blk + n]);
        if let Some(key) = self.sfc_key.filter(|_| self.sfc_scrambled(blk)) {
            fwsc::unmask(&mut b[..n], key ^ (i >> 2) as u16);
        }
        self.xip[i..i + n].copy_from_slice(&b[..n]);
    }

    /// SFCENC changed: the blocks that show the chip, read through the new setting.
    fn sfc_reread(&mut self) {
        if self.sfc_key.is_none() {
            return;
        }
        for i in 0..self.sfc_from_flash.len() {
            if self.sfc_from_flash[i] {
                self.xip_block(XIP_FLASH_OFFSET + 32 * i);
            }
        }
    }

    /// A package boot: the chip key, and SFCENC as the loader leaves it for the installed
    /// package (decrypted up to flash `app_end`, the protected image's end, and as stored above:
    /// CON 3, UNENC_L there, UNENC_H 0x07FFFFFF). The firmware's own window replaces it
    /// (Felucca's fm1_flash.h: CON &= ~2, UNENC_L / _H, CON |= 2; stock 0x93000, FM1 Open
    /// 0x73000, April OS 0x64000); a `--flash` file restored after this reads the same way.
    pub fn sfc_boot(&mut self, key: u16, app_end: usize) {
        self.sfc_key = Some(key);
        self.write32(0x4030c, XIP_BASE + (app_end - XIP_FLASH_OFFSET) as u32);
        self.write32(0x40308, 0x07ff_ffff);
        self.write32(0x40300, 3);
    }

    /// A saved flash chip in place of the booted one (fm1-live --flash): each 4 KiB sector that
    /// differs goes to the chip and, as the firmware's own erases and programs do, through to
    /// the XIP window (whose installed copy would otherwise still show the package's bytes).
    pub fn restore_flash(&mut self, image: &[u8]) {
        let n = image.len().min(spiflash::FLASH_SIZE);
        for at in (0..n).step_by(4096) {
            let end = (at + 4096).min(n);
            if self.flash.mem[at..end] != image[at..end] {
                self.flash.mem[at..end].copy_from_slice(&image[at..end]);
                self.flash.changed = Some((at, end));
                self.sync_xip();
                self.flash.revision = self.flash.revision.wrapping_add(1);
            }
        }
    }

    /// `ram_size` in bytes; `xip` = the app binary mapped at XIP_BASE.
    pub fn new(ram_size: usize, xip: Vec<u8>) -> Self {
        Self {
            board: Board::Fm1,
            bt_window: BT_WINDOW_BASE,
            xip,
            ram: Ram::new(ram_size),
            ovl: Ram::new(512 * 1024),
            sram2: Ram::new(256 * 1024),
            lcd: Lcd::new(),
            flash: SpiFlash::new(),
            console: Vec::new(),
            dbg_cause: 0,
            pc_guard_dirty: false,
            emu_cause: 0,
            lcd_timed: false,
            strict_align: false,
            xip_cache: None,
            core1_charging: false,
            core1_ticks: 0,
            core1_debt: 0,
            miss_hist: None,
            ticks: 0,
            periph: Periph::default(),
            sfr_lo: vec![0; (SFR_LO_END / 4) as usize],
            sfr: Default::default(),
            paused: [false; 2],
            lock_owner: None,
            crc: 0,
            rand: 0x9e37_79b9_7f4a_7c15,
            cpu1_release: false,
            cpu1_released_once: false,
            cpu1_idle: false,
            cpu1_survives: false,
            cpu1_fault: None,
            audio_trace: std::env::var_os("FM1_AUDIO_TRACE").is_some(),
            xip_flash: true,
            sfc_key: None,
            sfc_con: 0,
            sfc_unenc: (0, 0),
            sfc_from_flash: Vec::new(),
            gpio_writes: std::env::var("FM1_GPIO_TRACE").ok().map(|_| Default::default()),
            unknown_log: Vec::new(),
            flaky_core1: None,
            p33: Default::default(),
            hwdsp: Default::default(),
            hweq: hweq::HwEq::new(),
            hwsrc: Default::default(),
            usb: Default::default(),
            // (FM1_TRACE: a trace shows every instruction as it runs, not a probe's)
            loop_skip: machine::LoopSkip::new(std::env::var_os("FM1_NO_LOOPSKIP").is_none() && std::env::var_os("FM1_TRACE").is_none()),
            resets: 0,
            reset_req: None,
            boot: None,
        }
    }

    /// cpu0 starts (the runners call this before its first instruction): the chip as it is now
    /// is what a reset goes back to. A cpu0 that has already run is not at its start: nothing is
    /// taken, and a reset is then not carried out (`chip_reset`).
    pub fn take_boot(&mut self, cpu: &fm1_core::Cpu) {
        if self.boot.is_some() || cpu.insn_count != 0 {
            return;
        }
        // (a cpu that has not run has nothing in flight)
        let Some(state) = cpu.plain_state() else { return };
        const INFO: u32 = 24 + 32; // (write_boot_info's block)
        let r0 = cpu.regs[0];
        let info = r0.checked_sub(RAM_BASE).filter(|&o| (o + INFO) as usize <= self.ram.len()).map(|_| {
            (r0, (0..INFO).map(|i| self.ram.read8(r0 - RAM_BASE + i)).collect())
        });
        self.boot = Some(Box::new(Boot { cpu: state, sfr_lo: self.sfr_lo.clone(), sfr: self.sfr.clone(), sfc: (self.sfc_con, self.sfc_unenc), bt_window: self.bt_window, info }));
    }

    /// Is a chip reset due (the watchdog ran out, or the firmware asked through PR_PWR)?
    #[inline]
    pub fn reset_due(&self) -> bool {
        self.reset_req.is_some()
    }

    /// The reset that is due: cpu0 back where the loader started it, the chip's registers as
    /// the loader left them, core 1 held (the runner drops it: `resets` counts up), the boot
    /// parameters written again, the cause in P33's `RST_SRC`. What is not the chip's stays:
    /// RAM (a word a firmware leaves itself there, Felucca's "usb_update_mode" mailbox), the
    /// flash, the screen, and what `Periph::chip_reset` keeps.
    pub fn chip_reset(&mut self, cpu: &mut fm1_core::Cpu) {
        let Some(cause) = self.reset_req.take() else { return };
        let Some(boot) = self.boot.take() else {
            // (half a reset, the peripherals' without cpu0's, would be worse than none)
            eprintln!("chip reset not carried out: the emulator did not see cpu0 start");
            return;
        };
        if std::env::var_os("FM1_DUAL_TRACE").is_some() || std::env::var_os("FM1_RESET_TRACE").is_some() {
            eprintln!("chip reset at {} ms ({})", self.periph.ms(), if cause == p33::P33::RST_WDT { "watchdog" } else { "PR_PWR" });
        }
        self.resets += 1;
        self.periph.chip_reset();
        self.p33.chip_reset(cause);
        let mut old = std::mem::take(&mut self.usb);
        if old.cable {
            self.usb.plug(self.periph.hz);
        }
        self.usb.keep_recording(&mut old); // (a computer's recording goes on across the reset)
        (self.paused, self.lock_owner, self.cpu1_release, self.cpu1_released_once, self.crc) = ([false; 2], None, false, false, 0);
        (self.cpu1_idle, self.cpu1_fault) = (false, None);
        self.flash.chip_reset();
        cpu.set_plain_state(&boot.cpu);
        // the guards as at power-on: no causes, no PC limits (the restored SFRs hold none; the
        // runner gives the core `pc_windows` again before its next block)
        (self.dbg_cause, self.emu_cause) = (0, 0);
        (cpu.pc_win, cpu.pc_trip) = (None, false);
        self.pc_guard_dirty = true;
        // (core 1 is dropped: nothing it owes or stalled on carries over)
        (self.core1_charging, self.core1_ticks, self.core1_debt) = (false, 0, 0);
        self.sfr_lo.clone_from(&boot.sfr_lo);
        self.sfr = boot.sfr.clone();
        (self.sfc_con, self.sfc_unenc, self.bt_window) = (boot.sfc.0, boot.sfc.1, boot.bt_window);
        if let Some((at, bytes)) = boot.info.clone() {
            for (i, b) in bytes.into_iter().enumerate() {
                self.write8(at + i as u32, b);
            }
        }
        self.sfc_reread();
        self.loop_skip.next_try = 0;
        self.boot = Some(boot);
    }

    /// A P33 access done: what it asks of the chip (p33.rs). The watchdog starts its count
    /// when it is switched on or fed, and stops when it is switched off.
    fn p33_event(&mut self) {
        match self.p33.take_event() {
            Some(p33::Event::Wdt { fed }) => {
                let con = self.p33.reg(p33::P33::WDT_CON, false);
                let pd = &mut self.periph;
                let before = pd.wdt_at;
                if con & 0x10 == 0 {
                    pd.wdt_at = u64::MAX;
                } else if fed || pd.wdt_at == u64::MAX {
                    pd.wdt_at = pd.cycles + pd.cycle_of_ms(p33::P33::wdt_ms(con)).max(1);
                }
                if pd.wdt_at < before {
                    // sooner than anything the run loop planned for (switched on, or a shorter
                    // period fed): compiled code leaves at this write, the loop looks again
                    pd.next_due = 0;
                } else {
                    // (a feed moves the deadline later: nothing comes due sooner, and a polling
                    // loop that feeds it each round can still be skipped)
                    pd.next_due = pd.next_due.min(pd.wdt_at);
                }
            }
            Some(p33::Event::Reset) => {
                self.reset_req.get_or_insert(p33::P33::RST_SOFT);
                self.periph.next_due = 0;
            }
            None => {}
        }
    }

    /// Does a 32-bit load from `addr` only look (no side effect, nothing that depends on the
    /// clock)? `read32_slow` region by region; byte and halfword loads outside RAM read the
    /// aligned word. What a polling loop may read and still go round unchanged.
    pub(crate) fn pure_read(&self, addr: u32) -> bool {
        let fm1 = self.board == Board::Fm1;
        match self.region(addr) {
            Region::Xip | Region::XipFlash | Region::Ram | Region::Ovl | Region::Sram2 => true,
            Region::Usb => self.usb.pure(addr),
            Region::Irqc | Region::Iomap | Region::Clk | Region::Ana | Region::Bt | Region::Dbg | Region::Guard => true,
            Region::Spi2 | Region::Crc | Region::P33 | Region::Alnk | Region::Uart1 | Region::Sfcenc => true,
            // the tick counter's word counts the clock
            Region::Irqc2 => addr != 0x1eef0f0,
            // the timers' control and period words; the counters count the clock, and other
            // words tick `Soc::ticks`
            Region::Timer => matches!(addr, 0x10800 | 0x10900 | 0x10908) || matches!(addr & !0x300, 0x10400 | 0x10408),
            // the SMK-37's matrix rows and SPI1 busy follow the clock; its ADC is left alone too (SPI1
            // busy on the FM-1 too with `lcd_timed`)
            Region::Gpio | Region::Adc => fm1,
            Region::Spi1 => fm1 && !self.lcd_timed,
            // (the FFT / vector unit's done bit comes with the clock)
            Region::Spi0 | Region::Rand | Region::HwDsp | Region::HwEq | Region::HwSrc | Region::Unknown => false,
        }
    }

    /// May a polling loop's round store to `addr` (a 32-bit store; byte and halfword stores
    /// outside RAM write the aligned word)? Only where the round's effect is compared after it:
    /// the P33 port, and the FM-1's SARADC (its conversions are instant).
    pub(crate) fn loop_write_ok(&self, addr: u32) -> bool {
        match self.region(addr) {
            Region::P33 => true,
            Region::Adc => self.board == Board::Fm1 && matches!(addr, 0x13100 | 0x13104),
            _ => false,
        }
    }

    /// A load by core 1: what `read32` returns, unless a flaky core 1 misreads it (RAM only).
    pub fn core1_read32(&mut self, addr: u32) -> u32 {
        let v = self.read32(addr);
        self.core1_misread(addr, 4, v)
    }

    pub fn core1_read16(&mut self, addr: u32) -> u16 {
        let v = self.read16(addr);
        self.core1_misread(addr, 2, v as u32) as u16
    }

    pub fn core1_read8(&mut self, addr: u32) -> u8 {
        let v = self.read8(addr);
        self.core1_misread(addr, 1, v as u32) as u8
    }

    #[inline]
    fn core1_misread(&mut self, addr: u32, size: u32, v: u32) -> u32 {
        match self.flaky_core1.as_mut() {
            Some(f) if (addr.wrapping_sub(RAM_BASE) as usize) < self.ram.len() => f.load(addr, size, v),
            _ => v,
        }
    }

    /// Wire the chip into `board`'s panel (and its flash chip's id).
    pub fn set_board(&mut self, board: Board) {
        self.board = board;
        self.periph.board = board;
        if board == Board::Smk37 {
            self.flash.jedec = [0xeb, 0x60, 0x14];
        }
    }

    /// All unknown accesses so far — the peripheral bring-up TODO list.
    /// Advance the peripheral clock by `n` CPU cycles, moving any finished
    /// audio half out of RAM (int32 L/R holding a 24-bit sample in the low
    /// bits — Felucca writes Q15 << 7, audio.c OUT_SHIFT — so >> 8 to i16).
    pub fn advance(&mut self, n: u64) {
        let due = self.periph.advance(n);
        if due.wdt {
            self.reset_req.get_or_insert(p33::P33::RST_WDT);
        }
        if let Some((at, b)) = due.uart_rx {
            self.write8(at, b);
        }
        if due.usb_frame {
            let pullup = self.sfr_get(usb::USB_IO_CON0) & 0x40 != 0;
            let mut u = std::mem::take(&mut self.usb);
            u.frame(self, pullup);
            self.usb = u;
        }
        if let Some((adr, words)) = due.audio_half {
            if self.audio_trace {
                eprintln!("half {} ms {}", self.periph.ms(), self.periph.cycles);
            }
            for i in 0..words {
                let w = self.read32(adr.wrapping_add(i * 4)) as i32;
                self.periph.audio.push((w >> 8).clamp(-32768, 32767) as i16);
            }
        }
    }

    /// A USB cable to a host (usb.rs): the SIE answers, and a frame every millisecond.
    pub fn usb_plug(&mut self) {
        self.usb.plug(self.periph.hz);
        self.periph.usb_period = (self.periph.hz / 1000).max(1);
        self.periph.usb_next = self.periph.cycles + self.periph.usb_period;
        self.periph.next_due = 0;
    }

    /// The highest-priority enabled pending interrupt: (irq, prio).
    #[inline]
    pub fn pending_irq(&self) -> Option<(u8, u8)> {
        self.periph.pending_irq()
    }

    /// Core `core` (0, 1) is paused by the other through its CON register's bit 2
    /// (0x1EEE000 / 0x1EEE004): the stock firmware stops core 1 around flash work.
    pub fn core_paused(&self, core: u32) -> bool {
        core < 2 && self.paused[core as usize]
    }

    /// The highest-priority interrupt pending for core `core` (its own enable bank).
    pub fn pending_irq_for(&self, core: u32) -> Option<(u8, u8)> {
        self.periph.pending_irq_for(core)
    }

    pub fn unknown_accesses(&self) -> &[UnknownAccess] {
        &self.unknown_log
    }

    /// The debug console contents (DBG_MSG box writes).
    pub fn console_string(&self) -> String {
        String::from_utf8_lossy(&self.console).into_owned()
    }

    fn log_unknown(&mut self, addr: u32, width: u8, is_write: bool, value: u32) {
        self.unknown_log.push(UnknownAccess {
            addr,
            width,
            is_write,
            value,
        });
        // keep the last 8192..16384 (dropping one at a time shifted the whole log per access)
        if self.unknown_log.len() >= 16384 {
            self.unknown_log.drain(..8192);
        }
    }

    #[inline(always)]
    fn region(&self, addr: u32) -> Region {
        let o = addr.wrapping_sub(SFR_LO);
        if o < SFR_HI - SFR_LO {
            return SFR_MAP[(o >> 3) as usize];
        }
        // the rest below 0x0100_0000: no memory region starts there (RAM, SRAM2, XIP, the overlay
        // and the interrupt controllers all lie above), so the order below is unchanged for them
        if addr < 0x0100_0000 {
            return match addr {
                0x50000..=0x501ff => Region::Gpio,
                0x51000..=0x5103f => Region::Iomap,
                0x40300..=0x4030f => Region::Sfcenc,
                // the second USB PHY (the SMK-37's LED commons at 0x16A0C, its pads used as GPIO;
                // its stock firmware powers the PHY up at boot and waits for 0x16800 bit 4). The
                // FM-1's stock firmware touches the block too, as before: unknown.
                0x16800..=0x16a1f if self.board == Board::Smk37 => Region::Usb,
                0x20000..=0x2ffff => Region::Bt,
                hweq::BASE..=hweq::END | hweq::RAM..=hweq::RAM_END => Region::HwEq,
                hwsrc::BASE..=hwsrc::END => Region::HwSrc,
                _ => Region::Unknown,
            };
        }
        match addr {
            a if a >= XIP_BASE && (a - XIP_BASE) < self.xip.len() as u32 => Region::Xip,
            // past the loaded image the XIP window still shows the flash chip (the stock
            // firmware reads its USR preset area, which it writes over SPI, through XIP)
            a if self.xip_flash && a >= XIP_BASE && (a - XIP_BASE) as usize + XIP_FLASH_OFFSET < spiflash::FLASH_SIZE => Region::XipFlash,
            a if a >= RAM_BASE && (a - RAM_BASE) < self.ram.len() as u32 => Region::Ram,
            a if a >= OVL_BASE && (a - OVL_BASE) < self.ovl.len() as u32 => Region::Ovl,
            a if a >= SRAM2_BASE && (a - SRAM2_BASE) < self.sram2.len() as u32 => Region::Sram2,
            0x1eee000..=0x1eee0ff => Region::Irqc,
            0x1eef000..=0x1eef3ff => Region::Irqc2,
            0x1eee240..=0x1eee24f => Region::Dbg,
            hwdsp::BASE..=hwdsp::END => Region::HwDsp,
            0x1eee250..=0x1eee3ff => Region::Guard,
            _ => Region::Unknown,
        }
    }

    /// FM1_TPROF: a flash cache miss counted by core and line
    #[cold]
    fn miss_note(&mut self, addr: u32) {
        let core = self.core1_charging as u64;
        if let Some(h) = self.miss_hist.as_mut() {
            *h.entry(core << 32 | (addr & !63) as u64).or_insert(0) += 1;
        }
    }

    /// A flash cache miss's ticks: core 1's own time while it runs, else the clock's
    #[inline(always)]
    fn xip_charge(&mut self, t: u64) {
        if self.core1_charging {
            self.core1_ticks += t;
        } else {
            self.periph.cycles += t;
        }
    }

    /// The CPU's guards (as Felucca's and Dinghy's hal/fm1_guard.h arm them). A guard that trips
    /// sets its cause in DBG_MSG and raises the CPU exception, interrupt 1 (fm1_irq.h's fault
    /// report reads both); fm1-emu did not have them, so code that trips one on an FM-1 (a probe
    /// run from RAM outside the PC limits, 2026-10-24) ran on here.
    pub fn guard_trip(&mut self, bit: u32) {
        self.dbg_cause |= 1 << bit;
        self.raise_exception();
    }

    /// The CPU exception (interrupt 1) is raised: pending until cpu0 takes it, when the firmware
    /// has it enabled (as Felucca's, Dinghy's and fm1-dock's fm1_irq.h do). Not enabled, it is
    /// dropped (the cause stays in DBG_MSG / EMU_MSG): pending for good, it would keep the run
    /// from skipping a loop or an idle stretch ever again.
    fn raise_exception(&mut self) {
        if self.periph.irq_enabled(0, 1) {
            self.periph.exc_pending = true;
            self.periph.next_due = 0;
        }
    }

    /// The HW DSP / EQ / SRC units' clock: CPU cycles (their jobs' costs are), the clock's ticks
    /// over `Periph::cycle_ticks`
    #[inline]
    fn unit_clock(&self) -> u64 {
        self.periph.cycles / self.periph.cycle_ticks.max(1)
    }

    /// A misaligned 16- / 32-bit RAM access (`strict_align`): EMU_MSG bit 0 and the exception
    #[cold]
    fn misaligned(&mut self, addr: u32, size: u32) {
        if std::env::var_os("FM1_STRICT_LOG").is_some() {
            eprintln!("misaligned {size}-byte access at {addr:#010x}");
        }
        self.emu_cause |= 1;
        self.raise_exception();
    }

    /// A bus-invalid access (DBG_EN's bits 4 / 5: AXI read / write): a guard trip once armed
    fn bus_invalid(&mut self, bit: u32) {
        if self.sfr_get(0x1eee340) & (1 << bit) != 0 {
            self.guard_trip(bit);
        }
    }

    /// The PC limits (0x1EEE380 / 84: window 0 high / low, 88 / 8C: window 1): code may run inside
    /// either window and an instruction fetched outside both raises the exception (DBG_MSG bit
    /// 12). None until a firmware sets them.
    pub fn pc_windows(&mut self) -> Option<[(u32, u32); 2]> {
        let w = [(self.sfr_get(0x1eee384), self.sfr_get(0x1eee380)), (self.sfr_get(0x1eee38c), self.sfr_get(0x1eee388))];
        (w[0].1 != 0 || w[1].1 != 0).then_some(w)
    }

    fn sfr_get(&mut self, addr: u32) -> u32 {
        if addr < SFR_LO_END && addr & 3 == 0 {
            return self.sfr_lo[(addr >> 2) as usize];
        }
        *self.sfr.get(&addr).unwrap_or(&0)
    }

    fn sfr_set(&mut self, addr: u32, v: u32) {
        if addr < SFR_LO_END && addr & 3 == 0 {
            self.sfr_lo[(addr >> 2) as usize] = v;
            return;
        }
        self.sfr.insert(addr, v);
    }

    /// SPI1/MMIO side effects on 32-bit writes.
    fn spi1_write(&mut self, addr: u32, value: u32) {
        match addr {
            0x11d00 => {
                // CON: bit14 clears the pending transfer-done (and its IRQ 16), bit13 enables
                // the IRQ. The FM-1's stock LCD driver disables it before a transfer's interrupt
                // is due; taken anyway, the interrupt finds its handler idle, which returns
                // without clearing it, again and again.
                if value & 0x4000 != 0 {
                    self.periph.spi1_pending = false;
                }
                if value & 0x2000 == 0 {
                    self.periph.spi1_irq_off();
                }
                // (bit 15, done, is the read's; bit 14 only acts)
                self.sfr_set(addr, value & !0xc000);
            }
            0x11d04 => self.sfr_set(addr, value), // BAUD
            0x11d08 => {
                // BUF: single byte, D/C = PC8 state (SMK-37: PB5; its group B is the pad LEDs)
                if let Some(dc) = self.lcd_dc() {
                    self.lcd.feed(value as u8, dc);
                }
                self.sfr_set(addr, value);
                self.spi1_done(1);
            }
            0x11d0c => self.sfr_set(addr, value), // ADR
            0x11d10 => {
                // CNT: DMA `value` bytes from RAM[ADR] through the panel
                let adr = self.sfr_get(0x11d0c);
                let n = value.min(0x40000);
                // the DMA source is a bus address (RAM, SRAM or XIP)
                let bytes: Vec<u8> = (0..n).map(|i| self.read8(adr.wrapping_add(i))).collect();
                match self.lcd_dc() {
                    Some(dc) => {
                        for b in bytes {
                            self.lcd.feed(b, dc);
                        }
                    }
                    None => self.periph.smk.pad_frame(&bytes),
                }
                self.sfr_set(addr, 0);
                self.spi1_done(n);
            }
            _ => self.sfr_set(addr, value),
        }
    }

    /// Where SPI1's bytes go: the LCD with its D/C pin's state (FM-1: PC8; SMK-37: PB5, while
    /// IOMAP_CON1 bit 4 picks pin group A), or None: the SMK-37's pad LEDs (group B).
    fn lcd_dc(&mut self) -> Option<bool> {
        match self.board {
            Board::Fm1 => Some(self.sfr_get(0x50080) & 0x100 != 0),
            Board::Smk37 => (self.sfr_get(0x51020) & 0x10 == 0).then(|| self.sfr_get(0x50040) & 0x20 != 0),
        }
    }

    /// SPI2 (0x11E00, CON/BAUD/BUF/ADR/CNT like SPI1): the stock firmware shifts the key
    /// matrix's column pattern into the two 74HC595s with it, one 2-byte DMA per column,
    /// driven by its IRQ 37 handler (which also reads the rows and pulses RCLK on PA1).
    fn spi2_write(&mut self, addr: u32, value: u32) {
        match addr {
            0x11e00 => {
                if value & 0x4000 != 0 {
                    self.periph.spi2_pending = false;
                }
                self.sfr_set(addr, value & !0xc000);
            }
            0x11e08 => {
                self.periph.sr_shift_byte(value as u8);
                self.sfr_set(addr, value);
                self.spi2_started(1);
            }
            0x11e10 => {
                let adr = self.sfr_get(0x11e0c);
                for i in 0..value.min(64) {
                    let b = self.read8(adr.wrapping_add(i));
                    self.periph.sr_shift_byte(b);
                }
                self.sfr_set(addr, 0);
                self.spi2_started(value);
            }
            _ => self.sfr_set(addr, value),
        }
    }

    fn spi2_started(&mut self, bytes: u32) {
        let con = self.sfr_get(0x11e00);
        let baud = self.sfr_get(0x11e04);
        self.periph.spi2_start(bytes, baud, con & 0x2000 != 0);
    }

    /// A SPI1 transfer started: it takes the bytes' time on the wire (`Periph::spi1_start`): on the
    /// SMK-37, on the FM-1 with `lcd_timed` (else done at once there), and whenever CON bit 13
    /// (interrupt enable) is set, since its end is IRQ 16, the stock LCD driver's DMA-done (its
    /// layer flush waits for the handler). FM-1 V16's driver enables it, starts the DMA and only
    /// then marks the DMA as running, so an interrupt at the start found its handler idle, which
    /// returned without clearing it, again and again.
    fn spi1_done(&mut self, bytes: u32) {
        let irq = self.sfr_get(0x11d00) & 0x2000 != 0;
        if self.board == Board::Smk37 || irq || self.lcd_timed {
            let baud = self.sfr_get(0x11d04);
            self.periph.spi1_start(bytes, baud, irq);
        }
    }

    fn timer_read(&mut self, addr: u32) -> u32 {
        // "time" base ticks so timeout/wait loops make progress; count
        // registers report a moving value, the rest stored bits.
        self.ticks = self.ticks.wrapping_add(1);
        if addr & 0xff == 0x04 {
            let t = (self.ticks * 7) as u32;
            return t;
        }
        self.sfr_get(addr)
    }
}

/// `Soc::lcd_timed` as the native front ends (fm1-live, fm1-emu) take it from the environment: the
/// timed LCD is part of the realistic mode, on with FM1_STRICT=1 or FM1_LCD_TIMED=1, and
/// FM1_LCD_INSTANT=1 turns it off again (also under FM1_STRICT). Off by default, so the default
/// emulator's timing (and the catalog's recorded audio) stays as it was.
pub fn lcd_timed_from_env() -> bool {
    // (set at all is on, as the emulator's other switches)
    let on = |k: &str| std::env::var_os(k).is_some();
    (on("FM1_STRICT") || on("FM1_LCD_TIMED")) && !on("FM1_LCD_INSTANT")
}

impl Bus for Soc {
    fn irq_window(&mut self) {
        if self.periph.any_pending() {
            self.periph.next_due = 0;
        }
    }
    fn service(&mut self) {
        self.periph.next_due = 0;
    }

    fn idle(&mut self, core: u32) {
        if core == 0 {
            self.periph.idle();
        } else {
            self.cpu1_idle = true; // (a runner stepping core 1 against the SoC: fm1-emu's)
        }
    }

    fn hw_lock(&mut self, core: u32, set: bool) -> bool {
        match (set, self.lock_owner) {
            (true, Some(c)) if c != core => false,
            (true, _) => {
                self.lock_owner = Some(core);
                true
            }
            (false, Some(c)) if c == core => {
                self.lock_owner = None;
                true
            }
            (false, _) => true,
        }
    }

    #[inline(always)]
    fn tick(&mut self) -> bool {
        self.periph.tick()
    }
    #[inline(always)]
    fn irq_pending(&self) -> bool {
        self.periph.any_pending()
    }
    fn jit_mem(&mut self) -> Option<fm1_core::JitMem> {
        Some(fm1_core::JitMem {
            ram: self.ram.as_mut_ptr(),
            ram_base: RAM_BASE,
            ram_len: self.ram.len() as u32,
            ram_code: self.ram.code_ptr(),
            xip: self.xip.as_ptr(),
            xip_base: XIP_BASE,
            xip_len: self.xip.len() as u32,
        })
    }
    fn code_writes(&self) -> Option<u64> {
        Some(self.ram.code_writes)
    }
    fn code_mark(&mut self, lo: u32, hi: u32) {
        self.ram.mark_code(lo.wrapping_sub(RAM_BASE), hi.wrapping_sub(RAM_BASE));
    }
    #[inline]
    fn tick_room(&self) -> u64 {
        self.periph.tick_room()
    }
    #[inline]
    fn add_ticks(&mut self, n: u64) {
        self.periph.cycles += n;
    }
    #[inline]
    fn next_tick_due(&self) -> bool {
        self.periph.next_tick_due()
    }

    // RAM fast paths: same results as the region dispatch below (byte and
    // halfword accesses select within the aligned word), without walking it

    #[inline(always)]
    fn read8(&mut self, addr: u32) -> u8 {
        let o = addr.wrapping_sub(RAM_BASE);
        if (o as usize) < self.ram.len() {
            return self.ram.read8(o);
        }
        self.read32(addr & !3).to_le_bytes()[(addr & 3) as usize]
    }

    #[inline(always)]
    fn read16(&mut self, addr: u32) -> u16 {
        if self.strict_align && addr & 1 != 0 && (addr.wrapping_sub(RAM_BASE) as usize) < self.ram.len() {
            self.misaligned(addr, 2);
        }
        // halfwords sit at offsets 0 and 2 of the word (the old `>> 8` for
        // offset 2 returned bytes 1-2 and sheared every 16-bit pixel canvas)
        let v = self.read32(addr & !3);
        (v >> ((addr & 2) * 8)) as u16
    }

    #[inline(always)]
    fn read32(&mut self, addr: u32) -> u32 {
        let o = addr.wrapping_sub(RAM_BASE) as usize;
        if o + 4 <= self.ram.len() {
            if self.strict_align && addr & 3 != 0 {
                self.misaligned(addr, 4);
            }
            return self.ram.read32(o as u32);
        }
        let x = addr.wrapping_sub(XIP_BASE) as usize;
        if x + 4 <= self.xip.len() {
            if let Some(c) = self.xip_cache.as_mut() {
                if c.access(addr) {
                    self.xip_charge(XipCache::MISS_TICKS);
                    self.miss_note(addr);
                }
            }
            return u32::from_le_bytes([self.xip[x], self.xip[x + 1], self.xip[x + 2], self.xip[x + 3]]);
        }
        self.read32_slow(addr)
    }

    #[inline(always)]
    fn fetch_cost(&mut self, pc: u32) {
        if pc.wrapping_sub(XIP_BASE) < 0x0100_0000 {
            if let Some(c) = self.xip_cache.as_mut() {
                let (m1, m2) = (c.access(pc), c.access(pc.wrapping_add(4)));   /* (a 6-byte one may cross a line) */
                let miss = m1 as u64 + m2 as u64;
                self.xip_charge(miss * XipCache::MISS_TICKS);
                if m1 { self.miss_note(pc); }
                if m2 { self.miss_note(pc.wrapping_add(4)); }
            }
        }
    }

    /// An instruction fetch: never a misaligned access (`strict_align`), code is halfword-aligned
    #[inline(always)]
    fn fetch32(&mut self, addr: u32) -> u32 {
        let o = addr.wrapping_sub(RAM_BASE) as usize;
        if o + 4 <= self.ram.len() {
            return self.ram.read32(o as u32);
        }
        self.read32(addr)
    }

    #[inline(always)]
    fn write8(&mut self, addr: u32, value: u8) {
        let o = addr.wrapping_sub(RAM_BASE);
        if (o as usize) < self.ram.len() {
            return self.ram.write8(o, value);
        }
        self.write8_slow(addr, value)
    }

    #[inline(always)]
    fn write16(&mut self, addr: u32, value: u16) {
        if self.strict_align && addr & 1 != 0 && (addr.wrapping_sub(RAM_BASE) as usize) < self.ram.len() {
            self.misaligned(addr, 2);
        }
        // merged at (addr & 2) within the aligned word, as the slow path does
        let o = (addr & !1).wrapping_sub(RAM_BASE) as usize;
        if o + 2 <= self.ram.len() && addr & 1 == 0 {
            return self.ram.write16(o as u32, value);
        }
        self.write16_slow(addr, value)
    }

    #[inline(always)]
    fn write32(&mut self, addr: u32, value: u32) {
        let o = addr.wrapping_sub(RAM_BASE) as usize;
        if o + 4 <= self.ram.len() {
            if self.strict_align && addr & 3 != 0 {
                self.misaligned(addr, 4);
            }
            return self.ram.write32(o as u32, value);
        }
        self.write32_slow(addr, value)
    }
}

impl Soc {
    #[inline(never)]
    fn read32_slow(&mut self, addr: u32) -> u32 {
        match self.region(addr) {
            Region::Xip => {
                let o = (addr - XIP_BASE) as usize;
                let g = |i: usize| self.xip.get(i).copied().unwrap_or(0);
                u32::from_le_bytes([g(o), g(o + 1), g(o + 2), g(o + 3)])
            }
            Region::Ram => self.ram.read32(addr - RAM_BASE),
            Region::Sfcenc => self.sfr_get(addr),
            Region::XipFlash => {
                let o = (addr - XIP_BASE) as usize + XIP_FLASH_OFFSET;
                let g = |i: usize| self.flash.mem.get(i).copied().unwrap_or(0xff);
                u32::from_le_bytes([g(o), g(o + 1), g(o + 2), g(o + 3)])
            }
            Region::Ovl => self.ovl.read32(addr - OVL_BASE),
            Region::Sram2 => self.sram2.read32(addr - SRAM2_BASE),
            Region::Irqc => {
                // 0x1eee008 bit 14: controller ready (the early init spins on it);
                // 0x1eee000 / 0x1eee004 (C0_CON / C1_CON): see `core_paused`
                let v = self.sfr_get(addr);
                match addr {
                    0x1eee008 => v | 0x4000,
                    0x1eee000 | 0x1eee004 => {
                        let core = ((addr >> 2) & 1) as usize;
                        (v & !0x14) | if self.paused[core] { 0x10 } else { 0 }
                    }
                    _ => v,
                }
            }
            Region::Usb if (usb::USB_LO..=usb::USB_HI).contains(&addr) => self.usb.read(addr, self.periph.cycles),
            // (the second USB PHY ready: bit 4 of its control word, once it is powered up)
            Region::Usb if addr == 0x16800 => self.sfr_get(addr) | 0x10,
            Region::Iomap | Region::Usb | Region::Clk => self.sfr_get(addr),
            // JL_ANA (analog: WLA, DAC trim). 0x11978 is the DAC trim measurement after the
            // strobe at the same word: bit 5 = conversion done, bits 8-15 = the value (mid
            // scale here), bit 18 = "this trim step is right". With these the stock
            // firmware's audio_ana calibration finishes at once instead of polling forever
            // (a trim only nudges the DAC's DC offset).
            Region::Ana => {
                let v = self.sfr_get(addr);
                if addr == 0x11978 { (v & !0xff00) | 0x4_0000 | 0x8000 | 0x20 } else { v }
            }
            // the Bluetooth baseband (bsfr 0x20000; no radio here): reads 0, writes are
            // dropped (self-clearing requests like bt_clkn_compare_arm's capture at 0x2001C
            // must read done), except 0x2FC44, the base of the RAM window its buffers live in,
            // which the BT init writes (its .bd_base, which opens the BT bss, SDK
            // btctler_lib_bss.ld). The stock BT stack checks buffers against it: descriptors
            // within 2 KiB and below its own .bd_base + 1088, bulk buffers within 32 KiB; with
            // V13's base kept, the SMK-37 firmware's check failed and it reset.
            Region::Bt => {
                if addr == 0x2fc44 { self.bt_window } else { 0 }
            }
            // EMU_MSG: its causes only (what was written is not kept)
            Region::Irqc2 if addr == 0x1eef0d4 => self.emu_cause,
            Region::Gpio | Region::Adc | Region::Alnk | Region::Irqc2 | Region::Uart1 => {
                match self.periph.read(addr) {
                    Some(v) => v,
                    None => self.sfr_get(addr),
                }
            }
            Region::Timer => {
                match self.periph.read(addr) {
                    Some(v) => v,
                    None => self.timer_read(addr),
                }
            }
            Region::Spi0 => self.flash.read(addr),
            // SPI2: transfers finish at once (CON bit 15 reads done)
            Region::Spi2 => self.sfr_get(addr) | if addr == 0x11e00 { 0x8000 } else { 0 },
            Region::Spi1 => {
                if addr == 0x11d00 {
                    // CON: transfer-done, once the bytes have gone out (the FM-1 without `lcd_timed`:
                    // always, so polls finish)
                    let busy = (self.board == Board::Smk37 || self.lcd_timed) && self.periph.cycles < self.periph.spi1_done_at;
                    self.sfr_get(addr) | if busy { 0 } else { 0x8000 }
                } else if addr == 0x11d10 {
                    self.sfr_get(addr)
                } else {
                    self.sfr_get(addr)
                }
            }
            // DBG_MSG: the causes of the guards that tripped (`guard_trip`); a word written there
            // goes to the console (the emulator's own channel) and is not read back
            Region::Dbg => if addr == 0x1eee244 { self.dbg_cause } else { self.sfr_get(addr) },
            Region::Guard => self.sfr_get(addr),
            Region::Crc => if addr == 0x13504 { self.crc as u32 } else { 0 },
            // JL_RAND (0x13B00 R64L, 0x13B04 R64H): a new number at every read, from a fixed
            // seed so runs repeat. Read as 0 it hung the stock firmware ~8 s after boot: a
            // routine in its audio driver parks the task when a random word equals a fixed
            // descrambled one, which real hardware never hits.
            Region::Rand => {
                let mut x = self.rand;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.rand = x;
                (if addr == 0x13b00 { x } else { x >> 32 }) as u32
            }
            Region::P33 => self.p33.read(addr),
            Region::HwDsp => self.hwdsp.read(addr, self.unit_clock()),
            Region::HwEq => self.hweq.read(addr, self.unit_clock()),
            Region::HwSrc => self.hwsrc.read(addr, self.unit_clock()),
            Region::Unknown => {
                if addr < NULL_PAGE {
                    self.bus_invalid(4); // a read of the NULL page (DBG_MSG bit 4, AXI read)
                }
                self.log_unknown(addr, 4, false, 0);
                0
            }
        }
    }

    #[inline(never)]
    fn write8_slow(&mut self, addr: u32, value: u8) {
        let cur = self.read32(addr & !3);
        let shift = (addr & 3) * 8;
        let mask = !(0xffu32 << shift);
        let merged = (cur & mask) | ((value as u32) << shift);
        self.write32(addr & !3, merged);
    }

    #[inline(never)]
    fn write16_slow(&mut self, addr: u32, value: u16) {
        let cur = self.read32(addr & !3);
        let shift = (addr & 2) * 8;
        let mask = !(0xffffu32 << shift);
        let merged = (cur & mask) | ((value as u32) << shift);
        self.write32(addr & !3, merged);
    }

    #[inline(never)]
    fn write32_slow(&mut self, addr: u32, value: u32) {
        match self.region(addr) {
            Region::Xip | Region::XipFlash => self.log_unknown(addr, 4, true, value), // flash writes go via the ROM
            Region::Ram => self.ram.write32(addr - RAM_BASE, value),
            Region::Ovl => self.ovl.write32(addr - OVL_BASE, value),
            Region::Sram2 => self.sram2.write32(addr - SRAM2_BASE, value),
            Region::Irqc => {
                if addr == 0x1eee000 || addr == 0x1eee004 {
                    // C0_CON / C1_CON: writing bit 2 pauses that core (bit 4 reads "paused");
                    // a write with bit 3 and without bit 2 runs it again. Both are pulses:
                    // the stock IPC pauses with `|= 4` and resumes with `|= 8` on the same word.
                    let core = ((addr >> 2) & 1) as usize;
                    if value & 4 != 0 {
                        self.paused[core] = true;
                    } else if value & 8 != 0 {
                        self.paused[core] = false;
                        if core == 1 && !self.cpu1_released_once {
                            // M-VAVE's FM1 Open SDK releases core 1 here (C1_CON |= 8), not at 0x10008
                            // (once: the stock firmware's IPC resumes core 1 the same way)
                            self.cpu1_released_once = true;
                            self.cpu1_release = true;
                            self.periph.next_due = 0;
                        }
                    }
                    self.sfr_set(addr, value & !0x14);
                } else {
                    self.sfr_set(addr, value);
                }
            }
            Region::Gpio => {
                if let Some(h) = self.gpio_writes.as_mut() {
                    *h.entry(addr).or_insert(0) += 1;
                }
                // PD0 drives the flash chip select; PA1/PA3/PA4 clock the
                // key-matrix shift registers
                if addr == 0x500c0 {
                    self.flash.set_cs(value & 1 == 0);
                    self.sync_xip();
                }
                self.periph.write(addr, value);
                self.sfr_set(addr, value)
            }
            Region::Alnk | Region::Irqc2 => {
                if addr == 0x1eef0d4 {
                    // EMU_MSG: a write clears the causes it has 1s for and is not kept (Felucca's,
                    // Dinghy's and fm1-dock's fm1_irq.h write all ones at init: kept, every later
                    // read would show every cause)
                    self.emu_cause &= !value;
                    return;
                }
                if !self.periph.write(addr, value) {
                    self.sfr_set(addr, value);
                }
            }
            Region::Spi0 => {
                self.flash.write(addr, value);
                self.sync_xip();
            }
            Region::Iomap => self.sfr_set(addr, value),
            Region::Ana => self.sfr_set(addr, value),
            Region::P33 => {
                self.p33.write(addr, value);
                self.p33_event();
            }
            Region::Bt => {
                if addr == 0x2fc44 {
                    self.bt_window = value;
                }
            }
            Region::Clk => {
                if addr == 0x10008 && value & 8 != 0 && self.sfr_get(addr) & 8 == 0 && !self.cpu1_released_once {
                    // (once, as C1_CON's: set again later, with core 1 running, nothing would take
                    // the latch, and the runner would stop cpu0 after every block call)
                    self.cpu1_released_once = true;
                    self.cpu1_release = true;
                    self.periph.next_due = 0; // the runner looks at it before the next instruction
                }
                self.sfr_set(addr, value)
            }
            Region::Usb if (usb::USB_LO..=usb::USB_HI).contains(&addr) => self.usb.write(addr, value, self.periph.cycles),
            Region::Usb => {
                if addr == 0x16a0c && self.board == Board::Smk37 {
                    self.periph.smk.commons = value;
                }
                self.sfr_set(addr, value)
            }
            Region::Uart1 => {
                if !self.periph.write(addr, value) {
                    self.sfr_set(addr, value);
                }
            }
            Region::Adc => {
                if !self.periph.write(addr, value) {
                    self.sfr_set(addr, value);
                }
            }
            Region::Timer => {
                if !self.periph.write(addr, value) {
                    self.sfr_set(addr, value);
                }
            }
            Region::Spi1 => self.spi1_write(addr, value),
            Region::Spi2 => self.spi2_write(addr, value),
            // the CRC-16 unit (chip_crc16): [+4] = the register (the SDK seeds it with 0),
            // each write to [+0] feeds one byte through CRC-16/CCITT (0x1021, MSB first).
            // The stock NOR file system checks every directory entry with it.
            Region::Rand => {}
            Region::Crc => match addr {
                0x13500 => {
                    let mut c = self.crc ^ ((value as u16 & 0xff) << 8);
                    for _ in 0..8 {
                        c = if c & 0x8000 != 0 { (c << 1) ^ 0x1021 } else { c << 1 };
                    }
                    self.crc = c;
                }
                0x13504 => self.crc = value as u16,
                _ => {}
            },
            Region::Dbg => {
                match dbg_line(addr) {
                    "msg" => {
                        self.console.extend_from_slice(&value.to_le_bytes());
                    }
                    "msg_clr" => self.dbg_cause &= !value,
                    _ => {}
                }
                self.sfr_set(addr, value);
            }
            Region::Guard => {
                if (0x1eee380..=0x1eee38f).contains(&addr) {
                    // (the runner hands the core its new windows before the next block: the run
                    // stops at once, as for an event due now)
                    self.pc_guard_dirty = true;
                    self.periph.next_due = 0;
                }
                self.sfr_set(addr, value);
            }
            Region::Sfcenc => {
                match addr {
                    0x40300 => self.sfc_con = value & 0xff,
                    0x40308 => self.sfc_unenc.1 = value,
                    0x4030c => self.sfc_unenc.0 = value,
                    _ => {}
                }
                self.sfr_set(addr, value);
                self.sfc_reread();
            }
            Region::HwDsp => {
                // (the unit reads and writes RAM through the bus: taken out while it runs)
                let mut u = std::mem::take(&mut self.hwdsp);
                let cycles = self.unit_clock();
                u.write(self, addr, value, cycles);
                self.hwdsp = u;
            }
            Region::HwEq => {
                let mut u = std::mem::take(&mut self.hweq);
                let cycles = self.unit_clock();
                u.write(self, addr, value, cycles);
                self.hweq = u;
            }
            Region::HwSrc => {
                let mut u = std::mem::take(&mut self.hwsrc);
                let cycles = self.unit_clock();
                u.write(self, addr, value, cycles);
                self.hwsrc = u;
            }
            Region::Unknown => {
                if addr < NULL_PAGE {
                    self.bus_invalid(5); // a write to the NULL page (DBG_MSG bit 5, AXI write)
                }
                self.log_unknown(addr, 4, true, value)
            }
        }
    }
}

/// M-VAVE's stock firmware out of its update package: the XIP window's contents (the app
/// area, unscrambled), the whole flash (the image as written, grown to the chip's size, the
/// USR patch area given this emulator's default bank when blank and then an optional DX7
/// bank in slots 1-32) and the entry point.
pub struct Stock {
    pub xip: Vec<u8>,
    pub flash: Vec<u8>,
    pub entry: u32,
    /// The board the package is for (its identity), with its identity.
    pub board: Board,
    pub identity: String,
    /// The chip key the package's app area is scrambled with.
    pub key: u16,
    /// The protected image's end in flash (`fwsc::Unpacked::end`): decrypted up to here as
    /// installed (`Soc::sfc_boot`).
    pub app_end: usize,
    /// The USR area (the firmware's patch bank), if this package has one, and how its board's
    /// stock firmware lays the bank out in it.
    pub usr: Option<usize>,
    pub usr_layout: fwsc::UsrLayout,
}

pub fn stock_from_package(pkg: &[u8], syx: Option<&[u8]>) -> Result<Stock, String> {
    let u = fwsc::unpack(pkg)?;
    let board = Board::of_identity(&u.identity);
    let mut flash = fwsc::full_flash(&u.raw, FLASH_SIZE);
    if board == Board::Smk37 {
        // a unit's factory calibration record, with the stock defaults
        fwsc::smk_trim_fill_defaults(&mut flash);
    }
    // the patch bank real units carry from the factory (the SMK-37 Pro's stock firmware loads
    // its current patch from an erased area as it is, and plays nothing); any package with a USR
    // entry gets it, community firmwares too (none tested reads that area)
    let usr = fwsc::usr_offset(&u);
    let usr_layout = fwsc::UsrLayout::of(board);
    if let Some(usr) = usr {
        fwsc::usr_fill_defaults(&mut flash, usr, usr_layout);
        if let Some(b) = syx {
            fwsc::usr_load_syx(&mut flash, usr, usr_layout, b, 0)?;
        }
    }
    let app = u.entry("app.bin").map(|h| h.offset).unwrap_or(0x120);
    Ok(Stock { xip: u.xip, flash, entry: XIP_BASE + app as u32, board, identity: u.identity, key: u.key, app_end: u.end, usr, usr_layout })
}

/// Ready a Soc for a package's firmware: its board, its flash chip, and the SPL's boot
/// parameters at `params`. Returns the value for r0.
pub fn stock_boot(soc: &mut Soc, flash: &[u8], params: u32, board: Board) -> u32 {
    soc.set_board(board);
    soc.flash.load(flash);
    soc.xip_flash = true;
    write_boot_info(soc, params, flash)
}

/// The boot-parameter block the SPL (uboot.boot) hands the stock app in r0, as the SDK's
/// `BOOT_DEVICE_INFO` (include_lib/system/boot.h) and the SPL's own code at 0x31B2..0x3200
/// build it: fs_info (-> the flash header copied right after the struct), sfc_base_addr (the
/// flash offset behind XIP 0x02000000: 0x4000), app_addr 0x02000000, chip_id, trim_value, an
/// 8-byte MAC, then the 32-byte flash header unmasked (CRC-16 keystream from 0xFFFF; its CRC
/// checks). The app copies parts of it to 0x01C7FD50 (boot_hwinfo_save) and its NOR file
/// system finds the flash from sfc_base_addr. Writes the block at `at` (RAM); returns `at`.
pub fn write_boot_info(soc: &mut Soc, at: u32, flash: &[u8]) -> u32 {
    let mut head = [0u8; 32];
    head.copy_from_slice(&flash[..32]);
    let mut key: u16 = 0xffff;
    for b in head.iter_mut() {
        *b ^= key as u8;
        key = (key << 1) ^ if key & 0x8000 != 0 { 0x1021 } else { 0 };
    }
    let words = [at + 24, 0x4000, XIP_BASE, 0, 0, 0];
    for (i, w) in words.iter().enumerate() {
        soc.write32(at + 4 * i as u32, *w);
    }
    for (i, b) in head.iter().enumerate() {
        soc.write8(at + 24 + i as u32, *b);
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sfr_table_matches_ranges() {
        for a in SFR_LO..SFR_HI {
            assert_eq!(SFR_MAP[((a - SFR_LO) >> 3) as usize], sfr_region(a), "{a:#x}");
        }
    }

    /// `region` as the chain of compares it was: the table and the early return below
    /// 0x0100_0000 must give the same answer for every peripheral address, on both boards.
    fn region_chain(soc: &Soc, addr: u32) -> Region {
        match addr {
            0x1eee000..=0x1eee0ff => Region::Irqc,
            0x1eef000..=0x1eef3ff => Region::Irqc2,
            0x12e00..=0x12e3f => Region::Alnk,
            0x50000..=0x501ff => Region::Gpio,
            0x51000..=0x5103f => Region::Iomap,
            0x40300..=0x4030f => Region::Sfcenc,
            0x10000..=0x1001f => Region::Clk,
            0x10400..=0x1091f => Region::Timer,
            0x11c00..=0x11c1f => Region::Spi0,
            0x11d00..=0x11d1f => Region::Spi1,
            0x11e00..=0x11e1f => Region::Spi2,
            0x11800..=0x1183f => Region::Usb,
            0x16800..=0x16a1f if soc.board == Board::Smk37 => Region::Usb,
            0x12100..=0x1212f => Region::Uart1,
            0x13100..=0x1311f => Region::Adc,
            0x13500..=0x1350f => Region::Crc,
            0x13b00..=0x13b07 => Region::Rand,
            0x11900..=0x119ff => Region::Ana,
            0x13e08..=0x13e0f => Region::P33,
            0x20000..=0x2ffff => Region::Bt,
            0x1e000..=0x1e01f | 0x1e800..=0x1eb3f => Region::HwEq,
            0x14300..=0x14323 => Region::HwSrc,
            0x1eee240..=0x1eee24f => Region::Dbg,
            0x1eee250..=0x1eee3ff => Region::Guard,
            _ => Region::Unknown,
        }
    }

    #[test]
    fn region_by_table_matches_the_chain() {
        let mut soc = Soc::new(1 << 16, vec![0; 0x1000]);
        for board in [Board::Fm1, Board::Smk37] {
            soc.board = board;
            for a in (0..0x60000).chain(0x0100_0000..0x0100_0100) {
                assert_eq!(soc.region(a), region_chain(&soc, a), "{a:#x} on {board:?}");
            }
        }
    }

    #[test]
    fn the_sfc_decrypts_below_the_plain_window() {
        // a package's app area (flash 0x4000..0x7000) and a flash file from a run before, with
        // blocks scrambled as an updater writes them (fwsc::unscramble's cipher, its own inverse)
        let key = 0x980f;
        let mut soc = Soc::new(64 * 1024, vec![0xff; 0x20000]);
        soc.xip_flash = true;
        soc.sfc_boot(key, 0x7000);
        let plain: Vec<u8> = (0..0x1000u32).map(|i| (i * 7 + 3) as u8).collect();
        let scrambled = |at: usize| -> Vec<u8> {
            let mut b = plain.clone();
            for blk in (0..b.len()).step_by(32) {
                fwsc::unmask(&mut b[blk..blk + 32], key ^ ((at + blk - XIP_FLASH_OFFSET) >> 2) as u16);
            }
            b
        };
        let mut image = soc.flash.mem.clone();
        image[0x5000..0x6000].copy_from_slice(&scrambled(0x5000));
        image[0x7000..0x8000].copy_from_slice(&scrambled(0x7000));
        image[0x9000..0x9004].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
        soc.restore_flash(&image);
        assert_eq!(&soc.xip[0x1000..0x2000], &plain[..], "the app area: decrypted, as the loader leaves the SFC");
        assert_ne!(&soc.xip[0x3000..0x4000], &plain[..], "past the app area: as stored");
        assert_eq!(soc.read32(XIP_BASE + 0x5000), 0x7856_3412);
        // the firmware's own window, from flash 0x8000 (as Felucca's fm1_flash.h sets it)
        let con = soc.read8(0x40300);
        soc.write8(0x40300, con & !2);
        soc.write32(0x4030c, XIP_BASE + 0x8000 - XIP_FLASH_OFFSET as u32);
        soc.write32(0x40308, 0x07ff_ffff);
        let con = soc.read8(0x40300);
        soc.write8(0x40300, con | 2);
        assert_eq!(soc.read8(0x40300), 3, "decrypt (as the loader left it) and the plain window");
        assert_eq!(&soc.xip[0x3000..0x4000], &plain[..], "below the firmware's window: decrypted now");
        assert_eq!(&soc.xip[0x1000..0x2000], &plain[..]);
        assert_eq!(soc.read32(XIP_BASE + 0x5000), 0x7856_3412, "in the window: as stored");
        // a write below the window reaches XIP decrypted; in it, as stored
        soc.flash.mem[0x6000..0x7000].copy_from_slice(&scrambled(0x6000));
        soc.flash.changed = Some((0x6000, 0x7000));
        soc.sync_xip();
        assert_eq!(&soc.xip[0x2000..0x3000], &plain[..], "written below the plain window: decrypted");
        soc.flash.mem[0xa000..0xa004].copy_from_slice(&[1, 2, 3, 4]);
        soc.flash.changed = Some((0xa000, 0xa004));
        soc.sync_xip();
        assert_eq!(soc.read32(XIP_BASE + 0x6000), 0x0403_0201, "written in the plain window: as stored");
    }

    #[test]
    fn flash_writes_show_through_xip() {
        // a booted package's window (0x80000 bytes, flash 0x4000..0x84000): erasing the sector
        // at flash 0x74000 and programming two bytes there reads back through XIP 0x02070000,
        // as April OS's samples do
        let mut soc = Soc::new(1 << 16, vec![0xaa; 0x80000]);
        for cmd in [&[0x06][..], &[0x20, 0x07, 0x40, 0x00], &[0x06], &[0x02, 0x07, 0x40, 0x00, 0x12, 0x34]] {
            soc.write32(0x500c0, 0); // PD0 low: chip select
            for &b in cmd {
                soc.write32(spiflash::SPI0_BASE + 8, b as u32);
            }
            soc.write32(0x500c0, 1);
        }
        assert_eq!(soc.read32(0x0207_0000), 0xffff_3412);
        assert_eq!(soc.read32(0x0207_0ffc), 0xffff_ffff, "the rest of the sector erased");
        assert_eq!(soc.read32(0x0207_1000), 0xaaaa_aaaa, "the next sector as installed");
        assert_eq!(soc.flash.revision, 2, "erase and program persist even after XIP consumes changed");
        assert!(!soc.flash.busy());
    }

    #[test]
    fn a_restored_flash_shows_through_xip() {
        // the installed window (xip) holds 0xaa; the saved chip has new bytes at 0x74000 (in the
        // window) and 0xf0000 (past it): both reach the chip, the first also the window
        let mut soc = Soc::new(1 << 16, vec![0xaa; 0x80000]);
        soc.xip_flash = true;
        let mut image = soc.flash.mem.clone();
        image[0x74000..0x74004].copy_from_slice(&[0x46, 0x53, 0x4d, 0x36]);
        image[0xf0000] = 0x12;
        let rev = soc.flash.revision;
        soc.restore_flash(&image);
        assert_eq!(soc.flash.mem, image);
        assert_eq!(soc.read32(XIP_BASE + (0x74000 - XIP_FLASH_OFFSET) as u32), 0x364d_5346);
        assert_eq!(soc.read32(XIP_BASE + (0x75000 - XIP_FLASH_OFFSET) as u32), 0xaaaa_aaaa, "an unchanged sector keeps the installed bytes");
        assert_eq!(soc.flash.revision, rev.wrapping_add(2));
    }

    #[test]
    fn a_power_cut_stops_half_way_through_an_op() {
        // cut at op 2 (the page program): erased, two of the four bytes programmed, then nothing
        let mut soc = Soc::new(1 << 16, vec![0xaa; 0x80000]);
        soc.flash.cut_at = Some(2);
        for cmd in [&[0x06][..], &[0x20, 0x07, 0x40, 0x00], &[0x06], &[0x02, 0x07, 0x40, 0x00, 0x12, 0x34, 0x56, 0x78],
                    &[0x06], &[0x20, 0x07, 0x50, 0x00]] {
            soc.write32(0x500c0, 0);
            for &b in cmd {
                soc.write32(spiflash::SPI0_BASE + 8, b as u32);
            }
            soc.write32(0x500c0, 1);
        }
        assert_eq!(soc.read32(0x0207_0000), 0xffff_3412, "the first half of the page program");
        assert_eq!(soc.read32(0x0207_1000), 0xaaaa_aaaa, "the erase after the cut never happened");
        assert_eq!(soc.flash.ops, 3);
        assert!(soc.flash.cut());
    }

    /// A chip with IRQ 16 enabled in cpu0's bank and SPI1 at the LCD's BAUD.
    fn spi1_soc(board: Board) -> Soc {
        let mut soc = Soc::new(1 << 16, vec![0; 0x1000]);
        soc.set_board(board);
        soc.periph.icfg[(periph::IRQ_SPI1 >> 3) as usize] |= 1 << ((periph::IRQ_SPI1 & 7) * 4);
        soc.write32(0x11d04, 3);
        soc
    }

    #[test]
    fn spi1_interrupt_comes_after_the_bytes_time_on_the_wire() {
        let mut soc = spi1_soc(Board::Fm1);
        soc.write32(0x11d00, 0xa021);
        soc.write32(0x11d0c, RAM_BASE);
        soc.write32(0x11d10, 0x800);
        soc.advance(1_000);
        assert_eq!(soc.periph.pending_irq(), None, "still on the wire");
        soc.advance(1_000_000);
        assert_eq!(soc.periph.pending_irq().map(|(n, _)| n), Some(periph::IRQ_SPI1));
    }

    #[test]
    fn spi1_con_clears_its_interrupt_enable() {
        let mut soc = spi1_soc(Board::Fm1);
        soc.write32(0x11d00, 0xe021);
        assert_ne!(soc.read32(0x11d00) & 0x2000, 0);
        soc.write32(0x11d00, 0xc021);
        assert_eq!(soc.read32(0x11d00) & 0x2000, 0);
    }

    #[test]
    fn spi1_interrupt_disabled_before_it_is_due_never_comes() {
        // the stock LCD driver (FM-1 V16) after a held key: a DMA started with the interrupt
        // enabled, then acknowledged with the interrupt disabled before its bytes are out
        let mut soc = spi1_soc(Board::Fm1);
        soc.write32(0x11d00, 0xe021);
        soc.write32(0x11d0c, RAM_BASE);
        soc.write32(0x11d10, 0x800);
        soc.write32(0x11d00, 0xc021);
        soc.advance(1_000_000);
        assert_eq!(soc.periph.pending_irq(), None);
    }

    #[test]
    fn fm1_lcd_timing_is_the_socs_own() {
        // `Soc::lcd_timed`, per machine (no process-wide switch): the FM-1's LCD DMA is busy for
        // its time on the wire only with it; without, done at once (polls finish)
        for timed in [false, true, false] {
            let mut soc = spi1_soc(Board::Fm1);
            soc.lcd_timed = timed;
            soc.write32(0x11d00, 0x0021);
            soc.write32(0x11d0c, RAM_BASE);
            soc.write32(0x11d10, 0x800);
            assert_eq!(soc.read32(0x11d00) & 0x8000 == 0, timed, "timed {timed}");
            assert_eq!(soc.pure_read(0x11d00), !timed);
            soc.advance(1_000_000);
            assert_ne!(soc.read32(0x11d00) & 0x8000, 0, "done once the bytes are out");
        }
    }

    #[test]
    fn hw_unit_jobs_take_their_cycles_on_the_strict_clock() {
        // FM1_STRICT's clock counts eighths of a cycle (`Periph::cycle_ticks`): a HW DSP job takes
        // its cycles there too, 8 x the ticks
        let done_after = |scale: u64| {
            let mut soc = Soc::new(1 << 16, vec![0; 0x1000]);
            soc.periph.cycle_ticks = scale;
            let d = RAM_BASE + 0x100;
            soc.write32(d, (64 << 16) | 2); // a 64-point real FFT
            soc.write32(d + 4, RAM_BASE + 0x1000);
            soc.write32(d + 8, RAM_BASE + 0x2000);
            soc.write32(0x1ee0404, d);
            soc.write32(0x1ee0400, (1 << 8) | (1 << 6) | 1);
            let t0 = soc.periph.cycles;
            while soc.read32(0x1ee0400) & 0x80 == 0 {
                soc.periph.cycles += 1;
            }
            soc.periph.cycles - t0
        };
        let (one, eight) = (done_after(1), done_after(8));
        assert!(one > 1);
        assert!((8 * one - 7..=8 * one).contains(&eight), "{one} cycles, {eight} ticks");
    }

    #[test]
    fn smk37_spi1_done_bit_is_busy_after_a_read_modify_write() {
        // a driver that acknowledges with CON |= bit 14 writes back the done bit it read
        let mut soc = spi1_soc(Board::Smk37);
        soc.write32(0x11d08, 0x2c);
        soc.advance(1_000);
        let con = soc.read32(0x11d00);
        soc.write32(0x11d00, con | 0x4000);
        soc.write32(0x11d0c, RAM_BASE);
        soc.write32(0x11d10, 0x800);
        assert_eq!(soc.read32(0x11d00) & 0x8000, 0, "the DMA is still on the wire");
    }
}

impl hwdsp::Mem for Soc {
    fn rd(&mut self, a: u32) -> u32 {
        self.read32(a)
    }
    fn wr(&mut self, a: u32, v: u32) {
        self.write32(a, v)
    }
}

impl usb::Dma for Soc {
    fn dma_read8(&mut self, addr: u32) -> u8 {
        self.read8(addr)
    }
    fn dma_write8(&mut self, addr: u32, v: u8) {
        self.write8(addr, v)
    }
}
