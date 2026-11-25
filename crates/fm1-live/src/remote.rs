//! `--remote PORT`: the running emulator played and read by another program. The port serves a page
//! (the LCD, and `btn` / `enc` to try things from the browser's console) and, on the same port, a
//! WebSocket that takes JSON-RPC calls: press, turn, MIDI in; the LCD, its revision and a hash of
//! it, the LEDs, what the audio has played. A test can open a menu, read what the firmware drew and
//! act on it, which a script of fixed times (FM1_BENCH_KEYS) cannot.
//!
//! The emulation is the live one: its window, its audio and its MIDI inputs go on as without the
//! flag (`--headless` leaves them out: no window, no sound, and the socket the only way in). The
//! server is threads of its own, one per connection. What a connection asks of the machine is run
//! on the emulation thread between two of its runs (`Jobs::run`), so an answer is the machine at
//! one instant and an input lands as the window's do; a connection that sends nothing, or reads
//! nothing, holds up its own thread and no more.
//!
//! Who may connect: the port listens on 127.0.0.1 unless `--host` names another of this computer's
//! addresses. A browser lets any page open a socket to this computer, so every request is held to
//! two rules (`host_ok`, `origin_ok`): its Host is an address or `localhost`, never a name (a page
//! that points its own name at 127.0.0.1 has that name there), and its Origin, which a browser
//! always sends with a WebSocket and another program does not, is this server's own page. Past
//! that there is no password: with `--host` every computer that reaches the address can play and
//! read the emulator.

use std::io::{Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use fm1_soc::{smk, Action, Board};
use serde_json::{json, Value};
use tungstenite::protocol::{Role, WebSocketConfig};
use tungstenite::{Message, WebSocket};

use crate::Machine;

const PAGE: &str = include_str!("remote_page.html");
/// The LCD's side, in pixels.
const LCD: usize = 240;
/// Connections served at once; one more is closed as it arrives.
const MAX_CLIENTS: usize = 8;
/// A request's head: how long it may take to arrive, and how long it may be.
const HEAD_WAIT: Duration = Duration::from_secs(2);
const HEAD_MAX: usize = 8192;
/// A client that takes nothing for this long is dropped (a write that waits on it ends).
const WRITE_WAIT: Duration = Duration::from_secs(5);
/// The longest message a client may send: the calls are a line each.
const MESSAGE_MAX: usize = 64 << 10;
/// `FM1.streamLcd`: a frame at most this often, and the status line this often.
const FRAME_EVERY: Duration = Duration::from_millis(33);
const STATUS_EVERY: Duration = Duration::from_millis(500);
/// `FM1.pressButton`: how long the press lasts when the call does not say (emulated ms).
const PRESS_MS: i64 = 100;
/// `FM1.sendMidi`: the bytes of one call (the jack's own queue drops what it cannot take in a second).
const MIDI_MAX: usize = 1024;

type Job = Box<dyn FnOnce(&mut Machine) + Send>;

/// The emulation thread's end of the server: what the connections have asked since the last call.
pub struct Jobs(mpsc::Receiver<Job>);

impl Jobs {
    /// Run what is waiting. A connection waits for its answer before it asks again, so no more
    /// than MAX_CLIENTS jobs wait at a time, and each is a copy of the LCD at most.
    pub fn run(&self, m: &mut Machine) {
        while let Ok(job) = self.0.try_recv() {
            job(m);
        }
    }
}

/// What the firmware has played (`FM1.getAudioMetrics`): the emulation thread adds what it hands
/// to the audio device.
#[derive(Default)]
pub struct Heard {
    /// stereo frames since boot
    pub frames: u64,
    /// the largest sample since the last `FM1.getAudioMetrics`, 0..=32768
    pub peak: u16,
}

impl Heard {
    pub fn add(&mut self, audio: &[i16]) {
        self.frames += audio.len() as u64 / 2;
        self.peak = audio.iter().fold(self.peak, |p, s| p.max(s.unsigned_abs()));
    }
}

/// A connection's way to the machine and to what the emulation thread publishes for the window.
#[derive(Clone)]
struct Link {
    jobs: mpsc::Sender<Job>,
    name: String,
    board: Board,
    /// the LEDs as the window draws them (`Periph::led_shown`, every 16 ms; the SMK-37's in the first NLED)
    leds: Arc<Mutex<[f32; 41]>>,
    /// the window's status line; "halted: ..." once the machine has halted
    status: Arc<Mutex<String>>,
}

impl Link {
    /// `f`'s answer from the emulation thread. None once that thread has ended (the machine
    /// halted, or the program is closing): the job is dropped unrun, and its sender with it.
    fn ask<T: Send + 'static>(&self, f: impl FnOnce(&mut Machine) -> T + Send + 'static) -> Option<T> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.jobs.send(Box::new(move |m| { let _ = tx.send(f(m)); })).ok()?;
        rx.recv().ok()
    }

    fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }

    /// How many keys and buttons, and how many encoders, the board has: an id past them is
    /// refused here, where `Periph::apply` would pass over it without a word.
    fn controls(&self) -> (i64, i64) {
        match self.board {
            Board::Smk37 => ((smk::NKEY + smk::NBTN) as i64, smk::NENC as i64),
            _ => (41, 7),
        }
    }
}

