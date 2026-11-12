use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tungstenite::{connect, Message};

const FWSC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../reference/firmwares/felucca.fwsc");

fn have_firmware() -> bool {
    std::path::Path::new(FWSC).exists()
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn cargo_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    p.pop();
    p.push("fm1-live");
    p
}

struct CdpServer {
    child: Child,
    port: u16,
}

impl CdpServer {
    fn start() -> Self {
        let port = free_port();
        let child = Command::new(cargo_bin())
            .args(["--fwsc", FWSC, "--cdp", &port.to_string()])
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn fm1-live");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Self { child, port }
    }

    fn ws_url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }

    fn http_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for CdpServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn ws_connect(srv: &CdpServer) -> tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match connect(&srv.ws_url()) {
            Ok((ws, _)) => return ws,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => panic!("ws connect failed: {e}"),
        }
    }
}

fn rpc(ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>, method: &str, params: serde_json::Value) -> serde_json::Value {
    let id = 1;
    let req = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    ws.send(Message::Text(req.to_string())).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if Instant::now() > deadline { panic!("rpc timeout for {method}"); }
        match ws.read() {
            Ok(Message::Text(t)) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                    if v.get("id") == Some(&serde_json::json!(id)) {
                        return v["result"].clone();
                    }
                }
            }
            Ok(Message::Binary(_)) => continue,
            Ok(_) => continue,
            Err(e) => panic!("ws read error: {e}"),
        }
    }
}

fn wait_lcd_frame(ws: &mut tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if Instant::now() > deadline { panic!("timeout waiting for LCD frame"); }
        match ws.read() {
            Ok(Message::Binary(data)) => return data,
            Ok(_) => continue,
            Err(e) => panic!("ws read error waiting for frame: {e}"),
        }
    }
}

#[test]
fn cdp_http_serves_page() {
    if !have_firmware() { eprintln!("no firmware: skipped"); return; }
    let srv = CdpServer::start();
    let body = std::process::Command::new("curl")
        .args(["-s", &srv.http_url()])
        .output()
        .expect("curl");
    let html = String::from_utf8_lossy(&body.stdout);
    assert!(html.contains("<canvas"), "page should contain a canvas element");
    assert!(html.contains("pressButton"), "page should expose pressButton");
}

#[test]
fn cdp_connects_and_receives_lcd_frame() {
    if !have_firmware() { eprintln!("no firmware: skipped"); return; }
    let srv = CdpServer::start();
    let mut ws = ws_connect(&srv);
    let frame = wait_lcd_frame(&mut ws);
    assert_eq!(frame.len(), 240 * 240 * 4, "frame should be 230400 RGBA bytes");
    assert!(frame.iter().any(|&b| b != 0), "frame should not be all zeros");
}

#[test]
fn cdp_lcd_revision_increases() {
    if !have_firmware() { eprintln!("no firmware: skipped"); return; }
    let srv = CdpServer::start();
    let mut ws = ws_connect(&srv);
    let r1 = rpc(&mut ws, "FM1.lcdRevision", serde_json::json!({}));
    let rev1 = r1["revision"].as_u64().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let r2 = rpc(&mut ws, "FM1.lcdRevision", serde_json::json!({}));
    let rev2 = r2["revision"].as_u64().unwrap();
    assert!(rev2 > rev1, "revision should increase: {rev1} -> {rev2}");
}

#[test]
fn cdp_press_button_queues_action() {
    if !have_firmware() { eprintln!("no firmware: skipped"); return; }
    let srv = CdpServer::start();
    let mut ws = ws_connect(&srv);
    wait_lcd_frame(&mut ws);
    let rev_before = rpc(&mut ws, "FM1.lcdRevision", serde_json::json!({}))["revision"].as_u64().unwrap();
    rpc(&mut ws, "FM1.pressButton", serde_json::json!({"id": 12}));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if Instant::now() > deadline { panic!("revision did not change after pressButton"); }
        std::thread::sleep(Duration::from_millis(100));
        let rev = rpc(&mut ws, "FM1.lcdRevision", serde_json::json!({}))["revision"].as_u64().unwrap();
        if rev != rev_before { break; }
    }
}

#[test]
fn cdp_get_lcd_returns_rgba() {
    if !have_firmware() { eprintln!("no firmware: skipped"); return; }
    let srv = CdpServer::start();
    let mut ws = ws_connect(&srv);
    wait_lcd_frame(&mut ws);
    let r = rpc(&mut ws, "FM1.getLcd", serde_json::json!({"x": 0, "y": 0, "w": 240, "h": 1}));
    let b64 = r["data"].as_str().unwrap();
    let decoded = base64_decode(b64);
    assert_eq!(decoded.len(), 240 * 4, "one row: 960 bytes");
}

#[test]
fn cdp_get_info_returns_status() {
    if !have_firmware() { eprintln!("no firmware: skipped"); return; }
    let srv = CdpServer::start();
    let mut ws = ws_connect(&srv);
    std::thread::sleep(Duration::from_millis(500));
    let r = rpc(&mut ws, "FM1.getInfo", serde_json::json!({}));
    assert!(r["uptime_ms"].as_u64().unwrap() > 0);
    assert_eq!(r["halted"].as_bool().unwrap(), false);
}

#[test]
fn cdp_get_leds_returns_fractions() {
    if !have_firmware() { eprintln!("no firmware: skipped"); return; }
    let srv = CdpServer::start();
    let mut ws = ws_connect(&srv);
    wait_lcd_frame(&mut ws);
    let r = rpc(&mut ws, "FM1.getLeds", serde_json::json!({}));
    let leds = r["leds"].as_array().unwrap();
    assert_eq!(leds.len(), 41);
    for v in leds {
        let f = v.as_f64().unwrap();
        assert!((0.0..=1.0).contains(&f), "led value out of range: {f}");
    }
}

#[test]
fn cdp_turn_encoder_changes_state() {
    if !have_firmware() { eprintln!("no firmware: skipped"); return; }
    let srv = CdpServer::start();
    let mut ws = ws_connect(&srv);
    wait_lcd_frame(&mut ws);
    let rev_before = rpc(&mut ws, "FM1.lcdRevision", serde_json::json!({}))["revision"].as_u64().unwrap();
    rpc(&mut ws, "FM1.turnEncoder", serde_json::json!({"encoder": 3, "clicks": 5}));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if Instant::now() > deadline { panic!("revision did not change after turnEncoder"); }
        std::thread::sleep(Duration::from_millis(100));
        let rev = rpc(&mut ws, "FM1.lcdRevision", serde_json::json!({}))["revision"].as_u64().unwrap();
        if rev != rev_before { break; }
    }
}

#[test]
fn cdp_input_json_key_event() {
    if !have_firmware() { eprintln!("no firmware: skipped"); return; }
    let srv = CdpServer::start();
    let mut ws = ws_connect(&srv);
    wait_lcd_frame(&mut ws);
    ws.send(Message::Text(r#"{"type":"key","id":21,"down":true}"#.into())).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    ws.send(Message::Text(r#"{"type":"key","id":21,"down":false}"#.into())).unwrap();
    std::thread::sleep(Duration::from_millis(100));
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
