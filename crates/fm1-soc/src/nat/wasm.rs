// SPDX-License-Identifier: MIT

//! The NAT's host side in the browser: no sockets there, so a guest's HTTP request becomes
//! the page's `fetch()`. A connection is up at once; the request's bytes are collected until
//! its headers are complete, the page is asked for the URL (`fm1_net_fetch`, an import the
//! worklet provides), and the answer (`deliver`, from the page through `fm1_net_response`)
//! is served back as an HTTP/1.1 response followed by the end of the stream. HTTP over TCP
//! only; UDP has nothing behind it.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::io;

use super::{ConnectState, HostAddr, HostNet, TcpSock, UdpSock};

const MAX_REQ: usize = 8192;

#[link(wasm_import_module = "env")]
extern "C" {
    /// The page fetches `url` (UTF-8, `len` bytes) and answers with `fm1_net_response(id, ..)`.
    fn fm1_net_fetch(id: u32, url: *const u8, len: u32);
}

struct Response {
    status: u16,
    ctype: String,
    body: Vec<u8>,
}

thread_local! {
    static RESPONSES: RefCell<HashMap<u32, Response>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
}

/// The page's answer to fetch `id`: `status` 0 is a network error (no such host, CORS, mixed
/// content), anything else the HTTP status with the body as received.
pub fn deliver(id: u32, status: u16, ctype: &str, body: Vec<u8>) {
    RESPONSES.with(|r| r.borrow_mut().insert(id, Response { status, ctype: ctype.to_string(), body }));
}

/// The page's `fetch()` as the host network.
pub struct FetchNet;

impl HostNet for FetchNet {
    fn tcp_connect(&mut self, to: &HostAddr, port: u16) -> io::Result<Box<dyn TcpSock>> {
        let host = match to {
            HostAddr::Name(n) => n.clone(),
            HostAddr::Ip(ip) => format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
        };
        Ok(Box::new(FetchTcp { host, port, req: Vec::new(), id: None, rx: VecDeque::new(), done: false, method_err: false }))
    }

    fn udp_open(&mut self, _to: &HostAddr, _port: u16) -> io::Result<Box<dyn UdpSock>> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

struct FetchTcp {
    host: String,
    port: u16,
    /// The request so far, until its headers are complete.
    req: Vec<u8>,
    /// The fetch asked of the page.
    id: Option<u32>,
    /// The response as HTTP/1.1 bytes, as far as the guest has not read it.
    rx: VecDeque<u8>,
    /// The response is in `rx` (or failed): the end of the stream after it.
    done: bool,
    /// The guest used a method fetch() cannot forward (anything but GET/HEAD).
    method_err: bool,
}

impl FetchTcp {
    /// The request as a URL once its headers are complete: the request line's path on the
    /// name the guest connected to (its Host header when it connected by address).
    fn url(&self) -> Result<String, bool> {
        let end = self.req.windows(4).position(|w| w == b"\r\n\r\n").ok_or(false)?;
        let head = String::from_utf8_lossy(&self.req[..end]).into_owned();
        let mut lines = head.split("\r\n");
        let mut request = lines.next().ok_or(false)?.split_whitespace();
        let method = request.next().ok_or(false)?;
        if !method.eq_ignore_ascii_case("GET") && !method.eq_ignore_ascii_case("HEAD") {
            return Err(true);
        }
        let path = request.next().ok_or(false)?;
        let by_address = self.host.chars().all(|c| c.is_ascii_digit() || c == '.');
        let host_header = lines
            .filter_map(|l| l.split_once(':'))
            .find(|(k, _)| k.trim().eq_ignore_ascii_case("host"))
            .map(|(_, v)| v.trim().to_string());
        let host = match host_header {
            Some(h) if by_address => h,
            _ => self.host.clone(),
        };
        let port = if self.port == 80 { String::new() } else { format!(":{}", self.port) };
        Ok(format!("http://{host}{port}{path}"))
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "",
    }
}

impl TcpSock for FetchTcp {
    fn connect_state(&mut self) -> ConnectState {
        ConnectState::Ready
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.id.is_none() && !self.method_err {
            if self.req.len() + buf.len() > MAX_REQ {
                self.done = true;
                return Err(io::ErrorKind::InvalidData.into());
            }
            self.req.extend_from_slice(buf);
            match self.url() {
                Ok(url) => {
                    let id = NEXT_ID.with(|n| {
                        let id = n.get();
                        n.set(id.wrapping_add(1).max(1));
                        id
                    });
                    self.id = Some(id);
                    unsafe { fm1_net_fetch(id, url.as_ptr(), url.len() as u32) };
                }
                Err(true) => {
                    self.method_err = true;
                }
                Err(false) => {}
            }
        }
        Ok(buf.len())
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.rx.is_empty() && !self.done {
            if self.method_err {
                self.done = true;
                let head = "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                self.rx.extend(head.bytes());
                let n = buf.len().min(self.rx.len());
                for b in buf.iter_mut().take(n) {
                    *b = self.rx.pop_front().unwrap();
                }
                return Ok(n);
            }
            let Some(id) = self.id else { return Err(io::ErrorKind::WouldBlock.into()) };
            let Some(r) = RESPONSES.with(|t| t.borrow_mut().remove(&id)) else { return Err(io::ErrorKind::WouldBlock.into()) };
            self.done = true;
            if r.status == 0 {
                return Err(io::ErrorKind::ConnectionReset.into());
            }
            let ctype = if r.ctype.is_empty() { "application/octet-stream" } else { r.ctype.as_str() };
            let head = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                r.status,
                reason(r.status),
                ctype,
                r.body.len()
            );
            self.rx.extend(head.into_bytes());
            self.rx.extend(r.body);
        }
        if self.rx.is_empty() {
            return if self.done { Ok(0) } else { Err(io::ErrorKind::WouldBlock.into()) };
        }
        let n = buf.len().min(self.rx.len());
        for b in buf.iter_mut().take(n) {
            *b = self.rx.pop_front().unwrap();
        }
        Ok(n)
    }
}

impl Drop for FetchTcp {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            RESPONSES.with(|r| { r.borrow_mut().remove(&id); });
        }
    }
}