/// Why a call got no result, as JSON-RPC's error.
#[derive(Debug, PartialEq)]
enum No {
    Method,
    Params(String),
    Halted(String),
}

impl No {
    fn json(&self) -> Value {
        match self {
            No::Method => json!({"code": -32601, "message": "no such method"}),
            No::Params(why) => json!({"code": -32602, "message": why}),
            No::Halted(status) => json!({"code": -32000, "message": status}),
        }
    }
}

/// Parameter `k`: a whole number from `lo` to `hi`.
fn int(p: &Value, k: &str, lo: i64, hi: i64) -> Result<i64, No> {
    p.get(k).and_then(Value::as_i64).filter(|v| (lo..=hi).contains(v)).ok_or_else(|| No::Params(format!("{k}: a whole number from {lo} to {hi}")))
}

/// A part of the LCD, all of it inside the LCD.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

impl Rect {
    /// `x`, `y` (0 when left out), `w`, `h` (to the LCD's edge when left out). Each is held to the
    /// LCD by itself before any two are added: a size picked to wrap, or to reserve gigabytes,
    /// is a refusal.
    fn from(p: &Value) -> Result<Rect, No> {
        let get = |k: &str, lo: i64, hi: i64, or: i64| if p.get(k).is_some() { int(p, k, lo, hi) } else { Ok(or) };
        let side = LCD as i64;
        let (x, y) = (get("x", 0, side - 1, 0)?, get("y", 0, side - 1, 0)?);
        let (w, h) = (get("w", 1, side - x, side - x)?, get("h", 1, side - y, side - y)?);
        Ok(Rect { x: x as usize, y: y as usize, w: w as usize, h: h as usize })
    }

    /// Its pixels, row by row.
    fn of(&self, fb: &[u16]) -> Vec<u16> {
        (self.y..self.y + self.h).flat_map(|row| fb[row * LCD + self.x..][..self.w].iter().copied()).collect()
    }
}

fn rgb565_to_rgba(px: u16) -> [u8; 4] {
    let r = ((px >> 11) & 0x1F) as u8;
    let g = ((px >> 5) & 0x3F) as u8;
    let b = (px & 0x1F) as u8;
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2), 255]
}

fn rgba(px: &[u16]) -> Vec<u8> {
    px.iter().flat_map(|&p| rgb565_to_rgba(p)).collect()
}

