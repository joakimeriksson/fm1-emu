// SPDX-License-Identifier: MIT
//! Virtual full-speed MSC peripheral. New implementation from USB 2.0 chapter 9,
//! USB-IF Bulk-Only Transport 1.0 sections 3/5/6 and SCSI command formats.
//! This is a device, not a translation of modd's GPL host driver. Files are
//! opened by the frontend; only bounded sector I/O crosses this boundary.
use super::{Reply, SectorStorage};

const DEVICE: &[u8] = &[18, 1, 0, 2, 0, 0, 0, 64, 0, 0, 1, 0, 0, 1, 1, 2, 0, 1];
const CONFIG: &[u8] = &[
    9, 2, 32, 0, 1, 1, 0, 0x80, 50,
    9, 4, 0, 0, 2, 8, 6, 0x50, 0,
    7, 5, 0x81, 2, 64, 0, 0,
    7, 5, 0x02, 2, 64, 0, 0,
];

#[derive(Default)]
enum Control {
    #[default]
    Idle,
    In { data: Vec<u8>, pos: usize, zlp: bool },
    StatusOut,
    StatusIn(Effect),
    Stalled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Memory {
        bytes: Arc<Mutex<Vec<u8>>>,
        fail_read: bool,
        fail_write: Option<u32>,
    }
    impl SectorStorage for Memory {
        fn sectors(&self) -> u32 { (self.bytes.lock().unwrap().len() / 512) as u32 }
        fn read_sector(&mut self, lba: u32, d: &mut [u8; 512]) -> std::io::Result<()> {
            if self.fail_read { return Err(std::io::Error::other("read failure")); }
            let at = lba as usize * 512;
            d.copy_from_slice(&self.bytes.lock().unwrap()[at..at + 512]);
            Ok(())
        }
        fn write_sector(&mut self, lba: u32, d: &[u8; 512]) -> std::io::Result<()> {
            if self.fail_write == Some(lba) { return Err(std::io::Error::other("write failure")); }
            let at = lba as usize * 512;
            self.bytes.lock().unwrap()[at..at + 512].copy_from_slice(d);
            Ok(())
        }
    }

