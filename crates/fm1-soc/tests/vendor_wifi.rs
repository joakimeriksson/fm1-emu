//! Integration tests for the vendor WiFi stack's mailbox (--wifi jieli).
//!
//! Drives the Soc's MMIO registers at 0x30000..0x3011f exactly as the firmware (fm1-base's
//! plat_wifi_* built for the emulator) would: the state machine, scan results, and network
//! frames to and from the virtual subnet. (The NAT itself is covered in internet_e2e.rs.)

use fm1_core::Bus;
use fm1_soc::net::{GW_MAC, STA_MAC};

fn be16(b: &[u8]) -> u16 {
    ((b[0] as u16) << 8) | b[1] as u16
}
fn put_be16(b: &mut [u8], v: u16) {
    b[0] = (v >> 8) as u8;
    b[1] = v as u8;
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

/// The frames a connected firmware sends go to the SoC's subnet, and what the NAT's tick
/// brings comes back through the mailbox; before the join nothing moves, and a firmware that
/// stops reading does not grow the queue without bound.
#[test]
fn mailbox_frames_follow_the_connection_and_the_queue_is_bounded() {
    let mut soc = make_soc();
    assert!(soc.net.is_some(), "the subnet comes with the mailbox");
    soc.write32(CMD, CMD_ON);
    advance_ms(&mut soc, 500);
    tx_frame(&mut soc, &make_arp_who_has([10, 0, 2, 2]));
    assert_eq!(soc.read32(NET_RX_READY), 0, "not connected: the frame goes nowhere");
    connect_soc(&mut soc);
    for _ in 0..100 {
        tx_frame(&mut soc, &make_arp_who_has([10, 0, 2, 2]));
    }
    let mut got = 0;
    while rx_frame(&mut soc).is_some() {
        got += 1;
    }
    assert!(got >= 64 && got < 100, "{got} replies kept of 100: the queue is capped");
    // the network's tick runs the pump without the firmware writing anything
    let step = soc.periph.cycle_of_ms(10).max(1);
    soc.advance(step);
    soc.advance(step);
    assert_eq!(soc.read32(NET_RX_READY), 0);
}

/// --wifi-ssid / --wifi-password: the configured network is the one around and the one kept;
/// a join needs its passphrase, the old default no longer opens it.
#[test]
fn the_access_point_is_configurable() {
    let mut soc = fm1_soc::Soc::new(1 << 16, vec![0; 0x1000]);
    soc.set_wifi_ap(fm1_soc::net::Ap { ssid: "Studio".into(), password: "letmein12".into() });
    soc.enable_vendor_wifi();
    soc.write32(SAVED_IDX, 0);
    assert_eq!(read_ssid(&mut soc, SAVED_SSID0), "Studio", "kept, as Home was");
    soc.write32(CMD, CMD_ON);
    advance_ms(&mut soc, 600);
    soc.write32(CMD, CMD_SCAN);
    soc.write32(SCAN_IDX, 0);
    assert_eq!(read_ssid(&mut soc, SCAN_SSID0), "Studio");
    assert_eq!(soc.read32(SCAN_SEC), 1);
    soc.write32(SCAN_IDX, 1);
    assert_eq!(read_ssid(&mut soc, SCAN_SSID0), "Cafe");
    // the saved network joined by itself at 500 ms; forget it, then join with the passwords
    soc.write32(CMD, CMD_FORGET);
    assert_eq!(soc.read32(SAVED_N), 0);
    write_ssid(&mut soc, JOIN_SSID0, "Studio");
    write_ssid(&mut soc, JOIN_PASS0, "fm1pass");
    soc.write32(CMD, CMD_JOIN);
    assert_eq!(read_ssid(&mut soc, WHY0), "wrong password");
    write_ssid(&mut soc, JOIN_PASS0, "letmein12");
    soc.write32(CMD, CMD_JOIN);
    assert_eq!(read_ssid(&mut soc, WHY0), "");
    assert_eq!(soc.read32(STATE), 4, "joining");
    // a change after the fact reaches the running mailbox too
    soc.set_wifi_ap(fm1_soc::net::Ap { ssid: "Garage".into(), password: "opensesame".into() });
    soc.write32(SCAN_IDX, 0);
    assert_eq!(read_ssid(&mut soc, SCAN_SSID0), "Garage");
}