/// `FM1.lcdHash`: 32-bit FNV-1a over the pixels as the LCD holds them (RGB565, low byte first), so
/// any pixel that changes changes it.
fn lcd_hash(px: &[u16]) -> u32 {
    px.iter().flat_map(|p| p.to_le_bytes()).fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(CHARS[(n >> 18) as usize & 63] as char);
        out.push(CHARS[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { CHARS[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { CHARS[n as usize & 63] as char } else { '=' });
    }
    out
}

/// A key or button goes down or up now.
fn key(link: &Link, p: &Value) -> Result<Value, No> {
    let id = int(p, "id", 0, link.controls().0 - 1)? as u8;
    let down = p.get("down").and_then(Value::as_bool).ok_or_else(|| No::Params("down: true or false".into()))?;
    link.ask(move |m| m.soc.periph.apply(&Action::Key { id, down })).map(|_| json!({})).ok_or_else(|| No::Halted(link.status()))
}

/// Encoder `p[which]` turns by `clicks` detents.
fn turn(link: &Link, p: &Value, which: &str) -> Result<Value, No> {
    let enc = int(p, which, 0, link.controls().1 - 1)? as u8;
    let clicks = int(p, "clicks", -100, 100)? as i32;
    link.ask(move |m| m.soc.periph.apply(&Action::Enc { enc, clicks })).map(|_| json!({})).ok_or_else(|| No::Halted(link.status()))
}

/// The bytes of `data` into the MIDI IN jack.
fn midi(link: &Link, p: &Value) -> Result<Value, No> {
    let no = || No::Params(format!("data: up to {MIDI_MAX} bytes (whole numbers from 0 to 255)"));
    let data = p.get("data").and_then(Value::as_array).filter(|d| d.len() <= MIDI_MAX).ok_or_else(no)?;
    let bytes = data.iter().map(|b| b.as_u64().and_then(|b| u8::try_from(b).ok())).collect::<Option<Vec<u8>>>().ok_or_else(no)?;
    link.ask(move |m| bytes.iter().for_each(|&b| m.soc.periph.apply(&Action::Midi { b }))).map(|_| json!({})).ok_or_else(|| No::Halted(link.status()))
}

/// One JSON-RPC call (`FM1.streamLcd`, which is the connection's own, is `answer`'s).
fn call(link: &Link, method: &str, p: &Value) -> Result<Value, No> {
    let halted = || No::Halted(link.status());
    match method {
        // (the two that say whether it runs answer once it has halted too)
        "FM1.getInfo" => Ok(match link.ask(|m| (m.soc.periph.ms(), m.soc.periph.hz)) {
            Some((ms, hz)) => json!({
                "firmware": link.name, "board": if link.board == Board::Smk37 { "SMK-37" } else { "FM-1" }, "mhz": hz / 1_000_000,
                "status": if ms > 0 { "running" } else { "booting" }, "uptime_ms": ms, "halted": false,
            }),
            None => json!({"firmware": link.name, "status": link.status(), "halted": true}),
        }),
        "FM1.getEngineState" => Ok(match link.ask(|m| m.soc.periph.ms()) {
            Some(ms) => json!({"halted": false, "ms": ms}),
            None => json!({"halted": true}),
        }),
        "FM1.pressButton" => {
            let id = int(p, "id", 0, link.controls().0 - 1)? as u8;
            let ms = if p.get("ms").is_some() { int(p, "ms", 1, 60_000)? } else { PRESS_MS } as u64;
            // down now, and its release in its place among what is scheduled (`Periph::schedule`): a
            // call after this one is not kept waiting behind it
            link.ask(move |m| {
                let per = &mut m.soc.periph;
                per.apply(&Action::Key { id, down: true });
                per.schedule(per.cycles + per.hz / 1000 * ms, Action::Key { id, down: false });
            }).map(|_| json!({})).ok_or_else(halted)
        }
        "FM1.turnEncoder" => turn(link, p, "encoder"),
        "FM1.sendMidi" => midi(link, p),
        "FM1.getLeds" => {
            let leds = *link.leds.lock().unwrap();
            Ok(json!({"leds": leds[..if link.board == Board::Smk37 { smk::NLED } else { leds.len() }]}))
        }
        // (the emulation thread copies the pixels; turning them into RGBA and base64 is this thread's)
        "FM1.getLcd" => {
            let r = Rect::from(p)?;
            let px = link.ask(move |m| r.of(&m.soc.lcd.fb)).ok_or_else(halted)?;
            Ok(json!({"x": r.x, "y": r.y, "w": r.w, "h": r.h, "data": base64_encode(&rgba(&px))}))
        }
        "FM1.lcdHash" => {
            let r = Rect::from(p)?;
            let px = link.ask(move |m| r.of(&m.soc.lcd.fb)).ok_or_else(halted)?;
            Ok(json!({"hash": lcd_hash(&px)}))
        }
        "FM1.lcdRevision" => link.ask(|m| m.soc.lcd.revision).map(|r| json!({"revision": r})).ok_or_else(halted),
        "FM1.getAudioMetrics" => link
            .ask(|m| (m.heard.frames, std::mem::take(&mut m.heard.peak), m.soc.periph.audio_halves))
            .map(|(frames, peak, halves)| json!({"frames": frames, "peak": peak, "audio_halves": halves}))
            .ok_or_else(halted),
        _ => Err(No::Method),
    }
}

/// What to send back for the text message `text`, if anything. A message with a `method` is a
/// JSON-RPC call and is answered; one with a `type` is an input event (the page's mouse: a key
/// down, and later up), which is taken or not and has no answer.
fn answer(link: &Link, text: &str, stream: &mut bool) -> Option<String> {
    let reply = |id: Value, got: Result<Value, Value>| {
        let (k, v) = match got { Ok(v) => ("result", v), Err(v) => ("error", v) };
        Some(json!({"jsonrpc": "2.0", "id": id, k: v}).to_string())
    };
    let Ok(v) = serde_json::from_str::<Value>(text) else { return reply(Value::Null, Err(json!({"code": -32700, "message": "not JSON"}))) };
    let Some(method) = v.get("method").and_then(Value::as_str) else {
        let _ = match v.get("type").and_then(Value::as_str) {
            Some("key") => key(link, &v),
            Some("enc") => turn(link, &v, "enc"),
            Some("midi") => midi(link, &v),
            _ => Err(No::Method),
        };
        return None;
    };
    let none = json!({});
    let p = v.get("params").unwrap_or(&none);
    let got = if method == "FM1.streamLcd" {
        p.get("on").and_then(Value::as_bool).map(|on| { *stream = on; json!({}) }).ok_or_else(|| No::Params("on: true or false".into()))
    } else {
        call(link, method, p)
    };
    reply(v.get("id").cloned().unwrap_or(Value::Null), got.map_err(|no| no.json()))
}

/// A request's head: its method and path, and its headers with their names in lower case.
#[derive(Debug)]
struct Head {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn parse(head: &[u8]) -> Option<Head> {
        let mut lines = std::str::from_utf8(head).ok()?.split("\r\n");
        let mut first = lines.next()?.split(' ');
        let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
        let headers = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
        Some(Head { method, path, headers })
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// The head from the socket, a byte at a time so that nothing after it is taken (a WebSocket's
    /// first frame may follow at once). None when it is not all there within HEAD_WAIT or HEAD_MAX.
    fn read(s: &mut TcpStream) -> Option<Head> {
        let until = Instant::now() + HEAD_WAIT;
        let (mut head, mut b) = (Vec::new(), [0u8]);
        while !head.ends_with(b"\r\n\r\n") {
            let left = until.checked_duration_since(Instant::now()).filter(|d| !d.is_zero())?;
            s.set_read_timeout(Some(left)).ok()?;
            if head.len() >= HEAD_MAX || s.read(&mut b).ok()? == 0 {
                return None;
            }
            head.push(b[0]);
        }
        Head::parse(&head)
    }
}

/// The Host a request names is this computer by address, or `localhost`. A page elsewhere can
/// give its own name this computer's address (DNS rebinding) and would then be this server's own
/// origin to the browser; its requests carry that name here, and an address cannot be pointed
/// anywhere else.
fn host_ok(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(name, _port)| name),
    };
    name.eq_ignore_ascii_case("localhost") || name.parse::<IpAddr>().is_ok()
}

/// A WebSocket's Origin is this server's own page, or there is none (not a browser).
fn origin_ok(origin: Option<&str>, host: &str) -> bool {
    origin.is_none_or(|o| o.eq_ignore_ascii_case(&format!("http://{host}")))
}

/// An HTTP answer, and the connection closed once the client has it: the request was read whole
/// (`Head::read`), so the close does not reset the connection under the body.
fn respond(s: &mut TcpStream, status: &str, kind: &str, body: &[u8]) {
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
    if s.write_all(head.as_bytes()).and_then(|_| s.write_all(body)).is_ok() {
        let _ = s.shutdown(Shutdown::Write);
        // (until the client closes its side, for a moment at most)
        let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = s.read(&mut [0u8; 64]);
    }
}

fn refuse(s: &mut TcpStream, status: &str, why: &str) {
    respond(s, status, "text/plain; charset=utf-8", format!("{why}\n").as_bytes());
}

/// One connection: the page for a GET, or a WebSocket served until it closes.
fn client(mut s: TcpStream, link: &Link) {
    let _ = s.set_write_timeout(Some(WRITE_WAIT));
    let _ = s.set_nodelay(true);
    let Some(head) = Head::read(&mut s) else { return };
    let host = head.get("host").unwrap_or("");
    if !host_ok(host) {
        return refuse(&mut s, "403 Forbidden", "fm1-live --remote answers to an address or localhost, not to a name");
    }
    if head.method != "GET" {
        return refuse(&mut s, "405 Method Not Allowed", "GET only");
    }
    if !head.get("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
        return match head.path.split('?').next() {
            Some("/") | Some("/index.html") => respond(&mut s, "200 OK", "text/html; charset=utf-8", PAGE.as_bytes()),
            _ => refuse(&mut s, "404 Not Found", "the page is at /"),
        };
    }
    if !origin_ok(head.get("origin"), host) {
        return refuse(&mut s, "403 Forbidden", "fm1-live --remote takes a WebSocket from its own page or from a program, not from another site's page");
    }
    let Some(key) = head.get("sec-websocket-key") else { return refuse(&mut s, "400 Bad Request", "no Sec-WebSocket-Key") };
    let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
    if s.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).is_err() {
        return;
    }
    let config = WebSocketConfig { max_message_size: Some(MESSAGE_MAX), max_frame_size: Some(MESSAGE_MAX), ..Default::default() };
    serve(WebSocket::from_raw_socket(s, Role::Server, Some(config)), link);
}

