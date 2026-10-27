// SPDX-License-Identifier: MIT

//! BT region (0x20000-0x2ffff): three sub-modules behind one dispatch.
//!
//! * [`BleMailbox`] (offsets 0x100-0x138): fm1-base's `plat_ble_*` mock, a command/tick-driven
//!   state machine matching `plat_host.c`.
//! * [`BleBaseband`] (offsets 0x8000-0x803F): Optimist's `ble_hw_wl82.c` register interface,
//!   column port, interrupt enables/pending.
//! * [`BtAnalog`] (offsets 0xFC00-0xFC8F): shared BT analog registers (bt_window, RF config,
//!   AGC tables, BLE enable). Both firmware families write here.
//!
//! [`BtRegion`] composes all three and dispatches reads/writes by offset.

// ---- BleMailbox (fm1-base) ------------------------------------------------

// BLE states (matching plat_host.c)
const BLE_OFF: u32 = 0;
const BLE_STARTING: u32 = 1;
const BLE_READY: u32 = 2;

const BDEV_OFF: u32 = 0;
const BDEV_SCANNING: u32 = 1;
const BDEV_CONNECTING: u32 = 2;
const BDEV_READY: u32 = 3;

#[allow(dead_code)]
const CMD_NONE: u32 = 0;
const CMD_START: u32 = 1;
const CMD_SCAN_ON: u32 = 2;
const CMD_SCAN_OFF: u32 = 3;
const CMD_CONNECT: u32 = 4;
const CMD_DISCONNECT: u32 = 5;
const CMD_ENABLE_ON: u32 = 6;
const CMD_ENABLE_OFF: u32 = 7;

const START_MS: u32 = 1000;
const SCAN_MS: u32 = 500;
const CONNECT_MS: u32 = 300;

struct VirtualDevice {
    name: &'static str,
    rssi: i32,
}

const VIRTUAL_DEVICES: &[VirtualDevice] = &[VirtualDevice {
    name: "BLE MIDI",
    rssi: -50,
}];

/// fm1-base's `plat_ble_*` mock: a register-mapped mailbox at offsets 0x100-0x138
/// within Region::Bt, driven by commands and a tick clock.
#[derive(Default)]
pub struct BleMailbox {
    state: u32,
    enabled: bool,
    conn: bool,
    link: bool,
    bdev_state: u32,
    bdev_t: u32,
    ble_t0: u32,
    connected_device: i32,
    scan_idx: u32,
    last_tick_ms: u32,
}

impl BleMailbox {
    fn new() -> Self {
        Self {
            connected_device: -1,
            ..Default::default()
        }
    }

    fn tick(&mut self, ms: u32) {
        if self.state == BLE_STARTING && ms.wrapping_sub(self.ble_t0) >= START_MS {
            self.state = BLE_READY;
        }
        match self.bdev_state {
            BDEV_SCANNING if ms.wrapping_sub(self.bdev_t) >= SCAN_MS => {
                self.bdev_state = BDEV_READY;
            }
            BDEV_CONNECTING if ms.wrapping_sub(self.bdev_t) >= CONNECT_MS => {
                self.bdev_state = BDEV_READY;
                self.conn = true;
                self.connected_device = 0;
            }
            _ => {}
        }
    }

    fn handle_cmd(&mut self, cmd: u32, ms: u32) {
        match cmd {
            CMD_START => {
                if self.state == BLE_OFF {
                    self.state = BLE_STARTING;
                    self.ble_t0 = ms;
                }
            }
            CMD_SCAN_ON => {
                if self.state == BLE_READY {
                    self.bdev_state = BDEV_SCANNING;
                    self.bdev_t = ms;
                }
            }
            CMD_SCAN_OFF => {
                if self.bdev_state == BDEV_SCANNING {
                    self.bdev_state = BDEV_OFF;
                }
            }
            CMD_CONNECT => {
                if self.state == BLE_READY && self.bdev_state == BDEV_READY {
                    self.bdev_state = BDEV_CONNECTING;
                    self.bdev_t = ms;
                }
            }
            CMD_DISCONNECT => {
                self.conn = false;
                self.link = false;
                self.connected_device = -1;
                self.bdev_state = BDEV_OFF;
            }
            CMD_ENABLE_ON => self.enabled = true,
            CMD_ENABLE_OFF => {
                self.enabled = false;
                self.state = BLE_OFF;
                self.conn = false;
                self.link = false;
                self.connected_device = -1;
                self.bdev_state = BDEV_OFF;
            }
            _ => {}
        }
    }

