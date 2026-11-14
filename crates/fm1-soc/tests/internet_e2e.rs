//! End-to-end integration test: WiFi mailbox → VirtualNet → NAT → real internet.
//!
//! This test drives the Soc's MMIO registers exactly as firmware would, then sends
//! a TCP SYN through the network frame interface. The NAT opens a real TCP connection
//! to httpbin.org:80 and relays data. A successful HTTP GET proves real internet works
//! through the emulator's stack.
//!
//! Skipped (returns Ok) when the network is unreachable (CI without internet).

use fm1_core::Bus;
use fm1_soc::net::VirtualNet;

const STA_MAC: [u8; 6] = [0x02, 0x53, 0x49, 0x4d, 0x00, 0x15];
const GW_MAC: [u8; 6] = [0x02, 0x53, 0x49, 0x4d, 0x00, 0x02];
const STA_IP: [u8; 4] = [10, 0, 2, 15];

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
    // IP header
    frame[14] = 0x45;
    put_be16(&mut frame[16..], ip_len as u16);
    frame[22] = 64;
    frame[23] = 6; // TCP
    frame[26..30].copy_from_slice(&src_ip);
    frame[30..34].copy_from_slice(&dst_ip);
    let ck = checksum(&frame[14..34]);
    put_be16(&mut frame[24..], ck);
    // TCP header
    put_be16(&mut frame[34..], src_port);
    put_be16(&mut frame[36..], dst_port);
    put_be32(&mut frame[38..], seq);
    put_be32(&mut frame[42..], ack);
    frame[46] = 0x50; // data offset = 5
    frame[47] = flags;
    put_be16(&mut frame[48..], 65535); // window
    if !data.is_empty() {
        frame[54..54 + data.len()].copy_from_slice(data);
    }
    // TCP checksum
    let tcp_ck = tcp_checksum(src_ip, dst_ip, &frame[34..34 + tcp_len]);
    put_be16(&mut frame[50..], tcp_ck);
    frame
}

/// The wall clock in milliseconds: the NAT's timeouts over real sockets run on it here.
fn wall_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH).unwrap().as_millis() as u64
}

