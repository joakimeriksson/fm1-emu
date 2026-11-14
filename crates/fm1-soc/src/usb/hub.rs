// SPDX-License-Identifier: MIT
//! A virtual full-speed USB hub on the FM-1's host port: what the dock is, with the flash drive
//! and the network adapter behind it. USB 2.0 chapter 11 as a full-speed host sees it: the hub
//! descriptor, per-port power, status and change bits, port reset, and the device at the default
//! address behind the port last reset until the host addresses it. A new implementation from the
//! specification; no code copied. Transactions reach the children by address (`route`): at full
//! speed the hub repeats them as they are.
use super::{Peer, Reply};

const PORT_MAX: usize = 7;

const DEVICE: &[u8] = &[18, 1, 0, 2, 9, 0, 0, 64, 0x09, 0x12, 0x02, 0x00, 1, 0, 1, 2, 0, 1];
const CONFIG: &[u8] = &[
    9, 2, 25, 0, 1, 1, 0, 0xe0, 0,
    9, 4, 0, 0, 1, 0x09, 0x00, 0x00, 0,
    7, 5, 0x81, 3, 1, 0, 0xff,
];
// wPortStatus
const PS_CONNECTION: u16 = 0x0001;
const PS_ENABLE: u16 = 0x0002;
const PS_POWER: u16 = 0x0100;
// features
const PORT_ENABLE: u16 = 1;
const PORT_SUSPEND: u16 = 2;
const PORT_RESET: u16 = 4;
const PORT_POWER: u16 = 8;
const C_PORT_CONNECTION: u16 = 16;
const C_PORT_ENABLE: u16 = 17;
const C_PORT_SUSPEND: u16 = 18;
const C_PORT_OVER_CURRENT: u16 = 19;
const C_PORT_RESET: u16 = 20;

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
    Port(u8, u16, bool),
}

#[derive(Default, Clone, Copy)]
struct Port {
    powered: bool,
    enabled: bool,
    c_connection: bool,
    c_enable: bool,
    c_reset: bool,
}

pub(super) struct HubDevice {
    pub(super) address: u8,
    configuration: u8,
    control: Control,
    ports: Vec<Port>,
    pub(super) children: Vec<Peer>,
    /// The port whose device sits at the default address: the last one reset, until the
    /// host gives that device an address.
    default_port: Option<usize>,
}

impl HubDevice {
    pub(super) fn new(children: Vec<Peer>) -> Self {
        assert!((1..=PORT_MAX).contains(&children.len()));
        Self { address: 0, configuration: 0, control: Control::Idle, ports: vec![Port::default(); children.len()],
            children, default_port: None }
    }

    pub(super) fn reset(&mut self) {
        self.address = 0;
        self.configuration = 0;
        self.control = Control::Idle;
        self.ports = vec![Port::default(); self.children.len()];
        self.default_port = None;
        for c in &mut self.children {
            c.reset();
        }
    }

    /// Whom a transaction to `address` reaches: the hub, a child at that address, or, at the
    /// default address, the device behind the port last reset (the hub itself before it has
    /// an address).
    pub(super) fn route(&mut self, address: u8) -> Option<&mut Peer> {
        if address == self.address && self.address != 0 {
            return None; // (the caller handles the hub itself)
        }
        if let Some(i) = self.children.iter().position(|c| c.address() == address && address != 0) {
            return self.children.get_mut(i);
        }
        if address == 0 && self.address != 0 {
            if let Some(i) = self.default_port.filter(|&i| self.ports[i].enabled && self.children[i].address() == 0) {
                return self.children.get_mut(i);
            }
        }
        None
    }

    pub(super) fn is_me(&self, address: u8) -> bool {
        address == self.address && (address != 0 || self.default_port.is_none())
    }

    fn port_status(&self, i: usize) -> u32 {
        let p = &self.ports[i];
        if !p.powered {
            return 0;
        }
        let status = PS_POWER | PS_CONNECTION | if p.enabled { PS_ENABLE } else { 0 };
        let change = u16::from(p.c_connection) * PS_CONNECTION | u16::from(p.c_enable) * PS_ENABLE
            | u16::from(p.c_reset) * 0x0010;
        status as u32 | (change as u32) << 16
    }

    fn hub_descriptor(&self) -> Vec<u8> {
        // per-port power switching, not a compound device, global over-current; 100 ms to
        // power good; every port's device removable; power control on all ports
        vec![9, 0x29, self.ports.len() as u8, 0x01, 0x00, 50, 0, 0x00, 0xff]
    }

