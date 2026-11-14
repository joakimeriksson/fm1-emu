// SPDX-License-Identifier: MIT
//! A virtual full-speed USB Ethernet adapter on the FM-1's host port: CDC-ECM (USB CDC 1.2,
//! Ethernet Control Model 1.2), the class any plain adapter speaks and modd's usbnet_ecm.c
//! drives: a control interface (02/06) with the header, union and Ethernet-networking
//! functional descriptors (the MAC as a string descriptor), a data interface whose alternate
//! setting 1 has the bulk pair, raw Ethernet frames on it, SetEthernetPacketFilter. A new
//! implementation from USB 2.0 chapter 9 and the CDC/ECM specifications; no code copied.
//! Ethernet frames cross to the SoC's virtual subnet as a `crate::net::Link`.
use std::collections::VecDeque;

use super::Reply;
use crate::net::Link;

/// The adapter's MAC address (the subnet's station: net.rs).
pub(crate) const MAC: [u8; 6] = crate::net::STA_MAC;
/// An Ethernet frame at most: 1500 bytes of payload and the header.
const FRAME_MAX: usize = 1514;
/// Frames waiting for a station that is not reading.
const RX_CAP: usize = 64;

const DEVICE: &[u8] = &[18, 1, 0, 2, 2, 0, 0, 64, 0x09, 0x12, 0x01, 0x00, 1, 0, 1, 2, 0, 1];
const CONFIG: &[u8] = &[
    9, 2, 80, 0, 2, 1, 0, 0x80, 50,
    // the control interface: CDC, Ethernet Control Model
    9, 4, 0, 0, 1, 0x02, 0x06, 0x00, 0,
    5, 0x24, 0x00, 0x10, 0x01,                      // header: CDC 1.10
    5, 0x24, 0x06, 0, 1,                            // union: control 0, data 1
    13, 0x24, 0x0f, 3, 0, 0, 0, 0, 0xea, 0x05, 0, 0, 0, // Ethernet networking: iMACAddress 3, 1514
    7, 5, 0x83, 3, 16, 0, 16,                       // notifications
    // the data interface: nothing in alternate setting 0, the bulk pair in 1
    9, 4, 1, 0, 0, 0x0a, 0x00, 0x00, 0,
    9, 4, 1, 1, 2, 0x0a, 0x00, 0x00, 0,
    7, 5, 0x81, 2, 64, 0, 0,
    7, 5, 0x02, 2, 64, 0, 0,
];
const SET_ETHERNET_PACKET_FILTER: u8 = 0x43;

#[derive(Default)]
enum Control {
    #[default]
    Idle,
    In { data: Vec<u8>, pos: usize, zlp: bool },
    StatusOut,
    StatusIn(Effect),
    Stalled,
}

#[derive(Clone, Copy)]
enum Effect {
    None,
    Address(u8),
    Configuration(u8),
    Interface(u8),
    Filter(u16),
}

pub(crate) struct EthDevice {
    pub(super) address: u8,
    configuration: u8,
    alt: u8,
    pub(super) filter: u16,
    control: Control,
    /// A frame the host is sending, bulk packet by bulk packet.
    tx_partial: Vec<u8>,
    /// Frames the host sent, for the subnet.
    tx_done: Vec<Vec<u8>>,
    /// Frames for the host, and the one being read (its position).
    rx_frames: VecDeque<Vec<u8>>,
    rx: Option<(Vec<u8>, usize)>,
    pub(super) frames_in: u32,
    pub(super) frames_out: u32,
}

impl EthDevice {
    pub(super) fn new() -> Self {
        Self { address: 0, configuration: 0, alt: 0, filter: 0, control: Control::Idle, tx_partial: Vec::new(),
            tx_done: Vec::new(), rx_frames: VecDeque::new(), rx: None, frames_in: 0, frames_out: 0 }
    }

    pub(super) fn reset(&mut self) {
        self.address = 0;
        self.configuration = 0;
        self.alt = 0;
        self.filter = 0;
        self.control = Control::Idle;
        self.tx_partial.clear();
        self.tx_done.clear();
        self.rx_frames.clear();
        self.rx = None;
    }