/// Test real internet: DNS + TCP HTTP GET through VirtualNet + NAT.
/// Ignored by default (it opens real connections; offline it stalls on the probes):
/// `cargo test -p fm1-soc -- --ignored real_internet_http_get`.
#[test]
#[ignore = "real network: cargo test -p fm1-soc -- --ignored real_internet_http_get"]
fn real_internet_http_get() {
    // Quick connectivity check: try a few well-known servers
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
        eprintln!("SKIP: no internet connectivity, skipping real internet test");
        return;
    };
    eprintln!("Internet probe OK: {}", target);

    let mut vnet = VirtualNet::new();
    vnet.enable_nat(false);
    vnet.poll(wall_ms()); // the NAT's clock is the wall clock here

    // Step 1: DNS lookup for "example.com"
    let mut dns = vec![0u8; 12];
    dns[0] = 0xCA;
    dns[1] = 0xFE;
    dns[5] = 1; // qdcount
    dns.extend_from_slice(&[7]);
    dns.extend_from_slice(b"example");
    dns.extend_from_slice(&[3]);
    dns.extend_from_slice(b"com");
    dns.push(0);
    dns.extend_from_slice(&[0, 1, 0, 1]); // type A, class IN

    let dns_frame = make_dns_frame(&dns);
    let replies = vnet.handle(&dns_frame);
    assert_eq!(replies.len(), 1, "DNS should reply");
    assert_eq!(vnet.stats.dns_answers, 1);

    // Step 2: TCP SYN to the real server
    // DNS resolves everything to 10.0.2.3, but NAT connects to the real IP.
    // Use whichever target the probe found reachable.
    let server_ip: [u8; 4] = match target.ip() {
        std::net::IpAddr::V4(v4) => v4.octets(),
        _ => unreachable!(),
    };
    let server_port = if target.port() == 53 { 80 } else { target.port() };
    let client_port: u16 = 49152;
    let client_seq: u32 = 1000;

    let syn = make_tcp_frame(STA_IP, server_ip, client_port, server_port, client_seq, 0, 0x02, &[]);
    let mut replies = vnet.handle(&syn);
    // the host connect runs on a thread of its own: the SYN-ACK (or RST) comes from a later poll
    let mut waited = 0;
    while replies.is_empty() && waited < 120 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 1;
        replies = vnet.poll(wall_ms());
    }
    assert!(!replies.is_empty(), "TCP SYN should produce a response");
    let reply = &replies[0];

    // Check it's a TCP response
    assert!(reply.len() >= 54, "response too short");
    let reply_flags = reply[14 + 20 + 13]; // IP header + TCP flags offset
    if reply_flags & 0x04 != 0 {
        eprintln!("SKIP: got RST (server refused connection), but NAT path works");
        assert!(vnet.nat.as_ref().unwrap().stats.tcp_connects == 0);
        return;
    }
    assert_eq!(reply_flags & 0x12, 0x12, "expected SYN|ACK");
    assert!(
        vnet.nat.as_ref().unwrap().stats.tcp_connects >= 1,
        "NAT should have opened a real TCP connection"
    );

    // Extract server's seq number from SYN-ACK
    let server_seq = u32::from_be_bytes(reply[38..42].try_into().unwrap());
    let server_ack = u32::from_be_bytes(reply[42..46].try_into().unwrap());
    assert_eq!(server_ack, client_seq + 1, "server should ACK our SYN");

    // Step 3: ACK the SYN-ACK
    let ack = make_tcp_frame(
        STA_IP,
        server_ip,
        client_port,
        server_port,
        client_seq + 1,
        server_seq + 1,
        0x10, // ACK
        &[],
    );
    let _ = vnet.handle(&ack);

    // Step 4: Send HTTP GET
    let host = format!("{}", target.ip());
    let http_req = format!("GET / HTTP/1.0\r\nHost: {}\r\n\r\n", host);
    let get = make_tcp_frame(
        STA_IP,
        server_ip,
        client_port,
        server_port,
        client_seq + 1,
        server_seq + 1,
        0x18, // PSH|ACK
        http_req.as_bytes(),
    );
    let _ = vnet.handle(&get);

    // Step 5: Poll for response data
    let mut got_data = false;
    for _ in 0..200 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let frames = vnet.poll(wall_ms());
        for f in &frames {
            if f.len() > 54 {
                let flags = f[14 + 20 + 13];
                let data_start = 14 + 20 + 20;
                if f.len() > data_start && (flags & 0x08 != 0 || flags & 0x10 != 0) {
                    let body = &f[data_start..];
                    let text = String::from_utf8_lossy(body);
                    if text.contains("HTTP/1.") {
                        eprintln!("Got HTTP response ({} bytes): {}", body.len(), &text[..text.len().min(100)]);
                        got_data = true;
                    }
                }
            }
        }
        if got_data {
            break;
        }
    }

    assert!(got_data, "should have received HTTP response data from example.com");
    assert!(
        vnet.nat.as_ref().unwrap().stats.tcp_bytes_in > 0,
        "NAT should have received bytes from the real server"
    );
    eprintln!(
        "Real internet test PASSED: {} bytes received",
        vnet.nat.as_ref().unwrap().stats.tcp_bytes_in
    );
}

fn make_dns_frame(dns: &[u8]) -> Vec<u8> {
    let dns_ip: [u8; 4] = [10, 0, 2, 3];
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
    frame[42..42 + dns.len()].copy_from_slice(dns);
    frame
}