    fn read(&self, offset: u32) -> u32 {
        match offset {
            0x104 => self.state,
            0x108 => self.conn as u32,
            0x10C => self.link as u32,
            0x110 => {
                if self.bdev_state >= BDEV_READY {
                    VIRTUAL_DEVICES.len() as u32
                } else {
                    0
                }
            }
            0x114 => {
                let i = self.scan_idx as usize;
                if i < VIRTUAL_DEVICES.len() {
                    VIRTUAL_DEVICES[i].rssi as u32
                } else {
                    0
                }
            }
            0x11C => {
                let i = self.scan_idx as usize;
                if i < VIRTUAL_DEVICES.len() {
                    let name = VIRTUAL_DEVICES[i].name.as_bytes();
                    let mut v = 0u32;
                    for (j, &b) in name.iter().take(4).enumerate() {
                        v |= (b as u32) << (j * 8);
                    }
                    v
                } else {
                    0
                }
            }
            0x120 => {
                let i = self.scan_idx as usize;
                if i < VIRTUAL_DEVICES.len() {
                    let name = VIRTUAL_DEVICES[i].name.as_bytes();
                    let mut v = 0u32;
                    for (j, &b) in name.iter().skip(4).take(4).enumerate() {
                        v |= (b as u32) << (j * 8);
                    }
                    v
                } else {
                    0
                }
            }
            0x124 => self.connected_device as u32,
            0x12C => 0, // log_total
            0x130 => 0, // log_line
            0x138 => self.enabled as u32,
            _ => 0,
        }
    }

    fn write(&mut self, offset: u32, value: u32) {
        match offset {
            0x100 => self.handle_cmd(value, self.last_tick_ms),
            0x118 => self.scan_idx = value,
            0x128 => {
                self.last_tick_ms = value;
                self.tick(value);
            }
            0x134 => {} // log_idx
            _ => {}
        }
    }
}

// ---- BleBaseband (Optimist) -----------------------------------------------

/// Optimist's BLE baseband registers (0x28000-0x2803F, offset 0x8000+ within Region::Bt).
///
/// The column port (COLCMD/COLDATA/COLRES) controls the baseband engine's per-link
/// configuration RAM. Phase 0: accept writes, return last written values. Phase 1+
/// will add a state machine that fires IRQ 29 (RX) and IRQ 45 (EVENT).
#[derive(Default)]
pub struct BleBaseband {
    con: u32,
    timing: [u32; 6],
    col_cmd: u32,
    col_data: u32,
    col_res: u32,
    columns: [[u32; 16]; 2],
    ien: u32,
    ipnd: u32,
    t34: u32,
    stat: u32,
    pub rx_irq: bool,
    pub event_irq: bool,
}

impl BleBaseband {
    fn read(&self, off: u32) -> u32 {
        match off {
            0x00 => self.con,
            0x08..=0x18 => self.timing[((off - 0x08) / 4) as usize],
            0x1C => self.col_cmd,
            0x20 => self.col_data,
            0x24 => self.col_res,
            0x28 => self.ien,
            0x2C => 0, // IACK is write-only
            0x30 => self.ipnd,
            0x34 => self.t34,
            0x38 => self.stat,
            _ => 0,
        }
    }

    fn write(&mut self, off: u32, value: u32) {
        match off {
            0x00 => self.con = value,
            0x08..=0x18 => self.timing[((off - 0x08) / 4) as usize] = value,
            0x1C => {
                self.col_cmd = value;
                let col = ((value >> 10) & 0xF) as usize;
                let link = ((value >> 4) & 0x3) as usize;
                let op = value & 0xF;
                let link = link.min(1);
                let col = col.min(15);
                if op == 1 {
                    // write: store col_data into column
                    self.columns[link][col] = self.col_data;
                } else if op == 0 {
                    // read: load column into col_res
                    self.col_res = self.columns[link][col];
                }
            }
            0x20 => self.col_data = value,
            0x28 => self.ien = value,
            0x2C => {
                // IACK: clear acknowledged bits from pending
                self.ipnd &= !value;
                if self.ipnd == 0 {
                    self.rx_irq = false;
                    self.event_irq = false;
                }
            }
            0x30 => self.ipnd = value,
            0x34 => self.t34 = value,
            0x38 => self.stat = value,
            _ => {}
        }
    }
}

