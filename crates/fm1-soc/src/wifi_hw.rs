// SPDX-License-Identifier: MIT
// Hardware-level WiFi emulation for the cleanroom (modd) driver.
// Activated by --oss-wifi. Emulates the WDMA engine, the BBP indirect port, the MAC
// registers and a virtual 802.11 AP bridged to the virtual subnet and its NAT.

use std::collections::VecDeque;

use crate::ram::Ram;
use crate::RAM_BASE;

const AP_MAC: [u8; 6] = [0x02, 0x46, 0x4d, 0x31, 0x00, 0x01];
const AP_CHANNEL: u8 = 6;
const BEACON_INTERVAL: u16 = 100;
/// The virtual APs beacon every 100 ms of emulated time (`tick`)...
const BEACON_PERIOD_MS: u64 = 100;
/// ...unless this many frames already wait for the station (it is not reading).
const RX_QUEUE_BEACON_CAP: usize = 8;

const CAFE_MAC: [u8; 6] = [0x02, 0x46, 0x4d, 0x31, 0x00, 0x02];
const CAFE_SSID: &str = "Cafe";

const FC_BEACON: u16 = 0x0080;
const FC_PROBE_RSP: u16 = 0x0050;
const FC_AUTH: u16 = 0x00B0;
const FC_ASSOC_RSP: u16 = 0x0010;
const FC_DATA_FROMDS: u16 = 0x0208;

// EAPOL-Key frame offsets and constants for the WPA2 4-way handshake responder
const EAPOL_TYPE_OFF: usize = 1;
const EAPOL_KEY_TYPE: usize = 4;
const EAPOL_KEY_INFO: usize = 5;
const EAPOL_KEY_LEN: usize = 7;
const EAPOL_REPLAY: usize = 9;
const EAPOL_NONCE: usize = 17;
const EAPOL_MIC: usize = 81;
const EAPOL_DATA_LEN: usize = 97;
const EAPOL_DATA: usize = 99;
const EAPOL_HDR_LEN: usize = 99;

const KI_VER_HMAC_SHA1: u16 = 0x0002;
const KI_PAIRWISE: u16 = 0x0008;
const KI_INSTALL: u16 = 0x0040;
const KI_ACK: u16 = 0x0080;
const KI_MIC: u16 = 0x0100;
const KI_SECURE: u16 = 0x0200;
const KI_ENCRYPT: u16 = 0x1000;

// DMA register offsets from 0x40000
const DMA_INT_SOURCE: u32 = 0x200;
const DMA_INT_MASK: u32 = 0x204;
const DMA_GLO_CFG: u32 = 0x208;
const DMA_RST_IDX: u32 = 0x20C;
const DMA_DELAY_INT: u32 = 0x210;
const DMA_TX_BASE_PTR0: u32 = 0x230;
const DMA_TX_MAX_CNT0: u32 = 0x234;
const DMA_TX_CTX_IDX0: u32 = 0x238;
const DMA_TX_DTX_IDX0: u32 = 0x23C;
const DMA_RX_BASE_PTR: u32 = 0x290;
const DMA_RX_MAX_CNT: u32 = 0x294;
const DMA_RX_CRX_IDX: u32 = 0x298;
const DMA_RX_DRX_IDX: u32 = 0x29C;

const DMA_TX_EN: u32 = 1 << 0;
const DMA_RX_EN: u32 = 1 << 2;
const DMA_DONE: u32 = 1 << 17;
const INT_RX_DONE: u32 = 1 << 2;

// The WDMA engine (JieLi's, 0x30F00-0x30F1C; offsets from 0x30000) the clean-room
// driver moves frames with, modd SPECSHEET §7.11: a transmit is kicked by the
// length write, a receive comes in two phases (the length word, then RXWI + frame).
const WDMA_CTL: u32 = 0xF04;
const WDMA_TX_ADR: u32 = 0xF10;
const WDMA_TX_LEN: u32 = 0xF14;
const WDMA_RX_ADR: u32 = 0xF18;
const WDMA_RX_LEN: u32 = 0xF1C;
const WDMA_CTL_TX_ACK: u32 = 1 << 10;
const WDMA_CTL_RX_ACK: u32 = 1 << 11;
/// Bits 9-11 acknowledge and do not stick (the driver's init writes 0x00010E37 and reads 0x00010037 back).
const WDMA_CTL_ACKS: u32 = 7 << 9;
const WDMA_CTL_RX_AVAIL: u32 = 1 << 17;
const WDMA_CTL_TX_DONE: u32 = 1 << 20;
const WDMA_CTL_RX_DONE: u32 = 1 << 21;
/// The TX buffer from WDMA_TX_ADR: the queue word, the TXWI (16 bytes), the frame.
const WDMA_TX_FRAME_OFF: u32 = 20;
/// The length written for a frame: the frame's plus the queue word, the TXWI and 6 trailing bytes.
const WDMA_TX_OVERHEAD: u32 = 26;
const RXWI_LEN: usize = 16;
/// RSSI0 in the RXWI of everything the station receives (the driver reads it as dBm: -56).
const RX_RSSI: u8 = 0xC8;
// The MAC's TX gate the driver polls before a transmit (RT2800 PBF/TXQ registers): the
// queue-2 page limit (0x40C byte 1), the pages in use (0x438 byte 2), queue busy (0x434
// bit 18) and the packet buffer ready (0x43C bits 7:5)
const MAC_PBF_MAX_PCNT: u32 = 0x40C;
const MAC_PBF_DBG: u32 = 0x43C;

pub struct WifiHw {
    // frames waiting for the station: its RX ring, or the WDMA engine
    rx_queue: VecDeque<Vec<u8>>,

    // DMA registers (0x40200-0x4029C)
    dma_int_source: u32,
    dma_int_mask: u32,
    dma_glo_cfg: u32,
    dma_tx_base: u32,
    dma_tx_max: u32,
    dma_tx_ctx_idx: u32,
    dma_tx_dtx_idx: u32,
    dma_rx_base: u32,
    dma_rx_max: u32,
    dma_rx_crx_idx: u32,
    dma_rx_drx_idx: u32,

    // BBP registers (indirect port at 0x3101C)
    bbp_regs: [u8; 256],
    bbp_last_read: u8,

    // MAC config registers (offset from 0x30000)
    mac_regs: std::collections::HashMap<u32, u32>,

    // Wireless control registers (0x14028-0x1405C)
    wl_ctrl: std::collections::HashMap<u32, u32>,

    // the WDMA engine: the driver's control word (its ack bits aside), the done bits,
    // the addresses, and whether phase 1 of a receive (the length) has been read
    wdma_ctl: u32,
    wdma_tx_done: bool,
    wdma_rx_done: bool,
    wdma_tx_adr: u32,
    wdma_rx_adr: u32,
    wdma_rx_phase2: bool,

    // Station state
    sta_mac: [u8; 6],
    associated: bool,
    authenticated: bool,
    associated_bssid: [u8; 6],
    seq_num: u16,