    /// Configured with the data interface's alternate setting 1 on: the link is up.
    pub(super) fn up(&self) -> bool {
        self.configuration == 1 && self.alt == 1
    }

    /// The MAC as the ECM string descriptor carries it: twelve hex digits, UTF-16.
    fn mac_string() -> Vec<u8> {
        let mut d = vec![0, 3];
        for b in MAC {
            for c in format!("{b:02X}").encode_utf16() { d.extend(c.to_le_bytes()); }
        }
        d[0] = d.len() as u8;
        d
    }

    pub(super) fn setup(&mut self, packet: &[u8]) -> Reply {
        self.control = Control::Stalled; // a new SETUP aborts any old control transfer
        if packet.len() != 8 { return Reply::Stall; }
        let value = u16::from_le_bytes([packet[2], packet[3]]);
        let index = u16::from_le_bytes([packet[4], packet[5]]);
        let length = u16::from_le_bytes([packet[6], packet[7]]) as usize;
        let mut data = Vec::new();
        let effect = match (packet[0], packet[1]) {
            (0x80, 6) => {
                data = match (value >> 8, value & 255) {
                    (1, 0) if index == 0 => DEVICE.to_vec(),
                    (2, 0) if index == 0 => CONFIG.to_vec(),
                    (3, 0) if index == 0 => vec![4, 3, 9, 4],
                    (3, n @ 1..=3) if index == 0x409 => {
                        if n == 3 { Self::mac_string() } else {
                            let s = if n == 1 { "FM1 emulator" } else { "FM1 virtual USB Ethernet adapter (CDC-ECM)" };
                            let mut d = vec![0, 3];
                            for c in s.encode_utf16() { d.extend(c.to_le_bytes()); }
                            d[0] = d.len() as u8;
                            d
                        }
                    }
                    _ => return Reply::Stall,
                };
                Effect::None
            }
            (0, 5) if value <= 127 && index == 0 && length == 0 && self.configuration == 0 => Effect::Address(value as u8),
            (0, 9) if value <= 1 && index == 0 && length == 0 && self.address != 0 => Effect::Configuration(value as u8),
            (0x80, 8) if value == 0 && index == 0 && length == 1 => { data.push(self.configuration); Effect::None }
            (0x80, 0) if value == 0 && index == 0 && length == 2 => { data.extend([0, 0]); Effect::None }
            (0x81, 0) if self.configuration == 1 && value == 0 && index <= 1 && length == 2 => { data.extend([0, 0]); Effect::None }
            (0x82, 0) if self.configuration == 1 && value == 0 && length == 2 && matches!(index, 0x81 | 0x02 | 0x83) => {
                data.extend([0, 0]); Effect::None
            }
            (1, 11) if self.configuration == 1 && index == 1 && value <= 1 && length == 0 => Effect::Interface(value as u8),
            (1, 11) if self.configuration == 1 && index == 0 && value == 0 && length == 0 => Effect::None,
            (0x81, 10) if self.configuration == 1 && value == 0 && index <= 1 && length == 1 => {
                data.push(if index == 1 { self.alt } else { 0 }); Effect::None
            }
            // the one class request the host needs: which frames the adapter passes up
            (0x21, SET_ETHERNET_PACKET_FILTER) if self.configuration == 1 && index == 0 && length == 0 => Effect::Filter(value),
            _ => return Reply::Stall,
        };
        self.control = if packet[0] & 0x80 != 0 && length != 0 {
            data.truncate(length);
            let zlp = data.len() < length && data.len() % 64 == 0;
            Control::In { data, pos: 0, zlp }
        } else if packet[0] & 0x80 != 0 {
            Control::StatusOut
        } else {
            Control::StatusIn(effect)
        };
        Reply::Ack
    }

