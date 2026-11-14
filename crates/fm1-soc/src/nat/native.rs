// SPDX-License-Identifier: MIT

//! The NAT's host side on the host's own sockets (std::net).
//!
//! A connect (and the name resolution before it) runs on a thread of its own: the guest's
//! SYN arrives in the middle of an MMIO write, and the emulator (its audio with it) must
//! not wait for a far host. The thread hands the socket over a channel; `connect_state`
//! looks for it without blocking.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use super::{lan_allowed, ConnectState, HostAddr, HostNet, TcpSock, UdpSock, CONNECT_TIMEOUT_MS};

/// Datagrams kept for a UDP flow whose name is still being resolved.
const UDP_QUEUE_CAP: usize = 16;

/// The host's sockets.
pub struct NativeNet {
    /// This computer's own addresses and its LAN too (`lan_allowed`).
    pub allow_lan: bool,
}

/// `to:port` as a socket address the NAT may connect to: a name is resolved by the
/// host's resolver, IPv4 first; the result is checked, not the name.
fn resolve(to: &HostAddr, port: u16, allow_lan: bool) -> io::Result<SocketAddr> {
    let addr = match to {
        HostAddr::Ip(ip) => SocketAddr::new(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]).into(), port),
        HostAddr::Name(name) => {
            let addrs: Vec<SocketAddr> = (name.as_str(), port).to_socket_addrs()?.collect();
            addrs
                .iter()
                .find(|a| a.is_ipv4())
                .or(addrs.first())
                .copied()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{name}: no address")))?
        }
    };
    let allowed = match addr.ip() {
        std::net::IpAddr::V4(v4) => lan_allowed(v4.octets(), allow_lan),
        std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => lan_allowed(v4.octets(), allow_lan),
            None => !v6.is_multicast() && !v6.is_unspecified() && (allow_lan || !(v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local())),
        },
    };
    if !allowed {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("{addr}: not without --allow-lan")));
    }
    Ok(addr)
}

/// `f` on a thread of its own; its result comes back over the receiver.
fn in_background<T: Send + 'static>(f: impl FnOnce() -> io::Result<T> + Send + 'static) -> io::Result<Receiver<io::Result<T>>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("fm1-nat-connect".into()).spawn(move || {
        let _ = tx.send(f());
    })?;
    Ok(rx)
}

impl HostNet for NativeNet {
    fn tcp_connect(&mut self, to: &HostAddr, port: u16) -> io::Result<Box<dyn TcpSock>> {
        let (to, allow_lan) = (to.clone(), self.allow_lan);
        let rx = in_background(move || {
            let addr = resolve(&to, port, allow_lan)?;
            let s = TcpStream::connect_timeout(&addr, Duration::from_millis(CONNECT_TIMEOUT_MS))?;
            s.set_nonblocking(true)?;
            let _ = s.set_nodelay(true);
            Ok(s)
        })?;
        Ok(Box::new(NativeTcp { pending: Some(rx), stream: None, failed: None }))
    }

    fn udp_open(&mut self, to: &HostAddr, port: u16) -> io::Result<Box<dyn UdpSock>> {
        let open = move |addr: SocketAddr| -> io::Result<UdpSocket> {
            let s = UdpSocket::bind("0.0.0.0:0")?;
            s.set_nonblocking(true)?;
            s.connect(addr)?;
            Ok(s)
        };
        let allow_lan = self.allow_lan;
        match to {
            // (an address needs no resolver: the socket at once)
            HostAddr::Ip(_) => Ok(Box::new(NativeUdp { pending: None, sock: Some(open(resolve(to, port, allow_lan)?)?), queued: Vec::new() })),
            HostAddr::Name(_) => {
                let to = to.clone();
                let rx = in_background(move || open(resolve(&to, port, allow_lan)?))?;
                Ok(Box::new(NativeUdp { pending: Some(rx), sock: None, queued: Vec::new() }))
            }
        }
    }
}

struct NativeTcp {
    pending: Option<Receiver<io::Result<TcpStream>>>,
    stream: Option<TcpStream>,
    failed: Option<io::ErrorKind>,
}

impl TcpSock for NativeTcp {
    fn connect_state(&mut self) -> ConnectState {
        if let Some(kind) = self.failed {
            return ConnectState::Failed(kind);
        }
        if self.stream.is_some() {
            return ConnectState::Ready;
        }
        let Some(rx) = self.pending.as_ref() else { return ConnectState::Failed(io::ErrorKind::NotConnected) };
        let state = match rx.try_recv() {
            Ok(Ok(s)) => {
                self.stream = Some(s);
                ConnectState::Ready
            }
            Ok(Err(e)) => ConnectState::Failed(e.kind()),
            Err(TryRecvError::Empty) => return ConnectState::Pending,
            // (the thread is gone without a word: it panicked)
            Err(TryRecvError::Disconnected) => ConnectState::Failed(io::ErrorKind::Other),
        };
        self.pending = None;
        if let ConnectState::Failed(kind) = state {
            self.failed = Some(kind);
        }
        state
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.stream.as_mut() {
            Some(s) => s.write(buf),
            None => Err(io::ErrorKind::NotConnected.into()),
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.stream.as_mut() {
            Some(s) => s.read(buf),
            None => Err(io::ErrorKind::NotConnected.into()),
        }
    }

    fn shutdown_write(&mut self) {
        if let Some(s) = self.stream.as_ref() {
            let _ = s.shutdown(Shutdown::Write);
        }
    }
}

struct NativeUdp {
    pending: Option<Receiver<io::Result<UdpSocket>>>,
    sock: Option<UdpSocket>,
    /// Datagrams sent while the name was still being resolved.
    queued: Vec<Vec<u8>>,
}

impl NativeUdp {
    /// The socket, once the resolver is done: `None` while it is pending or when it failed.
    fn ready(&mut self) -> Option<&UdpSocket> {
        if self.sock.is_none() {
            if let Some(rx) = self.pending.as_ref() {
                match rx.try_recv() {
                    Ok(Ok(s)) => {
                        self.sock = Some(s);
                        self.pending = None;
                        for d in std::mem::take(&mut self.queued) {
                            let _ = self.sock.as_ref().unwrap().send(&d);
                        }
                    }
                    Ok(Err(_)) | Err(TryRecvError::Disconnected) => self.pending = None,
                    Err(TryRecvError::Empty) => {}
                }
            }
        }
        self.sock.as_ref()
    }
}

impl UdpSock for NativeUdp {
    fn send(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(s) = self.ready() {
            return s.send(buf);
        }
        if self.pending.is_none() {
            return Err(io::ErrorKind::NotConnected.into());
        }
        if self.queued.len() < UDP_QUEUE_CAP {
            self.queued.push(buf.to_vec());
        }
        Ok(buf.len())
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.ready() {
            Some(s) => s.recv(buf),
            None => Err(io::ErrorKind::WouldBlock.into()),
        }
    }
}
