// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Joakim Eriksson (@joakimeriksson), Alice (@aliceisjustplaying) and Mattia Fiumara (@mfiumara)
// Adapted from esp32sim esp-soc/src/nat.rs (mikroverk/esp32sim, MIT).

//! User-mode NAT: the guest's TCP and UDP over the host's sockets.
//!
//! When the virtual subnet (net.rs) sees a TCP segment or a UDP datagram for a host
//! outside it, the packet comes here. The NAT keeps one TCP state machine per guest
//! connection (sequence numbers, the handshake, FIN, RST) and relays the bytes through a
//! host socket; answers come back as Ethernet frames for the station.
//!
//! The host side is behind [`HostNet`]: [`native`] opens the host's sockets, [`mock`]
//! scripts them for tests, and the browser build gets a fetch shim (nat/wasm.rs). Every
//! socket is non-blocking: `poll()` must run periodically to move data and produce frames.

use std::collections::VecDeque;
use std::io;

#[cfg(not(target_arch = "wasm32"))]
pub mod native;
#[cfg(target_arch = "wasm32")]
pub mod wasm;
#[cfg(any(test, feature = "nat-mock"))]
pub mod mock;

const MAX_TCP: usize = 32;
const MAX_UDP: usize = 64;
/// The most a connection's window ever says.
const TCP_WINDOW: u16 = 65535;
/// Guest bytes kept for a host socket that does not take them yet; the window shrinks with it.
const TX_CAP: usize = 64 * 1024;
/// Host bytes sent to the guest but not yet ACKed; the host socket is paused when full.
const RX_CAP: usize = 64 * 1024;
/// The most data in one segment to the guest: a 1500-byte MTU less the IP and TCP headers
/// (the station's driver takes one Ethernet frame at a time, no IP reassembly).
const TCP_MSS: usize = 1460;
const IDLE_TIMEOUT_MS: u64 = 120_000;
const TIME_WAIT_MS: u64 = 30_000;
pub(crate) const CONNECT_TIMEOUT_MS: u64 = 5_000;

/// Where a guest connection goes on the host side: an address the guest named, or the
/// hostname behind one of the virtual subnet's DNS answers (net.rs resolves names to
/// addresses of its own; the host side resolves them again, for real).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostAddr {
    Ip([u8; 4]),
    Name(String),
}

/// How far a host connection has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectState {
    Pending,
    Ready,
    Failed(io::ErrorKind),
}

/// A host TCP connection, non-blocking.
pub trait TcpSock: Send {
    fn connect_state(&mut self) -> ConnectState;
    /// Bytes from the guest to the host: `Ok(n)` for the bytes taken (possibly fewer),
    /// `WouldBlock` when none fit for now.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize>;
    /// Bytes from the host to the guest: `Ok(0)` is the end of the stream, `WouldBlock`
    /// means nothing yet.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    /// The guest closed its side (FIN): no more writes will come.
    fn shutdown_write(&mut self) {}
}

/// A host UDP "connection": datagrams to one peer and back.
pub trait UdpSock: Send {
    fn send(&mut self, buf: &[u8]) -> io::Result<usize>;
    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

/// The host side of the NAT.
pub trait HostNet: Send {
    fn tcp_connect(&mut self, to: &HostAddr, port: u16) -> io::Result<Box<dyn TcpSock>>;
    fn udp_open(&mut self, to: &HostAddr, port: u16) -> io::Result<Box<dyn UdpSock>>;
}

/// No host network at all: every connection is refused.
pub struct NoNet;

impl HostNet for NoNet {
    fn tcp_connect(&mut self, _to: &HostAddr, _port: u16) -> io::Result<Box<dyn TcpSock>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    fn udp_open(&mut self, _to: &HostAddr, _port: u16) -> io::Result<Box<dyn UdpSock>> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

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
    /// The host connection is on its way; the guest's SYN waits for its SYN-ACK.
    Connecting,
    /// The host connection is up and our SYN-ACK is out; the guest's ACK is next.
    SynAckSent,
    Connected,
    /// The guest sent its FIN; the host may still answer.
    CloseWait,
    /// The host closed, our FIN is out; the guest may still send.
    FinWait,
    /// Both sides are done; the entry stays a while against late segments.
    TimeWait(u64),
    Closed,
}

struct TcpConn {
    guest_ip: [u8; 4],
    guest_port: u16,
    host_ip: [u8; 4],
    host_port: u16,
    sock: Box<dyn TcpSock>,
    state: TcpState,
    /// The next sequence number we expect from the guest (what we acknowledge).
    guest_seq: u32,
    /// The sequence number of our next byte to the guest.
    our_seq: u32,
    /// Guest bytes acknowledged but not yet taken by the host socket (`drain_tx`).
    tx: VecDeque<u8>,
    /// Host bytes sent to the guest but not yet ACKed: retransmitted on 3 duplicate ACKs.
    rx: VecDeque<u8>,
    /// The sequence number of the first byte in `rx`.
    rx_start: u32,
    /// Consecutive duplicate ACKs from the guest (fast retransmit at 3).
    dup_acks: u8,
    /// The guest's FIN came; the host side closes for writing once `tx` is out.
    fin_pending: bool,
    last_active: u64,
    connect_start: u64,
}

struct UdpFlow {
    guest_ip: [u8; 4],
    guest_port: u16,
    host_ip: [u8; 4],
    host_port: u16,
    sock: Box<dyn UdpSock>,
    last_active: u64,
}

pub struct Nat {
    net: Box<dyn HostNet>,
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

/// May the NAT connect to `ip`? Never to nothing (0/8), multicast or reserved space;
/// to this computer's own addresses and its LAN (loopback, link-local, the private
/// ranges, carrier NAT space) only with `allow_lan`.
pub fn lan_allowed(ip: [u8; 4], allow_lan: bool) -> bool {
    let [a, b, _, _] = ip;
    if a == 0 || a >= 224 {
        return false;
    }
    let lan = a == 127
        || a == 10
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 169 && b == 254)
        || (a == 100 && (64..=127).contains(&b));
    allow_lan || !lan
}

impl Nat {
    /// A NAT over the platform's network: the host's own sockets (`allow_lan`: see
    /// `lan_allowed`), or in the browser the page's `fetch()` (HTTP only; the page's own
    /// rules say what it may reach).
    pub fn platform(allow_lan: bool) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let net: Box<dyn HostNet> = Box::new(native::NativeNet { allow_lan });
        #[cfg(target_arch = "wasm32")]
        let net: Box<dyn HostNet> = {
            let _ = allow_lan;
            Box::new(wasm::FetchNet)
        };
        Self::with_backend(net)
    }