/// Test DMA-based WiFi TX/RX path matching the firmware's RT2800-style DMA.
/// Exercises: DMA register setup, TX descriptor kick, frame processing,
/// RX descriptor delivery with DMA_DONE, and INT_SOURCE_CSR.
#[test]
fn oss_wifi_dma_probe_auth_assoc() {
    let mut soc = fm1_soc::Soc::new(1 << 16, vec![0; 0x1000]);
    soc.enable_oss_wifi();

    const RAM_BASE: u32 = 0x01c0_0000;
    // Allocate test buffers in RAM (low offsets)
    const TXD_ADDR: u32 = RAM_BASE + 0x100; // TX descriptor (16 bytes)
    const RXD_ADDR: u32 = RAM_BASE + 0x200; // RX descriptor (16 bytes)
    const TX_BUF: u32 = RAM_BASE + 0x1000;  // TX frame buffer
    const RX_BUF: u32 = RAM_BASE + 0x2000;  // RX frame buffer (1600 bytes)

    let sta_mac: [u8; 6] = [0x02, 0x53, 0x49, 0x4d, 0x00, 0x15];
    let ap_mac: [u8; 6] = [0x02, 0x46, 0x4d, 0x31, 0x00, 0x01];

    // Verify MAC_ADDR_DW0/DW1 pre-populated at 0x30300/0x30304
    let dw0 = soc.read32(0x30300);
    let dw1 = soc.read32(0x30304);
    assert_eq!(dw0 & 0xFF, sta_mac[0] as u32);
    assert_eq!((dw0 >> 8) & 0xFF, sta_mac[1] as u32);
    assert_eq!((dw1 >> 8) & 0xFF, sta_mac[5] as u32);

    // DMA init sequence (matching firmware's wifi_dma_init)
    soc.write32(0x40208, 0); // WPDMA_GLO_CFG: disable TX+RX
    soc.write32(0x40230, TXD_ADDR); // TX_BASE_PTR0
    soc.write32(0x40234, 1);        // TX_MAX_CNT0
    soc.write32(0x40290, RXD_ADDR); // RX_BASE_PTR

    // Setup RX descriptor: buffer pointer + length, DMA_DONE=0
    soc.write32(RXD_ADDR, RX_BUF);     // sd_ptr0
    soc.write32(RXD_ADDR + 4, 1600);   // sd_len0
    soc.write32(RXD_ADDR + 8, 0);      // sd_ptr1
    soc.write32(RXD_ADDR + 12, 0);     // flags (no DMA_DONE)

    soc.write32(0x40294, 1);  // RX_MAX_CNT
    soc.write32(0x40238, 0);  // TX_CTX_IDX0
    soc.write32(0x40298, 0);  // RX_CRX_IDX
    soc.write32(0x40204, 0);  // INT_MASK = 0

    // Enable DMA
    soc.write32(0x40208, (1 << 0) | (1 << 2)); // TX_EN | RX_EN

    // Helper: write a frame to TX buffer and kick DMA
    let dma_tx = |soc: &mut fm1_soc::Soc, frame: &[u8]| {
        // Write frame bytes to TX buffer in RAM
        for (i, &b) in frame.iter().enumerate() {
            soc.write8(TX_BUF + i as u32, b);
        }
        // Setup TX descriptor
        soc.write32(TXD_ADDR, TX_BUF);
        soc.write32(TXD_ADDR + 4, (frame.len() as u32 & 0xFFFF) | (1 << 16)); // len | LAST_SEC0
        soc.write32(TXD_ADDR + 8, 0);
        soc.write32(TXD_ADDR + 12, 0); // clear DMA_DONE
        // Kick TX
        soc.write32(0x40238, 0); // TX_CTX_IDX0
    };

    // Helper: read RX frame from DMA
    let dma_rx = |soc: &mut fm1_soc::Soc| -> Option<Vec<u8>> {
        let flags = soc.read32(RXD_ADDR + 12);
        if flags & (1 << 17) == 0 { return None; } // no DMA_DONE

        let len = (soc.read32(RXD_ADDR + 4) & 0xFFFF) as usize;
        if len < 24 || len > 1600 { return None; }

        let mut frame = vec![0u8; len];
        for i in 0..len {
            frame[i] = soc.read8(RX_BUF + i as u32);
        }

        // Re-post RX buffer (like firmware does)
        soc.write32(RXD_ADDR, RX_BUF);
        soc.write32(RXD_ADDR + 4, 1600);
        soc.write32(RXD_ADDR + 8, 0);
        soc.write32(RXD_ADDR + 12, 0); // clear DMA_DONE
        soc.write32(0x40298, 0); // RX_CRX_IDX

        Some(frame)
    };

    // --- Probe request ---
    let mut probe = vec![0x40, 0x00, 0x00, 0x00]; // FC=probe req
    probe.extend_from_slice(&[0xFF; 6]);
    probe.extend_from_slice(&sta_mac);
    probe.extend_from_slice(&[0xFF; 6]);
    probe.extend_from_slice(&[0x00; 2]);
    probe.push(0); probe.push(0); // SSID IE (wildcard)
    dma_tx(&mut soc, &probe);

    // TX descriptor should have DMA_DONE
    assert!(soc.read32(TXD_ADDR + 12) & (1 << 17) != 0, "TX DMA_DONE not set");
    // INT_SOURCE should have TX_DONE0
    assert!(soc.read32(0x40200) & 1 != 0, "INT_TX_DONE0 not set");

    // RX: should get probe responses (Home first)
    let home_rsp = dma_rx(&mut soc).expect("Home probe response via DMA");
    let fc = u16::from_le_bytes([home_rsp[0], home_rsp[1]]);
    assert_eq!(fc, 0x0050, "expected probe response FC");

    let cafe_rsp = dma_rx(&mut soc).expect("Cafe probe response via DMA");
    assert_eq!(u16::from_le_bytes([cafe_rsp[0], cafe_rsp[1]]), 0x0050);

    // --- Auth request ---
    let mut auth = vec![0xB0, 0x00, 0x00, 0x00];
    auth.extend_from_slice(&ap_mac);
    auth.extend_from_slice(&sta_mac);
    auth.extend_from_slice(&ap_mac);
    auth.extend_from_slice(&[0; 2]); // seq ctrl
    auth.extend_from_slice(&[0; 2]); // algo: open
    auth.extend_from_slice(&[1, 0]); // seq: 1
    auth.extend_from_slice(&[0; 2]); // status
    dma_tx(&mut soc, &auth);

    let auth_rsp = dma_rx(&mut soc).expect("Auth response via DMA");
    assert_eq!(u16::from_le_bytes([auth_rsp[0], auth_rsp[1]]), 0x00B0);
    assert_eq!(u16::from_le_bytes([auth_rsp[28], auth_rsp[29]]), 0, "auth status");

    // --- Assoc request ---
    let mut assoc = vec![0x00, 0x00, 0x00, 0x00];
    assoc.extend_from_slice(&ap_mac);
    assoc.extend_from_slice(&sta_mac);
    assoc.extend_from_slice(&ap_mac);
    assoc.extend_from_slice(&[0; 2]);
    assoc.extend_from_slice(&[0x21, 0x00]); // capability 0x0021 (matching firmware)
    assoc.extend_from_slice(&[10, 0]); // listen interval
    assoc.push(0); assoc.push(4); assoc.extend_from_slice(b"Home");
    dma_tx(&mut soc, &assoc);

    let assoc_rsp = dma_rx(&mut soc).expect("Assoc response via DMA");
    assert_eq!(u16::from_le_bytes([assoc_rsp[0], assoc_rsp[1]]), 0x0010);
    assert_eq!(u16::from_le_bytes([assoc_rsp[26], assoc_rsp[27]]), 0, "assoc status");

    // EAPOL msg1 should follow (WPA2 handshake start)
    let msg1 = dma_rx(&mut soc).expect("EAPOL msg1 via DMA");
    assert_eq!(&msg1[30..32], &[0x88, 0x8E], "EAPOL ethertype");

    eprintln!("oss_wifi_dma_probe_auth_assoc PASSED: full scan/auth/assoc via DMA");
}

