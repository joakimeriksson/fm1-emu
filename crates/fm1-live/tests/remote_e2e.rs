//! `fm1-live --headless --remote 0` with Felucca, driven over its socket as a test tool would.
//! Needs reference/firmwares/felucca.fwsc (tools/fetch_firmwares.mjs felucca); skipped without it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

const FWSC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../reference/firmwares/felucca.fwsc");

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

fn have_firmware() -> bool {
    let have = std::path::Path::new(FWSC).exists();
    if !have {
        eprintln!("no {FWSC}: skipped");
    }
    have
}

/// A headless fm1-live with the remote on a port the system picked, killed when the test ends.
struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fm1-live"))
            .args(["--fwsc", FWSC, "--headless", "--remote", "0"])
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn fm1-live");
        // the port is in the line the server prints once it listens; the rest of what the run says is
        // read too, so that it never waits on a full pipe
        let mut said = BufReader::new(child.stderr.take().unwrap());
        let mut line = String::new();
        let port = loop {
            line.clear();
            assert!(said.read_line(&mut line).unwrap() > 0, "fm1-live ended before it listened");
            if let Some(rest) = line.strip_prefix("remote: http://127.0.0.1:") {
                break rest.split(' ').next().unwrap().parse().unwrap();
            }
        };
        std::thread::spawn(move || std::io::copy(&mut said, &mut std::io::sink()));
        Self { child, port }
    }

    fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn ws(&self) -> Ws {
        let (ws, _) = connect(format!("ws://{}", self.addr())).expect("the WebSocket");
        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        }
        ws
    }

    /// A request written as it is, and all that comes back until the server closes.
    fn raw(&self, request: &str) -> String {
        let mut s = TcpStream::connect(self.addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut got = Vec::new();
        let _ = s.read_to_end(&mut got);
        String::from_utf8_lossy(&got).into_owned()
    }

    /// The first line of the answer to a WebSocket's opening request with these headers.
    fn handshake(&self, headers: &str) -> String {
        let mut s = TcpStream::connect(self.addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.write_all(format!("GET / HTTP/1.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n{headers}\r\n").as_bytes()).unwrap();
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).unwrap();
        line.trim_end().to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The whole answer to a call: `result` or `error` in it. Binary messages on the way are counted.
fn call(ws: &mut Ws, method: &str, params: serde_json::Value, frames: &mut Vec<Vec<u8>>) -> serde_json::Value {
    ws.send(Message::Text(serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string())).unwrap();
    loop {
        match ws.read().unwrap_or_else(|e| panic!("{method}: {e}")) {
            Message::Text(t) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                if v.get("id") == Some(&serde_json::json!(1)) {
                    return v;
                }
            }
            Message::Binary(b) => frames.push(b),
            _ => {}
        }
    }
}

fn rpc(ws: &mut Ws, method: &str, params: serde_json::Value) -> serde_json::Value {
    let v = call(ws, method, params, &mut Vec::new());
    assert!(v.get("error").is_none(), "{method}: {v}");
    v["result"].clone()
}

fn ms(ws: &mut Ws) -> u64 {
    rpc(ws, "FM1.getInfo", serde_json::json!({}))["uptime_ms"].as_u64().unwrap()
}

fn hash(ws: &mut Ws) -> u64 {
    rpc(ws, "FM1.lcdHash", serde_json::json!({}))["hash"].as_u64().unwrap()
}

fn peak(ws: &mut Ws) -> u64 {
    rpc(ws, "FM1.getAudioMetrics", serde_json::json!({}))["peak"].as_u64().unwrap()
}

/// The LCD has stopped changing, and Felucca's opening screen (which rests too, for a while) is over.
fn booted(ws: &mut Ws) {
    let until = Instant::now() + Duration::from_secs(30);
    let mut last = (0, Instant::now());
    loop {
        assert!(Instant::now() < until, "the LCD never came to rest");
        let rev = rpc(ws, "FM1.lcdRevision", serde_json::json!({}))["revision"].as_u64().unwrap();
        if rev != last.0 {
            last = (rev, Instant::now());
        } else if rev > 0 && last.1.elapsed() > Duration::from_millis(400) && ms(ws) >= 1500 {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `what` holds within `within`.
fn soon(within: Duration, why: &str, mut what: impl FnMut() -> bool) {
    let until = Instant::now() + within;
    while !what() {
        assert!(Instant::now() < until, "{why}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// How fast the emulation runs over `over`: emulated ms per ms.
fn pace(ws: &mut Ws, over: Duration) -> f64 {
    let (t0, e0) = (Instant::now(), ms(ws));
    std::thread::sleep(over);
    let e1 = ms(ws);
    (e1 - e0) as f64 / t0.elapsed().as_millis() as f64
}

#[test]
fn the_page_arrives_whole_and_other_sites_are_refused() {
    if !have_firmware() { return; }
    let srv = Server::start();
    let host = srv.addr();
    // the page, each time, all of it
    for i in 0..20 {
        let got = srv.raw(&format!("GET / HTTP/1.1\r\nHost: {host}\r\nUser-Agent: test\r\nAccept: */*\r\n\r\n"));
        let (head, body) = got.split_once("\r\n\r\n").unwrap_or_else(|| panic!("try {i}: {got:?}"));
        assert!(head.starts_with("HTTP/1.1 200 OK"), "try {i}: {head}");
        let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).unwrap().parse().unwrap();
        assert_eq!(body.len(), len, "try {i}");
        assert!(body.contains("<canvas") && body.contains("pressButton"), "try {i}");
    }
    assert!(srv.raw(&format!("GET /other HTTP/1.1\r\nHost: {host}\r\n\r\n")).starts_with("HTTP/1.1 404"));
    assert!(srv.raw(&format!("POST / HTTP/1.1\r\nHost: {host}\r\n\r\n")).starts_with("HTTP/1.1 405"));
    // a name that was pointed at this computer: not the page, not a socket
    assert!(srv.raw(&format!("GET / HTTP/1.1\r\nHost: evil.example:{}\r\n\r\n", srv.port)).starts_with("HTTP/1.1 403"));
    assert!(srv.raw("GET / HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 403"), "no Host at all");
    assert_eq!(srv.handshake(&format!("Host: evil.example:{0}\r\nOrigin: http://evil.example:{0}\r\n", srv.port)), "HTTP/1.1 403 Forbidden");
    // a WebSocket: from a program (no Origin) and from this server's own page, not from another site's
    assert_eq!(srv.handshake(&format!("Host: {host}\r\n")), "HTTP/1.1 101 Switching Protocols");
    assert_eq!(srv.handshake(&format!("Host: {host}\r\nOrigin: http://{host}\r\n")), "HTTP/1.1 101 Switching Protocols");
    assert_eq!(srv.handshake(&format!("host: {host}\r\nUPGRADE: WebSocket\r\n")), "HTTP/1.1 101 Switching Protocols", "headers in any case");
    for origin in ["https://evil.example", "http://127.0.0.1:1", "null"] {
        assert_eq!(srv.handshake(&format!("Host: {host}\r\nOrigin: {origin}\r\n")), "HTTP/1.1 403 Forbidden", "{origin}");
    }
    // and it listens on 127.0.0.1 alone: at the address this computer has on its network (the one
    // a packet to anywhere else would leave from; none on a computer with no network) nobody answers
    let lan = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|u| u.connect("192.0.2.1:9").and_then(|_| u.local_addr()));
    if let Some(ip) = lan.ok().map(|a| a.ip()).filter(|ip| !ip.is_loopback() && !ip.is_unspecified()) {
        let there = TcpStream::connect_timeout(&(ip, srv.port).into(), Duration::from_secs(1));
        assert!(there.is_err(), "the remote answers at {ip}");
    }
}

#[test]
fn the_calls_read_the_machine_and_play_it() {
    if !have_firmware() { return; }
    let srv = Server::start();
    let mut ws = srv.ws();
    booted(&mut ws);
    let none = || serde_json::json!({});

    let info = rpc(&mut ws, "FM1.getInfo", none());
    assert_eq!((info["firmware"].as_str(), info["board"].as_str(), info["mhz"].as_u64()), (Some("felucca"), Some("FM-1"), Some(240)), "{info}");
    assert_eq!((info["status"].as_str(), info["halted"].as_bool()), (Some("running"), Some(false)), "{info}");
    assert!(info["uptime_ms"].as_u64().unwrap() > 0);
    assert_eq!(rpc(&mut ws, "FM1.getEngineState", none())["halted"].as_bool(), Some(false));
    let leds = rpc(&mut ws, "FM1.getLeds", none());
    let leds = leds["leds"].as_array().unwrap();
    assert_eq!(leds.len(), 41);
    assert!(leds.iter().all(|v| (0.0..=1.0).contains(&v.as_f64().unwrap())), "{leds:?}");

    // the LCD: all of it, a part of it, and the same pixels in both
    let all = rpc(&mut ws, "FM1.getLcd", none());
    let all_px = base64_decode(all["data"].as_str().unwrap());
    assert_eq!((all["w"].as_u64(), all["h"].as_u64(), all_px.len()), (Some(240), Some(240), 240 * 240 * 4));
    assert!(all_px.chunks(4).any(|p| p[..3] != all_px[..3]), "Felucca has drawn something");
    let part = rpc(&mut ws, "FM1.getLcd", serde_json::json!({"x": 16, "y": 100, "w": 200, "h": 2}));
    let part_px = base64_decode(part["data"].as_str().unwrap());
    assert_eq!(part_px.len(), 200 * 2 * 4);
    assert_eq!(part_px[..800], all_px[(100 * 240 + 16) * 4..][..800]);
    assert_eq!(part_px[800..], all_px[(101 * 240 + 16) * 4..][..800]);
    let rest = hash(&mut ws);
    assert_eq!(hash(&mut ws), rest, "at rest the LCD is the same LCD");
    // what does not fit the LCD is refused, at once, and the next call is answered
    for bad in [serde_json::json!({"w": 100000, "h": 100000}), serde_json::json!({"x": 239, "w": 2}), serde_json::json!({"y": 240})] {
        let t0 = Instant::now();
        for method in ["FM1.getLcd", "FM1.lcdHash"] {
            assert_eq!(call(&mut ws, method, bad.clone(), &mut Vec::new())["error"]["code"], -32602, "{method} {bad}");
        }
        assert!(t0.elapsed() < Duration::from_secs(1));
    }
    assert_eq!(call(&mut ws, "FM1.pressButton", serde_json::json!({"id": 41}), &mut Vec::new())["error"]["code"], -32602);
    assert_eq!(call(&mut ws, "FM1.nothing", none(), &mut Vec::new())["error"]["code"], -32601);

    // the LCD comes as frames to a connection that asked, and to no other
    let mut frames = Vec::new();
    for _ in 0..10 {
        call(&mut ws, "FM1.lcdRevision", none(), &mut frames);
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(frames.is_empty(), "frames nobody asked for");
    call(&mut ws, "FM1.streamLcd", serde_json::json!({"on": true}), &mut frames);
    soon(Duration::from_secs(3), "no frame after FM1.streamLcd", || {
        call(&mut ws, "FM1.lcdRevision", none(), &mut frames);
        !frames.is_empty()
    });
    let differ = frames[0].chunks(4).zip(all_px.chunks(4)).filter(|(a, b)| a != b).count();
    assert!(frames[0].len() == all_px.len() && differ == 0, "the frame is the LCD, as RGBA: {} bytes, {differ} pixels differ, hash now {} at rest {rest}", frames[0].len(), hash(&mut ws));
    call(&mut ws, "FM1.streamLcd", serde_json::json!({"on": false}), &mut frames);

    // input: a turned knob and a pressed button show on the LCD
    rpc(&mut ws, "FM1.turnEncoder", serde_json::json!({"encoder": 3, "clicks": 5}));
    soon(Duration::from_secs(3), "the LCD did not change after turnEncoder", || hash(&mut ws) != rest);
    booted(&mut ws);
    let before = hash(&mut ws);
    rpc(&mut ws, "FM1.pressButton", serde_json::json!({"id": 7}));
    soon(Duration::from_secs(3), "the LCD did not change after pressButton", || hash(&mut ws) != before);
    rpc(&mut ws, "FM1.pressButton", serde_json::json!({"id": 8}));

    // a key held down sounds (an input event: down now, up later), and is silent again after
    std::thread::sleep(Duration::from_millis(300));
    peak(&mut ws);
    std::thread::sleep(Duration::from_millis(300));
    let quiet = peak(&mut ws);
    ws.send(Message::Text(r#"{"type":"key","id":21,"down":true}"#.into())).unwrap();
    soon(Duration::from_secs(3), "the key did not sound", || peak(&mut ws) > quiet + 500);
    ws.send(Message::Text(r#"{"type":"key","id":21,"down":false}"#.into())).unwrap();
    let frames0 = rpc(&mut ws, "FM1.getAudioMetrics", none())["frames"].as_u64().unwrap();
    soon(Duration::from_secs(8), "the key did not fall silent", || {
        std::thread::sleep(Duration::from_millis(200));
        peak(&mut ws) <= quiet + 50
    });
    assert!(rpc(&mut ws, "FM1.getAudioMetrics", none())["frames"].as_u64().unwrap() > frames0, "the frames played are counted");

    // a press that lasts 20 s keeps nothing waiting: the note sent after it sounds now
    rpc(&mut ws, "FM1.pressButton", serde_json::json!({"id": 0, "ms": 20_000}));
    peak(&mut ws);
    rpc(&mut ws, "FM1.sendMidi", serde_json::json!({"data": [0x90, 60, 100]}));
    soon(Duration::from_secs(3), "the note waited behind the press", || peak(&mut ws) > quiet + 500);
    rpc(&mut ws, "FM1.sendMidi", serde_json::json!({"data": [0x80, 60, 0]}));
}

#[test]
fn the_emulation_keeps_its_pace_whatever_a_connection_does() {
    if !have_firmware() { return; }
    let srv = Server::start();
    let mut ws = srv.ws();
    booted(&mut ws);
    let alone = pace(&mut ws, Duration::from_millis(1500));
    // in real time, not as fast as it goes
    assert!(alone < 1.25, "{alone:.2}x real time");

    // from before they connect: the time a connection costs as it arrives counts too
    let (t0, e0) = (Instant::now(), ms(&mut ws));
    // connections that say nothing, one that stops in the middle of its request
    let mut idle: Vec<TcpStream> = (0..3).map(|_| TcpStream::connect(srv.addr()).unwrap()).collect();
    idle[0].write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1").unwrap();
    // and one that asks for the LCD's frames, sets the LCD moving (PLAY) and reads none of them
    let mut deaf = srv.ws();
    rpc(&mut deaf, "FM1.streamLcd", serde_json::json!({"on": true}));
    rpc(&mut deaf, "FM1.pressButton", serde_json::json!({"id": 12}));
    std::thread::sleep(Duration::from_millis(3000));
    let crowded = (ms(&mut ws) - e0) as f64 / t0.elapsed().as_millis() as f64;
    assert!(crowded > 0.7 * alone, "{crowded:.2}x with them against {alone:.2}x alone");
    drop(idle);
}

fn base64_decode(s: &str) -> Vec<u8> {
    const LUT: [u8; 256] = {
        let mut t = [255u8; 256];
        let mut i = 0u8;
        while i < 26 { t[(b'A' + i) as usize] = i; i += 1; }
        i = 0;
        while i < 26 { t[(b'a' + i) as usize] = 26 + i; i += 1; }
        i = 0;
        while i < 10 { t[(b'0' + i) as usize] = 52 + i; i += 1; }
        t[b'+' as usize] = 62;
        t[b'/' as usize] = 63;
        t
    };
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len() * 3 / 4);
    let mut i = 0;
    while i + 3 < b.len() {
        let (a, b2, c, d) = (LUT[b[i] as usize] as u32, LUT[b[i+1] as usize] as u32, LUT[b[i+2] as usize] as u32, LUT[b[i+3] as usize] as u32);
        let n = (a << 18) | (b2 << 12) | (c << 6) | d;
        out.push((n >> 16) as u8);
        if b[i+2] != b'=' { out.push((n >> 8) as u8); }
        if b[i+3] != b'=' { out.push(n as u8); }
        i += 4;
    }
    out
}
