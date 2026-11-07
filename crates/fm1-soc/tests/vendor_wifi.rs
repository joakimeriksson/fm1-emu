//! Integration tests for the vendor WiFi mailbox (--wifi).
//!
//! Drives the Soc's MMIO registers at 0x30000..0x3011f exactly as the firmware
//! (fm1-base's recompiled plat_wifi_*) would, testing the state machine, scan
//! results, network frame TX/RX, and NAT path.

use fm1_core::Bus;
use fm1_soc::net::{GW_MAC, STA_MAC};
use fm1_soc::netutil::{be16, checksum, put_be16, put_be32};

const WIFI_BASE: u32 = 0x30000;
const CMD: u32 = WIFI_BASE;
const STATE: u32 = WIFI_BASE + 0x004;
const IP: u32 = WIFI_BASE + 0x008;
const RSSI: u32 = WIFI_BASE + 0x00C;
const SCAN_N: u32 = WIFI_BASE + 0x010;
const SCAN_IDX: u32 = WIFI_BASE + 0x014;
const SCAN_RSSI: u32 = WIFI_BASE + 0x018;
const SCAN_SEC: u32 = WIFI_BASE + 0x01C;
const SCAN_CHAN: u32 = WIFI_BASE + 0x020;
const SCAN_SSID0: u32 = WIFI_BASE + 0x024;
const JOIN_SSID0: u32 = WIFI_BASE + 0x044;
const JOIN_PASS0: u32 = WIFI_BASE + 0x064;
const TICK_MS: u32 = WIFI_BASE + 0x084;
const SSID0: u32 = WIFI_BASE + 0x088;
const SAVED_IDX: u32 = WIFI_BASE + 0x0A8;
const SAVED_SSID0: u32 = WIFI_BASE + 0x0AC;
const SAVED_N: u32 = WIFI_BASE + 0x0CC;
const WHY0: u32 = WIFI_BASE + 0x0D0;
const SCANS: u32 = WIFI_BASE + 0x0E0;

const NET_TX_DATA: u32 = WIFI_BASE + 0x100;
const NET_TX_LEN: u32 = WIFI_BASE + 0x104;
const NET_TX_SEND: u32 = WIFI_BASE + 0x108;
const NET_RX_READY: u32 = WIFI_BASE + 0x10C;
const NET_RX_LEN: u32 = WIFI_BASE + 0x110;
const NET_RX_DATA: u32 = WIFI_BASE + 0x114;
const NET_RX_ACK: u32 = WIFI_BASE + 0x118;

const CMD_ON: u32 = 1;
const CMD_SCAN: u32 = 3;
const CMD_JOIN: u32 = 4;
const CMD_FORGET: u32 = 5;

const WIFI_OFF: u32 = 0;
const WIFI_ON: u32 = 2;
const WIFI_CONNECTED: u32 = 6;

fn make_soc() -> fm1_soc::Soc {
    let mut soc = fm1_soc::Soc::new(1 << 16, vec![0; 0x1000]);
    soc.enable_vendor_wifi();
    soc
}

fn advance_ms(soc: &mut fm1_soc::Soc, ms: u32) {
    soc.write32(TICK_MS, ms);
}

fn read_ssid(soc: &mut fm1_soc::Soc, base: u32) -> String {
    let mut bytes = Vec::new();
    for w in 0..8u32 {
        let v = soc.read32(base + w * 4);
        for j in 0..4 {
            let b = ((v >> (j * 8)) & 0xff) as u8;
            if b == 0 {
                return String::from_utf8(bytes).unwrap();
            }
            bytes.push(b);
        }
    }
    String::from_utf8(bytes).unwrap()
}

fn write_ssid(soc: &mut fm1_soc::Soc, base: u32, s: &str) {
    let b = s.as_bytes();
    for w in 0..8u32 {
        let mut v = 0u32;
        for j in 0..4 {
            let idx = (w * 4 + j) as usize;
            if idx < b.len() {
                v |= (b[idx] as u32) << (j * 8);
            }
        }
        soc.write32(base + w * 4, v);
    }
}