    pub(super) fn control_in(&mut self, status: bool) -> Reply {
        match std::mem::take(&mut self.control) {
            Control::In { data, pos, zlp } if !status => {
                let end = (pos + 64).min(data.len());
                let packet = data[pos..end].to_vec();
                self.control = if end < data.len() || (zlp && pos < data.len()) {
                    Control::In { data, pos: end, zlp }
                } else { Control::StatusOut };
                Reply::Data(packet)
            }
            Control::StatusIn(effect) if status => {
                match effect {
                    Effect::Address(a) => self.address = a,
                    Effect::Configuration(c) => {
                        self.configuration = c;
                        self.alt = 0;
                        self.filter = 0;
                        self.drop_frames();
                    }
                    Effect::Interface(alt) => {
                        self.alt = alt;
                        self.drop_frames();
                    }
                    Effect::Filter(f) => self.filter = f,
                    Effect::None => {}
                }
                Reply::Data(Vec::new())
            }
            _ => { self.control = Control::Stalled; Reply::Stall }
        }
    }

    pub(super) fn control_out(&mut self, data: &[u8], status: bool) -> Reply {
        if matches!(self.control, Control::StatusOut) && status && data.is_empty() {
            self.control = Control::Idle;
            Reply::Ack
        } else { self.control = Control::Stalled; Reply::Stall }
    }

    fn drop_frames(&mut self) {
        self.tx_partial.clear();
        self.rx_frames.clear();
        self.rx = None;
    }

    /// The host's bulk OUT: a frame, 64 bytes at a time. A short packet ends it; so does the
    /// IPv4 total length, since a frame of a whole number of packets gets no empty one after.
    pub(super) fn bulk_out(&mut self, ep: u8, data: &[u8]) -> Reply {
        if ep != 2 || !self.up() { return Reply::Stall; }
        if self.tx_partial.len() + data.len() > FRAME_MAX {
            self.tx_partial.clear();
            return Reply::Stall;
        }
        self.tx_partial.extend_from_slice(data);
        if data.len() < 64 || ipv4_complete(&self.tx_partial) {
            let frame = std::mem::take(&mut self.tx_partial);
            if frame.len() >= 14 {
                self.frames_out += 1;
                self.tx_done.push(frame);
            }
        }
        Reply::Ack
    }

    /// The host's bulk IN: the next 64 bytes of the frame being read, an empty packet after a
    /// frame of a whole number of them, NAK when nothing waits.
    pub(super) fn bulk_in(&mut self, ep: u8) -> Reply {
        if ep != 1 || !self.up() { return Reply::Stall; }
        if self.rx.is_none() {
            let Some(frame) = self.rx_frames.pop_front() else { return Reply::Nak };
            self.rx = Some((frame, 0));
        }
        let (buf, pos) = self.rx.take().unwrap();
        let end = (pos + 64).min(buf.len());
        let packet = buf[pos..end].to_vec();
        if end < buf.len() || packet.len() == 64 {
            self.rx = Some((buf, end));
        }
        Reply::Data(packet)
    }
}

/// Is `frame` a whole IPv4 frame (its header's total length is in)?
fn ipv4_complete(frame: &[u8]) -> bool {
    frame.len() >= 34 && frame[12..14] == [0x08, 0x00]
        && frame.len() >= 14 + u16::from_be_bytes([frame[16], frame[17]]) as usize
}