    // WPA2 authenticator state
    wpa: Option<WpaAuthenticator>,

    // the emulated time of the last beacons (`tick`)
    last_beacon_ms: u64,

    // Ethernet frames from the station, for the subnet (`crate::net::Link`)
    tx_queue: Vec<Vec<u8>>,
    /// The WPA2 access point: its name and passphrase (`crate::net::Ap`).
    pub ap: crate::net::Ap,
}

impl crate::net::Link for WifiHw {
    fn drain_tx(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.tx_queue)
    }

    fn push_rx(&mut self, eth: Vec<u8>) {
        self.eth_to_80211(eth);
    }
}

struct WpaAuthenticator {
    pmk: [u8; 32],
    ptk: [u8; 48],
    anonce: [u8; 32],
    replay: u64,
    step: u8,
    tx_pn: u64,
}

impl WifiHw {
    pub fn new() -> Self {
        let sta_mac = [0x02u8, 0x53, 0x49, 0x4d, 0x00, 0x15];

        let mut mac_regs = std::collections::HashMap::new();
        // MAC_ADDR_DW0/DW1 at offset 0x300/0x304 from 0x30000
        mac_regs.insert(0x300u32,
            sta_mac[0] as u32 | (sta_mac[1] as u32) << 8
            | (sta_mac[2] as u32) << 16 | (sta_mac[3] as u32) << 24);
        mac_regs.insert(0x304u32, sta_mac[4] as u32 | (sta_mac[5] as u32) << 8);
        // the TX gate open: 31 pages allowed, none in use, the packet buffer ready
        mac_regs.insert(MAC_PBF_MAX_PCNT, 0x1F << 8);
        mac_regs.insert(MAC_PBF_DBG, 0xE0);

        Self {
            rx_queue: VecDeque::new(),
            dma_int_source: 0,
            dma_int_mask: 0,
            dma_glo_cfg: 0,
            dma_tx_base: 0,
            dma_tx_max: 0,
            dma_tx_ctx_idx: 0,
            dma_tx_dtx_idx: 0,
            dma_rx_base: 0,
            dma_rx_max: 0,
            dma_rx_crx_idx: 0,
            dma_rx_drx_idx: 0,
            bbp_regs: [0; 256],
            bbp_last_read: 0,
            mac_regs,
            wl_ctrl: std::collections::HashMap::new(),
            wdma_ctl: 0,
            wdma_tx_done: false,
            wdma_rx_done: false,
            wdma_tx_adr: 0,
            wdma_rx_adr: 0,
            wdma_rx_phase2: false,
            sta_mac,
            associated: false,
            authenticated: false,
            associated_bssid: [0; 6],
            seq_num: 0,
            wpa: None,
            last_beacon_ms: 0,
            tx_queue: Vec::new(),
            ap: crate::net::Ap::default(),
        }
    }

    // --- DMA registers (0x40200-0x4029C, offset from 0x40000) ---

    pub fn dma_read(&mut self, offset: u32) -> u32 {
        match offset {
            DMA_INT_SOURCE => self.dma_int_source,
            DMA_INT_MASK => self.dma_int_mask,
            DMA_GLO_CFG => self.dma_glo_cfg,
            DMA_TX_BASE_PTR0 => self.dma_tx_base,
            DMA_TX_MAX_CNT0 => self.dma_tx_max,
            DMA_TX_CTX_IDX0 => self.dma_tx_ctx_idx,
            DMA_TX_DTX_IDX0 => self.dma_tx_dtx_idx,
            DMA_RX_BASE_PTR => self.dma_rx_base,
            DMA_RX_MAX_CNT => self.dma_rx_max,
            DMA_RX_CRX_IDX => self.dma_rx_crx_idx,
            DMA_RX_DRX_IDX => self.dma_rx_drx_idx,
            _ => 0,
        }
    }

    pub fn dma_write(&mut self, offset: u32, value: u32, ram: &mut Ram) {
        match offset {
            DMA_INT_SOURCE => {
                // Write-1-to-clear: firmware writes back status to ACK interrupts
                self.dma_int_source &= !value;
            }
            DMA_INT_MASK => self.dma_int_mask = value,
            DMA_GLO_CFG => {
                self.dma_glo_cfg = value;
                if value & DMA_RX_EN != 0 {
                    self.dma_deliver_rx(ram);
                }
            }
            DMA_RST_IDX | DMA_DELAY_INT => {}
            DMA_TX_BASE_PTR0 => self.dma_tx_base = value,
            DMA_TX_MAX_CNT0 => self.dma_tx_max = value,
            DMA_TX_CTX_IDX0 => {
                self.dma_tx_ctx_idx = value;
                if self.dma_glo_cfg & DMA_TX_EN != 0 {
                    self.dma_tx_kick(ram);
                }
            }
            DMA_TX_DTX_IDX0 => self.dma_tx_dtx_idx = value,
            DMA_RX_BASE_PTR => self.dma_rx_base = value,
            DMA_RX_MAX_CNT => self.dma_rx_max = value,
            DMA_RX_CRX_IDX => {
                self.dma_rx_crx_idx = value;
                self.dma_deliver_rx(ram);
            }
            DMA_RX_DRX_IDX => self.dma_rx_drx_idx = value,
            _ => {}
        }
    }

    fn dma_tx_kick(&mut self, ram: &mut Ram) {
        if self.dma_tx_base == 0 { return; }
        let desc_addr = self.dma_tx_base.wrapping_sub(RAM_BASE);
        if (desc_addr as usize + 16) > ram.len() { return; }

        let sd_ptr0 = ram.read32(desc_addr);
        let sd_len0 = ram.read32(desc_addr + 4);
        let len = (sd_len0 & 0xFFFF) as usize;

        if sd_ptr0 == 0 || len < 24 || len > 1600 { return; }

        let frame_addr = sd_ptr0.wrapping_sub(RAM_BASE);
        if (frame_addr as usize + len) > ram.len() { return; }

        let mut frame = vec![0u8; len];
        for i in 0..len {
            frame[i] = ram.read8(frame_addr + i as u32);
        }

        ram.write32(desc_addr + 12, DMA_DONE);
        self.dma_int_source |= 1 << 0; // INT_TX_DONE0

        self.handle_tx_frame(&frame);

        // TX response frames are now in rx_queue; deliver them to DMA RX ring
        self.dma_deliver_rx(ram);
    }

