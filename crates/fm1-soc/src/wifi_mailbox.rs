// SPDX-License-Identifier: MPL-2.0
// Derived from fm1-base's plat_host.c (MPL-2.0): the register layout and the state machine
// (its states, commands and timings) are that mock's; the Rust is new.

//! The vendor WiFi stack's view (--wifi jieli): a register-mapped mailbox standing in for
//! JieLi's libraries at fm1-base's plat_wifi_* seam.
//!
//! A firmware built with fm1-base's `wifi_stack = "emu"` (device/wifi_emu.c, ble/wifi/emu_net.c
//! instead of JieLi's driver and supplicant) reads and writes registers at 0x30000..=0x3011f.
//! The emulator drives a state machine matching plat_host.c's WiFi mock (two virtual APs,
//! "Home" and "Cafe"), and carries the firmware's lwIP frames to the SoC's virtual subnet as
//! a `crate::net::Link`. Addresses outside the mailbox go to unknown_log. The firmware as
//! shipped, with JieLi's binary driver linked in, does not talk to this.
//!
//! Mailbox layout (offsets within WiFi region, i.e. addr - 0x30000):
//!   0x000  CMD           firmware writes a command here
//!   0x004  STATE         current WiFi state (read by plat_wifi_state)
//!   0x008  IP            connected: the station's address, as the subnet's DHCP gives it (10.0.2.15), first byte lowest
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
//!   0x0D0  WHY0..3       why the last JOIN was refused ("wrong password"; 16 bytes), cleared by the next command
//!   0x0E0  SCANS         total scan count
//!
//! Network frame interface (Ethernet frames in/out, for the virtual subnet and NAT):
//!   0x100  NET_TX_DATA   write 4 bytes of the outgoing frame (auto-advance)
//!   0x104  NET_TX_LEN    set the outgoing frame's length before writing
//!   0x108  NET_TX_SEND   the frame goes to the subnet (`Link::drain_tx`)
//!   0x10C  NET_RX_READY  1 if a response frame is waiting
//!   0x110  NET_RX_LEN    length of the current RX frame
//!   0x114  NET_RX_DATA   read 4 bytes at the current position
//!   0x118  NET_RX_ACK    consume 4 bytes (advance); when the frame is done, pop the next
//!   0x11C  NET_POLL      (kept for the firmware: the SoC polls the NAT on its own tick)

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
    ssid: String,
    pass: String,
    rssi: i32,
    secured: bool,
    channel: u8,
}

/// The networks around: the configured WPA2 one (`crate::net::Ap`), and an open "Cafe".
fn virtual_aps(ap: &crate::net::Ap) -> Vec<VirtualAp> {
    vec![
        VirtualAp { ssid: ap.ssid.clone(), pass: ap.password.clone(), rssi: -48, secured: true, channel: 6 },
        VirtualAp { ssid: "Cafe".into(), pass: String::new(), rssi: -71, secured: false, channel: 11 },
    ]
}

/// The station's address as the virtual subnet's DHCP gives it (net.rs), first byte lowest.
fn sta_ip() -> u32 {
    u32::from_le_bytes(crate::net::STA_IP)
}

// Network frame buffer: firmware writes frames word-by-word, then triggers send.
// Responses are queued and read back word-by-word.
const NET_BUF_MAX: usize = 1600; // max Ethernet frame

pub struct WifiMailbox {
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
    /// WHY0..3: why the last JOIN was refused, zero-padded.
    why: [u8; 16],
    aps: Vec<VirtualAp>,
    // Network frame buffers: what the firmware sent, for the subnet; what waits for it
    tx_done: Vec<Vec<u8>>,
    tx_buf: Vec<u8>,
    tx_len: usize,
    tx_pos: usize,
    rx_queue: std::collections::VecDeque<Vec<u8>>,
    rx_buf: Vec<u8>,
    rx_pos: usize,
    rx_len: usize,
}

impl Default for WifiMailbox {
    fn default() -> Self {
        Self::with_ap(&crate::net::Ap::default())
    }
}

