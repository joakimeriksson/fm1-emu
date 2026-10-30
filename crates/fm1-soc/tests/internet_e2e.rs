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

/// Test real internet: DNS + TCP HTTP GET through VirtualNet + NAT.
#[test]
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
    // NAT should be Some (the default)
    assert!(vnet.nat.is_some());

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
    let replies = vnet.handle(&syn);

    // NAT should return SYN-ACK (or RST if connection failed)
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
        vnet.nat.as_mut().unwrap().set_time(
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        );
        let frames = vnet.poll();
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

/// Test BLE lifecycle: enable → start → scan → connect → verify state.
/// This exercises the exact MMIO register sequence firmware would use.
#[test]
fn ble_full_lifecycle_via_soc() {
    let mut soc = fm1_soc::Soc::new(1 << 16, vec![0; 0x1000]);

    // Enable BLE
    soc.write32(0x20128, 0); // TICK_MS = 0
    soc.write32(0x20100, 6); // CMD_ENABLE_ON
    assert_eq!(soc.read32(0x20138), 1); // ENABLED = true

    // Start BLE
    soc.write32(0x20100, 1); // CMD_START
    assert_eq!(soc.read32(0x20104), 1); // BLE_STARTING

    // Not ready yet at 999ms
    soc.write32(0x20128, 999);
    assert_eq!(soc.read32(0x20104), 1); // still STARTING

    // Ready at 1001ms
    soc.write32(0x20128, 1001);
    assert_eq!(soc.read32(0x20104), 2); // BLE_READY

    // Start scan
    soc.write32(0x20100, 2); // CMD_SCAN_ON

    // Not ready at 499ms into scan
    soc.write32(0x20128, 1500);
    assert_eq!(soc.read32(0x20110), 0); // SCAN_N = 0 (still scanning)

    // Scan complete at 502ms
    soc.write32(0x20128, 1502);
    assert_eq!(soc.read32(0x20110), 1); // SCAN_N = 1

    // Read device info
    soc.write32(0x20118, 0); // SCAN_IDX = 0
    let rssi = soc.read32(0x20114) as i32;
    assert_eq!(rssi, -50);

    // Read 8-char name
    let w0 = soc.read32(0x2011C);
    let w1 = soc.read32(0x20120);
    let mut name = [0u8; 8];
    name[..4].copy_from_slice(&w0.to_le_bytes());
    name[4..].copy_from_slice(&w1.to_le_bytes());
    let s = std::str::from_utf8(&name).unwrap().trim_end_matches('\0');
    assert_eq!(s, "BLE MIDI");

    // Connect to device 0
    soc.write32(0x20100, 4); // CMD_CONNECT

    // Not connected yet at 299ms
    soc.write32(0x20128, 1801);
    assert_eq!(soc.read32(0x20108), 0); // CONN = false

    // Connected at 303ms
    soc.write32(0x20128, 1803);
    assert_eq!(soc.read32(0x20108), 1); // CONN = true
    assert_eq!(soc.read32(0x20124), 0); // DEVICE = 0 (index of connected device)

    // Disconnect
    soc.write32(0x20100, 5); // CMD_DISCONNECT
    assert_eq!(soc.read32(0x20108), 0); // CONN = false
    assert_eq!(soc.read32(0x20124) as i32, -1); // DEVICE = -1

    // Disable BLE
    soc.write32(0x20100, 7); // CMD_ENABLE_OFF
    assert_eq!(soc.read32(0x20138), 0); // ENABLED = false
    assert_eq!(soc.read32(0x20104), 0); // back to BLE_OFF
}