    pub fn dma_deliver_rx(&mut self, ram: &mut Ram) {
        if self.dma_rx_base == 0 { return; }
        if self.dma_glo_cfg & DMA_RX_EN == 0 { return; }

        while let Some(frame) = self.rx_queue.front() {
            let desc_addr = self.dma_rx_base.wrapping_sub(RAM_BASE);
            if (desc_addr as usize + 16) > ram.len() { return; }

            let flags = ram.read32(desc_addr + 12);
            if flags & DMA_DONE != 0 { return; } // firmware hasn't reclaimed yet

            let sd_ptr0 = ram.read32(desc_addr);
            if sd_ptr0 == 0 { return; }

            let buf_addr = sd_ptr0.wrapping_sub(RAM_BASE);
            if (buf_addr as usize + frame.len()) > ram.len() { return; }

            let frame = self.rx_queue.pop_front().unwrap();
            for (i, &b) in frame.iter().enumerate() {
                ram.write8(buf_addr + i as u32, b);
            }

            ram.write32(desc_addr + 4, frame.len() as u32 & 0xFFFF);
            ram.write32(desc_addr + 12, DMA_DONE);
            self.dma_int_source |= INT_RX_DONE;
        }
    }

    fn handle_tx_frame(&mut self, frame: &[u8]) {
        if frame.len() < 24 { return; }
        let fc = u16::from_le_bytes([frame[0], frame[1]]);
        let fc_type = fc & 0x0C;
        let fc_stype = fc & 0xF0;

        match (fc_type, fc_stype) {
            (0x00, 0xB0) => self.handle_auth(frame),
            (0x00, 0x00) => self.handle_assoc_req(frame),
            (0x00, 0x40) => self.handle_probe_req(frame),
            (0x08, _) => self.handle_data(frame),
            _ => {}
        }
    }

    fn next_seq(&mut self) -> u16 {
        let s = self.seq_num;
        self.seq_num = self.seq_num.wrapping_add(1);
        s << 4
    }

    fn handle_auth(&mut self, _frame: &[u8]) {
        self.authenticated = true;
        let mut rsp = Vec::with_capacity(30);
        let seq = self.next_seq();
        rsp.extend_from_slice(&FC_AUTH.to_le_bytes());
        rsp.extend_from_slice(&0u16.to_le_bytes());
        rsp.extend_from_slice(&self.sta_mac);
        rsp.extend_from_slice(&AP_MAC);
        rsp.extend_from_slice(&AP_MAC);
        rsp.extend_from_slice(&seq.to_le_bytes());
        rsp.extend_from_slice(&0u16.to_le_bytes()); // auth algorithm: open
        rsp.extend_from_slice(&2u16.to_le_bytes()); // auth seq: 2
        rsp.extend_from_slice(&0u16.to_le_bytes()); // status: success
        self.rx_queue.push_back(rsp);
    }

    fn handle_assoc_req(&mut self, frame: &[u8]) {
        if !self.authenticated {
            return;
        }
        // Extract target BSSID from the frame (addr1 at offset 4)
        if frame.len() >= 10 {
            self.associated_bssid.copy_from_slice(&frame[4..10]);
        }
        self.associated = true;
        let mut rsp = Vec::with_capacity(50);
        let seq = self.next_seq();
        rsp.extend_from_slice(&FC_ASSOC_RSP.to_le_bytes());
        rsp.extend_from_slice(&0u16.to_le_bytes());
        rsp.extend_from_slice(&self.sta_mac);
        rsp.extend_from_slice(&AP_MAC);
        rsp.extend_from_slice(&AP_MAC);
        rsp.extend_from_slice(&seq.to_le_bytes());
        rsp.extend_from_slice(&0x0431u16.to_le_bytes()); // capability
        rsp.extend_from_slice(&0u16.to_le_bytes());       // status: success
        rsp.extend_from_slice(&1u16.to_le_bytes());       // AID
        rsp.push(1); rsp.push(4);
        rsp.extend_from_slice(&[0x82, 0x84, 0x8B, 0x96]);

        self.rx_queue.push_back(rsp);

        // WPA2 handshake starts after the assoc response is queued
        let is_home = frame.len() >= 10 && frame[4..10] == AP_MAC;
        if is_home {
            self.start_wpa_handshake();
        }
    }

    fn handle_probe_req(&mut self, _frame: &[u8]) {
        let ssid = self.ap.ssid.clone();
        self.send_beacon_or_probe(true, &ssid, &AP_MAC, true);
        self.send_beacon_or_probe(true, CAFE_SSID, &CAFE_MAC, false);
    }

    fn send_beacon_or_probe(&mut self, is_probe: bool, ssid: &str, bssid: &[u8; 6], has_rsn: bool) {
        let mut frame = Vec::with_capacity(128);
        let fc = if is_probe { FC_PROBE_RSP } else { FC_BEACON };
        let seq = self.next_seq();
        frame.extend_from_slice(&fc.to_le_bytes());
        frame.extend_from_slice(&0u16.to_le_bytes());
        frame.extend_from_slice(&self.sta_mac);
        frame.extend_from_slice(bssid);
        frame.extend_from_slice(bssid);
        frame.extend_from_slice(&seq.to_le_bytes());
        frame.extend_from_slice(&[0u8; 8]); // timestamp
        frame.extend_from_slice(&BEACON_INTERVAL.to_le_bytes());
        let cap: u16 = if has_rsn { 0x0431 } else { 0x0421 };
        frame.extend_from_slice(&cap.to_le_bytes());
        frame.push(0); frame.push(ssid.len() as u8);
        frame.extend_from_slice(ssid.as_bytes());
        frame.push(1); frame.push(4);
        frame.extend_from_slice(&[0x82, 0x84, 0x8B, 0x96]);
        frame.push(3); frame.push(1); frame.push(AP_CHANNEL);
        if has_rsn {
            let rsn = [
                0x01, 0x00,             // version
                0x00, 0x0F, 0xAC, 0x04, // group cipher: CCMP
                0x01, 0x00,             // pairwise count
                0x00, 0x0F, 0xAC, 0x04, // pairwise: CCMP
                0x01, 0x00,             // AKM count
                0x00, 0x0F, 0xAC, 0x02, // AKM: PSK
                0x00, 0x00,             // RSN capabilities
            ];
            frame.push(48);
            frame.push(rsn.len() as u8);
            frame.extend_from_slice(&rsn);
        }
        self.rx_queue.push_back(frame);
    }