    pub(super) fn setup(&mut self, packet: &[u8]) -> Reply {
        self.control = Control::Stalled;
        if packet.len() != 8 { return Reply::Stall; }
        let value = u16::from_le_bytes([packet[2], packet[3]]);
        let index = u16::from_le_bytes([packet[4], packet[5]]);
        let length = u16::from_le_bytes([packet[6], packet[7]]) as usize;
        let port = index as usize;
        let mut data = Vec::new();
        let effect = match (packet[0], packet[1]) {
            (0x80, 6) => {
                data = match (value >> 8, value & 255) {
                    (1, 0) if index == 0 => DEVICE.to_vec(),
                    (2, 0) if index == 0 => CONFIG.to_vec(),
                    (3, 0) if index == 0 => vec![4, 3, 9, 4],
                    (3, n @ 1..=2) if index == 0x409 => {
                        let s = if n == 1 { "FM1 emulator" } else { "FM1 virtual USB hub (the dock)" };
                        let mut d = vec![0, 3];
                        for c in s.encode_utf16() { d.extend(c.to_le_bytes()); }
                        d[0] = d.len() as u8;
                        d
                    }
                    _ => return Reply::Stall,
                };
                Effect::None
            }
            (0, 5) if value <= 127 && index == 0 && length == 0 && self.configuration == 0 => Effect::Address(value as u8),
            (0, 9) if value <= 1 && index == 0 && length == 0 && self.address != 0 => Effect::Configuration(value as u8),
            (0x80, 8) if value == 0 && index == 0 && length == 1 => { data.push(self.configuration); Effect::None }
            (0x80, 0) if value == 0 && index == 0 && length == 2 => { data.extend([1, 0]); Effect::None } // self-powered
            (0x81, 0) if self.configuration == 1 && value == 0 && index == 0 && length == 2 => { data.extend([0, 0]); Effect::None }
            (0x82, 0) if self.configuration == 1 && value == 0 && index == 0x81 && length == 2 => { data.extend([0, 0]); Effect::None }
            // the hub class: its descriptor, its status, and the ports' status and features
            (0xa0, 6) if value >> 8 == 0x29 || value >> 8 == 0 => { data = self.hub_descriptor(); Effect::None }
            (0xa0, 0) if value == 0 && index == 0 && length == 4 => { data.extend([0, 0, 0, 0]); Effect::None }
            (0xa3, 0) if value == 0 && (1..=self.ports.len()).contains(&port) && length == 4 => {
                data.extend(self.port_status(port - 1).to_le_bytes()); Effect::None
            }
            (0x23, 3) if (1..=self.ports.len()).contains(&port) && length == 0 => Effect::Port(port as u8 - 1, value, true),
            (0x23, 1) if (1..=self.ports.len()).contains(&port) && length == 0 => Effect::Port(port as u8 - 1, value, false),
            (0x20, 3 | 1) if index == 0 && length == 0 => Effect::None, // hub features: nothing to do
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

    /// SET_FEATURE / CLEAR_FEATURE on port `i`: false when the feature is not one a port has.
    fn port_feature(&mut self, i: usize, feature: u16, set: bool) -> bool {
        let p = &mut self.ports[i];
        match (feature, set) {
            (PORT_POWER, true) => {
                if !p.powered {
                    // the device on the port shows up as the power comes
                    p.powered = true;
                    p.c_connection = true;
                }
            }
            (PORT_POWER, false) => {
                *p = Port::default();
                self.children[i].reset();
                if self.default_port == Some(i) { self.default_port = None; }
            }
            (PORT_RESET, true) => {
                if !p.powered { return false; }
                // the reset is over at once: the device enabled, at the default address
                p.enabled = true;
                p.c_reset = true;
                self.children[i].reset();
                self.default_port = Some(i);
            }
            (PORT_ENABLE, false) => {
                if p.enabled { p.enabled = false; p.c_enable = true; }
            }
            (PORT_SUSPEND, _) | (C_PORT_SUSPEND, false) | (C_PORT_OVER_CURRENT, false) => {}
            (C_PORT_CONNECTION, false) => p.c_connection = false,
            (C_PORT_ENABLE, false) => p.c_enable = false,
            (C_PORT_RESET, false) => p.c_reset = false,
            _ => return false,
        }
        true
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
                    Effect::Configuration(c) => self.configuration = c,
                    Effect::Port(i, feature, set) => {
                        if !self.port_feature(i as usize, feature, set) {
                            self.control = Control::Stalled;
                            return Reply::Stall;
                        }
                    }
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

    /// The status-change endpoint: a bit per port with a change pending (bit 0 is the hub's).
    pub(super) fn bulk_in(&mut self, ep: u8) -> Reply {
        if ep != 1 || self.configuration != 1 { return Reply::Stall; }
        let bits: u8 = self.ports.iter().enumerate()
            .filter(|(_, p)| p.c_connection || p.c_enable || p.c_reset)
            .fold(0, |b, (i, _)| b | 1 << (i + 1));
        if bits == 0 { Reply::Nak } else { Reply::Data(vec![bits]) }
    }

    pub(super) fn bulk_out(&mut self, _ep: u8, _data: &[u8]) -> Reply {
        Reply::Stall
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb::ethernet::EthDevice;

    fn control(h: &mut HubDevice, setup: [u8; 8]) -> Vec<u8> {
        assert_eq!(h.setup(&setup), Reply::Ack, "{setup:?}");
        if setup[0] & 0x80 != 0 && setup[6..] != [0, 0] {
            let Reply::Data(d) = h.control_in(false) else { panic!("{setup:?}") };
            assert_eq!(h.control_out(&[], true), Reply::Ack);
            d
        } else {
            assert_eq!(h.control_in(true), Reply::Data(vec![]), "{setup:?}");
            vec![]
        }
    }

    #[test]
    fn ports_power_up_show_their_devices_and_hand_the_default_address_to_the_one_reset() {
        let mut h = HubDevice::new(vec![Peer::Ethernet(EthDevice::new()), Peer::Ethernet(EthDevice::new())]);
        assert!(h.is_me(0), "unaddressed: the hub answers at 0");
        control(&mut h, [0, 5, 1, 0, 0, 0, 0, 0]);
        assert_eq!(h.address, 1);
        control(&mut h, [0, 9, 1, 0, 0, 0, 0, 0]);
        let d = control(&mut h, [0xa0, 6, 0, 0x29, 0, 0, 7, 0]);
        assert_eq!(&d[..3], &[9, 0x29, 2], "a two-port hub");
        assert_eq!(d[3] & 3, 1, "per-port power switching");
        assert_eq!(control(&mut h, [0xa3, 0, 0, 0, 1, 0, 4, 0]), vec![0, 0, 0, 0], "unpowered: nothing there");
        assert_eq!(h.bulk_in(1), Reply::Nak);
        for port in 1..=2u8 {
            control(&mut h, [0x23, 3, 8, 0, port, 0, 0, 0]); // PORT_POWER
        }
        let st = u32::from_le_bytes(control(&mut h, [0xa3, 0, 0, 0, 1, 0, 4, 0]).try_into().unwrap());
        assert_eq!(st, 0x0001_0101, "powered, connected, connection changed; full speed");
        assert_eq!(h.bulk_in(1), Reply::Data(vec![0b110]), "both ports changed");
        control(&mut h, [0x23, 1, 16, 0, 1, 0, 0, 0]); // C_PORT_CONNECTION
        assert_eq!(u32::from_le_bytes(control(&mut h, [0xa3, 0, 0, 0, 1, 0, 4, 0]).try_into().unwrap()), 0x0101);
        // reset port 1: enabled, the reset change set, its device at the default address
        control(&mut h, [0x23, 3, 4, 0, 1, 0, 0, 0]);
        let st = u32::from_le_bytes(control(&mut h, [0xa3, 0, 0, 0, 1, 0, 4, 0]).try_into().unwrap());
        assert_eq!(st, 0x0010_0103, "enabled, reset done");
        control(&mut h, [0x23, 1, 20, 0, 1, 0, 0, 0]); // C_PORT_RESET
        assert!(!h.is_me(0) && h.route(0).is_some(), "address 0 is port 1's device now");
        assert!(h.is_me(1) && h.route(1).is_none());
        assert!(h.route(2).is_none(), "nobody at 2 yet");
        // the host addresses it; then port 2's device gets its turn at 0
        let child = h.route(0).unwrap();
        assert_eq!(child.setup(&[0, 5, 2, 0, 0, 0, 0, 0]), Reply::Ack);
        child.control_in(true);
        assert_eq!(child.address(), 2);
        assert!(h.route(0).is_none(), "nobody at the default address");
        assert!(h.route(2).is_some());
        control(&mut h, [0x23, 3, 4, 0, 2, 0, 0, 0]);
        assert!(h.route(0).is_some() && h.route(2).is_some());
        // power off port 2: its device is gone from the bus and reset
        control(&mut h, [0x23, 1, 8, 0, 2, 0, 0, 0]);
        assert!(h.route(0).is_none());
        assert_eq!(control(&mut h, [0xa3, 0, 0, 0, 2, 0, 4, 0]), vec![0, 0, 0, 0]);
        // a port that is not there, a feature that is not a port's
        assert_eq!(h.setup(&[0xa3, 0, 0, 0, 3, 0, 4, 0]), Reply::Stall);
        assert_eq!(h.setup(&[0x23, 3, 99, 0, 1, 0, 0, 0]), Reply::Ack);
        assert_eq!(h.control_in(true), Reply::Stall);
    }
}