/// --oss-wifi: `Soc::advance` ticks the chip every 10 ms of emulated time; the virtual
/// APs' beacons reach the station's RX ring without any TX from it.
#[test]
fn oss_wifi_tick_delivers_beacons_into_the_rx_ring() {
    let mut soc = fm1_soc::Soc::new(1 << 16, vec![0; 0x1000]);
    soc.enable_oss_wifi();
    const RAM_BASE: u32 = 0x01c0_0000;
    const RXD_ADDR: u32 = RAM_BASE + 0x200;
    const RX_BUF: u32 = RAM_BASE + 0x2000;
    soc.write32(0x40290, RXD_ADDR); // RX_BASE_PTR
    soc.write32(0x40294, 1); // RX_MAX_CNT
    soc.write32(RXD_ADDR, RX_BUF);
    soc.write32(RXD_ADDR + 4, 1600);
    soc.write32(RXD_ADDR + 8, 0);
    soc.write32(RXD_ADDR + 12, 0);
    soc.write32(0x40208, 1 << 2); // WPDMA_GLO_CFG: RX_EN
    let step = soc.periph.cycle_of_ms(1).max(1);
    let mut beacons = 0;
    for _ in 0..350 {
        soc.advance(step);
        if soc.read32(RXD_ADDR + 12) & (1 << 17) != 0 {
            assert_eq!(soc.read32(RX_BUF) & 0xffff, 0x0080, "a beacon's frame control");
            beacons += 1;
            soc.write32(RXD_ADDR + 12, 0); // reclaimed by the driver
        }
    }
    // two APs, every 100 ms: three rounds in 350 ms
    assert!(beacons >= 4, "{beacons} beacons in 350 ms");
}