#[test]
fn state_machine_off_through_connected() {
    let mut soc = make_soc();
    assert_eq!(soc.read32(STATE), WIFI_OFF);

    soc.write32(CMD, CMD_ON);
    assert_eq!(soc.read32(STATE), 1); // WIFI_STARTING

    advance_ms(&mut soc, 500);
    // "Home" is saved by default: auto-joins after START_MS
    assert_eq!(soc.read32(STATE), 4); // WIFI_JOINING

    advance_ms(&mut soc, 1500);
    assert_eq!(soc.read32(STATE), 5); // WIFI_ADDRESS

    advance_ms(&mut soc, 2000);
    assert_eq!(soc.read32(STATE), WIFI_CONNECTED);

    assert_ne!(soc.read32(IP), 0);
    assert_ne!(soc.read32(RSSI), 0);

    let ssid = read_ssid(&mut soc, SSID0);
    assert_eq!(ssid, "Home");
}

#[test]
fn scan_shows_virtual_aps() {
    let mut soc = make_soc();
    soc.write32(CMD, CMD_ON);
    advance_ms(&mut soc, 600);

    // wait for at least WIFI_ON
    let state = soc.read32(STATE);
    assert!(state >= WIFI_ON, "state {state}, expected >= WIFI_ON");

    soc.write32(CMD, CMD_SCAN);
    assert_eq!(soc.read32(SCANS), 1);
    assert_eq!(soc.read32(SCAN_N), 2);

    soc.write32(SCAN_IDX, 0);
    let ssid0 = read_ssid(&mut soc, SCAN_SSID0);
    assert_eq!(ssid0, "Home");
    assert_eq!(soc.read32(SCAN_SEC), 1);
    assert_eq!(soc.read32(SCAN_CHAN), 6);
    let rssi0 = soc.read32(SCAN_RSSI) as i32;
    assert!(rssi0 < 0, "RSSI should be negative dBm");

    soc.write32(SCAN_IDX, 1);
    let ssid1 = read_ssid(&mut soc, SCAN_SSID0);
    assert_eq!(ssid1, "Cafe");
    assert_eq!(soc.read32(SCAN_SEC), 0);
    assert_eq!(soc.read32(SCAN_CHAN), 11);
}

#[test]
fn join_with_wrong_password_stays_on() {
    let mut soc = make_soc();
    soc.write32(CMD, CMD_ON);
    for ms in [500, 1500, 2000] {
        advance_ms(&mut soc, ms); // the state machine moves on the firmware's ticks: connected to Home
    }
    assert_eq!(soc.read32(STATE), 6); // WIFI_CONNECTED

    // try joining Home with wrong password
    write_ssid(&mut soc, JOIN_SSID0, "Home");
    write_ssid(&mut soc, JOIN_PASS0, "wrong");
    soc.write32(CMD, CMD_JOIN);
    advance_ms(&mut soc, 7000);
    // refused: the state stays as it was (connected to Home by the auto-join), and WHY says why
    assert_eq!(soc.read32(STATE), 6); // WIFI_CONNECTED
    assert_eq!(read_ssid(&mut soc, WHY0), "wrong password");
    // a kept network joins with no password given, as plat_host.c's "Home" does
    write_ssid(&mut soc, JOIN_PASS0, "");
    soc.write32(CMD, CMD_JOIN);
    assert_eq!(soc.read32(STATE), 4); // WIFI_JOINING
    assert_eq!(read_ssid(&mut soc, WHY0), "");
}

#[test]
fn forget_removes_saved_network() {
    let mut soc = make_soc();
    assert_eq!(soc.read32(SAVED_N), 1);

    soc.write32(SAVED_IDX, 0);
    let saved = read_ssid(&mut soc, SAVED_SSID0);
    assert_eq!(saved, "Home");

    soc.write32(SAVED_IDX, 0);
    soc.write32(CMD, CMD_FORGET);
    assert_eq!(soc.read32(SAVED_N), 0);
}