    fn handle_data(&mut self, frame: &[u8]) {
        if !self.associated || frame.len() < 32 {
            return;
        }
        let fc = u16::from_le_bytes([frame[0], frame[1]]);
        // (a QoS data frame carries its 2-byte QoS control after the addresses)
        let hdr_len: usize = if fc & 0x0080 != 0 { 26 } else { 24 };

        // CCMP-protected frame: decrypt in place
        let decrypted;
        let llc_start;
        if fc & 0x4000 != 0 {
            // Protected bit set: CCMP header (8 bytes) between header and payload
            if frame.len() < hdr_len + 8 + 8 {
                return;
            }
            let tk = match self.wpa.as_ref() {
                Some(w) if w.step == 4 => &w.ptk[32..48],
                _ => return,
            };
            let pn = ccmp_read_pn(&frame[hdr_len..hdr_len + 8]);
            let enc_data = &frame[hdr_len + 8..];
            match ccmp_decrypt(&frame[..hdr_len], enc_data, tk, pn) {
                Some(plain) => {
                    decrypted = plain;
                    llc_start = 0;
                }
                None => {
                    if crate::net::trace_on() {
                        eprintln!("wifi: ccmp decrypt failed, fc {fc:#06x} pn {pn} len {}", frame.len());
                    }
                    return;
                }
            }
        } else {
            decrypted = frame.to_vec();
            llc_start = hdr_len;
        };

        let llc = &decrypted[llc_start..];
        if llc.len() < 8 {
            return;
        }
        // Check for EAPOL (ethertype 0x888E)
        if llc[0] == 0xAA && llc[1] == 0xAA && llc[2] == 0x03 {
            let ethertype = ((llc[6] as u16) << 8) | llc[7] as u16;
            if ethertype == 0x888E {
                self.handle_eapol(&llc[8..]);
                return;
            }
        }
        if llc[0] != 0xAA || llc[1] != 0xAA || llc[2] != 0x03 {
            return;
        }
        let ethertype = [llc[6], llc[7]];
        let payload = &llc[8..];
        if crate::net::trace_on() {
            eprintln!("wifi: data from the station, ethertype {:02x}{:02x}, {} bytes", ethertype[0], ethertype[1], payload.len());
        }

        let gw_mac = [0x02, 0x53, 0x49, 0x4d, 0x00, 0x02];
        let mut eth = Vec::with_capacity(14 + payload.len());
        eth.extend_from_slice(&gw_mac);
        eth.extend_from_slice(&self.sta_mac);
        eth.extend_from_slice(&ethertype);
        eth.extend_from_slice(payload);

        self.tx_queue.push(eth);
    }

    fn eth_to_80211(&mut self, eth: Vec<u8>) {
        if eth.len() < 14 {
            return;
        }
        let ethertype = [eth[12], eth[13]];
        let is_eapol = ethertype == [0x88, 0x8E];
        let payload = &eth[14..];
        let seq = self.next_seq();

        let encrypt = !is_eapol && self.wpa.as_ref().is_some_and(|w| w.step == 4);

        let mut fc = FC_DATA_FROMDS;
        if encrypt {
            fc |= 0x4000; // Protected bit
        }

        let mut hdr = Vec::with_capacity(24);
        hdr.extend_from_slice(&fc.to_le_bytes());
        hdr.extend_from_slice(&0u16.to_le_bytes());
        hdr.extend_from_slice(&self.sta_mac);
        hdr.extend_from_slice(&AP_MAC);
        hdr.extend_from_slice(&eth[6..12]);
        hdr.extend_from_slice(&seq.to_le_bytes());

        if encrypt {
            let wpa = self.wpa.as_mut().unwrap();
            let pn = wpa.tx_pn;
            wpa.tx_pn += 1;
            let tk = &wpa.ptk[32..48];

            let mut plaintext = Vec::with_capacity(8 + payload.len());
            plaintext.extend_from_slice(&[0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00]);
            plaintext.extend_from_slice(&ethertype);
            plaintext.extend_from_slice(payload);

            let (ciphertext, mic) = ccmp_encrypt(&hdr, &plaintext, tk, pn);

            let mut frame = Vec::with_capacity(hdr.len() + 8 + ciphertext.len() + 8);
            frame.extend_from_slice(&hdr);
            frame.extend_from_slice(&ccmp_hdr_bytes(pn));
            frame.extend_from_slice(&ciphertext);
            frame.extend_from_slice(&mic);
            self.rx_queue.push_back(frame);
        } else {
            let mut frame = Vec::with_capacity(hdr.len() + 8 + payload.len());
            frame.extend_from_slice(&hdr);
            frame.extend_from_slice(&[0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00]);
            frame.extend_from_slice(&ethertype);
            frame.extend_from_slice(payload);
            self.rx_queue.push_back(frame);
        }
    }

    // --- WPA2 4-way handshake (AP/authenticator side) ---

    fn start_wpa_handshake(&mut self) {
        // Derive PMK from passphrase + SSID (same as the firmware does)
        let pmk = pbkdf2_sha1(self.ap.password.as_bytes(), self.ap.ssid.as_bytes(), 4096, 32);
        let mut pmk_arr = [0u8; 32];
        pmk_arr.copy_from_slice(&pmk);

        // Generate ANonce
        let anonce = {
            let mut n = [0u8; 32];
            for (i, b) in n.iter_mut().enumerate() {
                *b = ((i as u8).wrapping_mul(0x37)).wrapping_add(0xAB);
            }
            n
        };

        self.wpa = Some(WpaAuthenticator {
            pmk: pmk_arr,
            ptk: [0; 48],
            anonce,
            replay: 1,
            step: 1,
            tx_pn: 0,
        });

        // Send EAPOL message 1 as an 802.11 data frame
        self.send_eapol_msg1();
    }

    fn send_eapol_msg1(&mut self) {
        let wpa = self.wpa.as_ref().unwrap();
        let mut eapol = vec![0u8; EAPOL_HDR_LEN];
        eapol[0] = 2; // version
        eapol[EAPOL_TYPE_OFF] = 3; // EAPOL-Key
        let body_len = (EAPOL_HDR_LEN - 4) as u16;
        eapol[2] = (body_len >> 8) as u8;
        eapol[3] = body_len as u8;
        eapol[EAPOL_KEY_TYPE] = 2; // RSN
        let ki = KI_VER_HMAC_SHA1 | KI_PAIRWISE | KI_ACK;
        eapol[EAPOL_KEY_INFO] = (ki >> 8) as u8;
        eapol[EAPOL_KEY_INFO + 1] = ki as u8;
        eapol[EAPOL_KEY_LEN] = 0;
        eapol[EAPOL_KEY_LEN + 1] = 16;
        // Replay counter
        let replay = wpa.replay;
        for i in 0..8 {
            eapol[EAPOL_REPLAY + i] = (replay >> (56 - 8 * i)) as u8;
        }
        // ANonce
        eapol[EAPOL_NONCE..EAPOL_NONCE + 32].copy_from_slice(&wpa.anonce);
        // key data length = 0
        eapol[EAPOL_DATA_LEN] = 0;
        eapol[EAPOL_DATA_LEN + 1] = 0;

        self.wrap_eapol_as_80211(&eapol);
    }

    fn handle_eapol(&mut self, data: &[u8]) {
        if data.len() < EAPOL_HDR_LEN {
            return;
        }
        if data[EAPOL_TYPE_OFF] != 3 || data[EAPOL_KEY_TYPE] != 2 {
            return;
        }
        let ki = ((data[EAPOL_KEY_INFO] as u16) << 8) | data[EAPOL_KEY_INFO + 1] as u16;

        if self.wpa.is_none() {
            return;
        }

        // Message 2 from STA: has MIC, no ACK, has SNonce
        if ki & KI_MIC != 0 && ki & KI_ACK == 0 && ki & KI_SECURE == 0 {
            self.handle_eapol_msg2(data);
        }
        // Message 4 from STA: has MIC, Secure, no ACK
        else if ki & KI_MIC != 0 && ki & KI_SECURE != 0 && ki & KI_ACK == 0 {
            self.handle_eapol_msg4(data);
        }
    }