impl Link for EthDevice {
    fn drain_tx(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.tx_done)
    }

    fn push_rx(&mut self, eth: Vec<u8>) {
        if !self.up() || eth.len() > FRAME_MAX || self.rx_frames.len() >= RX_CAP { return; }
        // the packet filter: directed (bit 2) and broadcast (bit 3) frames; multicast with
        // bit 1 (all) set
        let to = &eth[..6];
        let wanted = if to == MAC { self.filter & 4 != 0 }
            else if to == [0xff; 6] { self.filter & 8 != 0 }
            else if to[0] & 1 != 0 { self.filter & 2 != 0 }
            else { self.filter & 1 != 0 };
        if !wanted { return; }
        self.frames_in += 1;
        self.rx_frames.push_back(eth);
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in crate::usb) fn enumerate(d: &mut EthDevice) {
        assert_eq!(d.setup(&[0x80, 6, 0, 1, 0, 0, 8, 0]), Reply::Ack);
        assert_eq!(d.control_in(false), Reply::Data(DEVICE[..8].to_vec()));
        assert_eq!(d.control_out(&[], true), Reply::Ack);
        assert_eq!(d.setup(&[0, 5, 1, 0, 0, 0, 0, 0]), Reply::Ack);
        assert_eq!(d.control_in(true), Reply::Data(vec![]));
        assert_eq!(d.address, 1);
        assert_eq!(d.setup(&[0x80, 6, 0, 2, 0, 0, 80, 0]), Reply::Ack);
        let mut config = Vec::new();
        for _ in 0..2 {
            let Reply::Data(p) = d.control_in(false) else { panic!() };
            config.extend(p);
        }
        assert_eq!(config, CONFIG);
        assert_eq!(d.control_out(&[], true), Reply::Ack);
        // modd's ecm_attach: SET_CONFIGURATION, the data interface's alternate 1, the MAC
        // string, the packet filter
        assert_eq!(d.setup(&[0, 9, 1, 0, 0, 0, 0, 0]), Reply::Ack);
        assert_eq!(d.control_in(true), Reply::Data(vec![]));
        assert!(!d.up(), "the data interface is off until its alternate setting is selected");
        assert_eq!(d.setup(&[1, 11, 1, 0, 1, 0, 0, 0]), Reply::Ack);
        assert_eq!(d.control_in(true), Reply::Data(vec![]));
        assert!(d.up());
        assert_eq!(d.setup(&[0x80, 6, 3, 3, 9, 4, 64, 0]), Reply::Ack);
        let Reply::Data(s) = d.control_in(false) else { panic!() };
        assert_eq!(s.len(), 26);
        assert_eq!(String::from_utf16_lossy(&s[2..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()),
                   "0253494D0015");
        assert_eq!(d.control_out(&[], true), Reply::Ack);
        assert_eq!(d.setup(&[0x21, SET_ETHERNET_PACKET_FILTER, 0x0d, 0, 0, 0, 0, 0]), Reply::Ack);
        assert_eq!(d.control_in(true), Reply::Data(vec![]));
        assert_eq!(d.filter, 0x0d);
    }

    #[test]
    fn enumerates_as_the_cdc_ecm_adapter_modd_looks_for() {
        let mut d = EthDevice::new();
        enumerate(&mut d);
        // what the driver's parser walks: control interface 02/06 alt 0, the union naming data
        // interface 1, the Ethernet functional descriptor's string index, data alt 1's bulk pair
        assert_eq!(&CONFIG[9..18], &[9, 4, 0, 0, 1, 0x02, 0x06, 0x00, 0]);
        assert_eq!(&CONFIG[23..28], &[5, 0x24, 0x06, 0, 1]);
        assert_eq!((CONFIG[28], CONFIG[30], CONFIG[31]), (13, 0x0f, 3));
        assert_eq!(&CONFIG[57..66], &[9, 4, 1, 1, 2, 0x0a, 0, 0, 0]);
        assert_eq!((CONFIG[68], CONFIG[69], CONFIG[75], CONFIG[76]), (0x81, 2, 0x02, 2));
        assert_eq!(CONFIG[2] as usize, CONFIG.len());
        assert_eq!(DEVICE[4], 2, "a CDC device");
        // the control interface's own alternate 0 is fine; another is not
        assert_eq!(d.setup(&[1, 11, 0, 0, 0, 0, 0, 0]), Reply::Ack);
        assert_eq!(d.control_in(true), Reply::Data(vec![]));
        assert_eq!(d.setup(&[1, 11, 1, 0, 0, 0, 0, 0]), Reply::Stall);
        assert!(d.up());
    }

    #[test]
    fn frames_cross_both_ways_with_the_filter_and_the_packet_rules() {
        let mut d = EthDevice::new();
        assert_eq!(d.bulk_out(2, &[0; 42]), Reply::Stall, "nothing before the interface is up");
        enumerate(&mut d);
        // an ARP request: one short packet is a frame
        let mut arp = vec![0xff; 6];
        arp.extend(MAC);
        arp.extend([0x08, 0x06]);
        arp.extend([0; 28]);
        assert_eq!(d.bulk_out(2, &arp), Reply::Ack);
        assert_eq!(d.drain_tx(), vec![arp.clone()]);
        // an IPv4 frame of exactly two packets: complete by its total length, no empty packet needed
        let mut ip = vec![0u8; 128];
        ip[12..14].copy_from_slice(&[8, 0]);
        ip[14] = 0x45;
        ip[16..18].copy_from_slice(&114u16.to_be_bytes());
        assert_eq!(d.bulk_out(2, &ip[..64]), Reply::Ack);
        assert!(d.drain_tx().is_empty(), "the frame continues");
        assert_eq!(d.bulk_out(2, &ip[64..]), Reply::Ack);
        assert_eq!(d.drain_tx(), vec![ip.clone()]);
        // an unknown ethertype of two full packets waits for a short one
        let mut other = vec![0u8; 128];
        other[12..14].copy_from_slice(&[0x86, 0xdd]);
        assert_eq!(d.bulk_out(2, &other[..64]), Reply::Ack);
        assert_eq!(d.bulk_out(2, &other[64..]), Reply::Ack);
        assert!(d.drain_tx().is_empty());
        assert_eq!(d.bulk_out(2, &[]), Reply::Ack);
        assert_eq!(d.drain_tx(), vec![other]);
        assert_eq!(d.frames_out, 3);
        // nothing for the host: NAK. Then a 64-byte frame: one full packet and an empty one
        assert_eq!(d.bulk_in(1), Reply::Nak);
        let mut to_us = MAC.to_vec();
        to_us.extend([0xaa; 58]);
        d.push_rx(to_us.clone());
        assert_eq!(d.bulk_in(1), Reply::Data(to_us));
        assert_eq!(d.bulk_in(1), Reply::Data(vec![]), "a whole number of packets ends with an empty one");
        assert_eq!(d.bulk_in(1), Reply::Nak);
        // a 100-byte broadcast: 64 then 36
        let mut bcast = vec![0xff; 6];
        bcast.extend([0x55; 94]);
        d.push_rx(bcast);
        assert!(matches!(d.bulk_in(1), Reply::Data(p) if p.len() == 64 && p[..6] == [0xff; 6]));
        assert!(matches!(d.bulk_in(1), Reply::Data(p) if p.len() == 36));
        assert_eq!(d.bulk_in(1), Reply::Nak);
        // the filter: modd's 0x0d has the promiscuous bit, so a frame for someone else passes;
        // without it, it does not, and multicast needs the "all multicast" bit
        let mut other_mac = vec![0x02, 0, 0, 0, 0, 0x99];
        other_mac.extend([0; 54]);
        d.push_rx(other_mac.clone());
        assert_eq!(d.bulk_in(1), Reply::Data(other_mac.clone()));
        d.filter = 0x0c;
        d.push_rx(other_mac);
        assert_eq!(d.bulk_in(1), Reply::Nak);
        let mut mcast = vec![0x01, 0x00, 0x5e, 0, 0, 1];
        mcast.extend([0; 54]);
        d.push_rx(mcast.clone());
        assert_eq!(d.bulk_in(1), Reply::Nak);
        d.filter = 0x0e;
        d.push_rx(mcast.clone());
        assert_eq!(d.bulk_in(1), Reply::Data(mcast));
        assert_eq!(d.frames_in, 4);
        // the wrong endpoint, a frame too long
        assert_eq!(d.bulk_in(2), Reply::Stall);
        assert_eq!(d.bulk_out(1, &[0; 10]), Reply::Stall);
        let mut long = vec![0xff; 6];
        long.resize(FRAME_MAX + 1, 0);
        d.push_rx(long);
        assert_eq!(d.bulk_in(1), Reply::Nak, "an oversize frame is dropped");
    }
}
