// SPDX-License-Identifier: MIT
// Adapted from esp32sim esp-soc/src/nat.rs (mikroverk/esp32sim, MIT).

//! User-mode NAT: forwards guest TCP/UDP through real host sockets.
//!
//! Adapted from esp32sim's nat.rs (MIT). When the virtual subnet (net.rs)
//! receives a TCP SYN or UDP datagram to an external host, it hands the packet
//! here. The NAT creates a real host socket, relays the data, and constructs
//! response packets back into the virtual subnet's Ethernet frame format.
//!
//! All sockets are non-blocking. `poll()` must be called periodically to
//! drive the connections and produce response frames.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream, UdpSocket};

const MAX_TCP: usize = 32;
const MAX_UDP: usize = 64;
const TCP_WINDOW: u16 = 65535;
const IDLE_TIMEOUT_MS: u64 = 120_000;
const TIME_WAIT_MS: u64 = 30_000;
const CONNECT_TIMEOUT_MS: u64 = 5_000;

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

fn tcp_checksum(src_ip: [u8; 4], dst_ip: [u8; 4], tcp: &[u8]) -> u16 {
    let mut pseudo = Vec::with_capacity(12 + tcp.len());
    pseudo.extend_from_slice(&src_ip);
    pseudo.extend_from_slice(&dst_ip);
    pseudo.push(0);
    pseudo.push(6);
    pseudo.push((tcp.len() >> 8) as u8);
    pseudo.push(tcp.len() as u8);
    pseudo.extend_from_slice(tcp);
    checksum(&pseudo)
}

const GW_MAC: [u8; 6] = [0x02, 0x53, 0x49, 0x4d, 0x00, 0x02];
const STA_MAC: [u8; 6] = [0x02, 0x53, 0x49, 0x4d, 0x00, 0x15];

#[derive(Clone, Copy, PartialEq)]
enum TcpState {
    Connecting,
    Connected,
    TimeWait(u64),
    Closed,
}

struct TcpConn {
    guest_ip: [u8; 4],
    guest_port: u16,
    host_ip: [u8; 4],
    host_port: u16,
    stream: Option<TcpStream>,
    state: TcpState,
    guest_seq: u32,
    guest_ack: u32,
    our_seq: u32,
    last_active: u64,
    connect_start: u64,
    rx_buf: Vec<u8>,
}

struct UdpFlow {
    guest_ip: [u8; 4],
    guest_port: u16,
    host_ip: [u8; 4],
    host_port: u16,
    socket: UdpSocket,
    last_active: u64,
}

pub struct Nat {
    tcp: Vec<TcpConn>,
    udp: Vec<UdpFlow>,
    now_ms: u64,
    pub stats: NatStats,
}

#[derive(Default)]
pub struct NatStats {
    pub tcp_connects: u32,
    pub tcp_bytes_in: u64,
    pub tcp_bytes_out: u64,
    pub udp_packets: u32,
}

impl Nat {
    pub fn new() -> Self {
        Self {
            tcp: Vec::new(),
            udp: Vec::new(),
            now_ms: 0,
            stats: NatStats::default(),
        }
    }

    pub fn set_time(&mut self, ms: u64) {
        self.now_ms = ms;
    }

