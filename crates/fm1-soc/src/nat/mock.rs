// SPDX-License-Identifier: MIT

//! The NAT's host side scripted for tests: no real sockets. A test keeps the shared
//! [`State`] and drives every socket the NAT opened from there: when its connect
//! completes, what the host "sends", how many bytes a write takes, the end of the stream.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use super::{ConnectState, HostAddr, HostNet, TcpSock, UdpSock};

/// One scripted TCP connection.
pub struct MockSock {
    /// Where the NAT connected to.
    pub to: (HostAddr, u16),
    pub state: ConnectState,
    /// Bytes the host sends to the guest; `read` drains them.
    pub rx: VecDeque<u8>,
    /// The host closed once `rx` is drained.
    pub eof: bool,
    /// Bytes from the guest the host took.
    pub tx: Vec<u8>,
    /// Bytes a single `write` takes at most (None: all of them; Some(0): WouldBlock).
    pub accept_limit: Option<usize>,
    /// The guest's FIN reached the host side.
    pub shutdown: bool,
    /// The next `read` / `write` fails with this.
    pub read_error: Option<io::ErrorKind>,
    pub write_error: Option<io::ErrorKind>,
}

/// One scripted UDP flow.
pub struct MockUdp {
    pub to: (HostAddr, u16),
    /// Datagrams from the guest.
    pub sent: Vec<Vec<u8>>,
    /// Datagrams the host answers with; `recv` takes them one at a time.
    pub inbox: VecDeque<Vec<u8>>,
}

/// Everything the mock host saw, shared with the test.
#[derive(Default)]
pub struct MockState {
    /// Every TCP connection the NAT opened, in order.
    pub tcp: Vec<Arc<Mutex<MockSock>>>,
    /// Every UDP flow the NAT opened, in order.
    pub udp: Vec<Arc<Mutex<MockUdp>>>,
    /// `tcp_connect` fails: an unreachable host.
    pub refuse_tcp: bool,
    /// The state new TCP connections start in (Ready unless set).
    pub initial_state: Option<ConnectState>,
}

pub type State = Arc<Mutex<MockState>>;

/// The mock host: hand the NAT the `MockNet`, keep the `State`.
pub struct MockNet(State);

impl MockNet {
    pub fn new() -> (MockNet, State) {
        let state: State = Default::default();
        (MockNet(state.clone()), state)
    }
}

impl HostNet for MockNet {
    fn tcp_connect(&mut self, to: &HostAddr, port: u16) -> io::Result<Box<dyn TcpSock>> {
        let mut s = self.0.lock().unwrap();
        if s.refuse_tcp {
            return Err(io::ErrorKind::ConnectionRefused.into());
        }
        let sock = Arc::new(Mutex::new(MockSock {
            to: (to.clone(), port),
            state: s.initial_state.unwrap_or(ConnectState::Ready),
            rx: VecDeque::new(),
            eof: false,
            tx: Vec::new(),
            accept_limit: None,
            shutdown: false,
            read_error: None,
            write_error: None,
        }));
        s.tcp.push(sock.clone());
        Ok(Box::new(MockTcp(sock)))
    }

    fn udp_open(&mut self, to: &HostAddr, port: u16) -> io::Result<Box<dyn UdpSock>> {
        let flow = Arc::new(Mutex::new(MockUdp { to: (to.clone(), port), sent: Vec::new(), inbox: VecDeque::new() }));
        self.0.lock().unwrap().udp.push(flow.clone());
        Ok(Box::new(MockUdpSock(flow)))
    }
}

struct MockTcp(Arc<Mutex<MockSock>>);

impl TcpSock for MockTcp {
    fn connect_state(&mut self) -> ConnectState {
        self.0.lock().unwrap().state
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut s = self.0.lock().unwrap();
        if let Some(kind) = s.write_error.take() {
            return Err(kind.into());
        }
        let n = match s.accept_limit {
            Some(0) => return Err(io::ErrorKind::WouldBlock.into()),
            Some(limit) => buf.len().min(limit),
            None => buf.len(),
        };
        s.tx.extend_from_slice(&buf[..n]);
        Ok(n)
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut s = self.0.lock().unwrap();
        if let Some(kind) = s.read_error.take() {
            return Err(kind.into());
        }
        if s.rx.is_empty() {
            return if s.eof { Ok(0) } else { Err(io::ErrorKind::WouldBlock.into()) };
        }
        let n = buf.len().min(s.rx.len());
        for b in buf.iter_mut().take(n) {
            *b = s.rx.pop_front().unwrap();
        }
        Ok(n)
    }

    fn shutdown_write(&mut self) {
        self.0.lock().unwrap().shutdown = true;
    }
}

struct MockUdpSock(Arc<Mutex<MockUdp>>);

impl UdpSock for MockUdpSock {
    fn send(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().sent.push(buf.to_vec());
        Ok(buf.len())
    }

    fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut u = self.0.lock().unwrap();
        let Some(d) = u.inbox.pop_front() else { return Err(io::ErrorKind::WouldBlock.into()) };
        let n = buf.len().min(d.len());
        buf[..n].copy_from_slice(&d[..n]);
        Ok(n)
    }
}
