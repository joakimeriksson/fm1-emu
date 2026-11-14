//! SPI0 (0x11C00) driving the external SPI NOR flash.
//!
//! The firmware talks to the flash directly (with the SFC/XIP switched off)
//! for JEDEC probing, status reads, sector erase and page program — the
//! stock app in `norflash_*`, Felucca in `hal/fm1_flash.h`. Register use as
//! seen there: CON bit15 = transfer pending (set when a byte transfer
//! completes), bit14 = clear pending (write), bit12 = direction (1 = rx),
//! bit3 = enable; BUF holds the byte to send / the byte received. Chip
//! select is PD0, driven as a GPIO by the firmware.
//!
//! The flash is a 1 MiB part reporting JEDEC `85 60 14` (the Puya of most real
//! units), or `B3 60 14` (the UC25HQ80 of the newer ones: `FlashPart`). Commands modelled: 9F (JEDEC id), 4B (unique id), 05/35 (status
//! 1/2), 06/04 (WREN/WRDI), 03/0B (read / fast read), 20 (4 KiB erase),
//! D8 (64 KiB erase), 02 (page program). Erase/program are instantaneous;
//! status never reports busy.
//!
//! Power cuts (FM1_FLASH_CUT=N): the Nth erase or page program is cut half way (the first
//! half of its bytes changed, the rest as before), and nothing after it reaches the chip, as if
//! the power went there. With fm1-live's `--flash FILE` the next run boots from what was left.

pub const SPI0_BASE: u32 = 0x11c00;
pub const FLASH_SIZE: usize = 1 << 20;

/// The FM-1's flash part. Units sold until autumn 2026 carry a Puya P25Q80; the ones sold since
/// (mostly from AliExpress) a UC25HQ80 (fm1-base device/fm1_flash_ok.h). Both are 1 MiB with the
/// same plain SPI NOR commands, so only the JEDEC id the firmware reads tells them apart; a
/// firmware that knows only the Puya takes the newer units for unknown (its saves stay in RAM,
/// its update loader refuses to install). The SMK-37 Pro has its own part (`Soc::set_board`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlashPart {
    /// Puya P25Q80, JEDEC 85 60 14: the stock FM-1's
    #[default]
    P25q80,
    /// UC25HQ80, JEDEC B3 60 14: the newer units'
    Uc25hq80,
}

impl FlashPart {
    pub const NAMES: &'static str = "p25q80 (the stock part), uc25hq80 (units sold since autumn 2026)";

    pub fn jedec(self) -> [u8; 3] {
        match self {
            FlashPart::P25q80 => [0x85, 0x60, 0x14],
            FlashPart::Uc25hq80 => [0xb3, 0x60, 0x14],
        }
    }

    /// By its name (`p25q80`, `puya`; `uc25hq80`, `uc25`, `new`), in any case.
    pub fn parse(name: &str) -> Option<FlashPart> {
        match name.to_ascii_lowercase().as_str() {
            "p25q80" | "puya" | "stock" | "old" => Some(FlashPart::P25q80),
            "uc25hq80" | "uc25" | "new" => Some(FlashPart::Uc25hq80),
            _ => None,
        }
    }
}

pub struct SpiFlash {
    pub mem: Vec<u8>,
    pub con: u32,
    pub buf: u32,
    cs_low: bool,
    cmd: u8,
    nbytes: usize,
    addr: u32,
    wel: bool,
    /// bytes in the current command (after the opcode), for address assembly
    pending_prog: Vec<u8>,
    /// FM1_FLASH_TRACE: log each command on CS release
    trace: bool,
    /// JEDEC / unique-id reads
    pub jedec: [u8; 3],
    /// The flash range the last erase or page program changed (`Soc` mirrors it into the XIP
    /// window), taken by the reader.
    pub changed: Option<(usize, usize)>,
    /// Persistence generation, independent of the range consumed by XIP synchronization.
    pub revision: u32,
    /// Erases and page programs so far (the power-cut points)
    pub ops: u32,
    /// FM1_FLASH_CUT: the op that is cut half way; none reach the chip after it
    pub cut_at: Option<u32>,
}

