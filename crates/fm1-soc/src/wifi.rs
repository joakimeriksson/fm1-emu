// SPDX-License-Identifier: MIT
// WiFi state machine matches JieLi AC7916A behavior; fm1-base host/plat_host.c was used as reference.

//! WiFi controller: a register-mapped mailbox that emulates fm1-base's plat_wifi_* mock.
//!
//! The recompiled firmware's mock plat_wifi_* functions read/write registers at 0x30000..=0x3ffff
//! (a new Region::Wifi). The emulator intercepts these and drives a state machine matching
//! plat_host.c's WiFi mock.
//!
//! Mailbox layout (offsets within WiFi region, i.e. addr - 0x30000):
//!   0x000  CMD           firmware writes a command here
//!   0x004  STATE         current WiFi state (read by plat_wifi_state)
//!   0x008  IP            IP address (read by plat_wifi_ip, network byte order)
//!   0x00C  RSSI          signal strength (read by plat_wifi_rssi)
//!   0x010  SCAN_N        number of scan results
//!   0x014  SCAN_IDX      scan result index (firmware writes)
//!   0x018  SCAN_RSSI     RSSI of scan result [IDX]
//!   0x01C  SCAN_SEC      security flag of scan result [IDX]
//!   0x020  SCAN_CHAN     channel of scan result [IDX]
//!   0x024  SCAN_SSID0    bytes 0-3 of scan result SSID
//!   0x028  SCAN_SSID1    bytes 4-7
//!   0x02C  SCAN_SSID2    bytes 8-11
//!   0x030  SCAN_SSID3    bytes 12-15
//!   0x034  SCAN_SSID4    bytes 16-19
//!   0x038  SCAN_SSID5    bytes 20-23
//!   0x03C  SCAN_SSID6    bytes 24-27
//!   0x040  SCAN_SSID7    bytes 28-31
//!   0x044  JOIN_SSID0..7 SSID to join (firmware writes 8 words)
//!   0x064  JOIN_PASS0..7 passphrase (firmware writes 8 words)
//!   0x084  TICK_MS       firmware writes current ms
//!   0x088  SSID0..7      current connected SSID (8 words, read)
//!   0x0A8  SAVED_IDX     saved network index (firmware writes)
//!   0x0AC  SAVED_SSID0..7 saved network SSID (read after setting IDX)
//!   0x0CC  SAVED_N       number of saved networks
//!   0x0D0  WHY0..3       disconnect reason string (16 bytes)
//!   0x0E0  SCANS         total scan count
//!   0x0E4  NET_CMD       network command (for plat_net_task)
//!   0x0E8  NET_STATUS    network status
//!   0x0EC  NET_DATA      network data (4 bytes at a time)
//!   0x0F0  NET_LEN       network data length
//!   0x0F4  RX_FRAMES     rx frame count
//!   0x0F8  RX_RSSI       rx rssi

// WiFi states (matching plat_host.c)
const WIFI_OFF: u32 = 0;
const WIFI_STARTING: u32 = 1;
const WIFI_ON: u32 = 2;
#[allow(dead_code)]
const WIFI_SCANNING: u32 = 3;
const WIFI_JOINING: u32 = 4;
const WIFI_ADDRESS: u32 = 5;
const WIFI_CONNECTED: u32 = 6;

// Commands
#[allow(dead_code)]
const CMD_NONE: u32 = 0;
const CMD_ON: u32 = 1;
const CMD_OFF: u32 = 2;
const CMD_SCAN: u32 = 3;
const CMD_JOIN: u32 = 4;
const CMD_FORGET: u32 = 5;

// Timing (milliseconds)
const START_MS: u32 = 500;
const JOIN_MS: u32 = 1000;
const ADDR_MS: u32 = 500;

const MAX_SAVED: usize = 6;
const SSID_MAX: usize = 33;

struct VirtualAp {
    ssid: &'static str,
    rssi: i32,
    secured: bool,
    channel: u8,
}

const VIRTUAL_APS: &[VirtualAp] = &[
    VirtualAp {
        ssid: "Home",
        rssi: -48,
        secured: true,
        channel: 6,
    },
    VirtualAp {
        ssid: "Cafe",
        rssi: -71,
        secured: false,
        channel: 11,
    },
];

// 192.168.1.42 in network byte order
const FAKE_IP: u32 = 0x2A01_A8C0;

// Network frame buffer: firmware writes frames word-by-word, then triggers send.
// Responses are queued and read back word-by-word.
const NET_BUF_MAX: usize = 1600; // max Ethernet frame

pub struct WifiController {
    state: u32,
    state_t: u32,
    ssid: [u8; SSID_MAX],
    saved: [[u8; SSID_MAX]; MAX_SAVED],
    saved_n: usize,
    scan_idx: u32,
    scan_count: u32,
    join_ssid: [u8; SSID_MAX],
    join_pass: [u8; SSID_MAX],
    last_tick_ms: u32,
    saved_idx: u32,
    // Network frame buffers
    pub vnet: crate::net::VirtualNet,
    tx_buf: Vec<u8>,
    tx_len: usize,
    tx_pos: usize,
    rx_queue: std::collections::VecDeque<Vec<u8>>,
    rx_buf: Vec<u8>,
    rx_pos: usize,
    rx_len: usize,
}