/// --wifi-ssid / --wifi-password: the chip's beacons carry the configured name (the WPA2
/// handshake keys off the configured passphrase: `WifiHw::ap`).
#[test]
fn oss_wifi_beacons_carry_the_configured_ssid() {
    let mut soc = fm1_soc::Soc::new(1 << 16, vec![0; 0x1000]);
    soc.set_wifi_ap(fm1_soc::net::Ap { ssid: "Studio".into(), password: "letmein12".into() });
    soc.enable_oss_wifi();
    assert_eq!(soc.wifi_hw.as_ref().unwrap().ap.password, "letmein12");
    const RAM_BASE: u32 = 0x01c0_0000;
    const RXD_ADDR: u32 = RAM_BASE + 0x200;
    const RX_BUF: u32 = RAM_BASE + 0x2000;
    soc.write32(0x40290, RXD_ADDR);
    soc.write32(0x40294, 1);
    soc.write32(RXD_ADDR, RX_BUF);
    soc.write32(RXD_ADDR + 4, 1600);
    soc.write32(RXD_ADDR + 8, 0);
    soc.write32(RXD_ADDR + 12, 0);
    soc.write32(0x40208, 1 << 2);
    let step = soc.periph.cycle_of_ms(1).max(1);
    let mut names = Vec::new();
    for _ in 0..250 {
        soc.advance(step);
        if soc.read32(RXD_ADDR + 12) & (1 << 17) != 0 {
            // a beacon: 24 header, 8 timestamp, 2 interval, 2 capability, then the SSID element
            let len = soc.read8(RX_BUF + 37) as u32;
            let name: Vec<u8> = (0..len).map(|i| soc.read8(RX_BUF + 38 + i)).collect();
            names.push(String::from_utf8_lossy(&name).into_owned());
            soc.write32(RXD_ADDR + 12, 0);
        }
    }
    assert!(names.contains(&"Studio".to_string()) && names.contains(&"Cafe".to_string()), "{names:?}");
    assert!(!names.contains(&"Home".to_string()), "{names:?}");
}

/// The NAT over the host's own sockets, offline: a listener on 127.0.0.1. The SYN-ACK
/// comes from a later poll (the connect runs on a thread of its own), then data both
/// ways, and the host's close reaches the guest as a FIN.
#[test]
fn nat_connects_through_a_loopback_listener() {
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
        // (s dropped: the host closes)
    });
    let mut vnet = VirtualNet::new();
    vnet.enable_nat(true); // (loopback is the LAN)
    vnet.poll(wall_ms()); // the NAT's clock is the wall clock here (the chip's tick, in the emulator)
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

/// Without `allow_lan` the NAT refuses this computer's own addresses: a SYN to a listener
/// on 127.0.0.1 gets an RST and the listener sees no connection.
#[test]
fn nat_refuses_loopback_without_allow_lan() {
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
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock), "no connection was made");
    assert_eq!(vnet.nat.as_ref().unwrap().stats.tcp_connects, 0);
}

/// The WDMA engine as core-fm1's driver uses it (SPECSHEET §7.11): a probe request
/// kicked by the length write, the two probe responses read back in two phases each
/// (the big-endian length, then RXWI + frame), RX_AVAIL down once the queue is empty.
/// WDMA_CTL |= bits, as the driver acknowledges.
fn ctl_or(soc: &mut fm1_soc::Soc, bits: u32) {
    let v = soc.read32(0x30F04);
    soc.write32(0x30F04, v | bits);
}

