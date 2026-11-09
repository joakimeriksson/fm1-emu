use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::time::Duration;
use tungstenite::{Message, WebSocket};

use fm1_soc::Action;

use crate::Machine;

const CDP_PAGE: &str = include_str!("cdp_page.html");

fn rgb565_to_rgba(px: u16) -> [u8; 4] {
    let r = ((px >> 11) & 0x1F) as u8;
    let g = ((px >> 5) & 0x3F) as u8;
    let b = (px & 0x1F) as u8;
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2), 255]
}

fn lcd_to_rgba(fb: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(fb.len() * 4);
    for &px in fb {
        out.extend_from_slice(&rgb565_to_rgba(px));
    }
    out
}

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 2 < data.len() {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | data[i + 2] as u32;
        out.push(CHARS[(n >> 18) as usize & 63] as char);
        out.push(CHARS[(n >> 12) as usize & 63] as char);
        out.push(CHARS[(n >> 6) as usize & 63] as char);
        out.push(CHARS[n as usize & 63] as char);
        i += 3;
    }
    let rem = data.len() - i;
    if rem == 1 {
        let n = (data[i] as u32) << 16;
        out.push(CHARS[(n >> 18) as usize & 63] as char);
        out.push(CHARS[(n >> 12) as usize & 63] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8);
        out.push(CHARS[(n >> 18) as usize & 63] as char);
        out.push(CHARS[(n >> 12) as usize & 63] as char);
        out.push(CHARS[(n >> 6) as usize & 63] as char);
        out.push('=');
    }
    out
}

fn handle_jsonrpc(method: &str, params: &serde_json::Value, m: &mut Machine) -> serde_json::Value {
    match method {
        "FM1.getInfo" => {
            serde_json::json!({
                "firmware": "felucca",
                "status": if m.soc.periph.ms() > 0 { "running" } else { "booting" },
                "uptime_ms": m.soc.periph.ms(),
                "halted": false,
            })
        }
        "FM1.pressButton" => {
            if let Some(id) = params.get("id").and_then(|v| v.as_u64()) {
                let cycle = m.soc.periph.cycles;
                m.soc.periph.script.push_back((cycle, Action::Key { id: id as u8, down: true }));
                let release = cycle + m.soc.periph.hz / 10;
                m.soc.periph.script.push_back((release, Action::Key { id: id as u8, down: false }));
            }
            serde_json::json!({})
        }
        "FM1.turnEncoder" => {
            if let (Some(enc), Some(clicks)) = (
                params.get("encoder").and_then(|v| v.as_u64()),
                params.get("clicks").and_then(|v| v.as_i64()),
            ) {
                let cycle = m.soc.periph.cycles;
                m.soc.periph.script.push_back((cycle, Action::Enc { enc: enc as u8, clicks: clicks as i32 }));
            }
            serde_json::json!({})
        }
        "FM1.sendMidi" => {
            if let Some(data) = params.get("data").and_then(|v| v.as_array()) {
                let cycle = m.soc.periph.cycles;
                for b in data {
                    if let Some(b) = b.as_u64() {
                        m.soc.periph.script.push_back((cycle, Action::Midi { b: b as u8 }));
                    }
                }
            }
            serde_json::json!({})
        }
        "FM1.getLeds" => {
            let leds = m.soc.periph.led_peek();
            serde_json::json!({ "leds": leds.to_vec() })
        }
        "FM1.getLcd" => {
            let x = params.get("x").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let y = params.get("y").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let w = params.get("w").and_then(|v| v.as_u64()).unwrap_or(240) as usize;
            let h = params.get("h").and_then(|v| v.as_u64()).unwrap_or(240) as usize;
            let mut rgba = Vec::with_capacity(w * h * 4);
            for row in y..y + h {
                for col in x..x + w {
                    if row < 240 && col < 240 {
                        rgba.extend_from_slice(&rgb565_to_rgba(m.soc.lcd.fb[row * 240 + col]));
                    } else {
                        rgba.extend_from_slice(&[0, 0, 0, 255]);
                    }
                }
            }
            serde_json::json!({ "data": base64_encode(&rgba) })
        }
        "FM1.lcdHash" => {
            let x = params.get("x").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let y = params.get("y").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let w = params.get("w").and_then(|v| v.as_u64()).unwrap_or(240) as usize;
            let h = params.get("h").and_then(|v| v.as_u64()).unwrap_or(240) as usize;
            let mut hash: i32 = 0;
            for row in y..y + h {
                for col in x..x + w {
                    if row < 240 && col < 240 {
                        let rgba = rgb565_to_rgba(m.soc.lcd.fb[row * 240 + col]);
                        hash = hash.wrapping_shl(5).wrapping_sub(hash).wrapping_add(rgba[0] as i32);
                    }
                }
            }
            serde_json::json!({ "hash": hash })
        }
        "FM1.lcdRevision" => {
            serde_json::json!({ "revision": m.soc.lcd.revision })
        }
        "FM1.getEngineState" => {
            serde_json::json!({ "halted": false, "ms": m.soc.periph.ms() })
        }
        "FM1.getAudioMetrics" => {
            serde_json::json!({
                "audio_frames": m.soc.periph.audio.len() / 2,
                "audio_halves": m.soc.periph.audio_halves,
            })
        }
        _ => serde_json::json!({"error": "unknown method"}),
    }
}