    fn handle_eapol_msg2(&mut self, data: &[u8]) {
        // Extract SNonce from message 2
        let mut snonce = [0u8; 32];
        snonce.copy_from_slice(&data[EAPOL_NONCE..EAPOL_NONCE + 32]);

        let wpa = self.wpa.as_mut().unwrap();

        // Derive PTK
        let ptk = derive_ptk(&wpa.pmk, &wpa.anonce, &snonce, &AP_MAC, &self.sta_mac);
        wpa.ptk.copy_from_slice(&ptk);
        wpa.step = 3;
        wpa.replay += 1;

        // Send EAPOL message 3
        self.send_eapol_msg3();
    }

    fn send_eapol_msg3(&mut self) {
        let wpa = self.wpa.as_ref().unwrap();
        let mut eapol = vec![0u8; EAPOL_HDR_LEN];
        eapol[0] = 2;
        eapol[EAPOL_TYPE_OFF] = 3;
        let body_len = (EAPOL_HDR_LEN - 4) as u16;
        eapol[2] = (body_len >> 8) as u8;
        eapol[3] = body_len as u8;
        eapol[EAPOL_KEY_TYPE] = 2;
        let ki = KI_VER_HMAC_SHA1 | KI_PAIRWISE | KI_ACK | KI_MIC | KI_INSTALL | KI_SECURE;
        eapol[EAPOL_KEY_INFO] = (ki >> 8) as u8;
        eapol[EAPOL_KEY_INFO + 1] = ki as u8;
        eapol[EAPOL_KEY_LEN] = 0;
        eapol[EAPOL_KEY_LEN + 1] = 16;
        let replay = wpa.replay;
        for i in 0..8 {
            eapol[EAPOL_REPLAY + i] = (replay >> (56 - 8 * i)) as u8;
        }
        eapol[EAPOL_NONCE..EAPOL_NONCE + 32].copy_from_slice(&wpa.anonce);

        // Compute MIC with KCK (first 16 bytes of PTK)
        let kck = &wpa.ptk[..16];
        let mic = hmac_sha1(kck, &eapol);
        eapol[EAPOL_MIC..EAPOL_MIC + 16].copy_from_slice(&mic[..16]);

        self.wrap_eapol_as_80211(&eapol);
    }

    fn handle_eapol_msg4(&mut self, _data: &[u8]) {
        // 4-way handshake complete: WPA2 keys installed
        if let Some(ref mut wpa) = self.wpa {
            wpa.step = 4;
        }
    }

    fn wrap_eapol_as_80211(&mut self, eapol: &[u8]) {
        let seq = self.next_seq();
        let mut frame = Vec::with_capacity(32 + eapol.len());
        // 802.11 data frame, from-DS
        frame.extend_from_slice(&FC_DATA_FROMDS.to_le_bytes());
        frame.extend_from_slice(&0u16.to_le_bytes());
        frame.extend_from_slice(&self.sta_mac); // addr1 = DA
        frame.extend_from_slice(&AP_MAC);       // addr2 = BSSID
        frame.extend_from_slice(&AP_MAC);       // addr3 = SA
        frame.extend_from_slice(&seq.to_le_bytes());
        // LLC/SNAP + EAPOL ethertype (0x888E)
        frame.extend_from_slice(&[0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00]);
        frame.push(0x88);
        frame.push(0x8E);
        frame.extend_from_slice(eapol);
        self.rx_queue.push_back(frame);
    }

    pub fn send_beacons(&mut self) {
        let ssid = self.ap.ssid.clone();
        self.send_beacon_or_probe(false, &ssid, &AP_MAC, true);
        self.send_beacon_or_probe(false, CAFE_SSID, &CAFE_MAC, false);
    }

    /// The chip's tick, every 10 ms of emulated time (`Soc::advance`): the virtual APs'
    /// beacons, and the frames waiting for the station into its RX ring. (What the subnet
    /// and the NAT answered came in through `Link::push_rx`, as 802.11 data frames.)
    pub fn tick(&mut self, now_ms: u64, ram: &mut Ram) {
        if now_ms.saturating_sub(self.last_beacon_ms) >= BEACON_PERIOD_MS && self.rx_queue.len() < RX_QUEUE_BEACON_CAP {
            self.last_beacon_ms = now_ms;
            self.send_beacons();
        }
        self.dma_deliver_rx(ram);
    }

    // --- BBP indirect port (offset 0x101C within Region::Wifi, i.e. 0x3101C) ---

    pub fn bbp_read(&self, _offset: u32) -> u32 {
        // The firmware reads BBP_PORT to check start bit (17) and get result (7:0).
        // bit 17 = 0 (not busy), bits 7:0 = last read result
        self.bbp_last_read as u32
    }

    pub fn bbp_write(&mut self, _offset: u32, value: u32) {
        // BBP protocol: write = [start:1 << 17] | [rd:1 << 16] | [reg:8 << 8] | [data:8]
        let rd = value & (1 << 16) != 0;
        let reg = ((value >> 8) & 0xFF) as u8;
        let data = (value & 0xFF) as u8;

        if rd {
            self.bbp_last_read = self.bbp_regs[reg as usize];
        } else {
            self.bbp_regs[reg as usize] = data;
        }
        // bit 17 (start) auto-clears: bbp_read returns 0 for that bit
    }

    // --- MAC config registers (offset 0x300+ within Region::Wifi) ---

    pub fn mac_read(&self, offset: u32) -> u32 {
        match offset {
            WDMA_CTL => {
                let mut v = self.wdma_ctl;
                if !self.rx_queue.is_empty() {
                    v |= WDMA_CTL_RX_AVAIL;
                }
                if self.wdma_tx_done {
                    v |= WDMA_CTL_TX_DONE;
                }
                if self.wdma_rx_done {
                    v |= WDMA_CTL_RX_DONE;
                }
                v
            }
            WDMA_TX_ADR => self.wdma_tx_adr,
            WDMA_RX_ADR => self.wdma_rx_adr,
            _ => *self.mac_regs.get(&offset).unwrap_or(&0),
        }
    }

    pub fn mac_write(&mut self, offset: u32, value: u32, ram: &mut Ram) {
        match offset {
            WDMA_CTL => {
                // the ack bits clear the done bits and do not stick
                if value & WDMA_CTL_TX_ACK != 0 {
                    self.wdma_tx_done = false;
                }
                if value & WDMA_CTL_RX_ACK != 0 {
                    self.wdma_rx_done = false;
                }
                self.wdma_ctl = value & !(WDMA_CTL_ACKS | WDMA_CTL_RX_AVAIL | WDMA_CTL_TX_DONE | WDMA_CTL_RX_DONE);
            }
            WDMA_TX_ADR => self.wdma_tx_adr = value,
            WDMA_TX_LEN => self.wdma_tx(value, ram),
            WDMA_RX_ADR => self.wdma_rx_adr = value,
            WDMA_RX_LEN => self.wdma_rx(value, ram),
            _ => {
                self.mac_regs.insert(offset, value);
            }
        }
    }