impl SpiFlash {
    pub fn new() -> Self {
        Self {
            mem: vec![0xff; FLASH_SIZE],
            con: 0,
            buf: 0,
            cs_low: false,
            cmd: 0,
            nbytes: 0,
            addr: 0,
            wel: false,
            pending_prog: Vec::new(),
            jedec: FlashPart::P25q80.jedec(),
            trace: std::env::var_os("FM1_FLASH_TRACE").is_some(),
            changed: None,
            revision: 0,
            ops: 0,
            cut_at: std::env::var("FM1_FLASH_CUT").ok().and_then(|v| v.parse().ok()),
        }
    }

    /// Load a flash image at offset 0 (the rest stays erased).
    pub fn load(&mut self, image: &[u8]) {
        let n = image.len().min(self.mem.len());
        self.mem[..n].copy_from_slice(&image[..n]);
    }

    pub fn busy(&self) -> bool { self.cs_low }

    pub fn set_cs(&mut self, low: bool) {
        if !low && self.cs_low && self.trace && self.nbytes > 0 {
            eprintln!("flash cmd {:02x} addr {:06x} bytes {}", self.cmd, self.addr, self.nbytes);
        }
        if low && !self.cs_low {
            self.cmd = 0;
            self.nbytes = 0;
            self.pending_prog.clear();
        }
        if !low && self.cs_low && self.cmd == 0x02 {
            // page program commits on CS release
            let n = match self.op() { Some(true) => self.pending_prog.len(), Some(false) => self.pending_prog.len() / 2, None => 0 };
            for (i, b) in self.pending_prog.iter().take(n).enumerate() {
                let a = (self.addr as usize + i) & (FLASH_SIZE - 1);
                self.mem[a] &= *b;
            }
            let a = self.addr as usize & (FLASH_SIZE - 1);
            if n > 0 {
                self.changed = Some((a, (a + n).min(FLASH_SIZE)));
                self.revision = self.revision.wrapping_add(1);
            }
            self.pending_prog.clear();
            self.wel = false;
        }
        self.cs_low = low;
    }

    /// One byte exchanged on MOSI; returns the byte on MISO.
    fn transfer(&mut self, out: u8) -> u8 {
        if !self.cs_low {
            return 0xff;
        }
        let i = self.nbytes;
        self.nbytes += 1;
        if i == 0 {
            self.cmd = out;
            self.addr = 0;
            match out {
                0x06 => self.wel = true,
                0x04 => self.wel = false,
                _ => {}
            }
            return 0xff;
        }
        let k = i - 1; // byte index after the opcode
        match self.cmd {
            0x9f => self.jedec.get(k).copied().unwrap_or(0xff),
            0x4b => {
                // 4 dummy bytes then a 16-byte unique id
                if k < 4 { 0xff } else { [0x46, 0x4d, 0x31, 0x2d][(k - 4) & 3] }
            }
            0x05 => if self.wel { 0x02 } else { 0x00 },
            0x35 => 0x00,
            0x03 | 0x0b | 0x20 | 0xd8 | 0x02 => {
                if k < 3 {
                    self.addr = (self.addr << 8) | out as u32;
                    if k == 2 {
                        match self.cmd {
                            0x20 => self.erase(0x1000),
                            0xd8 => self.erase(0x10000),
                            _ => {}
                        }
                    }
                    0xff
                } else if self.cmd == 0x0b && k == 3 {
                    0xff // fast-read dummy byte
                } else if self.cmd == 0x02 {
                    self.pending_prog.push(out);
                    0xff
                } else if self.cmd == 0x03 || self.cmd == 0x0b {
                    let off = if self.cmd == 0x0b { k - 4 } else { k - 3 };
                    self.mem[(self.addr as usize + off) & (FLASH_SIZE - 1)]
                } else {
                    0xff
                }
            }
            _ => 0xff,
        }
    }