    fn device() -> (MscDevice, Arc<Mutex<Vec<u8>>>) {
        let bytes = Arc::new(Mutex::new(vec![0; 2048]));
        let mut d = MscDevice::new(Box::new(Memory { bytes: bytes.clone(), fail_read: false, fail_write: None }));
        status(&mut d, [0, 5, 1, 0, 0, 0, 0, 0]);
        status(&mut d, [0, 9, 1, 0, 0, 0, 0, 0]);
        (d, bytes)
    }
    fn status(d: &mut MscDevice, s: [u8; 8]) {
        assert_eq!(d.setup(&s), Reply::Ack);
        assert_eq!(d.control_in(true), Reply::Data(vec![]));
    }
    fn clear(d: &mut MscDevice, input: bool) {
        status(d, [2, 1, 0, 0, if input { 0x81 } else { 2 }, 0, 0, 0]);
    }
    fn recover(d: &mut MscDevice) {
        status(d, [0x21, 0xff, 0, 0, 0, 0, 0, 0]);
        clear(d, true);
        clear(d, false);
    }
    fn cbw(cdb: &[u8], length: u32, input: bool) -> Vec<u8> {
        let mut p = b"USBC".to_vec();
        p.extend(0x12345678u32.to_le_bytes());
        p.extend(length.to_le_bytes());
        p.extend([if input { 0x80 } else { 0 }, 0, cdb.len() as u8]);
        p.extend(cdb);
        p.resize(31, 0);
        p
    }
    fn rw(op: u8, lba: u32, count: u16) -> [u8; 10] {
        let mut c = [op, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        c[2..6].copy_from_slice(&lba.to_be_bytes());
        c[7..9].copy_from_slice(&count.to_be_bytes());
        c
    }
    fn data(r: Reply) -> Vec<u8> {
        match r { Reply::Data(d) => d, other => panic!("expected data: {other:?}") }
    }
    fn csw(d: &mut MscDevice, residue: u32, status: u8) {
        let p = data(d.bulk_in(1));
        assert_eq!(&p[..8], b"USBS\x78\x56\x34\x12");
        assert_eq!(u32::from_le_bytes(p[8..12].try_into().unwrap()), residue);
        assert_eq!(p.len(), 13);
        assert_eq!(p[12], status);
    }

    #[test]
    fn control_status_owns_address_configuration_and_reset_effects() {
        let (mut d, _) = device();
        d.reset();
        assert_eq!(d.setup(&[0, 5, 7, 0, 0, 0, 0, 0]), Reply::Ack);
        assert_eq!(d.address, 0);
        // Superseding SETUP cancels the pending address change.
        assert_eq!(d.setup(&[0x80, 6, 0, 1, 0, 0, 18, 0]), Reply::Ack);
        assert_eq!(data(d.control_in(false)), DEVICE);
        assert_eq!(d.control_out(&[], true), Reply::Ack);
        assert_eq!(d.address, 0);
        status(&mut d, [0, 5, 7, 0, 0, 0, 0, 0]);
        assert_eq!(d.address, 7);
        assert_eq!(d.setup(&[0, 9, 1, 0, 0, 0, 0, 0]), Reply::Ack);
        assert_eq!(d.bulk_in(1), Reply::Stall);
        assert_eq!(d.control_in(true), Reply::Data(vec![]));
        assert_eq!(d.bulk_in(1), Reply::Nak);
        assert_eq!(d.setup(&[0xa1, 0xfe, 0, 0, 0, 0, 1, 0]), Reply::Ack);
        assert_eq!(data(d.control_in(false)), [0]);
        assert_eq!(d.control_out(&[], true), Reply::Ack);
        assert_eq!(d.setup(&[0xa1, 0xfe, 0, 0, 1, 0, 1, 0]), Reply::Stall);
        assert_eq!(d.control_in(false), Reply::Stall);
        assert_eq!(d.setup(&[0x80, 6, 2, 3, 9, 4, 255, 0]), Reply::Ack);
        let first = data(d.control_in(false));
        assert_eq!(first.len(), 64);
        let last = data(d.control_in(false));
        assert_eq!(first[0] as usize, first.len() + last.len());
        assert_eq!(d.control_out(&[], true), Reply::Ack);
    }

    #[test]
    fn inquiry_capacity_and_multiple_distinct_sectors() {
        let (mut d, bytes) = device();
        assert_eq!(d.bulk_out(2, &cbw(&[0x12, 0, 0, 0, 36, 0], 36, true)), Reply::Ack);
        let inquiry = data(d.bulk_in(1));
        assert_eq!(inquiry.len(), 36);
        assert_eq!(&inquiry[8..16], b"FM1EMU  ");
        csw(&mut d, 0, 0);
        assert_eq!(d.bulk_out(2, &cbw(&rw(0x25, 0, 0), 8, true)), Reply::Ack);
        assert_eq!(data(d.bulk_in(1)), [0, 0, 0, 3, 0, 0, 2, 0]);
        csw(&mut d, 0, 0);
        let pattern: Vec<u8> = (0..1536).map(|i| (i * 19 + i / 512 * 33) as u8).collect();
        assert_eq!(d.bulk_out(2, &cbw(&rw(0x2a, 1, 3), 1536, false)), Reply::Ack);
        for p in pattern.chunks(64) { assert_eq!(d.bulk_out(2, p), Reply::Ack); }
        csw(&mut d, 0, 0);
        assert_eq!(&bytes.lock().unwrap()[512..], pattern);
        assert_eq!(d.bulk_out(2, &cbw(&rw(0x28, 1, 3), 1536, true)), Reply::Ack);
        let actual: Vec<u8> = (0..24).flat_map(|_| data(d.bulk_in(1))).collect();
        assert_eq!(actual, pattern);
        csw(&mut d, 0, 0);
    }

    #[test]
    fn malformed_cbw_requires_reset_then_both_halt_clears() {
        for kind in 0..6 {
            let (mut d, _) = device();
            let mut p = cbw(&[0; 6], 0, false);
            match kind { 0 => { p.pop(); }, 1 => p[0] = 0, 2 => p[12] = 1,
                3 => p[13] = 1, 4 => p[14] = 0, _ => p[14] = 17 }
            assert_eq!(d.bulk_out(2, &p), Reply::Stall);
            clear(&mut d, true);
            clear(&mut d, false);
            assert_eq!(d.bulk_in(1), Reply::Stall);
            assert_eq!(d.bulk_out(2, &cbw(&[0; 6], 0, false)), Reply::Stall);
            status(&mut d, [0x21, 0xff, 0, 0, 0, 0, 0, 0]);
            clear(&mut d, true);
            assert_eq!(d.bulk_out(2, &cbw(&[0; 6], 0, false)), Reply::Stall);
            clear(&mut d, false);
            assert_eq!(d.bulk_out(2, &cbw(&[0; 6], 0, false)), Reply::Ack);
            csw(&mut d, 0, 0);
        }
    }

    #[test]
    fn all_thirteen_bot_data_cases() {
        // BOT 1.0 table 6.1; send/receive intended data where permitted.
        for (host, hin, device_len, din, status, residue, bytes) in [
            (0, false, 0, false, 0, 0, 0), (0, false, 512, true, 2, 0, 0),
            (0, true, 512, false, 2, 0, 0), (64, true, 0, false, 0, 64, 0),
            (1024, true, 512, true, 0, 512, 512), (512, true, 512, true, 0, 0, 512),
            (64, true, 512, true, 2, 0, 64), (512, true, 512, false, 2, 512, 0),
            (64, false, 0, false, 0, 64, 0), (512, false, 512, true, 2, 512, 0),
            (1024, false, 512, false, 0, 512, 512), (512, false, 512, false, 0, 0, 512),
            (64, false, 512, false, 2, 64, 0),
        ] {
            let (mut d, _) = device();
            let cdb = if device_len == 0 { vec![0; 6] } else { rw(if din { 0x28 } else { 0x2a }, 0, 1).to_vec() };
            assert_eq!(d.bulk_out(2, &cbw(&cdb, host, hin)), Reply::Ack);
            for _ in 0..bytes / 64 {
                if hin { assert_eq!(data(d.bulk_in(1)).len(), 64); }
                else { assert_eq!(d.bulk_out(2, &[0xa5; 64]), Reply::Ack); }
            }
            if d.halt[0] { assert_eq!(d.bulk_in(1), Reply::Stall); clear(&mut d, true); }
            if d.halt[1] { assert_eq!(d.bulk_out(2, &[0; 64]), Reply::Stall); clear(&mut d, false); }
            csw(&mut d, residue, status);
            if status == 2 { assert_eq!(d.bulk_in(1), Reply::Stall); recover(&mut d); }
            assert_eq!(d.bulk_out(2, &cbw(&[0; 6], 0, false)), Reply::Ack);
            csw(&mut d, 0, 0);
        }
    }

    #[test]
    fn bounds_io_errors_short_write_and_reset_preserve_completed_sectors() {
        let (mut d, bytes) = device();
        for lba in [4, u32::MAX] {
            assert_eq!(d.bulk_out(2, &cbw(&rw(0x28, lba, 1), 512, true)), Reply::Ack);
            assert_eq!(d.bulk_in(1), Reply::Stall);
            clear(&mut d, true);
            csw(&mut d, 512, 1);
            assert_eq!(d.bulk_out(2, &cbw(&[3, 0, 0, 0, 18, 0], 18, true)), Reply::Ack);
            let sense = data(d.bulk_in(1));
            assert_eq!((sense[2], sense[12]), (5, 0x21));
            csw(&mut d, 0, 0);
        }
        d.storage = Box::new(Memory { bytes: bytes.clone(), fail_read: true, fail_write: Some(1) });
        assert_eq!(d.bulk_out(2, &cbw(&rw(0x28, 0, 1), 512, true)), Reply::Ack);
        assert_eq!(d.bulk_in(1), Reply::Stall);
        clear(&mut d, true);
        csw(&mut d, 512, 1);
        assert_eq!(d.sense, [3, 0x11, 0]);
        assert_eq!(d.bulk_out(2, &cbw(&rw(0x2a, 0, 2), 1024, false)), Reply::Ack);
        for _ in 0..15 { assert_eq!(d.bulk_out(2, &[0x55; 64]), Reply::Ack); }
        assert_eq!(d.bulk_out(2, &[0x55; 64]), Reply::Stall);
        clear(&mut d, false);
        csw(&mut d, 512, 1);
        assert_eq!(&bytes.lock().unwrap()[..512], &[0x55; 512]);
        assert_eq!(&bytes.lock().unwrap()[512..1024], &[0; 512]);
        assert_eq!(d.sense, [3, 0x0c, 0]);
        assert_eq!(d.bulk_out(2, &cbw(&rw(0x2a, 0, 1), 512, false)), Reply::Ack);
        assert_eq!(d.bulk_out(2, &[0xa5; 13]), Reply::Ack);
        clear(&mut d, false);
        csw(&mut d, 512, 2);
        recover(&mut d);
        assert_eq!(d.bulk_out(2, &cbw(&rw(0x2a, 0, 1), 512, false)), Reply::Ack);
        assert_eq!(d.bulk_out(2, &[0xa5; 64]), Reply::Ack);
        d.reset();
        assert_eq!(&bytes.lock().unwrap()[..512], &[0x55; 512]);
        assert_eq!(d.address, 0);
        assert_eq!(d.bulk_in(1), Reply::Stall);
    }
}

enum Effect {
    Address(u8),
    Configuration(u8),
    Halt(usize, bool),
    BotReset,
    None,
}

#[derive(Clone, Copy, PartialEq, Default)]
enum Phase { #[default] Cbw, In, Out, Csw, Locked }
#[derive(Clone, Copy, PartialEq, Default)]
enum Source { #[default] Small, Read, Write }

#[derive(Default)]
struct Bot {
    phase: Phase,
    source: Source,
    tag: u32,
    residue: u32,
    status: u8,
    remaining: u32,
    offset: usize,
    lba: u32,
    halt_after: bool,
}

pub(super) struct MscDevice {
    pub(super) storage: Box<dyn SectorStorage>,
    pub(super) address: u8,
    configuration: u8,
    control: Control,
    bot: Bot,
    sector: [u8; 512],
    /// IN, OUT. Invalid CBWs additionally require reset before either clear.
    halt: [bool; 2],
    recovery: u8,
    sense: [u8; 3],
}

impl MscDevice {
    pub(super) fn new(storage: Box<dyn SectorStorage>) -> Self {
        Self { storage, address: 0, configuration: 0, control: Control::Idle,
            bot: Bot::default(), sector: [0; 512], halt: [false; 2], recovery: 0, sense: [0; 3] }
    }