    /// The driver kicked a transmit of `len` bytes at WDMA_TX_ADR (the queue word, the
    /// TXWI, the frame, trailing zeros): the frame is on the air at once, and answered.
    fn wdma_tx(&mut self, len: u32, ram: &mut Ram) {
        let frame_len = len.saturating_sub(WDMA_TX_OVERHEAD) as usize;
        let at = self.wdma_tx_adr.wrapping_add(WDMA_TX_FRAME_OFF).wrapping_sub(RAM_BASE) as usize;
        if frame_len >= 24 && at + frame_len <= ram.len() {
            let frame: Vec<u8> = (0..frame_len).map(|i| ram.read8((at + i) as u32)).collect();
            self.handle_tx_frame(&frame);
        }
        self.wdma_tx_done = true;
    }

    /// The driver asked for `len` bytes of the frame waiting for it, into WDMA_RX_ADR:
    /// phase 1 is the 4-byte length (big-endian: RXWI + frame), phase 2 the RXWI, the
    /// frame and 4 more bytes. The frame leaves the queue with phase 2.
    fn wdma_rx(&mut self, len: u32, ram: &mut Ram) {
        self.wdma_rx_done = true;
        let Some(front) = self.rx_queue.front() else { return };
        let at = self.wdma_rx_adr.wrapping_sub(RAM_BASE) as usize;
        let data: Vec<u8> = if len == 4 && !self.wdma_rx_phase2 {
            self.wdma_rx_phase2 = true;
            ((RXWI_LEN + front.len()) as u32).to_be_bytes().to_vec()
        } else {
            let frame = self.rx_queue.pop_front().unwrap();
            self.wdma_rx_phase2 = false;
            let mut d = Vec::with_capacity(RXWI_LEN + frame.len() + 4);
            // RXWI word 0: the MPDU length in bits 27:16; word 2: RSSI0 in bits 7:0
            d.extend_from_slice(&((frame.len() as u32 & 0xFFF) << 16).to_le_bytes());
            d.extend_from_slice(&0u32.to_le_bytes());
            d.extend_from_slice(&(RX_RSSI as u32).to_le_bytes());
            d.extend_from_slice(&0u32.to_le_bytes());
            d.extend_from_slice(&frame);
            d.extend_from_slice(&[0; 4]);
            d.truncate(len as usize);
            d
        };
        if at + data.len() <= ram.len() {
            for (i, b) in data.iter().enumerate() {
                ram.write8((at + i) as u32, *b);
            }
        }
    }

    // --- Wireless control registers (0x14028-0x1405C) ---

    pub fn wl_ctrl_read(&self, addr: u32) -> u32 {
        *self.wl_ctrl.get(&addr).unwrap_or(&0)
    }

    pub fn wl_ctrl_write(&mut self, addr: u32, value: u32) {
        self.wl_ctrl.insert(addr, value);
    }
}

// --- Crypto helpers for WPA2 authenticator ---

fn sha1_block(h: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for i in 0..16 {
        w[i] = (block[4 * i] as u32) << 24
            | (block[4 * i + 1] as u32) << 16
            | (block[4 * i + 2] as u32) << 8
            | block[4 * i + 3] as u32;
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }
    let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
    for i in 0..80 {
        let (f, k) = match i {
            0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
            20..=39 => (b ^ c ^ d, 0x6ED9EBA1u32),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDCu32),
            _ => (b ^ c ^ d, 0xCA62C1D6u32),
        };
        let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(w[i]);
        e = d; d = c; c = b.rotate_left(30); b = a; a = t;
    }
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut buf = [0u8; 64];
    let mut off = 0;
    for &b in data {
        buf[off] = b;
        off += 1;
        if off == 64 {
            sha1_block(&mut h, &buf);
            off = 0;
        }
    }
    let bits = (data.len() as u64) * 8;
    buf[off] = 0x80;
    off += 1;
    if off > 56 {
        while off < 64 { buf[off] = 0; off += 1; }
        sha1_block(&mut h, &buf);
        buf = [0; 64];
        off = 0;
    }
    while off < 56 { buf[off] = 0; off += 1; }
    for i in 0..8 { buf[56 + i] = (bits >> (56 - 8 * i)) as u8; }
    sha1_block(&mut h, &buf);
    let mut out = [0u8; 20];
    for i in 0..5 {
        out[4 * i] = (h[i] >> 24) as u8;
        out[4 * i + 1] = (h[i] >> 16) as u8;
        out[4 * i + 2] = (h[i] >> 8) as u8;
        out[4 * i + 3] = h[i] as u8;
    }
    out
}

fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let h = sha1(key);
        k[..20].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = Vec::with_capacity(64 + data.len());
    for i in 0..64 { ipad.push(k[i] ^ 0x36); }
    ipad.extend_from_slice(data);
    let inner = sha1(&ipad);
    let mut opad = Vec::with_capacity(64 + 20);
    for i in 0..64 { opad.push(k[i] ^ 0x5C); }
    opad.extend_from_slice(&inner);
    sha1(&opad)
}

fn hmac_sha1_2(key: &[u8], d1: &[u8], d2: &[u8]) -> [u8; 20] {
    let mut combined = Vec::with_capacity(d1.len() + d2.len());
    combined.extend_from_slice(d1);
    combined.extend_from_slice(d2);
    hmac_sha1(key, &combined)
}

fn pbkdf2_sha1(pass: &[u8], salt: &[u8], iterations: u32, dk_len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(dk_len);
    let mut block = 1u32;
    while out.len() < dk_len {
        let be = block.to_be_bytes();
        let mut u = hmac_sha1_2(pass, salt, &be);
        let mut t = u;
        for _ in 1..iterations {
            u = hmac_sha1(pass, &u);
            for k in 0..20 { t[k] ^= u[k]; }
        }
        let copy = (dk_len - out.len()).min(20);
        out.extend_from_slice(&t[..copy]);
        block += 1;
    }
    out
}

fn prf(key: &[u8], label: &[u8], data: &[u8], out_bits: usize) -> Vec<u8> {
    let out_len = (out_bits + 7) / 8;
    let mut result = Vec::with_capacity(out_len);
    let mut r = Vec::with_capacity(label.len() + 1 + data.len() + 1);
    r.extend_from_slice(label);
    r.push(0);
    r.extend_from_slice(data);
    r.push(0);
    let mut i = 0u8;
    while result.len() < out_len {
        *r.last_mut().unwrap() = i;
        let digest = hmac_sha1(key, &r);
        let copy = (out_len - result.len()).min(20);
        result.extend_from_slice(&digest[..copy]);
        i += 1;
    }
    result
}