    /// A NAT over `net`.
    pub fn with_backend(net: Box<dyn HostNet>) -> Self {
        Self { net, tcp: Vec::new(), udp: Vec::new(), now_ms: 0, stats: NatStats::default() }
    }

    pub fn set_time(&mut self, ms: u64) {
        self.now_ms = ms;
    }

    /// How many TCP connections the NAT tracks (any state).
    pub fn tcp_count(&self) -> usize {
        self.tcp.len()
    }

    /// Handle a TCP segment from the guest (`ip`: its IPv4 header; `to`: where the host
    /// connection goes). Returns response Ethernet frames.
    pub fn tcp_in(&mut self, ip: &[u8], tcp: &[u8], to: &HostAddr) -> Vec<Vec<u8>> {
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
            c.guest_ip == guest_ip && c.guest_port == src_port && c.host_ip == host_ip && c.host_port == dst_port
        });

        if rst {
            if let Some(i) = idx {
                self.tcp[i].state = TcpState::Closed;
            }
            return vec![];
        }

        if syn && idx.is_none() {
            return self.tcp_new(guest_ip, src_port, host_ip, dst_port, seq, to);
        }

        let Some(i) = idx else { return vec![] };

        if syn {
            // a retransmitted SYN: the host connect is still pending (nothing to say yet),
            // or our SYN-ACK was lost (say it again)
            return match self.tcp[i].state {
                TcpState::SynAckSent => self.tcp_syn_ack(i, self.tcp[i].our_seq.wrapping_sub(1), self.tcp[i].guest_seq),
                _ => vec![],
            };
        }

        self.tcp[i].last_active = self.now_ms;
        // the ACK of our SYN-ACK (data may ride on it)
        if self.tcp[i].state == TcpState::SynAckSent && ack != 0 {
            self.tcp[i].state = TcpState::Connected;
        }

        let retransmit = self.process_guest_ack(i, ack);

        // the segment against the byte we expect next: a retransmit repeats bytes we have
        // (skipped), a segment past a hole is dropped for a duplicate ACK
        let behind = self.tcp[i].guest_seq.wrapping_sub(seq) as i32;
        if behind < 0 {
            let mut out = self.tcp_ack(i);
            out.extend(retransmit);
            return out;
        }
        let data = &data[(behind as usize).min(data.len())..];
        let old_fin = matches!(self.tcp[i].state, TcpState::CloseWait | TcpState::TimeWait(_));

        // into the send buffer, as far as there is room: what does not fit is not
        // acknowledged and comes again (the window says how much fits)
        let mut taken = data.len();
        if !data.is_empty() {
            if !matches!(self.tcp[i].state, TcpState::Connected | TcpState::FinWait) {
                return retransmit;
            }
            let room = TX_CAP.saturating_sub(self.tcp[i].tx.len());
            taken = data.len().min(room);
            self.tcp[i].tx.extend(&data[..taken]);
            self.tcp[i].guest_seq = self.tcp[i].guest_seq.wrapping_add(taken as u32);
            self.drain_tx(i);
        }

        if fin && !old_fin && taken == data.len() {
            self.tcp[i].guest_seq = self.tcp[i].guest_seq.wrapping_add(1);
            self.tcp[i].fin_pending = true;
            self.drain_tx(i);
            self.tcp[i].state = match self.tcp[i].state {
                TcpState::FinWait => TcpState::TimeWait(self.now_ms),
                TcpState::Closed => TcpState::Closed,
                _ => TcpState::CloseWait,
            };
            let mut out = self.tcp_ack(i);
            out.extend(retransmit);
            return out;
        }

        if !data.is_empty() || behind > 0 || fin {
            let mut out = self.tcp_ack(i);
            out.extend(retransmit);
            return out;
        }
        retransmit
    }

    /// Connection `i`'s send buffer into its host socket, as far as the socket takes it;
    /// then the half-close the guest's FIN asked for, once the buffer is empty.
    fn drain_tx(&mut self, i: usize) {
        let c = &mut self.tcp[i];
        while !c.tx.is_empty() {
            let (head, _) = c.tx.as_slices();
            match c.sock.write(head) {
                Ok(0) => break,
                Ok(n) => {
                    c.tx.drain(..n);
                    self.stats.tcp_bytes_out += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    c.state = TcpState::Closed;
                    return;
                }
            }
        }
        if c.tx.is_empty() && c.fin_pending {
            c.fin_pending = false;
            c.sock.shutdown_write();
        }
    }

    fn tcp_new(&mut self, guest_ip: [u8; 4], guest_port: u16, host_ip: [u8; 4], host_port: u16, seq: u32, to: &HostAddr) -> Vec<Vec<u8>> {
        if self.tcp.len() >= MAX_TCP {
            // evict oldest closed/timewait
            if let Some(i) = self.tcp.iter().position(|c| matches!(c.state, TcpState::Closed | TcpState::TimeWait(_))) {
                self.tcp.remove(i);
            } else {
                return vec![];
            }
        }

        let sock = match self.net.tcp_connect(to, host_port) {
            Ok(s) => s,
            Err(_) => return self.tcp_rst(host_ip, host_port, guest_ip, guest_port, 0, seq.wrapping_add(1)),
        };

        let our_seq: u32 = (self.now_ms as u32).wrapping_mul(0x9e37_79b9);

        let our_seq_init = our_seq.wrapping_add(1);
        self.tcp.push(TcpConn {
            guest_ip,
            guest_port,
            host_ip,
            host_port,
            sock,
            state: TcpState::Connecting,
            guest_seq: seq.wrapping_add(1),
            our_seq: our_seq_init,
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            rx_start: our_seq_init,
            dup_acks: 0,
            fin_pending: false,
            last_active: self.now_ms,
            connect_start: self.now_ms,
        });

        // the SYN-ACK once the host connection is up: now, or from a later poll
        self.tcp_connecting(self.tcp.len() - 1)
    }

    /// Connection `i` is `Connecting`: has the host connection come up? Its SYN-ACK then,
    /// an RST when it failed or took longer than CONNECT_TIMEOUT_MS, nothing while pending.
    fn tcp_connecting(&mut self, i: usize) -> Vec<Vec<u8>> {
        let c = &mut self.tcp[i];
        let state = c.sock.connect_state();
        let failed = match state {
            ConnectState::Ready => {
                c.state = TcpState::SynAckSent;
                self.stats.tcp_connects += 1;
                if crate::net::trace_on() {
                    eprintln!("nat: {}:{} connected", c.guest_port, c.host_port);
                }
                let (seq, ack) = (c.our_seq.wrapping_sub(1), c.guest_seq);
                return self.tcp_syn_ack(i, seq, ack);
            }
            ConnectState::Pending => self.now_ms.saturating_sub(c.connect_start) > CONNECT_TIMEOUT_MS,
            ConnectState::Failed(_) => true,
        };
        if !failed {
            return vec![];
        }
        if crate::net::trace_on() {
            eprintln!("nat: {}:{} connect {:?}: rst", c.guest_port, c.host_port, state);
        }
        c.state = TcpState::Closed;
        let (host_ip, host_port, guest_ip, guest_port, ack) = (c.host_ip, c.host_port, c.guest_ip, c.guest_port, c.guest_seq);
        self.tcp_rst(host_ip, host_port, guest_ip, guest_port, 0, ack)
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

    /// A bare TCP header from the host side of connection `idx`: `flags`, `seq`, `ack`, and
    /// the window: the room left in its send buffer.
    fn tcp_header(&self, idx: usize, flags: u8, seq: u32, ack: u32) -> Vec<u8> {
        let c = &self.tcp[idx];
        let mut tcp = vec![0u8; 20];
        put_be16(&mut tcp[0..], c.host_port);
        put_be16(&mut tcp[2..], c.guest_port);
        put_be32(&mut tcp[4..], seq);
        put_be32(&mut tcp[8..], ack);
        tcp[12] = 0x50;
        tcp[13] = flags;
        let window = TX_CAP.saturating_sub(c.tx.len()).min(TCP_WINDOW as usize) as u16;
        put_be16(&mut tcp[14..], window);
        tcp
    }

    fn tcp_syn_ack(&self, idx: usize, seq: u32, ack: u32) -> Vec<Vec<u8>> {
        let c = &self.tcp[idx];
        let mut tcp = self.tcp_header(idx, 0x12, seq, ack); // SYN|ACK
        // the MSS option: segments to us no larger than ours to the guest
        tcp.extend_from_slice(&[2, 4, (TCP_MSS >> 8) as u8, TCP_MSS as u8]);
        tcp[12] = 0x60; // data offset: 6 words
        vec![self.ip_tcp_frame(c.host_ip, c.guest_ip, &tcp)]
    }

    fn tcp_ack(&self, idx: usize) -> Vec<Vec<u8>> {
        let c = &self.tcp[idx];
        let tcp = self.tcp_header(idx, 0x10, c.our_seq, c.guest_seq); // ACK
        vec![self.ip_tcp_frame(c.host_ip, c.guest_ip, &tcp)]
    }

    fn tcp_ack_fin(&self, idx: usize) -> Vec<Vec<u8>> {
        let c = &self.tcp[idx];
        let tcp = self.tcp_header(idx, 0x11, c.our_seq, c.guest_seq); // FIN|ACK
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

    /// The guest's ACK of data we sent: release the retransmit buffer up to `ack`,
    /// or retransmit on 3 duplicate ACKs.
    fn process_guest_ack(&mut self, i: usize, ack: u32) -> Vec<Vec<u8>> {
        let c = &mut self.tcp[i];
        if c.rx.is_empty() {
            return vec![];
        }
        let advance = ack.wrapping_sub(c.rx_start) as i32;
        if advance > 0 && (advance as usize) <= c.rx.len() {
            c.rx.drain(..advance as usize);
            c.rx_start = ack;
            c.dup_acks = 0;
        } else if advance == 0 {
            c.dup_acks += 1;
            if c.dup_acks >= 3 {
                c.dup_acks = 0;
                return self.tcp_retransmit(i);
            }
        }
        vec![]
    }

    /// Retransmit the first MSS of the unACKed buffer for connection `i`.
    fn tcp_retransmit(&self, idx: usize) -> Vec<Vec<u8>> {
        let c = &self.tcp[idx];
        if c.rx.is_empty() {
            return vec![];
        }
        let len = c.rx.len().min(TCP_MSS);
        let (head, tail) = c.rx.as_slices();
        let mut data = Vec::with_capacity(len);
        let from_head = len.min(head.len());
        data.extend_from_slice(&head[..from_head]);
        if from_head < len {
            data.extend_from_slice(&tail[..len - from_head]);
        }
        let flags = if len == c.rx.len() { 0x18 } else { 0x10 };
        let mut tcp = self.tcp_header(idx, flags, c.rx_start, c.guest_seq);
        tcp.extend_from_slice(&data);
        vec![self.ip_tcp_frame(c.host_ip, c.guest_ip, &tcp)]
    }

    /// `data` from the host to the guest, from `our_seq` on: one segment per MSS, PSH on
    /// the last (the caller advances `our_seq` by the whole of it).
    fn tcp_data(&self, idx: usize, data: &[u8]) -> Vec<Vec<u8>> {
        let c = &self.tcp[idx];
        let mut frames = Vec::with_capacity(data.len().div_ceil(TCP_MSS));
        let mut seq = c.our_seq;
        let mut chunks = data.chunks(TCP_MSS).peekable();
        while let Some(chunk) = chunks.next() {
            let flags = if chunks.peek().is_none() { 0x18 } else { 0x10 }; // PSH|ACK, ACK
            let mut tcp = self.tcp_header(idx, flags, seq, c.guest_seq);
            tcp.extend_from_slice(chunk);
            frames.push(self.ip_tcp_frame(c.host_ip, c.guest_ip, &tcp));
            seq = seq.wrapping_add(chunk.len() as u32);
        }
        frames
    }

    /// Handle a UDP datagram from the guest. Returns response Ethernet frames.
    pub fn udp_out(&mut self, ip: &[u8], udp: &[u8], to: &HostAddr) -> Vec<Vec<u8>> {
        if udp.len() < 8 {
            return vec![];
        }
        let src_port = be16(&udp[0..2]);
        let dst_port = be16(&udp[2..4]);
        let data = &udp[8..];
        let guest_ip: [u8; 4] = ip[12..16].try_into().unwrap();
        let host_ip: [u8; 4] = ip[16..20].try_into().unwrap();

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
            let sock = match self.net.udp_open(to, dst_port) {
                Ok(s) => s,
                Err(_) => return vec![],
            };
            self.udp.push(UdpFlow {
                guest_ip,
                guest_port: src_port,
                host_ip,
                host_port: dst_port,
                sock,
                last_active: self.now_ms,
            });
            self.udp.len() - 1
        };

        let _ = self.udp[flow_idx].sock.send(data);
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
                TcpState::Connected | TcpState::CloseWait => {
                    self.drain_tx(i);
                    if self.tcp[i].state == TcpState::Closed {
                        continue;
                    }
                    loop {
                        let room = RX_CAP.saturating_sub(self.tcp[i].rx.len());
                        let read_len = buf.len().min(room);
                        if read_len == 0 {
                            break;
                        }
                        match self.tcp[i].sock.read(&mut buf[..read_len]) {
                            Ok(0) => {
                                if crate::net::trace_on() {
                                    eprintln!("nat: {}:{} host closed after {} bytes in", self.tcp[i].guest_port, self.tcp[i].host_port, self.stats.tcp_bytes_in);
                                }
                                frames.extend(self.tcp_ack_fin(i));
                                self.tcp[i].our_seq = self.tcp[i].our_seq.wrapping_add(1);
                                self.tcp[i].state = if self.tcp[i].state == TcpState::CloseWait {
                                    TcpState::TimeWait(self.now_ms)
                                } else {
                                    TcpState::FinWait
                                };
                                break;
                            }
                            Ok(n) => {
                                self.stats.tcp_bytes_in += n as u64;
                                if crate::net::trace_on() {
                                    let head = String::from_utf8_lossy(&buf[..n.min(40)]).replace(['\r', '\n'], " ");
                                    eprintln!("nat: {}:{} {n} bytes from the host: {head:?}", self.tcp[i].guest_port, self.tcp[i].host_port);
                                }
                                self.tcp[i].rx.extend(&buf[..n]);
                                frames.extend(self.tcp_data(i, &buf[..n]));
                                self.tcp[i].our_seq = self.tcp[i].our_seq.wrapping_add(n as u32);
                                self.tcp[i].last_active = self.now_ms;
                            }
                            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                            Err(_) => {
                                self.tcp[i].state = TcpState::Closed;
                                break;
                            }
                        }
                    }
                }
                // (the guest may still send after the host's FIN)
                TcpState::FinWait => self.drain_tx(i),
                TcpState::TimeWait(t) if self.now_ms.saturating_sub(t) > TIME_WAIT_MS => {
                    self.tcp[i].state = TcpState::Closed;
                }
                TcpState::Connecting => frames.extend(self.tcp_connecting(i)),
                _ => {}
            }
            if !matches!(self.tcp[i].state, TcpState::TimeWait(_) | TcpState::Closed)
                && self.now_ms.saturating_sub(self.tcp[i].last_active) > IDLE_TIMEOUT_MS
            {
                self.tcp[i].state = TcpState::Closed;
            }
        }

        // poll UDP flows
        for flow in &mut self.udp {
            match flow.sock.recv(&mut buf) {
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

#[cfg(test)]
mod tests {
    use super::mock::MockNet;
    use super::*;

    pub(super) const GUEST_IP: [u8; 4] = [10, 0, 2, 15];
    pub(super) const HOST_IP: [u8; 4] = [93, 184, 216, 34];
    pub(super) const SYN: u8 = 0x02;
    pub(super) const ACK: u8 = 0x10;
    pub(super) const PSH: u8 = 0x08;
    pub(super) const FIN: u8 = 0x01;
    pub(super) const RST: u8 = 0x04;

    /// An Ethernet frame from the guest (port `src_port`) to HOST_IP:`dst_port`.
    pub(super) fn guest_tcp(src_port: u16, dst_port: u16, seq: u32, ack: u32, flags: u8, data: &[u8]) -> Vec<u8> {
        let mut tcp = vec![0u8; 20 + data.len()];
        put_be16(&mut tcp[0..], src_port);
        put_be16(&mut tcp[2..], dst_port);
        put_be32(&mut tcp[4..], seq);
        put_be32(&mut tcp[8..], ack);
        tcp[12] = 0x50;
        tcp[13] = flags;
        put_be16(&mut tcp[14..], 8192);
        tcp[20..].copy_from_slice(data);
        let ck = tcp_checksum(GUEST_IP, HOST_IP, &tcp);
        put_be16(&mut tcp[16..], ck);
        let ip_len = 20 + tcp.len();
        let mut ip = vec![0u8; 20];
        ip[0] = 0x45;
        put_be16(&mut ip[2..], ip_len as u16);
        ip[8] = 64;
        ip[9] = 6;
        ip[12..16].copy_from_slice(&GUEST_IP);
        ip[16..20].copy_from_slice(&HOST_IP);
        let ck = checksum(&ip);
        put_be16(&mut ip[10..], ck);
        let mut f = Vec::with_capacity(14 + ip_len);
        f.extend_from_slice(&GW_MAC);
        f.extend_from_slice(&STA_MAC);
        f.extend_from_slice(&[0x08, 0x00]);
        f.extend_from_slice(&ip);
        f.extend_from_slice(&tcp);
        f
    }

    /// A UDP datagram from the guest (port `src_port`) to HOST_IP:`dst_port`.
    pub(super) fn guest_udp(src_port: u16, dst_port: u16, data: &[u8]) -> Vec<u8> {
        let udp_len = 8 + data.len();
        let ip_len = 20 + udp_len;
        let mut f = vec![0u8; 14 + ip_len];
        f[0..6].copy_from_slice(&GW_MAC);
        f[6..12].copy_from_slice(&STA_MAC);
        f[12..14].copy_from_slice(&[0x08, 0x00]);
        f[14] = 0x45;
        put_be16(&mut f[16..], ip_len as u16);
        f[22] = 64;
        f[23] = 17;
        f[26..30].copy_from_slice(&GUEST_IP);
        f[30..34].copy_from_slice(&HOST_IP);
        put_be16(&mut f[34..], src_port);
        put_be16(&mut f[36..], dst_port);
        put_be16(&mut f[38..], udp_len as u16);
        f[42..].copy_from_slice(data);
        f
    }

    pub(super) fn send_tcp(nat: &mut Nat, frame: &[u8]) -> Vec<Vec<u8>> {
        nat.tcp_in(&frame[14..34], &frame[34..], &HostAddr::Ip(HOST_IP))
    }

    pub(super) fn send_udp(nat: &mut Nat, frame: &[u8]) -> Vec<Vec<u8>> {
        nat.udp_out(&frame[14..34], &frame[34..], &HostAddr::Ip(HOST_IP))
    }

    pub(super) fn tcp_of(frame: &[u8]) -> &[u8] {
        &frame[34..]
    }
    pub(super) fn flags_of(frame: &[u8]) -> u8 {
        tcp_of(frame)[13]
    }
    pub(super) fn seq_of(frame: &[u8]) -> u32 {
        u32::from_be_bytes(tcp_of(frame)[4..8].try_into().unwrap())
    }
    pub(super) fn ack_of(frame: &[u8]) -> u32 {
        u32::from_be_bytes(tcp_of(frame)[8..12].try_into().unwrap())
    }
    pub(super) fn payload_of(frame: &[u8]) -> &[u8] {
        let t = tcp_of(frame);
        &t[((t[12] >> 4) as usize) * 4..]
    }
    pub(super) fn ip_len_of(frame: &[u8]) -> usize {
        be16(&frame[16..18]) as usize
    }
    pub(super) fn window_of(frame: &[u8]) -> u16 {
        be16(&tcp_of(frame)[14..16])
    }

    /// The mock's first TCP socket.
    fn sock0(state: &mock::State) -> std::sync::Arc<std::sync::Mutex<mock::MockSock>> {
        state.lock().unwrap().tcp[0].clone()
    }

    /// A NAT over a mock host with the guest's 40000 -> HOST_IP:80 connected; returns the
    /// NAT, the mock's state and our initial sequence number.
    pub(super) fn connected() -> (Nat, mock::State, u32) {
        let (net, state) = MockNet::new();
        let mut nat = Nat::with_backend(Box::new(net));
        let syn = send_tcp(&mut nat, &guest_tcp(40000, 80, 1000, 0, SYN, &[]));
        assert_eq!(syn.len(), 1, "one SYN-ACK");
        assert_eq!(flags_of(&syn[0]), SYN | ACK);
        let isn = seq_of(&syn[0]);
        let ack = send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, isn.wrapping_add(1), ACK, &[]));
        assert!(ack.is_empty(), "the handshake's ACK gets no answer");
        (nat, state, isn)
    }

    #[test]
    fn syn_gets_syn_ack_and_opens_the_host_connection() {
        let (net, state) = MockNet::new();
        let mut nat = Nat::with_backend(Box::new(net));
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1000, 0, SYN, &[]));
        assert_eq!(out.len(), 1);
        assert_eq!(flags_of(&out[0]), SYN | ACK);
        assert_eq!(ack_of(&out[0]), 1001);
        let s = state.lock().unwrap();
        assert_eq!(s.tcp.len(), 1);
        assert_eq!(s.tcp[0].lock().unwrap().to, (HostAddr::Ip(HOST_IP), 80));
        assert_eq!(nat.stats.tcp_connects, 1);
    }

    #[test]
    fn guest_data_reaches_the_host_and_is_acked() {
        let (mut nat, state, _isn) = connected();
        let req = b"GET / HTTP/1.0\r\n\r\n";
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, 0, PSH | ACK, req));
        assert_eq!(out.len(), 1);
        assert_eq!(flags_of(&out[0]), ACK);
        assert_eq!(ack_of(&out[0]), 1001 + req.len() as u32);
        assert_eq!(state.lock().unwrap().tcp[0].lock().unwrap().tx, req);
        assert_eq!(nat.stats.tcp_bytes_out, req.len() as u64);
    }

    #[test]
    fn host_data_becomes_a_push_segment_and_eof_a_fin() {
        let (mut nat, state, isn) = connected();
        state.lock().unwrap().tcp[0].lock().unwrap().rx.extend(b"hello");
        let out = nat.poll();
        assert_eq!(out.len(), 1);
        assert_eq!(flags_of(&out[0]), PSH | ACK);
        assert_eq!(payload_of(&out[0]), b"hello");
        assert_eq!(seq_of(&out[0]), isn.wrapping_add(1));
        assert!(nat.poll().is_empty(), "nothing more to read");
        state.lock().unwrap().tcp[0].lock().unwrap().eof = true;
        let out = nat.poll();
        assert_eq!(out.len(), 1);
        assert_eq!(flags_of(&out[0]), FIN | ACK);
        assert_eq!(seq_of(&out[0]), isn.wrapping_add(6));
    }

    #[test]
    fn guest_fin_is_acked_and_half_closes_the_host_side() {
        let (mut nat, state, isn) = connected();
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, isn.wrapping_add(1), FIN | ACK, &[]));
        assert_eq!(out.len(), 1);
        assert_eq!(flags_of(&out[0]), ACK, "the FIN acknowledged, the host side still open to answer");
        assert_eq!(ack_of(&out[0]), 1002);
        assert!(sock0(&state).lock().unwrap().shutdown);
        // the host answers after the guest's FIN, then closes: data + FIN in one poll
        sock0(&state).lock().unwrap().rx.extend(b"late");
        sock0(&state).lock().unwrap().eof = true;
        let out = nat.poll();
        assert_eq!(out.len(), 2, "data frame + FIN frame");
        assert_eq!(payload_of(&out[0]), b"late");
        assert_eq!(ack_of(&out[0]), 1002);
        assert_eq!(flags_of(&out[1]), FIN | ACK);
        assert_eq!(seq_of(&out[1]), isn.wrapping_add(5));
        // a retransmitted FIN from the guest: acknowledged again, counted once
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, isn.wrapping_add(6), FIN | ACK, &[]));
        assert_eq!(flags_of(&out[0]), ACK);
        assert_eq!(ack_of(&out[0]), 1002);
        nat.set_time(TIME_WAIT_MS + 1);
        nat.poll();
        assert_eq!(nat.tcp_count(), 0);
    }

    #[test]
    fn after_the_hosts_fin_the_guests_fin_is_acked_and_the_entry_expires() {
        let (mut nat, state, isn) = connected();
        sock0(&state).lock().unwrap().eof = true;
        let out = nat.poll();
        assert_eq!(flags_of(&out[0]), FIN | ACK);
        assert_eq!(seq_of(&out[0]), isn.wrapping_add(1));
        // the guest still sends (the host may read on), then closes too
        send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, isn.wrapping_add(2), PSH | ACK, b"tail"));
        assert_eq!(sock0(&state).lock().unwrap().tx, b"tail");
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1005, isn.wrapping_add(2), FIN | ACK, &[]));
        assert_eq!(flags_of(&out[0]), ACK);
        assert_eq!(ack_of(&out[0]), 1006);
        assert!(sock0(&state).lock().unwrap().shutdown);
        assert_eq!(nat.tcp_count(), 1, "TIME_WAIT");
        nat.set_time(TIME_WAIT_MS + 1);
        nat.poll();
        assert_eq!(nat.tcp_count(), 0);
    }

    #[test]
    fn syn_to_an_unreachable_host_gets_rst() {
        let (net, state) = MockNet::new();
        state.lock().unwrap().refuse_tcp = true;
        let mut nat = Nat::with_backend(Box::new(net));
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1000, 0, SYN, &[]));
        assert_eq!(out.len(), 1);
        assert_eq!(flags_of(&out[0]), RST | ACK);
        assert_eq!(ack_of(&out[0]), 1001);
        assert_eq!(nat.tcp_count(), 0);
    }

    #[test]
    fn udp_goes_out_and_the_answer_comes_back_as_a_datagram() {
        let (net, state) = MockNet::new();
        let mut nat = Nat::with_backend(Box::new(net));
        assert!(send_udp(&mut nat, &guest_udp(50000, 123, b"ping")).is_empty());
        {
            let s = state.lock().unwrap();
            assert_eq!(s.udp.len(), 1);
            let mut u = s.udp[0].lock().unwrap();
            assert_eq!(u.to, (HostAddr::Ip(HOST_IP), 123));
            assert_eq!(u.sent, vec![b"ping".to_vec()]);
            u.inbox.push_back(b"pong".to_vec());
        }
        let out = nat.poll();
        assert_eq!(out.len(), 1);
        let f = &out[0];
        assert_eq!(f[23], 17, "UDP");
        assert_eq!(&f[26..30], &HOST_IP);
        assert_eq!(&f[30..34], &GUEST_IP);
        assert_eq!(be16(&f[34..36]), 123);
        assert_eq!(be16(&f[36..38]), 50000);
        assert_eq!(&f[42..], b"pong");
        assert_eq!(ip_len_of(f), 20 + 8 + 4);
    }

    #[test]
    fn time_wait_expires_on_the_emulated_clock() {
        let (mut nat, state, isn) = connected();
        send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, isn.wrapping_add(1), FIN | ACK, &[]));
        sock0(&state).lock().unwrap().eof = true;
        assert_eq!(flags_of(&nat.poll()[0]), FIN | ACK, "the host's close after the guest's");
        assert_eq!(nat.tcp_count(), 1, "in TIME_WAIT");
        nat.set_time(TIME_WAIT_MS);
        nat.poll();
        assert_eq!(nat.tcp_count(), 1, "not yet");
        nat.set_time(TIME_WAIT_MS + 1);
        nat.poll();
        assert_eq!(nat.tcp_count(), 0, "gone: the port can be reused");
        // a new SYN from the same port opens a new connection
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 5000, 0, SYN, &[]));
        assert_eq!(flags_of(&out[0]), SYN | ACK);
    }

    #[test]
    fn idle_connection_closes_after_the_idle_timeout() {
        let (mut nat, _state, _isn) = connected();
        nat.set_time(IDLE_TIMEOUT_MS + 1);
        nat.poll();
        assert_eq!(nat.tcp_count(), 0);
    }

    #[test]
    fn syn_ack_waits_for_the_host_connection() {
        let (net, state) = MockNet::new();
        state.lock().unwrap().initial_state = Some(ConnectState::Pending);
        let mut nat = Nat::with_backend(Box::new(net));
        assert!(send_tcp(&mut nat, &guest_tcp(40000, 80, 1000, 0, SYN, &[])).is_empty(), "nothing until the host answers");
        assert!(nat.poll().is_empty());
        assert_eq!(nat.stats.tcp_connects, 0);
        // lwIP sends the SYN again meanwhile: absorbed, no second host connection
        assert!(send_tcp(&mut nat, &guest_tcp(40000, 80, 1000, 0, SYN, &[])).is_empty());
        assert_eq!(state.lock().unwrap().tcp.len(), 1);
        state.lock().unwrap().tcp[0].lock().unwrap().state = ConnectState::Ready;
        let out = nat.poll();
        assert_eq!(out.len(), 1);
        assert_eq!(flags_of(&out[0]), SYN | ACK);
        assert_eq!(ack_of(&out[0]), 1001);
        assert_eq!(nat.stats.tcp_connects, 1);
        let isn = seq_of(&out[0]);
        // the SYN-ACK was lost: the guest's SYN again gets it again, the same numbers
        let again = send_tcp(&mut nat, &guest_tcp(40000, 80, 1000, 0, SYN, &[]));
        assert_eq!(again.len(), 1);
        assert_eq!(seq_of(&again[0]), isn);
        assert_eq!(ack_of(&again[0]), 1001);
        // the guest's ACK, with data on it, completes the handshake
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, isn.wrapping_add(1), PSH | ACK, b"hi"));
        assert_eq!(flags_of(&out[0]), ACK);
        assert_eq!(ack_of(&out[0]), 1003);
        assert_eq!(state.lock().unwrap().tcp[0].lock().unwrap().tx, b"hi");
    }

    #[test]
    fn failed_host_connection_gets_rst() {
        let (net, state) = MockNet::new();
        state.lock().unwrap().initial_state = Some(ConnectState::Pending);
        let mut nat = Nat::with_backend(Box::new(net));
        assert!(send_tcp(&mut nat, &guest_tcp(40000, 80, 1000, 0, SYN, &[])).is_empty());
        state.lock().unwrap().tcp[0].lock().unwrap().state = ConnectState::Failed(io::ErrorKind::ConnectionRefused);
        let out = nat.poll();
        assert_eq!(out.len(), 1);
        assert_eq!(flags_of(&out[0]), RST | ACK);
        assert_eq!(ack_of(&out[0]), 1001);
        assert_eq!(nat.tcp_count(), 0);
    }

    #[test]
    fn host_connection_that_never_comes_up_times_out_with_rst() {
        let (net, state) = MockNet::new();
        state.lock().unwrap().initial_state = Some(ConnectState::Pending);
        let mut nat = Nat::with_backend(Box::new(net));
        assert!(send_tcp(&mut nat, &guest_tcp(40000, 80, 1000, 0, SYN, &[])).is_empty());
        nat.set_time(CONNECT_TIMEOUT_MS);
        assert!(nat.poll().is_empty(), "still within the timeout");
        nat.set_time(CONNECT_TIMEOUT_MS + 1);
        let out = nat.poll();
        assert_eq!(flags_of(&out[0]), RST | ACK);
        assert_eq!(nat.tcp_count(), 0);
    }

    #[test]
    fn a_slow_host_socket_buffers_the_guest_bytes_until_a_poll_sends_them() {
        let (mut nat, state, _isn) = connected();
        let sock = sock0(&state);
        sock.lock().unwrap().accept_limit = Some(0); // WouldBlock
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, 0, PSH | ACK, b"0123456789"));
        assert_eq!(ack_of(&out[0]), 1011, "acknowledged: the bytes are ours now");
        assert_eq!(window_of(&out[0]) as usize, TX_CAP - 10, "the window shrank by them");
        assert!(sock.lock().unwrap().tx.is_empty(), "nothing taken yet");
        assert_eq!(nat.stats.tcp_bytes_out, 0);
        sock.lock().unwrap().accept_limit = Some(4); // a few at a time
        nat.poll();
        assert_eq!(sock.lock().unwrap().tx, b"0123456789");
        assert_eq!(nat.stats.tcp_bytes_out, 10);
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1011, 0, PSH | ACK, b"x"));
        assert_eq!(window_of(&out[0]), TCP_WINDOW, "drained: the window is whole again");
    }

    #[test]
    fn a_retransmitted_segment_is_not_written_twice() {
        let (mut nat, state, _isn) = connected();
        let sock = sock0(&state);
        send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, 0, PSH | ACK, b"hello"));
        // the same segment again (our ACK was lost): acknowledged again, not written again
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, 0, PSH | ACK, b"hello"));
        assert_eq!(ack_of(&out[0]), 1006);
        assert_eq!(sock.lock().unwrap().tx, b"hello");
        // a segment overlapping the end of it (lwIP repacketizes): only the new bytes
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1003, 0, PSH | ACK, b"llo!!"));
        assert_eq!(ack_of(&out[0]), 1008);
        assert_eq!(sock.lock().unwrap().tx, b"hello!!");
    }

    #[test]
    fn a_segment_past_a_hole_is_dropped_for_a_duplicate_ack() {
        let (mut nat, state, _isn) = connected();
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1010, 0, PSH | ACK, b"late"));
        assert_eq!(out.len(), 1);
        assert_eq!(flags_of(&out[0]), ACK);
        assert_eq!(ack_of(&out[0]), 1001, "still waiting for 1001");
        assert!(sock0(&state).lock().unwrap().tx.is_empty());
    }

    #[test]
    fn a_full_send_buffer_stops_acknowledging() {
        let (mut nat, state, _isn) = connected();
        sock0(&state).lock().unwrap().accept_limit = Some(0);
        let big = vec![0x55u8; TX_CAP];
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, 0, PSH | ACK, &big));
        assert_eq!(ack_of(&out[0]), 1001 + TX_CAP as u32);
        assert_eq!(window_of(&out[0]), 0);
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1001 + TX_CAP as u32, 0, PSH | ACK, b"more"));
        assert_eq!(ack_of(&out[0]), 1001 + TX_CAP as u32, "not taken: the guest sends it again later");
        assert_eq!(window_of(&out[0]), 0);
    }

    #[test]
    fn the_guest_fin_half_closes_the_host_once_its_bytes_are_out() {
        let (mut nat, state, isn) = connected();
        let sock = sock0(&state);
        sock.lock().unwrap().accept_limit = Some(0);
        send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, 0, PSH | ACK, b"bye"));
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1004, isn.wrapping_add(1), FIN | ACK, &[]));
        assert_eq!(flags_of(&out[0]), ACK);
        assert_eq!(ack_of(&out[0]), 1005);
        assert!(!sock.lock().unwrap().shutdown, "bytes still to go");
        sock.lock().unwrap().accept_limit = None;
        nat.poll();
        assert_eq!(sock.lock().unwrap().tx, b"bye");
        assert!(sock.lock().unwrap().shutdown);
    }

    #[test]
    fn host_data_is_segmented_at_the_mss() {
        let (mut nat, state, isn) = connected();
        let big: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        sock0(&state).lock().unwrap().rx.extend(&big);
        let out = nat.poll();
        assert_eq!(out.len(), 3, "1460 + 1460 + 80");
        let mut seq = isn.wrapping_add(1);
        let mut got = Vec::new();
        for (n, f) in out.iter().enumerate() {
            assert!(ip_len_of(f) <= 1500, "frame {n}: IP length {} over the MTU", ip_len_of(f));
            assert_eq!(seq_of(f), seq, "frame {n}: contiguous");
            assert_eq!(flags_of(f) & PSH != 0, n == 2, "frame {n}: PSH on the last only");
            got.extend_from_slice(payload_of(f));
            seq = seq.wrapping_add(payload_of(f).len() as u32);
        }
        assert_eq!(got, big);
        // the next byte from the host follows on
        sock0(&state).lock().unwrap().rx.extend(b"!");
        assert_eq!(seq_of(&nat.poll()[0]), isn.wrapping_add(3001));
    }

    #[test]
    fn syn_ack_offers_an_mss_of_1460() {
        let (net, _state) = MockNet::new();
        let mut nat = Nat::with_backend(Box::new(net));
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1000, 0, SYN, &[]));
        let tcp = tcp_of(&out[0]);
        assert_eq!(tcp[12] >> 4, 6, "data offset: 6 words");
        assert_eq!(&tcp[20..24], &[2, 4, 0x05, 0xB4], "MSS 1460");
        assert_eq!(ip_len_of(&out[0]), 20 + 24);
    }

    #[test]
    fn lan_allowed_knows_the_ranges() {
        let public = [[93, 184, 216, 34], [1, 1, 1, 1], [172, 15, 0, 1], [172, 32, 0, 1], [100, 128, 0, 1], [11, 0, 0, 1]];
        let lan = [[127, 0, 0, 1], [10, 0, 0, 1], [172, 16, 0, 1], [172, 31, 255, 254], [192, 168, 1, 1], [169, 254, 1, 1], [100, 64, 0, 1]];
        let never = [[0, 0, 0, 0], [224, 0, 0, 1], [239, 255, 255, 255], [240, 0, 0, 1], [255, 255, 255, 255]];
        for ip in public {
            assert!(lan_allowed(ip, false), "{ip:?} is public");
            assert!(lan_allowed(ip, true));
        }
        for ip in lan {
            assert!(!lan_allowed(ip, false), "{ip:?} is the LAN");
            assert!(lan_allowed(ip, true), "{ip:?} with --allow-lan");
        }
        for ip in never {
            assert!(!lan_allowed(ip, false), "{ip:?} is nowhere");
            assert!(!lan_allowed(ip, true), "{ip:?} is nowhere, even with --allow-lan");
        }
    }

    #[test]
    fn no_net_refuses_everything() {
        let mut nat = Nat::with_backend(Box::new(NoNet));
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1000, 0, SYN, &[]));
        assert_eq!(flags_of(&out[0]), RST | ACK);
        assert!(send_udp(&mut nat, &guest_udp(50000, 123, b"ping")).is_empty());
        assert!(nat.poll().is_empty());
    }

    #[test]
    fn guest_ack_releases_the_retransmit_buffer() {
        let (mut nat, state, isn) = connected();
        sock0(&state).lock().unwrap().rx.extend(b"hello");
        let out = nat.poll();
        assert_eq!(payload_of(&out[0]), b"hello");
        assert_eq!(nat.tcp[0].rx.len(), 5, "5 bytes in the retransmit buffer");
        // the guest ACKs the data
        send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, isn.wrapping_add(6), ACK, &[]));
        assert!(nat.tcp[0].rx.is_empty(), "ACKed: retransmit buffer released");
    }

    #[test]
    fn three_duplicate_acks_trigger_retransmit() {
        let (mut nat, state, isn) = connected();
        let big: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        sock0(&state).lock().unwrap().rx.extend(&big);
        let out = nat.poll();
        assert_eq!(out.len(), 3, "3 segments: 1460 + 1460 + 80");
        let first_data_seq = isn.wrapping_add(1);
        let second_seg_seq = first_data_seq.wrapping_add(1460);
        // the guest ACKs the first segment (advances), then sends 3 duplicate ACKs
        let ack_first = first_data_seq.wrapping_add(1460);
        send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, ack_first, ACK, &[]));
        assert!(send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, ack_first, ACK, &[])).is_empty());
        assert!(send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, ack_first, ACK, &[])).is_empty());
        let out = send_tcp(&mut nat, &guest_tcp(40000, 80, 1001, ack_first, ACK, &[]));
        // 3rd dup ACK triggers retransmit
        assert!(!out.is_empty(), "retransmit on 3rd dup ACK");
        assert_eq!(seq_of(&out[0]), second_seg_seq, "retransmit starts at the unACKed byte");
        assert_eq!(payload_of(&out[0]).len(), TCP_MSS.min(3000 - 1460));
    }

    #[test]
    fn full_retransmit_buffer_pauses_host_reads() {
        let (mut nat, state, _isn) = connected();
        let big = vec![0xAAu8; RX_CAP + 4096];
        sock0(&state).lock().unwrap().rx.extend(&big);
        nat.poll();
        assert_eq!(nat.tcp[0].rx.len(), RX_CAP, "capped at RX_CAP");
        assert_eq!(nat.stats.tcp_bytes_in, RX_CAP as u64, "read exactly RX_CAP from the host");
        // the host still has more data; another poll does nothing until the guest ACKs
        nat.poll();
        assert_eq!(nat.stats.tcp_bytes_in, RX_CAP as u64, "no further reads while buffer is full");
    }
}