    pub(super) fn reset(&mut self) {
        self.address = 0;
        self.configuration = 0;
        self.control = Control::Idle;
        self.bot = Bot::default();
        self.halt = [false; 2];
        self.recovery = 0;
        self.sense = [0; 3];
    }

    pub(super) fn setup(&mut self, packet: &[u8]) -> Reply {
        self.control = Control::Stalled; // a new SETUP aborts any old control transfer
        if packet.len() != 8 { return Reply::Stall; }
        let value = u16::from_le_bytes([packet[2], packet[3]]);
        let index = u16::from_le_bytes([packet[4], packet[5]]);
        let length = u16::from_le_bytes([packet[6], packet[7]]) as usize;
        let endpoint = match index { 0x81 => Some(0), 0x02 => Some(1), _ => None };
        let mut data = Vec::new();
        let effect = match (packet[0], packet[1]) {
            (0x80, 6) => {
                data = match (value >> 8, value & 255) {
                    (1, 0) if index == 0 => DEVICE.to_vec(),
                    (2, 0) if index == 0 => CONFIG.to_vec(),
                    (3, 0) if index == 0 => vec![4, 3, 9, 4],
                    (3, n @ 1..=2) if index == 0x409 => {
                        let s = if n == 1 { "FM1 emulator" } else { "FM1 virtual full-speed USB flash drive" };
                        let mut d = vec![0, 3];
                        for c in s.encode_utf16() { d.extend(c.to_le_bytes()); }
                        d[0] = d.len() as u8;
                        d
                    }
                    _ => return Reply::Stall,
                };
                Effect::None
            }
            (0, 5) if value <= 127 && index == 0 && length == 0 && self.configuration == 0 => Effect::Address(value as u8),
            (0, 9) if value <= 1 && index == 0 && length == 0 && self.address != 0 => Effect::Configuration(value as u8),
            (0x80, 8) if value == 0 && index == 0 && length == 1 => {
                data.push(self.configuration); Effect::None
            }
            (0x80, 0) if value == 0 && index == 0 && length == 2 => {
                data.extend([0, 0]); Effect::None
            }
            (0x81, 0) if self.configuration == 1 && value == 0 && index == 0 && length == 2 => {
                data.extend([0, 0]); Effect::None
            }
            (0x82, 0) if self.configuration == 1 && value == 0 && length == 2 && endpoint.is_some() => {
                data.extend([u8::from(self.halt[endpoint.unwrap()]), 0]); Effect::None
            }
            (2, request @ (1 | 3)) if self.configuration == 1 && value == 0 && length == 0 && endpoint.is_some() => {
                Effect::Halt(endpoint.unwrap(), request == 3)
            }
            (0x81, 10) if self.configuration == 1 && value == 0 && index == 0 && length == 1 => {
                data.push(0); Effect::None
            }
            (1, 11) if self.configuration == 1 && value == 0 && index == 0 && length == 0 => Effect::None,
            (0xa1, 0xfe) if self.configuration == 1 && value == 0 && index == 0 && length == 1 => {
                data.push(0); Effect::None
            }
            (0x21, 0xff) if self.configuration == 1 && value == 0 && index == 0 && length == 0 => Effect::BotReset,
            _ => return Reply::Stall,
        };
        self.control = if packet[0] & 0x80 != 0 && length != 0 {
            data.truncate(length);
            let zlp = data.len() < length && data.len() % 64 == 0;
            Control::In { data, pos: 0, zlp }
        } else if packet[0] & 0x80 != 0 {
            Control::StatusOut
        } else {
            Control::StatusIn(effect)
        };
        Reply::Ack
    }