fn derive_ptk(pmk: &[u8; 32], anonce: &[u8; 32], snonce: &[u8; 32],
              aa: &[u8; 6], spa: &[u8; 6]) -> [u8; 48] {
    let label = b"Pairwise key expansion";
    let (lo_addr, hi_addr) = if aa < spa { (aa.as_slice(), spa.as_slice()) } else { (spa.as_slice(), aa.as_slice()) };
    let (lo_nonce, hi_nonce) = if anonce < snonce { (anonce.as_slice(), snonce.as_slice()) } else { (snonce.as_slice(), anonce.as_slice()) };
    let mut data = Vec::with_capacity(76);
    data.extend_from_slice(lo_addr);
    data.extend_from_slice(hi_addr);
    data.extend_from_slice(lo_nonce);
    data.extend_from_slice(hi_nonce);
    let ptk_bytes = prf(pmk, label, &data, 48 * 8);
    let mut ptk = [0u8; 48];
    ptk.copy_from_slice(&ptk_bytes[..48]);
    ptk
}

// --- AES-128 (FIPS 197) for CCMP ---

#[rustfmt::skip]
const AES_SBOX: [u8; 256] = [
    0x63,0x7c,0x77,0x7b,0xf2,0x6b,0x6f,0xc5,0x30,0x01,0x67,0x2b,0xfe,0xd7,0xab,0x76,
    0xca,0x82,0xc9,0x7d,0xfa,0x59,0x47,0xf0,0xad,0xd4,0xa2,0xaf,0x9c,0xa4,0x72,0xc0,
    0xb7,0xfd,0x93,0x26,0x36,0x3f,0xf7,0xcc,0x34,0xa5,0xe5,0xf1,0x71,0xd8,0x31,0x15,
    0x04,0xc7,0x23,0xc3,0x18,0x96,0x05,0x9a,0x07,0x12,0x80,0xe2,0xeb,0x27,0xb2,0x75,
    0x09,0x83,0x2c,0x1a,0x1b,0x6e,0x5a,0xa0,0x52,0x3b,0xd6,0xb3,0x29,0xe3,0x2f,0x84,
    0x53,0xd1,0x00,0xed,0x20,0xfc,0xb1,0x5b,0x6a,0xcb,0xbe,0x39,0x4a,0x4c,0x58,0xcf,
    0xd0,0xef,0xaa,0xfb,0x43,0x4d,0x33,0x85,0x45,0xf9,0x02,0x7f,0x50,0x3c,0x9f,0xa8,
    0x51,0xa3,0x40,0x8f,0x92,0x9d,0x38,0xf5,0xbc,0xb6,0xda,0x21,0x10,0xff,0xf3,0xd2,
    0xcd,0x0c,0x13,0xec,0x5f,0x97,0x44,0x17,0xc4,0xa7,0x7e,0x3d,0x64,0x5d,0x19,0x73,
    0x60,0x81,0x4f,0xdc,0x22,0x2a,0x90,0x88,0x46,0xee,0xb8,0x14,0xde,0x5e,0x0b,0xdb,
    0xe0,0x32,0x3a,0x0a,0x49,0x06,0x24,0x5c,0xc2,0xd3,0xac,0x62,0x91,0x95,0xe4,0x79,
    0xe7,0xc8,0x37,0x6d,0x8d,0xd5,0x4e,0xa9,0x6c,0x56,0xf4,0xea,0x65,0x7a,0xae,0x08,
    0xba,0x78,0x25,0x2e,0x1c,0xa6,0xb4,0xc6,0xe8,0xdd,0x74,0x1f,0x4b,0xbd,0x8b,0x8a,
    0x70,0x3e,0xb5,0x66,0x48,0x03,0xf6,0x0e,0x61,0x35,0x57,0xb9,0x86,0xc1,0x1d,0x9e,
    0xe1,0xf8,0x98,0x11,0x69,0xd9,0x8e,0x94,0x9b,0x1e,0x87,0xe9,0xce,0x55,0x28,0xdf,
    0x8c,0xa1,0x89,0x0d,0xbf,0xe6,0x42,0x68,0x41,0x99,0x2d,0x0f,0xb0,0x54,0xbb,0x16,
];

const AES_RCON: [u8; 10] = [0x01,0x02,0x04,0x08,0x10,0x20,0x40,0x80,0x1b,0x36];

fn aes_xtime(x: u8) -> u8 {
    (x << 1) ^ (((x >> 7) & 1) * 0x1b)
}

fn aes_key_expand(key: &[u8; 16]) -> [u8; 176] {
    let mut rk = [0u8; 176];
    rk[..16].copy_from_slice(key);
    for i in (16..176).step_by(4) {
        let mut t = [rk[i - 4], rk[i - 3], rk[i - 2], rk[i - 1]];
        if i % 16 == 0 {
            t = [AES_SBOX[t[1] as usize] ^ AES_RCON[i / 16 - 1],
                 AES_SBOX[t[2] as usize],
                 AES_SBOX[t[3] as usize],
                 AES_SBOX[t[0] as usize]];
        }
        for j in 0..4 {
            rk[i + j] = rk[i - 16 + j] ^ t[j];
        }
    }
    rk
}

fn aes_encrypt_block(block: &mut [u8; 16], key: &[u8; 16]) {
    let rk = aes_key_expand(key);
    for i in 0..16 { block[i] ^= rk[i]; }
    for round in 1..10 {
        // SubBytes
        for i in 0..16 { block[i] = AES_SBOX[block[i] as usize]; }
        // ShiftRows
        let t = *block;
        block[1] = t[5]; block[5] = t[9]; block[9] = t[13]; block[13] = t[1];
        block[2] = t[10]; block[6] = t[14]; block[10] = t[2]; block[14] = t[6];
        block[3] = t[15]; block[7] = t[3]; block[11] = t[7]; block[15] = t[11];
        // MixColumns
        for c in 0..4 {
            let i = c * 4;
            let (a0, a1, a2, a3) = (block[i], block[i+1], block[i+2], block[i+3]);
            let x = a0 ^ a1 ^ a2 ^ a3;
            block[i]   = a0 ^ aes_xtime(a0 ^ a1) ^ x;
            block[i+1] = a1 ^ aes_xtime(a1 ^ a2) ^ x;
            block[i+2] = a2 ^ aes_xtime(a2 ^ a3) ^ x;
            block[i+3] = a3 ^ aes_xtime(a3 ^ a0) ^ x;
        }
        // AddRoundKey
        let off = round * 16;
        for i in 0..16 { block[i] ^= rk[off + i]; }
    }
    // Final round (no MixColumns)
    for i in 0..16 { block[i] = AES_SBOX[block[i] as usize]; }
    let t = *block;
    block[1] = t[5]; block[5] = t[9]; block[9] = t[13]; block[13] = t[1];
    block[2] = t[10]; block[6] = t[14]; block[10] = t[2]; block[14] = t[6];
    block[3] = t[15]; block[7] = t[3]; block[11] = t[7]; block[15] = t[11];
    for i in 0..16 { block[i] ^= rk[160 + i]; }
}

