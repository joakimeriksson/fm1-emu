// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Joakim Eriksson (@joakimeriksson), Alice (@aliceisjustplaying) and Mattia Fiumara (@mfiumara)
// Adapted from esp32sim esp-soc/src/net.rs (mikroverk/esp32sim, MIT).

//! Virtual network subnet for emulated WiFi: ARP, DHCP, DNS, SNTP, ICMP.
//!
//! Adapted from esp32sim's net.rs (MIT). Provides a 10.0.2.0/24 subnet
//! (same as QEMU's user-mode networking / Android's SLIRP):
//!   10.0.2.2   gateway + DHCP server (MAC 02:53:49:4d:00:02)
//!   10.0.2.3   DNS resolver
//!   10.0.2.15  the emulated station
//!
//! The station sends Ethernet frames in; this module answers ARP, DHCP, DNS,
//! SNTP, and ICMP locally. TCP/UDP to external hosts are forwarded to the
//! optional NAT backend (nat.rs for native, WebSocket for WASM).

use std::collections::HashMap;
#[cfg(not(target_arch = "wasm32"))]
use std::time::SystemTime;

use crate::nat::HostAddr;

/// The virtual access point both WiFi flavours offer (--wifi-ssid, --wifi-password): WPA2,
/// the SSID at most 32 bytes, the passphrase at most 63. (WPA2 wants 8 at least; the default
/// "fm1pass" predates the check and the emulated AP takes a short one, so the floor is 1.)
#[derive(Clone, Debug, PartialEq)]
pub struct Ap {
    pub ssid: String,
    pub password: String,
}

impl Default for Ap {
    fn default() -> Self {
        Self { ssid: "Home".into(), password: "fm1pass".into() }
    }
}

impl Ap {
    /// Why `ssid` and `password` would not do for a WPA2 network, if they would not.
    pub fn check(ssid: &str, password: &str) -> Result<(), String> {
        if ssid.is_empty() || ssid.len() > 32 {
            return Err("the SSID is 1 to 32 bytes".into());
        }
        if !(1..=63).contains(&password.len()) || !password.is_ascii() {
            return Err("a WPA2 passphrase is 1 to 63 ASCII characters (8 or more on a real network)".into());
        }
        Ok(())
    }
}

/// A link the station reaches the subnet over (the WiFi chip, a USB network adapter). The
/// SoC pumps Ethernet frames between every link and the `VirtualNet` (`Soc::net_pump`).
pub trait Link {
    /// The Ethernet frames the station sent since the last call.
    fn drain_tx(&mut self) -> Vec<Vec<u8>>;
    /// An Ethernet frame for the station.
    fn push_rx(&mut self, eth: Vec<u8>);
}

/// The first and last host octets the DNS hands out for names (10.0.2.100 .. 10.0.2.254).
const DNS_POOL: (u8, u8) = (100, 254);

/// FM1_NET_TRACE=1: what the virtual subnet and the NAT do, on stderr (never in the browser).
pub fn trace_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("FM1_NET_TRACE").is_some())
}

/// The names the station asked the DNS for, each behind an address of its own in the
/// virtual subnet: the station connects to the address, the NAT's host side resolves the
/// name again, for real (see `VirtualNet::host_addr`).
#[derive(Default)]
pub struct DnsMap {
    ip_to_name: HashMap<[u8; 4], String>,
    name_to_ip: HashMap<String, [u8; 4]>,
    /// The host octet the next new name gets; past the pool's end the oldest is taken over.
    next: u8,
}

impl DnsMap {
    /// The address for `name`: the one it has, or the next free one (the pool wraps:
    /// 155 names on, the oldest loses its address).
    pub fn resolve(&mut self, name: &str) -> [u8; 4] {
        let name = name.to_ascii_lowercase();
        if let Some(&ip) = self.name_to_ip.get(&name) {
            return ip;
        }
        let octet = if (DNS_POOL.0..=DNS_POOL.1).contains(&self.next) { self.next } else { DNS_POOL.0 };
        let ip = [10, 0, 2, octet];
        if let Some(old) = self.ip_to_name.remove(&ip) {
            self.name_to_ip.remove(&old);
        }
        self.ip_to_name.insert(ip, name.clone());
        self.name_to_ip.insert(name, ip);
        self.next = if octet >= DNS_POOL.1 { DNS_POOL.0 } else { octet + 1 };
        ip
    }

    /// The name behind `ip`, if the DNS handed it out.
    pub fn lookup(&self, ip: &[u8; 4]) -> Option<&str> {
        self.ip_to_name.get(ip).map(String::as_str)
    }
}