impl Default for WifiController {
    fn default() -> Self {
        let mut saved = [[0u8; SSID_MAX]; MAX_SAVED];
        // "Home" is saved by default (matching plat_host.c)
        for (i, b) in b"Home".iter().enumerate() {
            saved[0][i] = *b;
        }
        Self {
            state: WIFI_OFF,
            state_t: 0,
            ssid: [0; SSID_MAX],
            saved,
            saved_n: 1,
            vnet: crate::net::VirtualNet::new(),
            tx_buf: vec![0u8; NET_BUF_MAX],
            tx_len: 0,
            tx_pos: 0,
            rx_queue: std::collections::VecDeque::new(),
            rx_buf: Vec::new(),
            rx_pos: 0,
            rx_len: 0,
            scan_idx: 0,
            scan_count: 0,
            join_ssid: [0; SSID_MAX],
            join_pass: [0; SSID_MAX],
            last_tick_ms: 0,
            saved_idx: 0,
        }
    }
}

impl WifiController {
    pub fn new() -> Self {
        Self::default()
    }

    fn tick(&mut self, ms: u32) {
        let elapsed = ms.wrapping_sub(self.state_t);
        match self.state {
            WIFI_STARTING => {
                if elapsed >= START_MS {
                    // auto-join saved network if available
                    if self.saved_n > 0 && self.saved[0][0] != 0 {
                        self.ssid = self.saved[0];
                        self.state = WIFI_JOINING;
                        self.state_t = ms;
                    } else {
                        self.state = WIFI_ON;
                    }
                }
            }
            WIFI_JOINING => {
                if elapsed >= JOIN_MS {
                    self.state = WIFI_ADDRESS;
                    self.state_t = ms;
                }
            }
            WIFI_ADDRESS => {
                if elapsed >= ADDR_MS {
                    self.state = WIFI_CONNECTED;
                }
            }
            _ => {}
        }
    }

    fn handle_cmd(&mut self, cmd: u32, ms: u32) {
        match cmd {
            CMD_ON => {
                if self.state == WIFI_OFF {
                    self.state = WIFI_STARTING;
                    self.state_t = ms;
                }
            }
            CMD_OFF => {
                self.state = WIFI_OFF;
                self.ssid = [0; SSID_MAX];
            }
            CMD_SCAN => {
                if self.state >= WIFI_ON {
                    self.scan_count += 1;
                }
            }
            CMD_JOIN => {
                if self.state >= WIFI_ON {
                    self.ssid = self.join_ssid;
                    self.state = WIFI_JOINING;
                    self.state_t = ms;
                    // save the network
                    let mut found = false;
                    for i in 0..self.saved_n {
                        if self.saved[i] == self.join_ssid {
                            found = true;
                            break;
                        }
                    }
                    if !found && self.saved_n < MAX_SAVED {
                        self.saved[self.saved_n] = self.join_ssid;
                        self.saved_n += 1;
                    }
                }
            }
            CMD_FORGET => {
                let idx = self.saved_idx as usize;
                if idx < self.saved_n {
                    for i in idx..self.saved_n - 1 {
                        self.saved[i] = self.saved[i + 1];
                    }
                    self.saved_n -= 1;
                    self.saved[self.saved_n] = [0; SSID_MAX];
                }
            }
            _ => {}
        }
    }

    fn read_ssid_word(ssid: &[u8], word: u32) -> u32 {
        let off = (word * 4) as usize;
        let mut v = 0u32;
        for j in 0..4 {
            if off + j < ssid.len() {
                v |= (ssid[off + j] as u32) << (j * 8);
            }
        }
        v
    }

    fn write_ssid_word(ssid: &mut [u8], word: u32, value: u32) {
        let off = (word * 4) as usize;
        for j in 0..4 {
            if off + j < ssid.len() {
                ssid[off + j] = ((value >> (j * 8)) & 0xff) as u8;
            }
        }
    }