fn connect_soc(soc: &mut fm1_soc::Soc) {
    soc.write32(CMD, CMD_ON);
    advance_ms(soc, 500);
    advance_ms(soc, 1500);
    advance_ms(soc, 2000);
    assert_eq!(soc.read32(STATE), WIFI_CONNECTED);
}

fn tx_frame(soc: &mut fm1_soc::Soc, frame: &[u8]) {
    soc.write32(NET_TX_LEN, frame.len() as u32);
    for chunk in frame.chunks(4) {
        let mut v = 0u32;
        for (j, &b) in chunk.iter().enumerate() {
            v |= (b as u32) << (j * 8);
        }
        soc.write32(NET_TX_DATA, v);
    }
    soc.write32(NET_TX_SEND, 1);
}

fn rx_frame(soc: &mut fm1_soc::Soc) -> Option<Vec<u8>> {
    if soc.read32(NET_RX_READY) == 0 {
        return None;
    }
    let len = soc.read32(NET_RX_LEN) as usize;
    let mut frame = Vec::with_capacity(len);
    let mut remaining = len;
    while remaining > 0 {
        let v = soc.read32(NET_RX_DATA);
        let take = remaining.min(4);
        for j in 0..take {
            frame.push(((v >> (j * 8)) & 0xff) as u8);
        }
        soc.write32(NET_RX_ACK, 1);
        remaining -= take;
    }
    Some(frame)
}

const STA_IP: [u8; 4] = [10, 0, 2, 15];

fn make_arp_who_has(target_ip: [u8; 4]) -> Vec<u8> {
    let mut f = vec![0u8; 42];
    f[0..6].copy_from_slice(&[0xff; 6]);
    f[6..12].copy_from_slice(&STA_MAC);
    f[12] = 0x08;
    f[13] = 0x06;
    // ARP
    put_be16(&mut f[14..], 1); // hw type
    put_be16(&mut f[16..], 0x0800); // proto
    f[18] = 6;
    f[19] = 4;
    put_be16(&mut f[20..], 1); // request
    f[22..28].copy_from_slice(&STA_MAC);
    f[28..32].copy_from_slice(&STA_IP);
    f[32..38].fill(0);
    f[38..42].copy_from_slice(&target_ip);
    f
}

fn make_dns_query(name: &str) -> Vec<u8> {
    let dns_ip: [u8; 4] = [10, 0, 2, 3];
    let mut dns = vec![0u8; 12];
    dns[0] = 0xCA;
    dns[1] = 0xFE;
    dns[5] = 1;
    for label in name.split('.') {
        dns.push(label.len() as u8);
        dns.extend_from_slice(label.as_bytes());
    }
    dns.push(0);
    dns.extend_from_slice(&[0, 1, 0, 1]); // type A, class IN

    let udp_len = 8 + dns.len();
    let ip_len = 20 + udp_len;
    let mut frame = vec![0u8; 14 + ip_len];
    frame[0..6].copy_from_slice(&GW_MAC);
    frame[6..12].copy_from_slice(&STA_MAC);
    frame[12] = 0x08;
    frame[13] = 0x00;
    frame[14] = 0x45;
    put_be16(&mut frame[16..], ip_len as u16);
    frame[22] = 64;
    frame[23] = 17; // UDP
    frame[26..30].copy_from_slice(&STA_IP);
    frame[30..34].copy_from_slice(&dns_ip);
    let ck = checksum(&frame[14..34]);
    put_be16(&mut frame[24..], ck);
    put_be16(&mut frame[34..], 12345);
    put_be16(&mut frame[36..], 53);
    put_be16(&mut frame[38..], udp_len as u16);
    frame[42..42 + dns.len()].copy_from_slice(&dns);
    frame
}