fn aes_block(key: &[u8; 16], input: &[u8; 16]) -> [u8; 16] {
    let mut out = *input;
    aes_encrypt_block(&mut out, key);
    out
}

// --- CCMP (AES-CCM, IEEE 802.11i section 8.3.3) ---

/// The CCMP AAD of an 802.11 header (IEEE 802.11-2020 12.5.3.3.3): the frame control
/// with Retry, Power Management, More Data and Order cleared and Protected set, the three
/// addresses, the sequence control with its sequence number zeroed, and for a QoS data
/// frame its TID. 22 bytes, 24 with QoS.
fn ccmp_aad(hdr: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(24);
    aad.push(hdr[0]);
    aad.push((hdr[1] & 0x07) | 0x40);
    aad.extend_from_slice(&hdr[4..22]);
    aad.push(hdr[22] & 0x0F);
    aad.push(0);
    if hdr[0] & 0x80 != 0 {
        aad.push(hdr[24] & 0x0F);
        aad.push(0);
    }
    aad
}

/// The CCMP nonce: the priority (a QoS frame's TID, else 0), A2, the packet number.
fn ccmp_nonce(hdr: &[u8], pn: u64) -> [u8; 13] {
    let a2 = &hdr[10..16];
    let mut n = [0u8; 13];
    n[0] = if hdr[0] & 0x80 != 0 { hdr[24] & 0x0F } else { 0 };
    n[1..7].copy_from_slice(&a2[..6]);
    n[7] = (pn >> 40) as u8;
    n[8] = (pn >> 32) as u8;
    n[9] = (pn >> 24) as u8;
    n[10] = (pn >> 16) as u8;
    n[11] = (pn >> 8) as u8;
    n[12] = pn as u8;
    n
}

fn ccmp_cbc_mac(tk: &[u8; 16], nonce: &[u8; 13], aad: &[u8], plaintext: &[u8]) -> [u8; 8] {
    // B0: flags(0x59) || nonce(13) || payload_len(2)
    let mut b = [0u8; 16];
    b[0] = 0x59;
    b[1..14].copy_from_slice(nonce);
    b[14] = (plaintext.len() >> 8) as u8;
    b[15] = plaintext.len() as u8;
    let mut x = aes_block(tk, &b);

    // AAD block: 2-byte length prefix + AAD, padded to 16-byte boundary
    b = [0; 16];
    b[0] = (aad.len() >> 8) as u8;
    b[1] = aad.len() as u8;
    let mut ai = 0;
    for j in 2..16 {
        if ai < aad.len() { b[j] = aad[ai]; ai += 1; }
    }
    for j in 0..16 { x[j] ^= b[j]; }
    x = aes_block(tk, &x);
    while ai < aad.len() {
        b = [0; 16];
        for j in 0..16 {
            if ai < aad.len() { b[j] = aad[ai]; ai += 1; }
        }
        for j in 0..16 { x[j] ^= b[j]; }
        x = aes_block(tk, &x);
    }

    // Plaintext blocks
    let mut pi = 0;
    while pi < plaintext.len() {
        b = [0; 16];
        for j in 0..16 {
            if pi + j < plaintext.len() { b[j] = plaintext[pi + j]; }
        }
        for j in 0..16 { x[j] ^= b[j]; }
        x = aes_block(tk, &x);
        pi += 16;
    }

    let mut mic = [0u8; 8];
    mic.copy_from_slice(&x[..8]);
    mic
}

fn ccmp_ctr_xor(tk: &[u8; 16], nonce: &[u8; 13], data: &mut [u8], start_ctr: u16) {
    let mut a = [0u8; 16];
    a[0] = 0x01;
    a[1..14].copy_from_slice(nonce);
    let mut ctr = start_ctr;
    let mut i = 0;
    while i < data.len() {
        a[14] = (ctr >> 8) as u8;
        a[15] = ctr as u8;
        let s = aes_block(tk, &a);
        let blk = (data.len() - i).min(16);
        for j in 0..blk {
            data[i + j] ^= s[j];
        }
        i += blk;
        ctr += 1;
    }
}

fn ccmp_hdr_bytes(pn: u64) -> [u8; 8] {
    [
        pn as u8,
        (pn >> 8) as u8,
        0,
        0x20, // ExtIV
        (pn >> 16) as u8,
        (pn >> 24) as u8,
        (pn >> 32) as u8,
        (pn >> 40) as u8,
    ]
}

fn ccmp_read_pn(hdr: &[u8]) -> u64 {
    hdr[0] as u64
        | ((hdr[1] as u64) << 8)
        | ((hdr[4] as u64) << 16)
        | ((hdr[5] as u64) << 24)
        | ((hdr[6] as u64) << 32)
        | ((hdr[7] as u64) << 40)
}

fn ccmp_encrypt(hdr: &[u8], plaintext: &[u8], tk: &[u8], pn: u64) -> (Vec<u8>, [u8; 8]) {
    let tk16: &[u8; 16] = tk.try_into().unwrap();
    let nonce = ccmp_nonce(hdr, pn);
    let mic = ccmp_cbc_mac(tk16, &nonce, &ccmp_aad(hdr), plaintext);
    let mut ct = plaintext.to_vec();
    ccmp_ctr_xor(tk16, &nonce, &mut ct, 1);
    // Encrypt MIC with counter 0
    let mut a0 = [0u8; 16];
    a0[0] = 0x01;
    a0[1..14].copy_from_slice(&nonce);
    let s0 = aes_block(tk16, &a0);
    let mut enc_mic = mic;
    for i in 0..8 { enc_mic[i] ^= s0[i]; }
    (ct, enc_mic)
}

fn ccmp_decrypt(hdr: &[u8], enc_payload: &[u8], tk: &[u8], pn: u64) -> Option<Vec<u8>> {
    if enc_payload.len() < 8 { return None; }
    let tk16: &[u8; 16] = tk.try_into().unwrap();
    let data_len = enc_payload.len() - 8;
    let nonce = ccmp_nonce(hdr, pn);

    // Extract and decrypt MIC
    let mut enc_mic = [0u8; 8];
    enc_mic.copy_from_slice(&enc_payload[data_len..]);
    let mut a0 = [0u8; 16];
    a0[0] = 0x01;
    a0[1..14].copy_from_slice(&nonce);
    let s0 = aes_block(tk16, &a0);
    for i in 0..8 { enc_mic[i] ^= s0[i]; }

    // Decrypt payload
    let mut plain = enc_payload[..data_len].to_vec();
    ccmp_ctr_xor(tk16, &nonce, &mut plain, 1);

    // Verify MIC
    let mic = ccmp_cbc_mac(tk16, &nonce, &ccmp_aad(hdr), &plain);
    if mic != enc_mic { return None; }
    Some(plain)
}