// ---- BtAnalog (shared) ----------------------------------------------------

const AGC_TABLE_SIZE: usize = 128;

/// BT analog registers (0x2FC00-0x2FC8F, offset 0xFC00+ within Region::Bt).
/// Shared between firmware families: fm1-base and Optimist both write bt_window (0xFC44).
/// Optimist also writes RF config, AGC tables, PLL, and BLE enable during `fm1_ble_rf_init()`.
pub struct BtAnalog {
    pub window: u32,
    ble_en: u32,
    bb_base: u32,
    bb_end: u32,
    pll_tbl: u32,
    agc: Vec<u32>,
    agc_idx: u32,
    config: [u32; 32],
}

impl BtAnalog {
    fn new(window: u32) -> Self {
        Self {
            window,
            ble_en: 0,
            bb_base: 0,
            bb_end: 0,
            pll_tbl: 0,
            agc: vec![0; AGC_TABLE_SIZE],
            agc_idx: 0,
            config: [0; 32],
        }
    }

    fn read(&self, off: u32) -> u32 {
        match off {
            0x44 => self.window,
            0x4C => self.bb_base,
            0x50 => self.bb_end,
            0x54 => self.pll_tbl,
            0x60 => self.agc[0],
            0x64 => self.agc_idx,
            0x68 => {
                let i = (self.agc_idx as usize).min(AGC_TABLE_SIZE - 1);
                self.agc[i]
            }
            0x80 => self.ble_en,
            // general config (0xFC00-0xFC7C)
            0x00..=0x7C if (off & 3) == 0 => self.config[(off / 4) as usize],
            _ => 0,
        }
    }

    fn write(&mut self, off: u32, value: u32) {
        match off {
            0x44 => self.window = value,
            0x4C => self.bb_base = value,
            0x50 => self.bb_end = value,
            0x54 => self.pll_tbl = value,
            0x60 => {
                if !self.agc.is_empty() {
                    self.agc[0] = value;
                }
            }
            0x64 => self.agc_idx = value,
            0x68 => {
                let i = (self.agc_idx as usize).min(AGC_TABLE_SIZE - 1);
                self.agc[i] = value;
            }
            0x80 => self.ble_en = value,
            // general config (0xFC00-0xFC7C)
            0x00..=0x7C if (off & 3) == 0 => self.config[(off / 4) as usize] = value,
            _ => {}
        }
    }
}

// ---- BtRegion (top-level dispatch) ----------------------------------------

/// The BT region (0x20000-0x2ffff): dispatches to sub-modules by offset.
pub struct BtRegion {
    pub analog: BtAnalog,
    pub mailbox: BleMailbox,
    pub baseband: BleBaseband,
}

impl BtRegion {
    pub fn new(window: u32) -> Self {
        Self {
            analog: BtAnalog::new(window),
            mailbox: BleMailbox::new(),
            baseband: BleBaseband::default(),
        }
    }

    pub fn read(&self, offset: u32) -> u32 {
        match offset {
            // BR/EDR baseband (Classic BT): accept, return 0 (Phase 4)
            0x0000..=0x00FF => 0,
            // fm1-base mailbox
            0x0100..=0x013F => self.mailbox.read(offset),
            // Optimist BLE baseband
            0x8000..=0x803F => self.baseband.read(offset - 0x8000),
            // BT analog (shared)
            0xFC00..=0xFC8F => self.analog.read(offset - 0xFC00),
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u32, value: u32) {
        match offset {
            0x0000..=0x00FF => {} // BR/EDR: accept silently
            0x0100..=0x013F => self.mailbox.write(offset, value),
            0x8000..=0x803F => self.baseband.write(offset - 0x8000, value),
            0xFC00..=0xFC8F => self.analog.write(offset - 0xFC00, value),
            _ => {}
        }
    }
}