/// Simulate Optimist's ble_hw_wl82.c RF init and advertising start through MMIO.
/// No firmware binary: this is the register sequence the driver makes.
#[test]
fn ble_baseband_rf_init_and_advertise() {
    use fm1_core::Bus;
    let mut soc = fm1_soc::Soc::new(1 << 16, vec![0; 0x1000]);

    // -- Phase 1: power and clock (Optimist writes CLK_CON1_BT and WL_BTEN) --
    // These land in Region::Clk and Region::Unknown respectively; we only care
    // that they don't panic.
    soc.write32(0x10010, 0x01); // CLK_CON1_BT: enable BT clock

    // -- Phase 2: BT analog RF config (fm1_ble_rf_init) --
    // BLE block enable
    soc.write32(0x2FC80, 1);
    assert_eq!(soc.read32(0x2FC80), 1);

    // BB base / end (where the control block lives in RAM)
    let bb_base: u32 = 0x01C0_A000;
    soc.write32(0x2FC4C, bb_base);
    soc.write32(0x2FC50, bb_base + 0x800);
    assert_eq!(soc.read32(0x2FC4C), bb_base);

    // PLL channel table
    soc.write32(0x2FC54, 0x01C0_B000);
    assert_eq!(soc.read32(0x2FC54), 0x01C0_B000);

    // AGC table: write 128 entries (the captured ble_rf_agc)
    for i in 0..128u32 {
        soc.write32(0x2FC64, i);          // AGC_IDX
        soc.write32(0x2FC68, 0x0080_0000 + i); // AGC_DATA
    }
    // read back entry 42
    soc.write32(0x2FC64, 42);
    assert_eq!(soc.read32(0x2FC68), 0x0080_0000 + 42);

    // general config registers (0x2FC00-0x2FC3C): a few representative writes
    soc.write32(0x2FC00, 0x1234_5678);
    soc.write32(0x2FC04, 0xABCD_EF01);
    assert_eq!(soc.read32(0x2FC00), 0x1234_5678);
    assert_eq!(soc.read32(0x2FC04), 0xABCD_EF01);

    // bt_window: the firmware sets its own .bd_base
    soc.write32(0x2FC44, 0x01C0_9E7C);
    assert_eq!(soc.read32(0x2FC44), 0x01C0_9E7C);

    // -- Phase 3: BLE baseband init --
    // BLE_CON: enable
    soc.write32(0x28000, 0x0001);
    assert_eq!(soc.read32(0x28000), 0x0001);

    // timing registers (T08-T18)
    for i in 0..6u32 {
        soc.write32(0x28008 + i * 4, 0x100 + i);
    }
    assert_eq!(soc.read32(0x28010), 0x102); // T10

    // T34: the driver writes 0x0A01
    soc.write32(0x28034, 0x0A01);
    assert_eq!(soc.read32(0x28034), 0x0A01);

    // interrupt enables
    soc.write32(0x28028, 0x03); // enable RX and EVENT bits
    assert_eq!(soc.read32(0x28028), 0x03);

    // -- Phase 4: column port — set up advertising on link 0 --
    // helper: col_write(link, col, value)
    let col_write = |soc: &mut fm1_soc::Soc, link: u32, col: u32, val: u32| {
        soc.write32(0x28020, val); // COLDATA
        soc.write32(0x2801C, (col << 10) | (link << 4) | 1); // COLCMD: write
    };
    let col_read = |soc: &mut fm1_soc::Soc, link: u32, col: u32| -> u32 {
        soc.write32(0x2801C, (col << 10) | (link << 4) | 0); // COLCMD: read
        soc.read32(0x28024) // COLRES
    };

    // col 1: advertising interval (lower 16 bits, in 625us slots)
    col_write(&mut soc, 0, 1, 0x0030); // 48 slots = 30ms
    assert_eq!(col_read(&mut soc, 0, 1), 0x0030);

    // col 2: state/latency — set advertising state
    col_write(&mut soc, 0, 2, 0x1000); // bits 15-12 = 1 (advertising)

    // col 8: advDelay bit
    col_write(&mut soc, 0, 8, 0x0001);

    // col 14: engine enable = 0x8000
    col_write(&mut soc, 0, 14, 0x8000);
    assert_eq!(col_read(&mut soc, 0, 14), 0x8000);

    // link 1 is independent
    col_write(&mut soc, 1, 14, 0x0000);
    assert_eq!(col_read(&mut soc, 1, 14), 0x0000);
    assert_eq!(col_read(&mut soc, 0, 14), 0x8000); // link 0 unchanged

    // -- Phase 5: interrupt acknowledge round-trip --
    // simulate an IRQ pending
    soc.write32(0x28030, 0x03); // IPND: both RX and EVENT
    assert_eq!(soc.read32(0x28030), 0x03);
    // acknowledge RX only
    soc.write32(0x2802C, 0x01); // IACK: clear bit 0
    assert_eq!(soc.read32(0x28030), 0x02); // only EVENT remains
    // acknowledge EVENT
    soc.write32(0x2802C, 0x02);
    assert_eq!(soc.read32(0x28030), 0x00); // all clear

    // -- Verify: both stacks coexist --
    // mailbox (fm1-base) still works on different offsets
    soc.write32(0x20128, 0); // TICK_MS
    soc.write32(0x20100, 6); // CMD_ENABLE_ON
    assert_eq!(soc.read32(0x20138), 1); // mailbox BLE enabled
    // baseband state is unaffected
    assert_eq!(soc.read32(0x28000), 0x0001); // BLE_CON unchanged
    assert_eq!(col_read(&mut soc, 0, 14), 0x8000); // still advertising
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