/// The name a DNS query asks about (its first question), dotted.
fn dns_question_name(data: &[u8]) -> Option<String> {
    let mut i = 12;
    let mut labels = Vec::new();
    while i < data.len() && data[i] != 0 {
        let len = data[i] as usize;
        let label = data.get(i + 1..i + 1 + len)?;
        labels.push(std::str::from_utf8(label).ok()?);
        i += 1 + len;
    }
    (!labels.is_empty()).then(|| labels.join("."))
}

pub const GW_MAC: [u8; 6] = [0x02, 0x53, 0x49, 0x4d, 0x00, 0x02];
pub const STA_MAC: [u8; 6] = [0x02, 0x53, 0x49, 0x4d, 0x00, 0x15];

const GW_IP: [u8; 4] = [10, 0, 2, 2];
const DNS_IP: [u8; 4] = [10, 0, 2, 3];
pub const STA_IP: [u8; 4] = [10, 0, 2, 15];
const MASK: [u8; 4] = [255, 255, 255, 0];

const DHCP_LEASE: u32 = 86400;

fn be16(b: &[u8]) -> u16 {
    ((b[0] as u16) << 8) | b[1] as u16
}

fn put_be16(b: &mut [u8], v: u16) {
    b[0] = (v >> 8) as u8;
    b[1] = v as u8;
}