    pub(super) fn control_in(&mut self, status: bool) -> Reply {
        match std::mem::take(&mut self.control) {
            Control::In { data, pos, zlp } if !status => {
                let end = (pos + 64).min(data.len());
                let packet = data[pos..end].to_vec();
                self.control = if end < data.len() || (zlp && pos < data.len()) {
                    Control::In { data, pos: end, zlp }
                } else { Control::StatusOut };
                Reply::Data(packet)
            }
            Control::StatusIn(effect) if status => {
                match effect {
                    Effect::Address(a) => self.address = a,
                    Effect::Configuration(c) => {
                        self.configuration = c;
                        self.bot = Bot::default();
                        self.halt = [false; 2];
                        self.recovery = 0;
                    }
                    Effect::Halt(ep, halt) => {
                        self.halt[ep] = halt;
                        if !halt && self.recovery & 1 == 0 { self.recovery &= !(2 << ep); }
                    }
                    Effect::BotReset => {
                        self.bot = Bot::default();
                        self.recovery = 6; // reset preserves halts; both clears complete recovery
                    }
                    Effect::None => {}
                }
                Reply::Data(Vec::new())
            }
            _ => { self.control = Control::Stalled; Reply::Stall }
        }
    }

    pub(super) fn control_out(&mut self, data: &[u8], status: bool) -> Reply {
        if matches!(self.control, Control::StatusOut) && status && data.is_empty() {
            self.control = Control::Idle;
            Reply::Ack
        } else { self.control = Control::Stalled; Reply::Stall }
    }