    /// One more erase / program: Some(true) done whole, Some(false) cut half way, None after the cut.
    fn op(&mut self) -> Option<bool> {
        self.ops += 1;
        match self.cut_at {
            Some(c) if self.ops > c => None,
            Some(c) if self.ops == c => Some(false),
            _ => Some(true),
        }
    }

    /// The chip reset (`Soc::chip_reset`): SPI0 back to off, and the command in progress, if
    /// chip select was low, abandoned (a page program's bytes are not programmed). The flash
    /// itself keeps its power, its contents and its write enable.
    pub fn chip_reset(&mut self) {
        (self.con, self.buf, self.cs_low, self.cmd, self.nbytes) = (0, 0, false, 0, 0);
        self.pending_prog.clear();
    }

    /// Has the power been cut (FM1_FLASH_CUT reached)?
    pub fn cut(&self) -> bool { self.cut_at.is_some_and(|c| self.ops >= c) }

    fn erase(&mut self, size: usize) {
        if !self.wel {
            return;
        }
        let n = match self.op() { Some(true) => size, Some(false) => size / 2, None => 0 };
        let base = (self.addr as usize) & !(size - 1) & (FLASH_SIZE - 1);
        self.wel = false;
        if n == 0 {
            return;
        }
        for b in &mut self.mem[base..(base + n).min(FLASH_SIZE)] {
            *b = 0xff;
        }
        self.changed = Some((base, (base + n).min(FLASH_SIZE)));
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn read(&mut self, addr: u32) -> u32 {
        match addr - SPI0_BASE {
            0x0 => self.con,
            0x8 => self.buf,
            _ => 0,
        }
    }

    pub fn write(&mut self, addr: u32, value: u32) {
        match addr - SPI0_BASE {
            0x0 => {
                // bit14 clears the pending flag; bit15 itself is read-only
                let mut v = (value & !0x8000) | (self.con & 0x8000);
                if value & 0x4000 != 0 {
                    v &= !0xc000;
                }
                self.con = v;
            }
            0x8 => {
                // a BUF write starts one byte transfer (tx or, in rx mode, a
                // dummy out byte); the received byte lands in BUF with PND set
                let rx = self.transfer(value as u8);
                self.buf = if self.con & 0x1000 != 0 { rx as u32 } else { value & 0xff };
                self.con |= 0x8000;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod part_tests {
    use super::FlashPart;
    use crate::{Board, Soc};

    /// The FM-1 reads the part asked for at its JEDEC probe; the SMK-37 Pro keeps its own.
    #[test]
    fn the_flash_part_sets_the_fm1s_jedec_id() {
        assert_eq!(FlashPart::parse("UC25HQ80"), Some(FlashPart::Uc25hq80));
        assert_eq!(FlashPart::parse("new"), Some(FlashPart::Uc25hq80));
        assert_eq!(FlashPart::parse("puya"), Some(FlashPart::P25q80));
        assert_eq!(FlashPart::parse("w25q80"), None);
        let mut soc = Soc::new(64 * 1024, vec![0; 64]);
        assert_eq!(soc.flash.jedec, [0x85, 0x60, 0x14], "the stock Puya unless asked");
        soc.set_flash_part(FlashPart::Uc25hq80);
        assert_eq!(soc.flash.jedec, [0xb3, 0x60, 0x14]);
        soc.set_board(Board::Fm1);
        assert_eq!(soc.flash.jedec, [0xb3, 0x60, 0x14], "a boot keeps the part");
        soc.set_board(Board::Smk37);
        assert_eq!(soc.flash.jedec, [0xeb, 0x60, 0x14], "the SMK-37 Pro's own part");
    }
}