    /// Read a WiFi mailbox register. `offset` is addr - 0x30000.
    pub fn read(&self, offset: u32) -> Option<u32> {
        match offset {
            0x004 => Some(self.state),
            0x008 => {
                if self.state == WIFI_CONNECTED {
                    Some(FAKE_IP)
                } else {
                    Some(0)
                }
            }
            0x00C => {
                if self.state == WIFI_CONNECTED {
                    Some(-48i32 as u32)
                } else {
                    Some(0)
                }
            }
            0x010 => Some(VIRTUAL_APS.len() as u32),
            0x018 => {
                let i = self.scan_idx as usize;
                if i < VIRTUAL_APS.len() {
                    Some(VIRTUAL_APS[i].rssi as u32)
                } else {
                    Some(0)
                }
            }
            0x01C => {
                let i = self.scan_idx as usize;
                if i < VIRTUAL_APS.len() {
                    Some(VIRTUAL_APS[i].secured as u32)
                } else {
                    Some(0)
                }
            }
            0x020 => {
                let i = self.scan_idx as usize;
                if i < VIRTUAL_APS.len() {
                    Some(VIRTUAL_APS[i].channel as u32)
                } else {
                    Some(0)
                }
            }
            o @ 0x024..=0x040 => {
                let i = self.scan_idx as usize;
                if i < VIRTUAL_APS.len() {
                    let word = (o - 0x024) / 4;
                    Some(Self::read_ssid_word(VIRTUAL_APS[i].ssid.as_bytes(), word))
                } else {
                    Some(0)
                }
            }
            o @ 0x088..=0x0A4 => {
                let word = (o - 0x088) / 4;
                Some(Self::read_ssid_word(&self.ssid, word))
            }
            o @ 0x0AC..=0x0C8 => {
                let idx = self.saved_idx as usize;
                if idx < self.saved_n {
                    let word = (o - 0x0AC) / 4;
                    Some(Self::read_ssid_word(&self.saved[idx], word))
                } else {
                    Some(0)
                }
            }
            0x0CC => Some(self.saved_n as u32),
            0x0E0 => Some(self.scan_count),
            // Network frame RX registers (0x100..0x11F)
            // NET_RX_READY: 1 if a response frame is waiting
            0x10C => Some(if !self.rx_buf.is_empty() || !self.rx_queue.is_empty() { 1 } else { 0 }),
            // NET_RX_LEN: length of current RX frame
            0x110 => Some(self.rx_len as u32),
            // NET_RX_DATA: read 4 bytes at current position, auto-advance
            0x114 => Some(self.net_rx_read()),
            _ => None,
        }
    }

    /// Write a WiFi mailbox register. Returns true if handled.
    pub fn write(&mut self, offset: u32, value: u32) -> bool {
        match offset {
            0x000 => {
                self.handle_cmd(value, self.last_tick_ms);
                true
            }
            0x014 => {
                self.scan_idx = value;
                true
            }
            o @ 0x044..=0x060 => {
                let word = (o - 0x044) / 4;
                Self::write_ssid_word(&mut self.join_ssid, word, value);
                true
            }
            o @ 0x064..=0x080 => {
                let word = (o - 0x064) / 4;
                Self::write_ssid_word(&mut self.join_pass, word, value);
                true
            }
            0x084 => {
                self.last_tick_ms = value;
                self.tick(value);
                true
            }
            0x0A8 => {
                self.saved_idx = value;
                true
            }
            // Network frame TX registers (0x100..0x10B)
            // NET_TX_DATA: write 4 bytes at current position, auto-advance
            0x100 => {
                self.net_tx_write(value);
                true
            }
            // NET_TX_LEN: set frame length
            0x104 => {
                self.tx_len = (value as usize).min(NET_BUF_MAX);
                self.tx_pos = 0;
                true
            }
            // NET_TX_SEND: dispatch the frame through VirtualNet
            0x108 => {
                self.net_tx_send();
                true
            }
            // NET_RX_ACK: consume current RX frame, advance to next
            0x118 => {
                self.net_rx_ack();
                true
            }
            // NET_POLL: poll NAT connections for incoming data
            0x11C => {
                self.net_poll();
                true
            }
            _ => false,
        }
    }

    fn net_tx_write(&mut self, value: u32) {
        let bytes = value.to_le_bytes();
        for &b in &bytes {
            if self.tx_pos < self.tx_len && self.tx_pos < NET_BUF_MAX {
                self.tx_buf[self.tx_pos] = b;
                self.tx_pos += 1;
            }
        }
    }

    fn net_tx_send(&mut self) {
        if self.tx_len == 0 || self.state != WIFI_CONNECTED {
            return;
        }
        let frame = &self.tx_buf[..self.tx_len.min(self.tx_pos)];
        let responses = self.vnet.handle(frame);
        for r in responses {
            self.rx_queue.push_back(r);
        }
        self.tx_len = 0;
        self.tx_pos = 0;
        // prime the first RX frame if nothing is being read
        if self.rx_buf.is_empty() {
            self.net_rx_advance();
        }
    }

    fn net_rx_advance(&mut self) {
        if let Some(frame) = self.rx_queue.pop_front() {
            self.rx_len = frame.len();
            self.rx_buf = frame;
            self.rx_pos = 0;
        } else {
            self.rx_buf.clear();
            self.rx_len = 0;
            self.rx_pos = 0;
        }
    }

    fn net_rx_read(&self) -> u32 {
        if self.rx_pos >= self.rx_buf.len() {
            return 0;
        }
        let mut v = 0u32;
        for j in 0..4 {
            let i = self.rx_pos + j;
            if i < self.rx_buf.len() {
                v |= (self.rx_buf[i] as u32) << (j * 8);
            }
        }
        v
    }

    fn net_rx_ack(&mut self) {
        // advance read position by 4 bytes
        self.rx_pos += 4;
        if self.rx_pos >= self.rx_len {
            // done with this frame, advance to next
            self.net_rx_advance();
        }
    }

    fn net_poll(&mut self) {
        let responses = self.vnet.poll();
        for r in responses {
            self.rx_queue.push_back(r);
        }
        if self.rx_buf.is_empty() {
            self.net_rx_advance();
        }
    }
}