    fn lock(&mut self) -> Reply {
        self.bot.phase = Phase::Locked;
        self.halt = [true; 2];
        self.recovery = 7;
        Reply::Stall
    }

    fn command_failed(&mut self, key: u8, asc: u8) {
        self.sense = [key, asc, 0];
        self.bot.status = 1;
    }

    fn cbw(&mut self, p: &[u8]) -> Reply {
        if p.len() != 31 || &p[..4] != b"USBC" || p[12] & 0x7f != 0 || p[13] != 0 || !(1..=16).contains(&p[14]) {
            return self.lock();
        }
        let host_len = u32::from_le_bytes(p[8..12].try_into().unwrap());
        let host_in = p[12] & 0x80 != 0;
        self.bot = Bot { tag: u32::from_le_bytes(p[4..8].try_into().unwrap()), residue: host_len,
            phase: Phase::Csw, ..Default::default() };
        self.sector.fill(0);
        let c = &p[15..31];
        let mut intended = 0;
        let mut device_in = true;
        let minimum = match c[0] { 0x00 | 0x03 | 0x12 => 6, 0x25 | 0x28 | 0x2a => 10, _ => 1 };
        if p[14] < minimum {
            self.command_failed(5, 0x24);
        } else {
            match c[0] {
                0x00 => {},
                0x12 if c[1] == 0 && c[2] == 0 => {
                    self.sector[..8].copy_from_slice(&[0, 0x80, 4, 2, 31, 0, 0, 0]);
                    self.sector[8..16].copy_from_slice(b"FM1EMU  ");
                    self.sector[16..32].copy_from_slice(b"Virtual USB Disk");
                    self.sector[32..36].copy_from_slice(b"0001");
                    intended = 36.min(c[4] as u32);
                }
                0x03 if c[1] & 1 == 0 => {
                    self.sector[0] = 0x70;
                    self.sector[2] = self.sense[0];
                    self.sector[7] = 10;
                    self.sector[12..14].copy_from_slice(&self.sense[1..]);
                    self.sense = [0; 3];
                    intended = 18.min(c[4] as u32);
                }
                0x25 if c[8] == 0 && c[2..6] == [0; 4] => {
                    self.sector[..4].copy_from_slice(&self.storage.sectors().saturating_sub(1).to_be_bytes());
                    self.sector[4..8].copy_from_slice(&512u32.to_be_bytes());
                    intended = 8;
                }
                0x28 | 0x2a => {
                    let lba = u32::from_be_bytes(c[2..6].try_into().unwrap());
                    let count = u16::from_be_bytes([c[7], c[8]]) as u32;
                    if c[1] & !0x18 != 0 || c[6] != 0 {
                        self.command_failed(5, 0x24);
                    } else if count != 0 && (lba >= self.storage.sectors() || count > self.storage.sectors() - lba) {
                        self.command_failed(5, 0x21);
                    } else {
                        intended = count * 512;
                        device_in = c[0] == 0x28;
                        self.bot.source = if device_in { Source::Read } else { Source::Write };
                        self.bot.lba = lba;
                    }
                }
                0x12 | 0x03 | 0x25 => self.command_failed(5, 0x24),
                _ => self.command_failed(5, 0x20),
            }
        }
        // BOT table 6.1: zero host length ignores direction; mismatched direction
        // or an undersized host transfer yields phase error, not command failure.
        if host_len == 0 {
            if intended != 0 { self.bot.status = 2; }
        } else if intended == 0 {
            self.halt[if host_in { 0 } else { 1 }] = true;
        } else if host_in != device_in || (!host_in && host_len < intended) {
            self.bot.status = 2;
            self.halt[if host_in { 0 } else { 1 }] = true;
        } else {
            self.bot.remaining = intended.min(host_len);
            self.bot.phase = if host_in { Phase::In } else { Phase::Out };
            self.bot.halt_after = host_len > intended;
            if host_len < intended { self.bot.status = 2; }
        }
        Reply::Ack
    }