impl WifiMailbox {
    /// The mailbox with `ap` as the network around, kept (as plat_host.c keeps "Home"): the
    /// firmware's auto-join finds it.
    pub fn with_ap(ap: &crate::net::Ap) -> Self {
        let mut saved = [[0u8; SSID_MAX]; MAX_SAVED];
        for (i, b) in ap.ssid.bytes().take(SSID_MAX - 1).enumerate() {
            saved[0][i] = b;
        }
        Self {
            aps: virtual_aps(ap),
            state: WIFI_OFF,
            state_t: 0,
            ssid: [0; SSID_MAX],
            saved,
            saved_n: 1,
            tx_done: Vec::new(),
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
            why: [0; 16],
        }
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Another network around (and kept) in place of the configured one.
    pub fn set_ap(&mut self, ap: &crate::net::Ap) {
        self.aps = virtual_aps(ap);
        self.saved[0] = [0; SSID_MAX];
        for (i, b) in ap.ssid.bytes().take(SSID_MAX - 1).enumerate() {
            self.saved[0][i] = b;
        }
    }

    fn find_ap(&self, ssid: &[u8; SSID_MAX]) -> Option<&VirtualAp> {
        self.aps.iter().find(|ap| {
            let ap_bytes = ap.ssid.as_bytes();
            let ssid_len = ssid.iter().position(|&b| b == 0).unwrap_or(SSID_MAX);
            ssid_len == ap_bytes.len() && &ssid[..ssid_len] == ap_bytes
        })
    }

    fn pass_matches(join_pass: &[u8; SSID_MAX], expected: &str) -> bool {
        let pass_len = join_pass.iter().position(|&b| b == 0).unwrap_or(SSID_MAX);
        pass_len == expected.len() && &join_pass[..pass_len] == expected.as_bytes()
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

    fn set_why(&mut self, why: &str) {
        self.why = [0; 16];
        for (d, s) in self.why.iter_mut().zip(why.bytes()) {
            *d = s;
        }
    }

    fn handle_cmd(&mut self, cmd: u32, ms: u32) {
        self.why = [0; 16];
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
                    if let Some(ap) = self.find_ap(&self.join_ssid) {
                        // a kept network joins with no password given, as plat_host.c's "Home" does
                        let kept = self.join_pass[0] == 0 && self.saved[..self.saved_n].contains(&self.join_ssid);
                        if ap.secured && !kept && !Self::pass_matches(&self.join_pass, &ap.pass) {
                            // wrong password: the state stays, WHY says why
                            self.set_why("wrong password");
                            return;
                        }
                    }
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
                    Some(sta_ip())
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
            0x010 => Some(self.aps.len() as u32),
            0x018 => {
                let i = self.scan_idx as usize;
                if i < self.aps.len() {
                    Some(self.aps[i].rssi as u32)
                } else {
                    Some(0)
                }
            }
            0x01C => {
                let i = self.scan_idx as usize;
                if i < self.aps.len() {
                    Some(self.aps[i].secured as u32)
                } else {
                    Some(0)
                }
            }
            0x020 => {
                let i = self.scan_idx as usize;
                if i < self.aps.len() {
                    Some(self.aps[i].channel as u32)
                } else {
                    Some(0)
                }
            }
            o @ 0x024..=0x040 => {
                let i = self.scan_idx as usize;
                if i < self.aps.len() {
                    let word = (o - 0x024) / 4;
                    Some(Self::read_ssid_word(self.aps[i].ssid.as_bytes(), word))
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
            o @ 0x0D0..=0x0DC => Some(Self::read_ssid_word(&self.why, (o - 0x0D0) / 4)),
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
        let frame = self.tx_buf[..self.tx_len.min(self.tx_pos)].to_vec();
        self.tx_done.push(frame);
        self.tx_len = 0;
        self.tx_pos = 0;
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
        if self.rx_buf.is_empty() {
            self.net_rx_advance();
        }
    }
}

/// Frames waiting for a firmware that is not reading them: the NAT's host sockets are read
/// no further than this (the subnet keeps the rest on the socket).
const RX_CAP: usize = 64;

impl crate::net::Link for WifiMailbox {
    fn drain_tx(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.tx_done)
    }

    fn push_rx(&mut self, eth: Vec<u8>) {
        if self.state != WIFI_CONNECTED || self.rx_queue.len() >= RX_CAP {
            return;
        }
        self.rx_queue.push_back(eth);
        if self.rx_buf.is_empty() {
            self.net_rx_advance();
        }
    }
}