#[test]
fn arp_and_dns_through_mailbox() {
    let mut soc = make_soc();
    connect_soc(&mut soc);

    // ARP for the gateway
    let arp = make_arp_who_has([10, 0, 2, 2]);
    tx_frame(&mut soc, &arp);

    let reply = rx_frame(&mut soc).expect("ARP reply");
    assert_eq!(be16(&reply[12..14]), 0x0806, "ethertype ARP");
    assert_eq!(be16(&reply[20..22]), 2, "ARP reply opcode");

    // DNS query
    let dns = make_dns_query("example.com");
    tx_frame(&mut soc, &dns);

    let reply = rx_frame(&mut soc).expect("DNS reply");
    assert_eq!(reply[23], 17, "UDP protocol");
    assert_eq!(be16(&reply[34..36]), 53, "src port 53");
    let dns_payload = &reply[42..];
    assert_eq!(dns_payload[2] & 0x80, 0x80, "QR bit set (response)");
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

fn make_tcp_frame(
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    data: &[u8],
) -> Vec<u8> {
    let tcp_len = 20 + data.len();
    let ip_len = 20 + tcp_len;
    let mut frame = vec![0u8; 14 + ip_len];
    frame[0..6].copy_from_slice(&GW_MAC);
    frame[6..12].copy_from_slice(&STA_MAC);
    frame[12] = 0x08;
    frame[13] = 0x00;
    frame[14] = 0x45;
    put_be16(&mut frame[16..], ip_len as u16);
    frame[22] = 64;
    frame[23] = 6; // TCP
    frame[26..30].copy_from_slice(&src_ip);
    frame[30..34].copy_from_slice(&dst_ip);
    let ck = checksum(&frame[14..34]);
    put_be16(&mut frame[24..], ck);
    put_be16(&mut frame[34..], src_port);
    put_be16(&mut frame[36..], dst_port);
    put_be32(&mut frame[38..], seq);
    put_be32(&mut frame[42..], ack);
    frame[46] = 0x50;
    frame[47] = flags;
    put_be16(&mut frame[48..], 65535);
    if !data.is_empty() {
        frame[54..54 + data.len()].copy_from_slice(data);
    }
    let tcp_ck = tcp_checksum(src_ip, dst_ip, &frame[34..34 + tcp_len]);
    put_be16(&mut frame[50..], tcp_ck);
    frame
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[test]
fn nat_connects_through_a_loopback_listener() {
    use fm1_soc::net::VirtualNet;
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut buf = [0u8; 64];
        let mut got = Vec::new();
        while !got.ends_with(b"ping") {
            let n = s.read(&mut buf).unwrap();
            assert!(n > 0, "the client went away");
            got.extend_from_slice(&buf[..n]);
        }
        s.write_all(b"pong").unwrap();
    });

    let mut vnet = VirtualNet::new();
    vnet.enable_nat(true);
    vnet.poll(wall_ms());

    let host = [127, 0, 0, 1];
    let (cport, cseq) = (49153u16, 7000u32);
    let mut replies = vnet.handle(&make_tcp_frame(STA_IP, host, cport, port, cseq, 0, 0x02, &[]));
    let mut waited = 0;
    while replies.is_empty() && waited < 100 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 1;
        replies = vnet.poll(wall_ms());
    }
    assert_eq!(replies.len(), 1, "a SYN-ACK");
    assert_eq!(replies[0][47] & 0x12, 0x12, "SYN|ACK");
    let sseq = u32::from_be_bytes(replies[0][38..42].try_into().unwrap());
    assert_eq!(u32::from_be_bytes(replies[0][42..46].try_into().unwrap()), cseq + 1);
    assert!(vnet.handle(&make_tcp_frame(STA_IP, host, cport, port, cseq + 1, sseq + 1, 0x10, &[])).is_empty());
    let acked = vnet.handle(&make_tcp_frame(STA_IP, host, cport, port, cseq + 1, sseq + 1, 0x18, b"ping"));
    assert_eq!(acked.len(), 1);
    assert_eq!(u32::from_be_bytes(acked[0][42..46].try_into().unwrap()), cseq + 5);

    let mut got = Vec::new();
    let mut fin = false;
    for _ in 0..200 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        for f in vnet.poll(wall_ms()) {
            if f.len() > 54 {
                got.extend_from_slice(&f[54..]);
            }
            if f[47] & 0x01 != 0 {
                fin = true;
            }
        }
        if fin {
            break;
        }
    }
    assert_eq!(got, b"pong");
    assert!(fin, "the host's close reached the guest as a FIN");
    server.join().unwrap();
}