#[test]
fn oss_wifi_wdma_probe_request_gets_the_responses() {
    let mut soc = fm1_soc::Soc::new(1 << 16, vec![0; 0x1000]);
    soc.enable_oss_wifi();
    const RAM_BASE: u32 = 0x01c0_0000;
    const TX_BUF: u32 = RAM_BASE + 0x1000;
    const RX_BUF: u32 = RAM_BASE + 0x2000;
    const CTL: u32 = 0x30F04;
    const TX_ACK: u32 = 1 << 10;
    const RX_ACK: u32 = 1 << 11;
    const RX_AVAIL: u32 = 1 << 17;
    const TX_DONE: u32 = 1 << 20;
    const RX_DONE: u32 = 1 << 21;
    let sta_mac: [u8; 6] = [0x02, 0x53, 0x49, 0x4d, 0x00, 0x15];

    // wdma_init: the control word as the driver writes it reads back without the ack bits
    soc.write32(CTL, 0x00010E37);
    assert_eq!(soc.read32(CTL), 0x00010037);
    // the TX gate the driver polls: pages under the limit, queue not busy, buffer ready
    let limit = (soc.read32(0x3040C) >> 8) & 0xFF;
    let pages = (soc.read32(0x30438) >> 16) & 0xFF;
    assert!(pages < limit && soc.read32(0x30434) & (1 << 18) == 0 && soc.read32(0x3043C) & 0xE0 != 0);

    // a wildcard probe request in the driver's buffer layout: queue word, TXWI, frame, 6 zeros
    let mut probe = vec![0x40, 0x00, 0x00, 0x00];
    probe.extend_from_slice(&[0xFF; 6]);
    probe.extend_from_slice(&sta_mac);
    probe.extend_from_slice(&[0xFF; 6]);
    probe.extend_from_slice(&[0x00; 2]);
    probe.extend_from_slice(&[0, 0]); // SSID IE, wildcard
    soc.write32(TX_BUF, 5); // the queue word (management)
    for i in 0..16u32 {
        soc.write8(TX_BUF + 4 + i, 0); // TXWI
    }
    for (i, &b) in probe.iter().enumerate() {
        soc.write8(TX_BUF + 20 + i as u32, b);
    }
    assert_eq!(soc.read32(CTL) & RX_AVAIL, 0, "nothing to receive yet");
    ctl_or(&mut soc, TX_ACK);
    soc.write32(0x30F10, TX_BUF);
    soc.write32(0x30F14, probe.len() as u32 + 26);
    assert_ne!(soc.read32(CTL) & TX_DONE, 0, "the transmit is done at once");
    ctl_or(&mut soc, TX_ACK);
    assert_eq!(soc.read32(CTL) & TX_DONE, 0, "acknowledged");
    assert_ne!(soc.read32(CTL) & RX_AVAIL, 0, "the probe responses wait");

    // the driver's receive: phase 1 the length, phase 2 RXWI + frame + 4
    let rx_phase = |soc: &mut fm1_soc::Soc, at: u32, n: u32| {
        ctl_or(soc, RX_ACK);
        soc.write32(0x30F18, at);
        soc.write32(0x30F1C, n);
        assert_ne!(soc.read32(CTL) & RX_DONE, 0, "the receive is done at once");
        ctl_or(soc, RX_ACK);
        assert_eq!(soc.read32(CTL) & RX_DONE, 0);
    };
    for ap in ["Home", "Cafe"] {
        assert_ne!(soc.read32(CTL) & RX_AVAIL, 0, "{ap}: a frame waits");
        rx_phase(&mut soc, RX_BUF, 4);
        let rx_len = u32::from_be_bytes([soc.read8(RX_BUF), soc.read8(RX_BUF + 1), soc.read8(RX_BUF + 2), soc.read8(RX_BUF + 3)]);
        assert!(rx_len > 16 && rx_len < 4089, "{ap}: rx_len {rx_len}");
        rx_phase(&mut soc, RX_BUF + 4, rx_len + 4);
        let frame_len = (soc.read32(RX_BUF + 4) >> 16) & 0xFFF;
        assert_eq!(frame_len, rx_len - 16, "{ap}: the RXWI's MPDU length");
        let rssi = (soc.read32(RX_BUF + 4 + 8) & 0xFF) as u8 as i8;
        assert!(rssi < 0, "{ap}: a dBm reading");
        let fc = soc.read8(RX_BUF + 20) as u16 | (soc.read8(RX_BUF + 21) as u16) << 8;
        assert_eq!(fc, 0x0050, "{ap}: a probe response");
        let ssid: Vec<u8> = (0..ap.len() as u32).map(|i| soc.read8(RX_BUF + 20 + 24 + 12 + 2 + i)).collect();
        assert_eq!(ssid, ap.as_bytes(), "the SSID IE after the fixed fields");
    }
    assert_eq!(soc.read32(CTL) & RX_AVAIL, 0, "both read: nothing waits");
}