    /// Handle a TCP segment from the guest. Returns response Ethernet frames.
    pub fn tcp_in(&mut self, ip: &[u8], ihl: usize, tcp: &[u8]) -> Vec<Vec<u8>> {
        if tcp.len() < 20 {
            return vec![];
        }
        let src_port = be16(&tcp[0..2]);
        let dst_port = be16(&tcp[2..4]);
        let seq = u32::from_be_bytes(tcp[4..8].try_into().unwrap());
        let ack = u32::from_be_bytes(tcp[8..12].try_into().unwrap());
        let data_off = ((tcp[12] >> 4) as usize) * 4;
        let flags = tcp[13];
        let syn = flags & 0x02 != 0;
        let fin = flags & 0x01 != 0;
        let rst = flags & 0x04 != 0;
        let data = if data_off < tcp.len() { &tcp[data_off..] } else { &[] };

        let guest_ip: [u8; 4] = ip[12..16].try_into().unwrap();
        let host_ip: [u8; 4] = ip[16..20].try_into().unwrap();

        // find existing connection
        let idx = self.tcp.iter().position(|c| {
            c.guest_ip == guest_ip
                && c.guest_port == src_port
                && c.host_ip == host_ip
                && c.host_port == dst_port
        });

        if rst {
            if let Some(i) = idx {
                self.tcp[i].state = TcpState::Closed;
            }
            return vec![];
        }

        if syn && idx.is_none() {
            return self.tcp_new(guest_ip, src_port, host_ip, dst_port, seq);
        }

        let Some(i) = idx else { return vec![] };
        self.tcp[i].last_active = self.now_ms;
        self.tcp[i].guest_seq = seq.wrapping_add(data.len() as u32);

        if fin {
            self.tcp[i].guest_seq = self.tcp[i].guest_seq.wrapping_add(1);
            let frames = self.tcp_ack_fin(i);
            self.tcp[i].state = TcpState::TimeWait(self.now_ms);
            return frames;
        }

        if !data.is_empty() && self.tcp[i].state == TcpState::Connected {
            if let Some(ref mut stream) = self.tcp[i].stream {
                let _ = stream.write_all(data);
                self.stats.tcp_bytes_out += data.len() as u64;
            }
            return self.tcp_ack(i);
        }

        // ACK for our SYN-ACK or just a keepalive
        if self.tcp[i].state == TcpState::Connecting && ack != 0 {
            self.tcp[i].state = TcpState::Connected;
            self.tcp[i].guest_ack = ack;
        }

        vec![]
    }

    fn tcp_new(&mut self, guest_ip: [u8; 4], guest_port: u16, host_ip: [u8; 4], host_port: u16, seq: u32) -> Vec<Vec<u8>> {
        if self.tcp.len() >= MAX_TCP {
            // evict oldest closed/timewait
            if let Some(i) = self.tcp.iter().position(|c| matches!(c.state, TcpState::Closed | TcpState::TimeWait(_))) {
                self.tcp.remove(i);
            } else {
                return vec![];
            }
        }

        let addr = SocketAddr::new(
            Ipv4Addr::new(host_ip[0], host_ip[1], host_ip[2], host_ip[3]).into(),
            host_port,
        );

        let stream = match TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(CONNECT_TIMEOUT_MS)) {
            Ok(s) => {
                let _ = s.set_nonblocking(true);
                let _ = s.set_nodelay(true);
                s
            }
            Err(_) => {
                return self.tcp_rst(host_ip, host_port, guest_ip, guest_port, 0, seq.wrapping_add(1));
            }
        };

        self.stats.tcp_connects += 1;
        let our_seq: u32 = (self.now_ms as u32).wrapping_mul(0x9e37_79b9);

        self.tcp.push(TcpConn {
            guest_ip,
            guest_port,
            host_ip,
            host_port,
            stream: Some(stream),
            state: TcpState::Connecting,
            guest_seq: seq.wrapping_add(1),
            guest_ack: 0,
            our_seq: our_seq.wrapping_add(1),
            last_active: self.now_ms,
            connect_start: self.now_ms,
            rx_buf: Vec::new(),
        });