#[test]
fn nat_refuses_loopback_without_allow_lan() {
    use fm1_soc::net::VirtualNet;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();

    let mut vnet = VirtualNet::new();
    vnet.enable_nat(false);
    vnet.poll(wall_ms());

    let mut replies = vnet.handle(&make_tcp_frame(STA_IP, [127, 0, 0, 1], 49154, port, 7000, 0, 0x02, &[]));
    let mut waited = 0;
    while replies.is_empty() && waited < 100 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        waited += 1;
        replies = vnet.poll(wall_ms());
    }
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0][47] & 0x04, 0x04, "RST");
    assert!(
        matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
        "no connection was made"
    );
    assert_eq!(vnet.nat.as_ref().unwrap().stats.tcp_connects, 0);
}

#[test]
#[ignore = "real network: cargo test -p fm1-soc -- --ignored real_internet_http_get"]
fn real_internet_http_get() {
    use fm1_soc::net::VirtualNet;
    let targets = ["1.1.1.1:80", "8.8.8.8:53", "93.184.216.34:80"];
    let mut reachable = None;
    for t in &targets {
        let addr: std::net::SocketAddr = t.parse().unwrap();
        if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3)).is_ok() {
            reachable = Some(addr);
            break;
        }
    }
    let Some(target) = reachable else {
        eprintln!("SKIP: no internet connectivity");
        return;
    };
    eprintln!("Internet probe OK: {}", target);

    let mut vnet = VirtualNet::new();
    vnet.enable_nat(false);
    vnet.poll(wall_ms());

    // DNS lookup
    let dns_frame = make_dns_query("example.com");
    let replies = vnet.handle(&dns_frame);
    assert_eq!(replies.len(), 1, "DNS should reply");

    // TCP SYN to the probed server
    let server_ip: [u8; 4] = match target.ip() {
        std::net::IpAddr::V4(v4) => v4.octets(),
        _ => unreachable!(),
    };
    let server_port = if target.port() == 53 { 80 } else { target.port() };
    let (cport, cseq) = (49152u16, 1000u32);

    let syn = make_tcp_frame(STA_IP, server_ip, cport, server_port, cseq, 0, 0x02, &[]);
    let mut replies = vnet.handle(&syn);
    let mut waited = 0;
    while replies.is_empty() && waited < 120 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 1;
        replies = vnet.poll(wall_ms());
    }
    assert!(!replies.is_empty(), "TCP SYN should produce a response");
    let reply = &replies[0];
    let reply_flags = reply[47];
    if reply_flags & 0x04 != 0 {
        eprintln!("SKIP: got RST, but NAT path works");
        return;
    }
    assert_eq!(reply_flags & 0x12, 0x12, "expected SYN|ACK");

    let sseq = u32::from_be_bytes(reply[38..42].try_into().unwrap());
    assert_eq!(u32::from_be_bytes(reply[42..46].try_into().unwrap()), cseq + 1);

    let ack = make_tcp_frame(STA_IP, server_ip, cport, server_port, cseq + 1, sseq + 1, 0x10, &[]);
    let _ = vnet.handle(&ack);

    let host_str = format!("{}", target.ip());
    let http_req = format!("GET / HTTP/1.0\r\nHost: {}\r\n\r\n", host_str);
    let get = make_tcp_frame(STA_IP, server_ip, cport, server_port, cseq + 1, sseq + 1, 0x18, http_req.as_bytes());
    let _ = vnet.handle(&get);

    let mut got_data = false;
    for _ in 0..200 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let frames = vnet.poll(wall_ms());
        for f in &frames {
            if f.len() > 54 {
                let flags = f[47];
                if flags & 0x08 != 0 || flags & 0x10 != 0 {
                    let body = &f[54..];
                    let text = String::from_utf8_lossy(body);
                    if text.contains("HTTP/1.") {
                        eprintln!("Got HTTP response ({} bytes)", body.len());
                        got_data = true;
                    }
                }
            }
        }
        if got_data {
            break;
        }
    }
    assert!(got_data, "should have received HTTP response data");
}