fn put_be32(b: &mut [u8], v: u32) {
    b[0] = (v >> 24) as u8;
    b[1] = (v >> 16) as u8;
    b[2] = (v >> 8) as u8;
    b[3] = v as u8;
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += be16(&data[i..]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !sum as u16
}

pub struct VirtualNet {
    epoch: Option<u64>,
    /// The wall clock (unix seconds) the SNTP answers from, set by the host before the first
    /// query: the browser build has no clock of its own.
    pub epoch_hint: Option<u64>,
    pub stats: NetStats,
    pub nat: Option<crate::nat::Nat>,
    /// The names behind the DNS's answers.
    pub dns_map: DnsMap,
}

#[derive(Default)]
pub struct NetStats {
    pub dhcp_acks: u32,
    pub dns_answers: u32,
    pub ntp_responses: u32,
    pub tcp_rejects: u32,
    pub arp_replies: u32,
    pub pings: u32,
}

impl Default for VirtualNet {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualNet {
    pub fn new() -> Self {
        Self {
            epoch: None,
            epoch_hint: None,
            stats: NetStats::default(),
            nat: None,
            dns_map: DnsMap::default(),
        }
    }

    /// The real network behind the subnet (off unless asked: a firmware in the emulator
    /// reaches nothing outside it by default). `allow_lan`: this computer's own addresses
    /// and its LAN too; without it the NAT refuses loopback, link-local and private ranges.
    pub fn enable_nat(&mut self, allow_lan: bool) {
        self.nat = Some(crate::nat::Nat::platform(allow_lan));
    }

    /// The real network behind the subnet, through `net`.
    pub fn enable_nat_with(&mut self, net: Box<dyn crate::nat::HostNet>) {
        self.nat = Some(crate::nat::Nat::with_backend(net));
    }

    /// Where a packet to `dst` goes on the host side: the name behind one of our DNS
    /// answers, or the address itself. None inside our own subnet: nothing is there.
    fn host_addr(&self, dst: [u8; 4]) -> Option<HostAddr> {
        if let Some(name) = self.dns_map.lookup(&dst) {
            return Some(HostAddr::Name(name.to_string()));
        }
        if dst[..3] == GW_IP[..3] {
            return None;
        }
        Some(HostAddr::Ip(dst))
    }

    /// Poll the NAT's connections for incoming data at `now_ms` (the emulated clock, in
    /// milliseconds: the NAT's timeouts run on it). Returns response frames.
    pub fn poll(&mut self, now_ms: u64) -> Vec<Vec<u8>> {
        if let Some(ref mut nat) = self.nat {
            nat.set_time(now_ms);
            nat.poll()
        } else {
            vec![]
        }
    }

    /// Process an Ethernet frame from the station. Returns response frames (may be empty).
    pub fn handle(&mut self, frame: &[u8]) -> Vec<Vec<u8>> {
        if frame.len() < 14 {
            return vec![];
        }
        let ethertype = be16(&frame[12..14]);
        match ethertype {
            0x0806 => self.arp(frame),
            0x0800 => self.ipv4(frame),
            _ => vec![],
        }
    }

    fn frame(&self, payload: &[u8], ethertype: u16) -> Vec<u8> {
        let mut f = Vec::with_capacity(14 + payload.len());
        f.extend_from_slice(&STA_MAC);
        f.extend_from_slice(&GW_MAC);
        f.push((ethertype >> 8) as u8);
        f.push(ethertype as u8);
        f.extend_from_slice(payload);
        f
    }

    fn arp(&mut self, frame: &[u8]) -> Vec<Vec<u8>> {
        if frame.len() < 42 {
            return vec![];
        }
        let op = be16(&frame[20..22]);
        if op != 1 {
            return vec![];
        }
        let target_ip = &frame[38..42];
        if target_ip == STA_IP {
            return vec![];
        }
        self.stats.arp_replies += 1;
        let mut reply = vec![0u8; 28];
        put_be16(&mut reply[0..], 0x0001); // hardware: ethernet
        put_be16(&mut reply[2..], 0x0800); // protocol: ipv4
        reply[4] = 6; // hw addr len
        reply[5] = 4; // proto addr len
        put_be16(&mut reply[6..], 2); // op: reply
        reply[8..14].copy_from_slice(&GW_MAC);
        reply[14..18].copy_from_slice(target_ip);
        reply[18..24].copy_from_slice(&frame[22..28]); // sender hw+ip
        reply[24..28].copy_from_slice(&frame[28..32]);
        vec![self.frame(&reply, 0x0806)]
    }

    fn ipv4(&mut self, frame: &[u8]) -> Vec<Vec<u8>> {
        let ip = &frame[14..];
        if ip.len() < 20 {
            return vec![];
        }
        let ihl = (ip[0] & 0xf) as usize * 4;
        if ip.len() < ihl {
            return vec![];
        }
        let total = be16(&ip[2..4]) as usize;
        if ip.len() < total || total < ihl {
            return vec![];
        }
        // reject fragments
        let frag = be16(&ip[6..8]);
        if frag & 0x3fff != 0 {
            return vec![];
        }
        let proto = ip[9];
        let payload = &ip[ihl..total];
        match proto {
            1 => self.icmp(ip, ihl, total),
            17 => self.udp(ip, ihl, payload),
            6 => self.tcp(ip, payload),
            _ => vec![],
        }
    }

    fn icmp(&mut self, ip: &[u8], ihl: usize, total: usize) -> Vec<Vec<u8>> {
        let icmp = &ip[ihl..total];
        if icmp.len() < 8 || icmp[0] != 8 {
            return vec![];
        }
        self.stats.pings += 1;
        let mut reply = ip[..total].to_vec();
        // swap src/dst IP
        let src: [u8; 4] = reply[12..16].try_into().unwrap();
        let dst: [u8; 4] = reply[16..20].try_into().unwrap();
        reply[12..16].copy_from_slice(&dst);
        reply[16..20].copy_from_slice(&src);
        // echo reply
        reply[ihl] = 0;
        // recalc ICMP checksum
        reply[ihl + 2] = 0;
        reply[ihl + 3] = 0;
        let ck = checksum(&reply[ihl..total]);
        put_be16(&mut reply[ihl + 2..], ck);
        // recalc IP checksum
        reply[10] = 0;
        reply[11] = 0;
        let ck = checksum(&reply[..ihl]);
        put_be16(&mut reply[10..], ck);
        vec![self.frame(&reply, 0x0800)]
    }

    fn udp(&mut self, ip: &[u8], _ihl: usize, payload: &[u8]) -> Vec<Vec<u8>> {
        if payload.len() < 8 {
            return vec![];
        }
        let src_port = be16(&payload[0..2]);
        let dst_port = be16(&payload[2..4]);
        let udp_data = &payload[8..];
        match dst_port {
            67 => self.dhcp(ip, src_port, udp_data),
            53 => self.dns(ip, src_port, udp_data),
            123 => self.sntp(ip, src_port, udp_data),
            _ => {
                let to = self.host_addr(ip[16..20].try_into().unwrap());
                match (to, self.nat.as_mut()) {
                    (Some(to), Some(nat)) => nat.udp_out(ip, payload, &to),
                    _ => vec![],
                }
            }
        }
    }

    fn ip_udp_frame(&self, src_ip: [u8; 4], dst_ip: [u8; 4], src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
        let udp_len = 8 + data.len();
        let ip_len = 20 + udp_len;
        let mut pkt = vec![0u8; ip_len];
        // IP header
        pkt[0] = 0x45;
        put_be16(&mut pkt[2..], ip_len as u16);
        pkt[8] = 64; // TTL
        pkt[9] = 17; // UDP
        pkt[12..16].copy_from_slice(&src_ip);
        pkt[16..20].copy_from_slice(&dst_ip);
        let ck = checksum(&pkt[..20]);
        put_be16(&mut pkt[10..], ck);
        // UDP header
        put_be16(&mut pkt[20..], src_port);
        put_be16(&mut pkt[22..], dst_port);
        put_be16(&mut pkt[24..], udp_len as u16);
        pkt[28..28 + data.len()].copy_from_slice(data);
        self.frame(&pkt, 0x0800)
    }

    fn dhcp(&mut self, _ip: &[u8], src_port: u16, data: &[u8]) -> Vec<Vec<u8>> {
        if data.len() < 240 {
            return vec![];
        }
        let msg_type = data[0];
        // BOOTP: op=1 is request
        if msg_type != 1 {
            return vec![];
        }
        // find DHCP message type in options (after byte 236, magic cookie 99.130.83.99)
        let opts = &data[240..];
        let mut dhcp_type = 0u8;
        let mut i = 0;
        while i < opts.len() {
            let opt = opts[i];
            if opt == 0xff {
                break;
            }
            if opt == 0 {
                i += 1;
                continue;
            }
            if i + 1 >= opts.len() {
                break;
            }
            let len = opts[i + 1] as usize;
            if opt == 53 && len >= 1 && i + 2 < opts.len() {
                dhcp_type = opts[i + 2];
            }
            i += 2 + len;
        }
        // 1=DISCOVER, 3=REQUEST
        let reply_type = match dhcp_type {
            1 => 2u8, // OFFER
            3 => 5u8, // ACK
            _ => return vec![],
        };
        self.stats.dhcp_acks += 1;
        let mut reply = vec![0u8; 300];
        reply[0] = 2; // op: reply
        reply[1] = 1; // htype: ethernet
        reply[2] = 6; // hlen
        reply[4..8].copy_from_slice(&data[4..8]); // xid
        reply[16..20].copy_from_slice(&STA_IP); // yiaddr
        reply[20..24].copy_from_slice(&GW_IP); // siaddr
        reply[28..34].copy_from_slice(&data[28..34]); // chaddr
        // magic cookie
        reply[236..240].copy_from_slice(&[99, 130, 83, 99]);
        let mut o = 240;
        // DHCP message type
        reply[o] = 53; reply[o + 1] = 1; reply[o + 2] = reply_type; o += 3;
        // subnet mask
        reply[o] = 1; reply[o + 1] = 4; reply[o + 2..o + 6].copy_from_slice(&MASK); o += 6;
        // router
        reply[o] = 3; reply[o + 1] = 4; reply[o + 2..o + 6].copy_from_slice(&GW_IP); o += 6;
        // DNS
        reply[o] = 6; reply[o + 1] = 4; reply[o + 2..o + 6].copy_from_slice(&DNS_IP); o += 6;
        // lease time
        reply[o] = 51; reply[o + 1] = 4; put_be32(&mut reply[o + 2..], DHCP_LEASE); o += 6;
        // server identifier
        reply[o] = 54; reply[o + 1] = 4; reply[o + 2..o + 6].copy_from_slice(&GW_IP); o += 6;
        // end
        reply[o] = 0xff;
        reply.truncate(o + 1);
        if trace_on() {
            eprintln!("net: dhcp type {dhcp_type} answered: {}.{}.{}.{}", STA_IP[0], STA_IP[1], STA_IP[2], STA_IP[3]);
        }
        vec![self.ip_udp_frame(GW_IP, [255, 255, 255, 255], 67, src_port, &reply)]
    }

    fn dns(&mut self, ip: &[u8], src_port: u16, data: &[u8]) -> Vec<Vec<u8>> {
        if data.len() < 12 {
            return vec![];
        }
        self.stats.dns_answers += 1;
        // the answer goes back to whoever asked (a station need not sit at STA_IP)
        let src_ip: [u8; 4] = ip[12..16].try_into().unwrap();
        let id = &data[0..2];
        // skip over the question section to find the qtype
        let mut i = 12;
        while i < data.len() && data[i] != 0 {
            i += 1 + data[i] as usize;
        }
        i += 1; // null terminator
        let qtype = if i + 2 <= data.len() { be16(&data[i..i + 2]) } else { 1 };
        // AAAA: return empty (skip IPv6)
        if qtype == 28 {
            let mut reply = vec![0u8; 12];
            reply[0..2].copy_from_slice(id);
            put_be16(&mut reply[2..], 0x8180); // response, no error
            put_be16(&mut reply[4..], 1); // qdcount
            // copy question
            reply.extend_from_slice(&data[12..]);
            return vec![self.ip_udp_frame(DNS_IP, src_ip, 53, src_port, &reply)];
        }
        // A record: an address of our own for the name (the NAT connects to the name)
        let answer = match dns_question_name(data) {
            Some(name) => self.dns_map.resolve(&name),
            None => DNS_IP,
        };
        if trace_on() {
            eprintln!("net: dns {:?} -> {}.{}.{}.{}", dns_question_name(data).unwrap_or_default(), answer[0], answer[1], answer[2], answer[3]);
        }
        let mut reply = vec![0u8; 12];
        reply[0..2].copy_from_slice(id);
        put_be16(&mut reply[2..], 0x8180);
        put_be16(&mut reply[4..], 1); // qdcount
        put_be16(&mut reply[6..], 1); // ancount
        // copy question
        reply.extend_from_slice(&data[12..]);
        // answer: pointer to name in question (0xC00C), type A, class IN, TTL 300, rdata
        reply.extend_from_slice(&[0xC0, 0x0C, 0x00, 0x01, 0x00, 0x01]);
        let ttl_pos = reply.len();
        reply.extend_from_slice(&[0, 0, 0, 0]); // TTL placeholder
        put_be32(&mut reply[ttl_pos..], 300);
        reply.extend_from_slice(&[0x00, 0x04]); // rdlength
        reply.extend_from_slice(&answer);
        vec![self.ip_udp_frame(DNS_IP, src_ip, 53, src_port, &reply)]
    }

    fn sntp(&mut self, ip: &[u8], src_port: u16, data: &[u8]) -> Vec<Vec<u8>> {
        if data.len() < 48 {
            return vec![];
        }
        self.stats.ntp_responses += 1;
        let src_ip: [u8; 4] = ip[12..16].try_into().unwrap();
        let hint = self.epoch_hint;
        let now = self.epoch.get_or_insert_with(|| {
            hint.unwrap_or_else(|| {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(1_700_000_000)
                }
                #[cfg(target_arch = "wasm32")]
                {
                    1_700_000_000
                }
            })
        });
        // NTP epoch offset (1900 to 1970)
        let ntp_secs = *now + 2_208_988_800;
        let mut reply = vec![0u8; 48];
        reply[0] = 0x24; // LI=0, VN=4, mode=4 (server)
        reply[1] = 1; // stratum
        reply[2] = 6; // poll
        reply[3] = 0xEC_u8; // precision (-20, ~1us)
        // reference timestamp
        put_be32(&mut reply[16..], ntp_secs as u32);
        // originate timestamp (copy client's transmit)
        reply[24..32].copy_from_slice(&data[40..48]);
        // receive timestamp
        put_be32(&mut reply[32..], ntp_secs as u32);
        // transmit timestamp
        put_be32(&mut reply[40..], ntp_secs as u32);
        vec![self.ip_udp_frame(GW_IP, src_ip, 123, src_port, &reply)]
    }

    fn tcp(&mut self, ip: &[u8], payload: &[u8]) -> Vec<Vec<u8>> {
        let to = self.host_addr(ip[16..20].try_into().unwrap());
        if trace_on() && payload.len() >= 20 && payload[13] & 0x02 != 0 {
            eprintln!("net: tcp syn to {:?}:{} (nat: {})", to, be16(&payload[2..4]), self.nat.is_some());
        }
        match (to, self.nat.as_mut()) {
            (Some(to), Some(nat)) => nat.tcp_in(ip, payload, &to),
            _ => self.tcp_reject(ip, payload),
        }
    }

    fn tcp_reject(&mut self, ip: &[u8], payload: &[u8]) -> Vec<Vec<u8>> {
        if payload.len() < 20 {
            return vec![];
        }
        self.stats.tcp_rejects += 1;
        let src_port = be16(&payload[0..2]);
        let dst_port = be16(&payload[2..4]);
        let seq = u32::from_be_bytes(payload[4..8].try_into().unwrap());
        let data_off = ((payload[12] >> 4) as usize) * 4;
        let seg_len = payload.len().saturating_sub(data_off);
        let flags = payload[13];
        let syn = flags & 0x02 != 0;
        let ack_num = if syn { seq.wrapping_add(1) } else { seq.wrapping_add(seg_len as u32) };

        let ip_len = 20 + 20;
        let mut pkt = vec![0u8; ip_len];
        pkt[0] = 0x45;
        put_be16(&mut pkt[2..], ip_len as u16);
        pkt[8] = 64;
        pkt[9] = 6; // TCP
        pkt[12..16].copy_from_slice(&ip[16..20]); // swap src/dst
        pkt[16..20].copy_from_slice(&ip[12..16]);
        let ck = checksum(&pkt[..20]);
        put_be16(&mut pkt[10..], ck);
        // TCP RST|ACK
        put_be16(&mut pkt[20..], dst_port);
        put_be16(&mut pkt[22..], src_port);
        put_be32(&mut pkt[24..], 0); // seq = 0
        put_be32(&mut pkt[28..], ack_num);
        pkt[32] = 0x50; // data offset = 5 words
        pkt[33] = 0x14; // RST|ACK
        put_be16(&mut pkt[34..], 0); // window
        // TCP checksum (pseudo-header + segment)
        let mut pseudo = Vec::with_capacity(32);
        pseudo.extend_from_slice(&pkt[12..16]);
        pseudo.extend_from_slice(&pkt[16..20]);
        pseudo.push(0);
        pseudo.push(6); // TCP
        pseudo.extend_from_slice(&[0, 20]); // TCP length
        pseudo.extend_from_slice(&pkt[20..40]);
        let ck = checksum(&pseudo);
        put_be16(&mut pkt[36..], ck);
        vec![self.frame(&pkt, 0x0800)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_arp_request(target_ip: [u8; 4]) -> Vec<u8> {
        let mut frame = vec![0u8; 42];
        // dst MAC (broadcast)
        frame[0..6].copy_from_slice(&[0xff; 6]);
        // src MAC (station)
        frame[6..12].copy_from_slice(&STA_MAC);
        // ethertype ARP
        frame[12] = 0x08;
        frame[13] = 0x06;
        // ARP: ethernet, ipv4, hw=6, proto=4, op=request
        put_be16(&mut frame[14..], 0x0001);
        put_be16(&mut frame[16..], 0x0800);
        frame[18] = 6;
        frame[19] = 4;
        put_be16(&mut frame[20..], 1);
        // sender hw+ip
        frame[22..28].copy_from_slice(&STA_MAC);
        frame[28..32].copy_from_slice(&STA_IP);
        // target ip
        frame[38..42].copy_from_slice(&target_ip);
        frame
    }

    #[test]
    fn arp_resolves_gateway() {
        let mut net = VirtualNet::new();
        net.nat = None;
        let frame = make_arp_request(GW_IP);
        let replies = net.handle(&frame);
        assert_eq!(replies.len(), 1);
        assert_eq!(net.stats.arp_replies, 1);
        // reply should contain gateway MAC
        let reply = &replies[0];
        assert_eq!(&reply[6..12], &GW_MAC);
    }

    #[test]
    fn arp_does_not_resolve_self() {
        let mut net = VirtualNet::new();
        net.nat = None;
        let frame = make_arp_request(STA_IP);
        let replies = net.handle(&frame);
        assert!(replies.is_empty());
    }

    fn make_udp_frame(src_ip: [u8; 4], dst_ip: [u8; 4], src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
        let udp_len = 8 + data.len();
        let ip_len = 20 + udp_len;
        let mut frame = vec![0u8; 14 + ip_len];
        // ethernet
        frame[0..6].copy_from_slice(&GW_MAC);
        frame[6..12].copy_from_slice(&STA_MAC);
        frame[12] = 0x08;
        frame[13] = 0x00;
        // IP header
        frame[14] = 0x45;
        put_be16(&mut frame[16..], ip_len as u16);
        frame[22] = 64; // TTL
        frame[23] = 17; // UDP
        frame[26..30].copy_from_slice(&src_ip);
        frame[30..34].copy_from_slice(&dst_ip);
        let ck = checksum(&frame[14..34]);
        put_be16(&mut frame[24..], ck);
        // UDP
        put_be16(&mut frame[34..], src_port);
        put_be16(&mut frame[36..], dst_port);
        put_be16(&mut frame[38..], udp_len as u16);
        frame[42..42 + data.len()].copy_from_slice(data);
        frame
    }

    #[test]
    fn dhcp_discover_gets_offer() {
        let mut net = VirtualNet::new();
        net.nat = None;
        // minimal DHCP DISCOVER
        let mut dhcp = vec![0u8; 244];
        dhcp[0] = 1; // op: request
        dhcp[1] = 1; // htype: ethernet
        dhcp[2] = 6; // hlen
        dhcp[4..8].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]); // xid
        dhcp[28..34].copy_from_slice(&STA_MAC); // chaddr
        // magic cookie
        dhcp[236..240].copy_from_slice(&[99, 130, 83, 99]);
        // option 53: DHCP message type = 1 (DISCOVER)
        dhcp.push(53);
        dhcp.push(1);
        dhcp.push(1);
        dhcp.push(0xff); // end
        let frame = make_udp_frame(STA_IP, [255, 255, 255, 255], 68, 67, &dhcp);
        let replies = net.handle(&frame);
        assert_eq!(replies.len(), 1);
        assert_eq!(net.stats.dhcp_acks, 1);
    }

    /// A minimal DNS query: one question, `name` (dotted), type A, class IN.
    fn dns_a_query(name: &str) -> Vec<u8> {
        let mut dns = vec![0u8; 12];
        dns[0] = 0xAB; dns[1] = 0xCD; // ID
        dns[5] = 1; // qdcount = 1
        for label in name.split('.') {
            dns.push(label.len() as u8);
            dns.extend_from_slice(label.as_bytes());
        }
        dns.push(0); // null terminator
        dns.extend_from_slice(&[0, 1]); // type A
        dns.extend_from_slice(&[0, 1]); // class IN
        dns
    }

    /// The IPv4 destination of an Ethernet frame.
    fn ip_dst(frame: &[u8]) -> [u8; 4] {
        frame[30..34].try_into().unwrap()
    }

    #[test]
    fn dns_a_query_resolves() {
        let mut net = VirtualNet::new();
        net.nat = None;
        let frame = make_udp_frame(STA_IP, DNS_IP, 12345, 53, &dns_a_query("example.com"));
        let replies = net.handle(&frame);
        assert_eq!(replies.len(), 1);
        assert_eq!(net.stats.dns_answers, 1);
        assert_eq!(ip_dst(&replies[0]), STA_IP);
    }

    /// The A record's address in a DNS answer frame (its last four bytes).
    fn dns_answer_ip(frame: &[u8]) -> [u8; 4] {
        frame[frame.len() - 4..].try_into().unwrap()
    }

    /// A TCP segment from the station to `dst`:`dst_port`, `flags`, no payload.
    fn make_tcp_frame(dst: [u8; 4], src_port: u16, dst_port: u16, seq: u32, flags: u8) -> Vec<u8> {
        let mut f = vec![0u8; 14 + 40];
        f[0..6].copy_from_slice(&GW_MAC);
        f[6..12].copy_from_slice(&STA_MAC);
        f[12] = 0x08;
        f[14] = 0x45;
        put_be16(&mut f[16..], 40);
        f[22] = 64;
        f[23] = 6;
        f[26..30].copy_from_slice(&STA_IP);
        f[30..34].copy_from_slice(&dst);
        let ck = checksum(&f[14..34]);
        put_be16(&mut f[24..], ck);
        put_be16(&mut f[34..], src_port);
        put_be16(&mut f[36..], dst_port);
        put_be32(&mut f[38..], seq);
        f[46] = 0x50;
        f[47] = flags;
        put_be16(&mut f[48..], 8192);
        f
    }

    #[test]
    fn dns_gives_each_name_an_address_of_its_own() {
        let mut net = VirtualNet::new();
        net.nat = None;
        let ask = |net: &mut VirtualNet, name: &str| {
            let replies = net.handle(&make_udp_frame(STA_IP, DNS_IP, 12345, 53, &dns_a_query(name)));
            dns_answer_ip(&replies[0])
        };
        assert_eq!(ask(&mut net, "ip-api.com"), [10, 0, 2, 100]);
        assert_eq!(ask(&mut net, "api.open-meteo.com"), [10, 0, 2, 101]);
        assert_eq!(ask(&mut net, "ip-api.com"), [10, 0, 2, 100], "the same name, the same address");
        assert_eq!(ask(&mut net, "IP-API.com"), [10, 0, 2, 100], "names are case-insensitive");
        assert_eq!(net.dns_map.lookup(&[10, 0, 2, 100]), Some("ip-api.com"));
        assert_eq!(net.dns_map.lookup(&[10, 0, 2, 101]), Some("api.open-meteo.com"));
        assert_eq!(net.dns_map.lookup(&[10, 0, 2, 102]), None);
    }

    #[test]
    fn dns_pool_wraps_and_takes_the_oldest_address_over() {
        let mut map = DnsMap::default();
        for n in 0..155u32 {
            assert_eq!(map.resolve(&format!("host{n}.example")), [10, 0, 2, 100 + n as u8]);
        }
        assert_eq!(map.resolve("one.more"), [10, 0, 2, 100], "the pool wraps");
        assert_eq!(map.lookup(&[10, 0, 2, 100]), Some("one.more"));
        assert_eq!(map.resolve("host0.example"), [10, 0, 2, 101], "host0 lost its address; the next is taken");
        assert_eq!(map.lookup(&[10, 0, 2, 101]), Some("host0.example"));
        assert_eq!(map.resolve("host5.example"), [10, 0, 2, 105], "the others keep theirs");
    }

    #[test]
    fn a_syn_to_a_dns_address_connects_to_the_name() {
        let (mock, state) = crate::nat::mock::MockNet::new();
        let mut net = VirtualNet::new();
        net.nat = Some(crate::nat::Nat::with_backend(Box::new(mock)));
        net.handle(&make_udp_frame(STA_IP, DNS_IP, 12345, 53, &dns_a_query("ip-api.com")));
        let replies = net.handle(&make_tcp_frame([10, 0, 2, 100], 40000, 80, 1000, 0x02));
        assert_eq!(replies.len(), 1, "the mock connects at once: a SYN-ACK");
        assert_eq!(replies[0][47] & 0x12, 0x12);
        let s = state.lock().unwrap();
        assert_eq!(s.tcp.len(), 1);
        assert_eq!(s.tcp[0].lock().unwrap().to, (HostAddr::Name("ip-api.com".into()), 80));
    }

    #[test]
    fn a_syn_into_the_subnet_with_no_name_behind_it_is_refused() {
        let (mock, state) = crate::nat::mock::MockNet::new();
        let mut net = VirtualNet::new();
        net.nat = Some(crate::nat::Nat::with_backend(Box::new(mock)));
        let replies = net.handle(&make_tcp_frame([10, 0, 2, 200], 40000, 80, 1000, 0x02));
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0][47] & 0x04, 0x04, "RST");
        assert_eq!(net.stats.tcp_rejects, 1);
        assert!(state.lock().unwrap().tcp.is_empty(), "no host connection");
        // an address outside the subnet connects as itself
        net.handle(&make_tcp_frame([93, 184, 216, 34], 40001, 80, 1000, 0x02));
        assert_eq!(state.lock().unwrap().tcp[0].lock().unwrap().to, (HostAddr::Ip([93, 184, 216, 34]), 80));
    }

    #[test]
    fn dns_reply_goes_to_the_requester() {
        // a station at another address (a static one, or FAKE_IP 192.168.1.42) gets its answer
        let mut net = VirtualNet::new();
        net.nat = None;
        let custom_ip = [192, 168, 1, 42];
        let frame = make_udp_frame(custom_ip, DNS_IP, 12345, 53, &dns_a_query("example.com"));
        let replies = net.handle(&frame);
        assert_eq!(replies.len(), 1);
        assert_eq!(ip_dst(&replies[0]), custom_ip);
    }

    #[test]
    fn sntp_gets_response() {
        let mut net = VirtualNet::new();
        net.nat = None;
        let mut sntp = vec![0u8; 48];
        sntp[0] = 0x23; // LI=0, VN=4, mode=3 (client)
        // set transmit timestamp (bytes 40-47)
        sntp[40] = 0xE8; sntp[41] = 0x01; sntp[42] = 0x02; sntp[43] = 0x03;
        let custom_ip = [192, 168, 1, 42];
        let frame = make_udp_frame(custom_ip, GW_IP, 54321, 123, &sntp);
        let replies = net.handle(&frame);
        assert_eq!(replies.len(), 1);
        assert_eq!(net.stats.ntp_responses, 1);
        assert_eq!(ip_dst(&replies[0]), custom_ip, "the answer goes to the requester");
    }

    #[test]
    fn tcp_without_nat_gets_rst() {
        let mut net = VirtualNet::new();
        net.nat = None; // no NAT: TCP should be rejected
        // build a TCP SYN
        let tcp_len = 20;
        let ip_len = 20 + tcp_len;
        let mut frame = vec![0u8; 14 + ip_len];
        frame[0..6].copy_from_slice(&GW_MAC);
        frame[6..12].copy_from_slice(&STA_MAC);
        frame[12] = 0x08; frame[13] = 0x00;
        frame[14] = 0x45;
        put_be16(&mut frame[16..], ip_len as u16);
        frame[22] = 64;
        frame[23] = 6; // TCP
        frame[26..30].copy_from_slice(&STA_IP);
        frame[30..34].copy_from_slice(&[93, 184, 216, 34]); // example.com
        let ck = checksum(&frame[14..34]);
        put_be16(&mut frame[24..], ck);
        // TCP SYN
        put_be16(&mut frame[34..], 12345); // src port
        put_be16(&mut frame[36..], 80); // dst port
        put_be32(&mut frame[38..], 1000); // seq
        frame[46] = 0x50; // data offset = 5
        frame[47] = 0x02; // SYN
        put_be16(&mut frame[48..], 65535); // window
        let replies = net.handle(&frame);
        assert_eq!(replies.len(), 1, "should get RST back");
        assert_eq!(net.stats.tcp_rejects, 1);
    }

    #[test]
    fn icmp_echo_gets_reply() {
        let mut net = VirtualNet::new();
        net.nat = None;
        let ip_len = 20 + 8;
        let mut frame = vec![0u8; 14 + ip_len];
        frame[0..6].copy_from_slice(&GW_MAC);
        frame[6..12].copy_from_slice(&STA_MAC);
        frame[12] = 0x08; frame[13] = 0x00;
        frame[14] = 0x45;
        put_be16(&mut frame[16..], ip_len as u16);
        frame[22] = 64;
        frame[23] = 1; // ICMP
        frame[26..30].copy_from_slice(&STA_IP);
        frame[30..34].copy_from_slice(&GW_IP);
        let ck = checksum(&frame[14..34]);
        put_be16(&mut frame[24..], ck);
        // ICMP echo request
        frame[34] = 8; // type: echo request
        frame[35] = 0; // code
        // id + seq
        frame[38] = 0x00; frame[39] = 0x01;
        frame[40] = 0x00; frame[41] = 0x01;
        let ck = checksum(&frame[34..42]);
        put_be16(&mut frame[36..], ck);
        let replies = net.handle(&frame);
        assert_eq!(replies.len(), 1);
        assert_eq!(net.stats.pings, 1);
    }
}