/// A WebSocket until it closes: its calls answered as they come, and with `FM1.streamLcd` on the LCD
/// as a binary message (RGBA, 240 x 240) when it has changed and the status line twice a second.
/// The frames are fetched as they are sent, one at a time, so a client that reads slowly gets
/// fewer of them and none are kept for it.
fn serve(mut ws: WebSocket<TcpStream>, link: &Link) {
    let mut stream = false;
    let mut shown = None;
    let (mut frame_at, mut status_at) = (Instant::now(), Instant::now());
    loop {
        // (a read that waits: for the next frame's time while streaming, else for the next call)
        let wait = if stream { FRAME_EVERY.saturating_sub(frame_at.elapsed()).max(Duration::from_millis(1)) } else { STATUS_EVERY };
        let _ = ws.get_ref().set_read_timeout(Some(wait));
        match ws.read() {
            Ok(Message::Text(text)) => {
                if let Some(reply) = answer(link, &text, &mut stream) {
                    if ws.send(Message::Text(reply)).is_err() {
                        return;
                    }
                }
            }
            Ok(_) => {}
            // (no message within the wait: WouldBlock on Unix, TimedOut on Windows)
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
        if !stream {
            shown = None;
            continue;
        }
        if frame_at.elapsed() >= FRAME_EVERY {
            frame_at = Instant::now();
            let frame = link.ask(move |m| (shown != Some(m.soc.lcd.revision)).then(|| (m.soc.lcd.revision, m.soc.lcd.fb.clone())));
            if let Some(Some((rev, fb))) = frame {
                shown = Some(rev);
                if ws.send(Message::Binary(rgba(&fb))).is_err() {
                    return;
                }
            }
        }
        if status_at.elapsed() >= STATUS_EVERY {
            status_at = Instant::now();
            if ws.send(Message::Text(json!({"type": "status", "text": link.status()}).to_string())).is_err() {
                return;
            }
        }
    }
}

/// Listen on `host`:`port` (port 0: one the system picks) and serve from threads of this one's own.
/// Returns the address it listens on and the emulation thread's end.
pub fn start(host: IpAddr, port: u16, name: &str, board: Board, leds: Arc<Mutex<[f32; 41]>>, status: Arc<Mutex<String>>) -> std::io::Result<(SocketAddr, Jobs)> {
    let listener = TcpListener::bind((host, port))?;
    let addr = listener.local_addr()?;
    let (jobs, rx) = mpsc::channel();
    let link = Link { jobs, name: name.to_string(), board, leds, status };
    std::thread::spawn(move || {
        let clients = Arc::new(AtomicUsize::new(0));
        for s in listener.incoming() {
            let Ok(s) = s else {
                std::thread::sleep(Duration::from_millis(50)); // (out of descriptors: not a busy loop)
                continue;
            };
            if clients.fetch_add(1, Ordering::SeqCst) >= MAX_CLIENTS {
                clients.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let (link, clients) = (link.clone(), clients.clone());
            std::thread::spawn(move || {
                client(s, &link);
                clients.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    Ok((addr, Jobs(rx)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_an_address_or_localhost_and_never_a_name() {
        for ok in ["127.0.0.1:9000", "127.0.0.1", "localhost:9000", "LOCALHOST", "[::1]:9000", "192.168.1.20:9000", "0.0.0.0:1"] {
            assert!(host_ok(ok), "{ok}");
        }
        for no in ["", "evil.example:9000", "evil.example", "127.0.0.1.evil.example:9000", "localhost.evil.example", "mac.local:9000", "[evil]:9000"] {
            assert!(!host_ok(no), "{no}");
        }
    }

    #[test]
    fn a_websocket_comes_from_this_servers_page_or_from_no_page() {
        assert!(origin_ok(None, "127.0.0.1:9000"));
        assert!(origin_ok(Some("http://127.0.0.1:9000"), "127.0.0.1:9000"));
        assert!(origin_ok(Some("http://localhost:9000"), "localhost:9000"));
        for no in ["https://evil.example", "http://127.0.0.1:9001", "http://localhost:9000", "null", "", "https://127.0.0.1:9000"] {
            assert!(!origin_ok(Some(no), "127.0.0.1:9000"), "{no}");
        }
    }

    #[test]
    fn a_head_is_its_method_its_path_and_its_headers_whatever_their_case() {
        let h = Head::parse(b"GET /?x=1 HTTP/1.1\r\nHOST: 127.0.0.1:9\r\nupgrade:  WebSocket \r\nSec-WebSocket-Key: abc==\r\n\r\n").unwrap();
        assert_eq!((h.method.as_str(), h.path.as_str()), ("GET", "/?x=1"));
        assert_eq!(h.get("host"), Some("127.0.0.1:9"));
        assert_eq!(h.get("upgrade"), Some("WebSocket"));
        assert_eq!(h.get("sec-websocket-key"), Some("abc=="));
        assert_eq!(h.get("origin"), None);
        assert!(Head::parse(b"\r\n\r\n").is_none());
        assert!(Head::parse(&[0xff, 0xfe, b'\r', b'\n', b'\r', b'\n']).is_none());
    }

    #[test]
    fn a_part_of_the_lcd_is_inside_the_lcd() {
        let r = |v: Value| Rect::from(&v);
        assert_eq!(r(json!({})), Ok(Rect { x: 0, y: 0, w: 240, h: 240 }));
        assert_eq!(r(json!({"x": 200, "y": 239})), Ok(Rect { x: 200, y: 239, w: 40, h: 1 }));
        assert_eq!(r(json!({"x": 10, "y": 20, "w": 230, "h": 220})), Ok(Rect { x: 10, y: 20, w: 230, h: 220 }));
        for no in [json!({"w": 100000, "h": 100000}), json!({"w": 241}), json!({"x": 240}), json!({"x": 1, "w": 240}), json!({"y": 239, "h": 2}),
                   json!({"w": 0}), json!({"x": -1}), json!({"h": 1.5}), json!({"w": "240"}), json!({"y": u64::MAX, "h": 2}), json!({"h": u64::MAX})] {
            assert!(matches!(r(no.clone()), Err(No::Params(_))), "{no}");
        }
        // the pixels are the rectangle's, row by row
        let fb: Vec<u16> = (0..LCD * LCD).map(|i| i as u16).collect();
        assert_eq!(Rect { x: 238, y: 1, w: 2, h: 2 }.of(&fb), [478, 479, 718, 719]);
        assert_eq!(Rect { x: 0, y: 0, w: 240, h: 240 }.of(&fb), fb);
    }

    #[test]
    fn the_hash_moves_with_any_pixel_and_base64_pads() {
        let a = vec![0x0000u16; 16];
        for (i, px) in [(0, 0x0001u16), (7, 0x0020), (15, 0x0800), (3, 0x8000)] {
            let mut b = a.clone();
            b[i] = px;
            assert_ne!(lcd_hash(&a), lcd_hash(&b), "pixel {i} = {px:#06x}");
        }
        assert_eq!(lcd_hash(&[]), 0x811c_9dc5);
        for (raw, text) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64_encode(raw.as_bytes()), text);
        }
        assert_eq!(rgba(&[0xffff, 0x0000, 0xf800]), [255, 255, 255, 255, 0, 0, 0, 255, 255, 0, 0, 255]);
    }

    /// A link whose jobs nobody runs any more: the machine has halted.
    fn halted_link(board: Board) -> Link {
        let (jobs, _) = mpsc::channel();
        Link { jobs, name: "Test".into(), board, leds: Arc::new(Mutex::new([0.25; 41])), status: Arc::new(Mutex::new("halted: at 0x1234".into())) }
    }

    #[test]
    fn what_a_call_asks_for_is_checked_before_the_machine_sees_it() {
        let link = halted_link(Board::Fm1);
        let params = |method: &str, p: Value| matches!(call(&link, method, &p), Err(No::Params(_)));
        assert!(params("FM1.pressButton", json!({})));
        assert!(params("FM1.pressButton", json!({"id": 41})));
        assert!(params("FM1.pressButton", json!({"id": 4294967308u64})), "an id that would wrap to 12");
        assert!(params("FM1.pressButton", json!({"id": 12, "ms": 0})));
        assert!(params("FM1.turnEncoder", json!({"encoder": 7, "clicks": 1})), "the FM-1 has 7");
        assert!(params("FM1.turnEncoder", json!({"encoder": 0, "clicks": 1i64 << 40})));
        assert!(params("FM1.turnEncoder", json!({"encoder": 0})));
        assert!(params("FM1.sendMidi", json!({"data": [144, 60, 256]})));
        assert!(params("FM1.sendMidi", json!({"data": [144, -1]})));
        assert!(params("FM1.sendMidi", json!({"data": vec![0; MIDI_MAX + 1]})));
        assert!(params("FM1.sendMidi", json!({"data": "90 3c 64"})));
        assert!(params("FM1.getLcd", json!({"w": 100000, "h": 100000})));
        assert!(params("FM1.lcdHash", json!({"x": 240})));
        assert_eq!(call(&link, "FM1.nothing", &json!({})), Err(No::Method));
        // an SMK-37 has more of both
        let smk = halted_link(Board::Smk37);
        assert_eq!(call(&smk, "FM1.turnEncoder", &json!({"encoder": 7, "clicks": 1})), Err(No::Halted("halted: at 0x1234".into())));
        assert_eq!(call(&smk, "FM1.pressButton", &json!({"id": 54})), Err(No::Halted("halted: at 0x1234".into())));
        assert!(matches!(call(&smk, "FM1.pressButton", &json!({"id": 55})), Err(No::Params(_))));
    }

    #[test]
    fn a_halted_machine_says_so() {
        let link = halted_link(Board::Fm1);
        assert_eq!(call(&link, "FM1.getInfo", &json!({})), Ok(json!({"firmware": "Test", "status": "halted: at 0x1234", "halted": true})));
        assert_eq!(call(&link, "FM1.getEngineState", &json!({})), Ok(json!({"halted": true})));
        assert_eq!(call(&link, "FM1.lcdRevision", &json!({})), Err(No::Halted("halted: at 0x1234".into())));
        assert_eq!(call(&link, "FM1.pressButton", &json!({"id": 12})), Err(No::Halted("halted: at 0x1234".into())));
        // the LEDs are the last the window drew
        assert_eq!(call(&link, "FM1.getLeds", &json!({})).unwrap()["leds"].as_array().unwrap().len(), 41);
        assert_eq!(call(&halted_link(Board::Smk37), "FM1.getLeds", &json!({})).unwrap()["leds"].as_array().unwrap().len(), smk::NLED);
    }

    #[test]
    fn a_message_is_a_call_with_its_answer_or_an_input_without_one() {
        let link = halted_link(Board::Fm1);
        let mut stream = false;
        let got = |text: &str, stream: &mut bool| answer(&link, text, stream).map(|s| serde_json::from_str::<Value>(&s).unwrap());
        assert_eq!(got(r#"{"jsonrpc":"2.0","id":7,"method":"FM1.getEngineState"}"#, &mut stream), Some(json!({"jsonrpc": "2.0", "id": 7, "result": {"halted": true}})));
        assert_eq!(got(r#"{"id":"a","method":"FM1.nothing","params":{}}"#, &mut stream), Some(json!({"jsonrpc": "2.0", "id": "a", "error": {"code": -32601, "message": "no such method"}})));
        assert_eq!(got("{not json", &mut stream).unwrap()["error"]["code"], -32700);
        assert_eq!(got(r#"{"id":1,"method":"FM1.getLcd","params":{"w":100000}}"#, &mut stream).unwrap()["error"]["code"], -32602);
        // the stream is the connection's: on and off with no machine asked
        assert_eq!(got(r#"{"id":2,"method":"FM1.streamLcd","params":{"on":true}}"#, &mut stream).unwrap()["result"], json!({}));
        assert!(stream);
        assert_eq!(got(r#"{"id":3,"method":"FM1.streamLcd","params":{}}"#, &mut stream).unwrap()["error"]["code"], -32602);
        assert!(stream);
        assert_eq!(got(r#"{"id":4,"method":"FM1.streamLcd","params":{"on":false}}"#, &mut stream).unwrap()["result"], json!({}));
        assert!(!stream);
        // input events: taken or not, never answered
        for text in [r#"{"type":"key","id":12,"down":true}"#, r#"{"type":"key","id":300,"down":true}"#, r#"{"type":"enc","enc":3,"clicks":5}"#,
                     r#"{"type":"midi","data":[144,60,100]}"#, r#"{"type":"other"}"#, "{}", "[]", "7"] {
            assert_eq!(got(text, &mut stream), None, "{text}");
        }
    }

    #[test]
    fn what_was_played_is_counted_in_frames_with_its_peak() {
        let mut h = Heard::default();
        h.add(&[0, 0, 100, -200]);
        h.add(&[i16::MIN, 5]);
        assert_eq!((h.frames, h.peak), (3, 32768));
    }
}