        // SYN-ACK
        self.tcp_syn_ack(self.tcp.len() - 1, our_seq, seq.wrapping_add(1))
    }

    fn eth_frame(&self, payload: &[u8], ethertype: u16) -> Vec<u8> {
        let mut f = Vec::with_capacity(14 + payload.len());
        f.extend_from_slice(&STA_MAC);
        f.extend_from_slice(&GW_MAC);
        f.push((ethertype >> 8) as u8);
        f.push(ethertype as u8);
        f.extend_from_slice(payload);
        f
    }

    fn ip_tcp_frame(&self, src_ip: [u8; 4], dst_ip: [u8; 4], tcp_payload: &[u8]) -> Vec<u8> {
        let ip_len = 20 + tcp_payload.len();
        let mut pkt = vec![0u8; ip_len];
        pkt[0] = 0x45;
        put_be16(&mut pkt[2..], ip_len as u16);
        pkt[8] = 64;
        pkt[9] = 6;
        pkt[12..16].copy_from_slice(&src_ip);
        pkt[16..20].copy_from_slice(&dst_ip);
        let ck = checksum(&pkt[..20]);
        put_be16(&mut pkt[10..], ck);
        pkt[20..].copy_from_slice(tcp_payload);
        // fix TCP checksum
        let tcp_ck = tcp_checksum(src_ip, dst_ip, &pkt[20..]);
        put_be16(&mut pkt[36..], tcp_ck);
        self.eth_frame(&pkt, 0x0800)
    }

    fn tcp_syn_ack(&self, idx: usize, seq: u32, ack: u32) -> Vec<Vec<u8>> {
        let c = &self.tcp[idx];
        let mut tcp = vec![0u8; 20];
        put_be16(&mut tcp[0..], c.host_port);
        put_be16(&mut tcp[2..], c.guest_port);
        put_be32(&mut tcp[4..], seq);
        put_be32(&mut tcp[8..], ack);
        tcp[12] = 0x50;
        tcp[13] = 0x12; // SYN|ACK
        put_be16(&mut tcp[14..], TCP_WINDOW);
        vec![self.ip_tcp_frame(c.host_ip, c.guest_ip, &tcp)]
    }

    fn tcp_ack(&self, idx: usize) -> Vec<Vec<u8>> {
        let c = &self.tcp[idx];
        let mut tcp = vec![0u8; 20];
        put_be16(&mut tcp[0..], c.host_port);
        put_be16(&mut tcp[2..], c.guest_port);
        put_be32(&mut tcp[4..], c.our_seq);
        put_be32(&mut tcp[8..], c.guest_seq);
        tcp[12] = 0x50;
        tcp[13] = 0x10; // ACK
        put_be16(&mut tcp[14..], TCP_WINDOW);
        vec![self.ip_tcp_frame(c.host_ip, c.guest_ip, &tcp)]
    }

    fn tcp_ack_fin(&self, idx: usize) -> Vec<Vec<u8>> {
        let c = &self.tcp[idx];
        let mut tcp = vec![0u8; 20];
        put_be16(&mut tcp[0..], c.host_port);
        put_be16(&mut tcp[2..], c.guest_port);
        put_be32(&mut tcp[4..], c.our_seq);
        put_be32(&mut tcp[8..], c.guest_seq);
        tcp[12] = 0x50;
        tcp[13] = 0x11; // FIN|ACK
        put_be16(&mut tcp[14..], TCP_WINDOW);
        vec![self.ip_tcp_frame(c.host_ip, c.guest_ip, &tcp)]
    }

    fn tcp_rst(&self, src_ip: [u8; 4], src_port: u16, dst_ip: [u8; 4], dst_port: u16, seq: u32, ack: u32) -> Vec<Vec<u8>> {
        let mut tcp = vec![0u8; 20];
        put_be16(&mut tcp[0..], src_port);
        put_be16(&mut tcp[2..], dst_port);
        put_be32(&mut tcp[4..], seq);
        put_be32(&mut tcp[8..], ack);
        tcp[12] = 0x50;
        tcp[13] = 0x14; // RST|ACK
        put_be16(&mut tcp[14..], 0);
        vec![self.ip_tcp_frame(src_ip, dst_ip, &tcp)]
    }

    fn tcp_data(&self, idx: usize, data: &[u8]) -> Vec<Vec<u8>> {
        let c = &self.tcp[idx];
        let mut tcp = vec![0u8; 20 + data.len()];
        put_be16(&mut tcp[0..], c.host_port);
        put_be16(&mut tcp[2..], c.guest_port);
        put_be32(&mut tcp[4..], c.our_seq);
        put_be32(&mut tcp[8..], c.guest_seq);
        tcp[12] = 0x50;
        tcp[13] = 0x18; // PSH|ACK
        put_be16(&mut tcp[14..], TCP_WINDOW);
        tcp[20..].copy_from_slice(data);
        vec![self.ip_tcp_frame(c.host_ip, c.guest_ip, &tcp)]
    }

    /// Handle a UDP datagram from the guest. Returns response Ethernet frames.
    pub fn udp_out(&mut self, ip: &[u8], _ihl: usize, udp: &[u8]) -> Vec<Vec<u8>> {
        if udp.len() < 8 {
            return vec![];
        }
        let src_port = be16(&udp[0..2]);
        let dst_port = be16(&udp[2..4]);
        let data = &udp[8..];
        let guest_ip: [u8; 4] = ip[12..16].try_into().unwrap();
        let host_ip: [u8; 4] = ip[16..20].try_into().unwrap();

        let addr = SocketAddr::new(
            Ipv4Addr::new(host_ip[0], host_ip[1], host_ip[2], host_ip[3]).into(),
            dst_port,
        );

        // find or create flow
        let idx = self.udp.iter().position(|u| {
            u.guest_ip == guest_ip && u.guest_port == src_port && u.host_ip == host_ip && u.host_port == dst_port
        });

        let flow_idx = if let Some(i) = idx {
            self.udp[i].last_active = self.now_ms;
            i
        } else {
            if self.udp.len() >= MAX_UDP {
                // evict oldest
                if let Some(oldest) = self.udp.iter().enumerate().min_by_key(|(_, u)| u.last_active).map(|(i, _)| i) {
                    self.udp.remove(oldest);
                }
            }
            let sock = match UdpSocket::bind("0.0.0.0:0") {
                Ok(s) => {
                    let _ = s.set_nonblocking(true);
                    let _ = s.connect(addr);
                    s
                }
                Err(_) => return vec![],
            };
            self.udp.push(UdpFlow {
                guest_ip,
                guest_port: src_port,
                host_ip,
                host_port: dst_port,
                socket: sock,
                last_active: self.now_ms,
            });
            self.udp.len() - 1
        };

        let _ = self.udp[flow_idx].socket.send(data);
        self.stats.udp_packets += 1;
        vec![]
    }

    /// Poll all connections for incoming data. Returns Ethernet frames to deliver to guest.
    pub fn poll(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let mut buf = [0u8; 4096];

        // poll TCP connections
        for i in 0..self.tcp.len() {
            match self.tcp[i].state {
                TcpState::Connected => {
                    if let Some(ref mut stream) = self.tcp[i].stream {
                        match stream.read(&mut buf) {
                            Ok(0) => {
                                // EOF: send FIN
                                frames.extend(self.tcp_ack_fin(i));
                                self.tcp[i].our_seq = self.tcp[i].our_seq.wrapping_add(1);
                                self.tcp[i].state = TcpState::TimeWait(self.now_ms);
                            }
                            Ok(n) => {
                                self.stats.tcp_bytes_in += n as u64;
                                frames.extend(self.tcp_data(i, &buf[..n]));
                                self.tcp[i].our_seq = self.tcp[i].our_seq.wrapping_add(n as u32);
                                self.tcp[i].last_active = self.now_ms;
                            }
                            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                            Err(_) => {
                                self.tcp[i].state = TcpState::Closed;
                            }
                        }
                    }
                }
                TcpState::TimeWait(t) if self.now_ms.saturating_sub(t) > TIME_WAIT_MS => {
                    self.tcp[i].state = TcpState::Closed;
                }
                TcpState::Connecting if self.now_ms.saturating_sub(self.tcp[i].connect_start) > CONNECT_TIMEOUT_MS => {
                    self.tcp[i].state = TcpState::Closed;
                }
                _ => {}
            }
            if matches!(self.tcp[i].state, TcpState::Connected | TcpState::Connecting)
                && self.now_ms.saturating_sub(self.tcp[i].last_active) > IDLE_TIMEOUT_MS
            {
                self.tcp[i].state = TcpState::Closed;
            }
        }

        // poll UDP flows
        for flow in &mut self.udp {
            match flow.socket.recv(&mut buf) {
                Ok(n) if n > 0 => {
                    flow.last_active = self.now_ms;
                    // build UDP response
                    let udp_len = 8 + n;
                    let ip_len = 20 + udp_len;
                    let mut pkt = vec![0u8; ip_len];
                    pkt[0] = 0x45;
                    put_be16(&mut pkt[2..], ip_len as u16);
                    pkt[8] = 64;
                    pkt[9] = 17;
                    pkt[12..16].copy_from_slice(&flow.host_ip);
                    pkt[16..20].copy_from_slice(&flow.guest_ip);
                    let ck = checksum(&pkt[..20]);
                    put_be16(&mut pkt[10..], ck);
                    put_be16(&mut pkt[20..], flow.host_port);
                    put_be16(&mut pkt[22..], flow.guest_port);
                    put_be16(&mut pkt[24..], udp_len as u16);
                    pkt[28..28 + n].copy_from_slice(&buf[..n]);
                    let mut f = Vec::with_capacity(14 + ip_len);
                    f.extend_from_slice(&STA_MAC);
                    f.extend_from_slice(&GW_MAC);
                    f.extend_from_slice(&[0x08, 0x00]);
                    f.extend_from_slice(&pkt);
                    frames.push(f);
                }
                _ => {}
            }
        }

        // clean up closed connections
        self.tcp.retain(|c| !matches!(c.state, TcpState::Closed));
        self.udp.retain(|u| self.now_ms.saturating_sub(u.last_active) < IDLE_TIMEOUT_MS);

        frames
    }
}