    pub(super) fn bulk_out(&mut self, ep: u8, data: &[u8]) -> Reply {
        if ep != 2 || self.configuration != 1 { return Reply::Stall; }
        if self.halt[1] || self.recovery != 0 { return Reply::Stall; }
        if self.bot.phase == Phase::Cbw { return self.cbw(data); }
        if self.bot.phase != Phase::Out || data.len() > 64 { return self.lock(); }
        if data.is_empty() || data.len() as u32 > self.bot.remaining {
            self.bot.phase = Phase::Csw;
            self.bot.status = 2;
            self.halt[1] = true;
            return Reply::Stall;
        }
        let end = self.bot.offset + data.len();
        self.sector[self.bot.offset..end].copy_from_slice(data);
        self.bot.offset = end;
        self.bot.remaining -= data.len() as u32;
        if end == 512 {
            if self.storage.write_sector(self.bot.lba, &self.sector).is_err() {
                self.command_failed(3, 0x0c);
                self.bot.phase = Phase::Csw;
                self.halt[1] = true;
                return Reply::Stall;
            }
            self.bot.lba += 1;
            self.bot.offset = 0;
            self.bot.residue -= 512;
        }
        if self.bot.remaining == 0 {
            self.bot.phase = Phase::Csw;
            self.halt[1] = self.bot.halt_after;
        } else if data.len() < 64 {
            self.bot.phase = Phase::Csw;
            self.bot.status = 2;
            self.halt[1] = true;
        }
        Reply::Ack
    }