fn handle_input(text: &str, m: &mut Machine) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
    let cycle = m.soc.periph.cycles;
    match v.get("type").and_then(|t| t.as_str()) {
        Some("key") => {
            if let (Some(id), Some(down)) = (
                v.get("id").and_then(|v| v.as_u64()),
                v.get("down").and_then(|v| v.as_bool()),
            ) {
                m.soc.periph.script.push_back((cycle, Action::Key { id: id as u8, down }));
            }
        }
        Some("enc") => {
            if let (Some(enc), Some(clicks)) = (
                v.get("enc").and_then(|v| v.as_u64()),
                v.get("clicks").and_then(|v| v.as_i64()),
            ) {
                m.soc.periph.script.push_back((cycle, Action::Enc { enc: enc as u8, clicks: clicks as i32 }));
            }
        }
        Some("midi") => {
            if let Some(data) = v.get("data").and_then(|v| v.as_array()) {
                for b in data {
                    if let Some(b) = b.as_u64() {
                        m.soc.periph.script.push_back((cycle, Action::Midi { b: b as u8 }));
                    }
                }
            }
        }
        _ => {}
    }
}

fn serve_http(mut stream: TcpStream) {
    let body = CDP_PAGE.as_bytes();
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.write_all(body);
}

pub fn serve(m: &mut Machine, port: u16) {
    let listener = TcpListener::bind(format!("0.0.0.0:{port}")).unwrap_or_else(|e| {
        eprintln!("cdp: cannot bind port {port}: {e}");
        std::process::exit(1);
    });
    listener.set_nonblocking(true).expect("set_nonblocking");
    eprintln!("cdp: http://localhost:{port}  ws://localhost:{port} (WebSocket on same port)");

    let mut ws: Option<WebSocket<TcpStream>> = None;
    let mut last_rev: u32 = 0;
    let mut status_ms: u64 = 0;

    loop {
        // accept new TCP connections
        if ws.is_none() {
            if let Ok((stream, _)) = listener.accept() {
                let mut peek = [0u8; 1024];
                stream.set_nonblocking(false).ok();
                stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                match stream.peek(&mut peek) {
                    Ok(n) => {
                        let req = String::from_utf8_lossy(&peek[..n]);
                        if req.contains("Upgrade: websocket") || req.contains("upgrade: websocket") {
                            match tungstenite::accept(stream) {
                                Ok(w) => {
                                    w.get_ref().set_nonblocking(true).ok();
                                    eprintln!("cdp: WebSocket connected");
                                    ws = Some(w);
                                }
                                Err(e) => eprintln!("cdp: ws handshake: {e}"),
                            }
                        } else {
                            serve_http(stream);
                        }
                    }
                    Err(e) => eprintln!("cdp: peek: {e}"),
                }
            }
        }

        // poll WebSocket
        if let Some(ref mut w) = ws {
            let mut closed = false;
            loop {
                match w.read() {
                    Ok(Message::Text(text)) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
                                let params = v.get("params").cloned().unwrap_or(serde_json::json!({}));
                                let result = handle_jsonrpc(method, &params, m);
                                let id = v.get("id").cloned().unwrap_or(serde_json::json!(null));
                                let resp = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                                let _ = w.send(Message::Text(resp.to_string()));
                            } else {
                                handle_input(&text, m);
                            }
                        }
                    }
                    Ok(Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => {
                        eprintln!("cdp: WebSocket closed");
                        closed = true;
                        break;
                    }
                    Err(tungstenite::Error::Io(ref e)) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => { closed = true; break; }
                    _ => {}
                }
            }
            if closed { ws = None; }
        }

        // run ~1ms of emulation
        if let Err(e) = m.run((m.soc.periph.hz / 1000) as u32) {
            eprintln!("cdp: halted: {e}");
            if let Some(ref mut w) = ws {
                let msg = serde_json::json!({"type": "status", "text": format!("halted: {e}")});
                let _ = w.send(Message::Text(msg.to_string()));
            }
            break;
        }

        // push LCD frame if changed
        if let Some(ref mut w) = ws {
            let rev = m.soc.lcd.revision;
            if rev != last_rev {
                last_rev = rev;
                let rgba = lcd_to_rgba(&m.soc.lcd.fb);
                let _ = w.send(Message::Binary(rgba));
            }

            let ms = m.soc.periph.ms();
            if ms >= status_ms + 500 && ms > 0 {
                status_ms = ms;
                let ips = m.soc.periph.cycles as f64 * 1000.0 / ms as f64;
                let msg = serde_json::json!({"type": "status", "text": format!("{:.0} instructions/s", ips)});
                let _ = w.send(Message::Text(msg.to_string()));
            }
        }

        std::thread::sleep(Duration::from_micros(100));
    }
}