    pub(super) fn bulk_in(&mut self, ep: u8) -> Reply {
        if ep != 1 || self.configuration != 1 || self.halt[0] || self.recovery != 0 { return Reply::Stall; }
        match self.bot.phase {
            Phase::In => {
                if self.bot.source == Source::Read && self.bot.offset == 0 &&
                    self.storage.read_sector(self.bot.lba, &mut self.sector).is_err() {
                    self.command_failed(3, 0x11);
                    self.bot.phase = Phase::Csw;
                    self.halt[0] = true;
                    return Reply::Stall;
                }
                let n = self.bot.remaining.min(64) as usize;
                let data = self.sector[self.bot.offset..self.bot.offset + n].to_vec();
                self.bot.offset += n;
                self.bot.remaining -= n as u32;
                self.bot.residue -= n as u32;
                if self.bot.offset == 512 { self.bot.offset = 0; self.bot.lba += 1; }
                if self.bot.remaining == 0 {
                    self.bot.phase = Phase::Csw;
                    self.halt[0] = self.bot.halt_after;
                }
                Reply::Data(data)
            }
            Phase::Csw => {
                let mut csw = b"USBS".to_vec();
                csw.extend(self.bot.tag.to_le_bytes());
                csw.extend(self.bot.residue.to_le_bytes());
                csw.push(self.bot.status);
                if self.bot.status == 2 { self.lock(); } else { self.bot = Bot::default(); }
                Reply::Data(csw)
            }
            Phase::Cbw | Phase::Out => Reply::Nak,
            Phase::Locked => Reply::Stall,
        }
    }
}
