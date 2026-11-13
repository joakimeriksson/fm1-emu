//! FM-1 update packages (.fwsc) and the flash they become, for running M-VAVE's stock
//! firmware (community firmwares only need their app.bin: web/fwsc.js).
//!
//! The format is web/fwsc.js's (see there): masked header blocks naming the flash image (20, or
//! more in SMK-37 packages: `container`);
//! in it a JLFS directory of masked 32-byte heads, and the app area (from 0x4000), scrambled
//! per 32 bytes with the chip key. `unpack` returns the flash as the updater writes it (what
//! the SPI side reads) and the app area unscrambled (what the CPU sees through XIP, where the
//! SFC decrypts on the fly), with the area's directory.
//!
//! The stock firmware keeps its patches in the USR area (flash 0xEA000..0xFC000 on the FM-1's
//! V13, 0xF4000..0xFE000 on the SMK-37 Pro), which updates leave alone: real units carry the
//! factory bank there. `usr_fill_defaults` puts this emulator's own small bank into a blank one
//! (in the board's layout, `UsrLayout`), `usr_load_syx` a DX7 32-voice bank.

pub(crate) fn unmask(buf: &mut [u8], mut key: u16) {
    for b in buf {
        *b ^= key as u8;
        key = (key << 1) ^ if key & 0x8000 != 0 { 0x1021 } else { 0 };
    }
}

fn crc16(buf: &[u8]) -> u16 {
    let mut crc = 0u16;
    for &b in buf {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// A 32-byte JLFS head: offset +4, size +8, flags +12, last +14, name +16.
#[derive(Clone, Debug)]
pub struct Head {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub flags: u8,
    pub last: bool,
}

fn head(b: &[u8], o: usize) -> Head {
    let name = b[o + 16..o + 32].iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
    Head {
        name,
        offset: u32le(b, o + 4) as usize,
        size: u32le(b, o + 8) as usize,
        flags: b[o + 12],
        last: u16le(b, o + 14) != 0,
    }
}

pub struct Unpacked {
    /// The package's identity, as its header blocks' markers spell it (`FM-1_015`,
    /// `SMK-37 Pro_016`): which product it is for.
    pub identity: String,
    /// The flash image byte for byte as the updater writes it.
    pub raw: Vec<u8>,
    /// The app area unscrambled, from its start: the XIP window's contents.
    pub xip: Vec<u8>,
    /// Where the app area starts in flash (XIP 0x02000000 maps here).
    pub area: usize,
    /// The app area's size (its head's), from `area`.
    pub size: usize,
    /// Where the protected image ends in flash (the area's `PRCT` entry; else the app area's
    /// end): decrypted up to here. Past the app area it holds the `EXT_RESERVED` directory, whose
    /// entries name reserved areas past the image (V16's `USER` bank at 0xED000).
    pub end: usize,
    /// The app area's directory: app.bin (offset in the area), the reserved areas VM, PRCT,
    /// BTIF, USR (flash offsets).
    pub dir: Vec<Head>,
    /// The chip key the app area is scrambled with (the SFC decrypts with it: `Soc::sfc_key`).
    pub key: u16,
    /// The app's resource region as the update loader writes it after the app (`RES_TYPE`):
    /// (flash offset, bytes). `stock_from_package` lays it into the flash, as an install does.
    pub res: Option<(usize, Vec<u8>)>,
}

impl Unpacked {
    pub fn entry(&self, name: &str) -> Option<&Head> {
        self.dir.iter().find(|h| h.name == name)
    }
}

/// The header of a package: blocks of 0x30 bytes whose last byte is a marker, `identity[i] +
/// i + 1` while the identity lasts and 0x7D after it (fm1-to-smk37 docs/02, 2.2). FM-1
/// packages have 20 blocks; SMK-37 Pro v016 has 36 (14 identity characters, 22 x 0x7D): blocks
/// are counted while i < 20 or the marker is 0x7D. Returns the count and the identity.
pub fn container(file: &[u8]) -> (usize, String) {
    let marker = |i: usize| file.get(i * 0x30 + 0x2f).copied();
    let mut n = 0;
    while let Some(m) = marker(n) {
        if n >= 20 && m != 0x7d {
            break;
        }
        n += 1;
    }
    // the identity: the markers up to the run of 0x7D that ends the header
    let mut end = n;
    while end > 0 && marker(end - 1) == Some(0x7d) {
        end -= 1;
    }
    let identity = (0..end).filter_map(|i| marker(i).map(|m| m.wrapping_sub(i as u8 + 1) as char)).collect();
    (n, identity)
}

/// The flash image inside a package, as stored.
pub fn raw_flash(file: &[u8]) -> Result<Vec<u8>, String> {
    raw_flash_n(file, container(file).0)
}

/// The package as its entries' offsets count it (the file without its header's markers), and its
/// entries, each unmasked: type +0, offset +8 and length +12 in that stream, and a word of the
/// entry's own at +0x14 (a resource region's flash address). The header taken as `blocks` blocks.
fn directory(file: &[u8], blocks: usize) -> Result<(Vec<u8>, Vec<Vec<u8>>), String> {
    if file.len() < blocks * 0x30 || blocks < 20 {
        return Err("too short for an FM-1 update package (.fwsc)".into());
    }
    let mut hdr = Vec::with_capacity(blocks * 0x2f);
    for i in 0..blocks {
        hdr.extend_from_slice(&file[i * 0x30..i * 0x30 + 0x2f]);
    }
    unmask(&mut hdr[..0x40], 0xffff);
    if crc16(&hdr[2..0x40]) != u16le(&hdr, 0) {
        return Err("not an FM-1 update package (.fwsc)".into());
    }
    let mut entries = Vec::new();
    for k in 0..u16le(&hdr, 8) as usize {
        let at = 0x40 + k * 0x50;
        if at + 0x50 > hdr.len() {
            break;
        }
        let mut e = hdr[at..at + 0x50].to_vec();
        unmask(&mut e, 0xffff);
        entries.push(e);
    }
    // 36 blocks (0x6C0 bytes) reach into the image at 0x400, so the markers come out of the
    // image's start too
    let mut stream = hdr;
    stream.extend_from_slice(&file[blocks * 0x30..]);
    Ok((stream, entries))
}

/// An entry's bytes in the stream.
fn entry_data<'a>(stream: &'a [u8], e: &[u8]) -> Option<&'a [u8]> {
    let (off, len) = (u32le(e, 8) as usize, u32le(e, 12) as usize);
    stream.get(off..off.checked_add(len)?)
}

/// The flash image, the header taken as `blocks` blocks.
fn raw_flash_n(file: &[u8], blocks: usize) -> Result<Vec<u8>, String> {
    let (stream, entries) = directory(file, blocks)?;
    match entries.iter().find(|e| u16le(e, 0) == 0) {
        Some(e) => entry_data(&stream, e).map(|s| s.to_vec()).ok_or("truncated package".into()),
        None => Err("no flash image in the package".into()),
    }
}

/// A package entry of this type is an app's resource region (`res.bin`: read-only data kept out
/// of the app's image, Melodee's first, fm1-base's after it). The package's own update loader
/// writes it after the app, so a package is the whole install again. Two loaders do, told apart
/// by the region's commit record:
/// - Melodee's (keremimo/melodee firmware/loader/ldr_core.c `ldr_res`): magic `MRS1`, always at
///   its fixed `0xCB000`, up to `0xE0000`;
/// - fm1-base's, after it (felucca-bsp loader/ldr_core.c `ldr_res`): magic `RES1`, where the
///   entry's word at +0x14 says, whole sectors of the data area `0x97000`..`0xE0000`.
pub const RES_TYPE: u16 = 101;
const RES_AT: usize = 0x14;
const RES_LO: usize = 0x97000;
const RES_HI: usize = 0xE0000;
const RES_MELODEE: usize = 0xCB000;
const MAGIC_MELODEE: u32 = 0x3153_524d; // "MRS1"
const MAGIC_BASE: u32 = 0x3153_4552; // "RES1"

/// The CRC-32 the loaders check records and payloads with (reflected, 0xEDB88320, as zlib's).
fn crc32(buf: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in buf {
        c ^= b as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ if c & 1 != 0 { 0xedb8_8320 } else { 0 };
        }
    }
    !c
}

/// The package's resource region as its loader leaves it on a fresh install: (flash offset, the
/// bytes there), or None where the loader writes nothing. As both loaders: the last `RES_TYPE`
/// entry counts; its 32-byte commit record (magic, payload length, the payload's CRC-32, ...,
/// the CRC-32 of the 28 bytes before it) must check and the payload fit; whole sectors only.
/// The record goes last, and only over a payload whose CRC-32 is its own: with a payload that
/// does not check, the region is written and its record left erased (the app does without).
fn resources_n(file: &[u8], blocks: usize) -> Option<(usize, Vec<u8>)> {
    let (stream, entries) = directory(file, blocks).ok()?;
    let e = entries.iter().rev().find(|e| u16le(e, 0) == RES_TYPE)?;
    let data = entry_data(&stream, e)?;
    if data.len() < 32 || data.len() % 0x1000 != 0 || u32le(data, 28) != crc32(&data[..28]) {
        return None;
    }
    let base = match u32le(data, 0) {
        MAGIC_MELODEE => RES_MELODEE,
        MAGIC_BASE => u32le(e, RES_AT) as usize,
        _ => return None,
    };
    if base % 0x1000 != 0 || base < RES_LO || base >= RES_HI || data.len() > RES_HI - base {
        return None;
    }
    let len = u32le(data, 4) as usize;
    if len > data.len() - 32 {
        return None;
    }
    let mut region = data.to_vec();
    if crc32(&data[32..32 + len]) != u32le(data, 8) {
        region[..32].fill(0xff);
    }
    Some((base, region))
}

/// Lay a package's resource region into the flash, as the update loader writes it.
pub fn lay_resources(flash: &mut [u8], res: &Option<(usize, Vec<u8>)>) {
    if let Some((at, data)) = res {
        if let Some(dst) = flash.get_mut(*at..*at + data.len()) {
            dst.copy_from_slice(data);
        }
    }
}

/// The 16-bit key the app area is scrambled with, from isd_config.ini's 32-byte blob.
fn chip_key(blob: &[u8]) -> u16 {
    let mut sum = (blob[..16].iter().map(|&b| b as u32).sum::<u32>() & 0xff) as u8;
    if sum >= 0xe0 {
        sum = 0xaa;
    } else if sum <= 0x10 {
        sum = 0x55;
    }
    let mut key = 0u16;
    for i in 0..16 {
        if (blob[16 + i] ^ blob[15 - i]) < sum {
            key |= 1 << i;
        }
    }
    key
}

fn unscramble(buf: &mut [u8], from: usize, len: usize, base: usize, key: u16) {
    let mut i = 0;
    while i < len {
        let n = 32.min(len - i);
        unmask(&mut buf[from + i..from + i + n], key ^ ((from + i - base) >> 2) as u16);
        i += 32;
    }
}

pub fn unpack(file: &[u8]) -> Result<Unpacked, String> {
    let (blocks, identity) = container(file);
    // (a data byte past the header can read as 0x7D: then the counted header is too long and
    // nothing in the image checks out; 20 blocks is the fallback, as installers do)
    match unpack_n(file, blocks) {
        Err(_) if blocks != 20 => unpack_n(file, 20),
        r => r,
    }
    .map(|u| Unpacked { identity, ..u })
}

fn unpack_n(file: &[u8], blocks: usize) -> Result<Unpacked, String> {
    let raw = raw_flash_n(file, blocks)?;
    let mut flash = raw.clone();
    let (mut key, mut area) = (None, None);
    // (an isd_config.ini whose CRC does not check: SMK-37 stock packages store wrong CRCs for
    // their small resource files, fm1-to-smk37 docs/02 2.2; taken when no checked one is found)
    let mut key_unchecked = None;
    let mut o = 32;
    while o + 32 <= flash.len() {
        unmask(&mut flash[o..o + 32], 0xffff);
        let h = head(&flash, o);
        if h.name == "isd_config.ini" && h.offset + 34 <= flash.len() {
            let k = chip_key(&flash[h.offset..h.offset + 32]);
            if crc16(&flash[h.offset..h.offset + 32]) == u16le(&flash, h.offset + 32) {
                key = Some(k);
            } else {
                key_unchecked = Some(k);
            }
        }
        if h.name == "app_dir_head" && h.flags == 0x81 && area.is_none() {
            area = Some(h.offset);
        }
        if h.last {
            break;
        }
        o += 32;
    }
    let (Some(key), Some(area)) = (key.or(key_unchecked), area) else {
        return Err("no chip key or app area in the flash image".into());
    };
    let first = ((area + 32 + 31) & !31) - area;
    unscramble(&mut flash, area, first, area, key);
    let size = head(&flash, area).size;
    if area + size > flash.len() {
        return Err("app area runs past the flash image".into());
    }
    if size > first {
        unscramble(&mut flash, area + first, size - first, area, key);
    }
    let mut dir = Vec::new();
    let mut o = area + 32;
    while o + 32 <= area + size {
        let h = head(&flash, o);
        let last = h.last;
        dir.push(h);
        if last {
            break;
        }
        o += 32;
    }
    if !dir.iter().any(|h| h.name == "app.bin") {
        return Err("no app.bin in the app area".into());
    }
    // The SFC decrypts the whole protected image, not only the app area (the stock firmware sets
    // its own window to the image's end: 0x93000 on V15). Right after the area comes the
    // `EXT_RESERVED` directory: V16 finds its `USER` bank through it (sdfile walks from the area's
    // head by its size), and without it the synth engine and the UI never start. The area may end
    // mid-block: the block it ends in is decrypted again from its start.
    let end = dir
        .iter()
        .find(|h| h.name == "PRCT" && h.offset == 0)
        .map(|h| h.size)
        .filter(|&e| e > area + size && e <= flash.len())
        .unwrap_or(area + size);
    if end > area + size {
        let blk = area + first + ((area + size).saturating_sub(area + first) & !31);
        let mut t = raw[..end].to_vec();
        unscramble(&mut t, blk, end - blk, area, key);
        flash[area + size..end].copy_from_slice(&t[area + size..end]);
    }
    let res = resources_n(file, blocks);
    Ok(Unpacked { identity: String::new(), raw, xip: flash[area..].to_vec(), area, size, end, dir, key, res })
}

/// The SMK-37's factory calibration record (USRTRIM, flash 0xC1000; fm1-to-smk37 docs/01, 2.1):
/// 16 pad and 37 key velocity curves and a pedal curve, 5 rising points each, and the marker
/// `trimdat\0` at +0x224. A real unit carries its own; with none, the stock firmware writes
/// these defaults over the sector and ports fall back to them in RAM. Fills a blank record.
pub fn smk_trim_fill_defaults(flash: &mut [u8]) {
    const AT: usize = 0xc1000;
    if flash.len() < AT + 0x22c || flash[AT + 0x224..AT + 0x22c] == *b"trimdat\0" {
        return;
    }
    let mut put = |o: usize, v: &[u16]| {
        for (i, x) in v.iter().enumerate() {
            flash[AT + o + 2 * i..AT + o + 2 * i + 2].copy_from_slice(&x.to_le_bytes());
        }
    };
    for p in 0..16 {
        put(0x08 + p * 10, &[50, 550, 720, 850, 950]);
    }
    for k in 0..37 {
        put(0xa8 + k * 10, &[5, 640, 800, 920, 980]);
    }
    put(0x21a, &[20, 850, 920, 980, 1010]);
    flash[AT + 0x224..AT + 0x22c].copy_from_slice(b"trimdat\0");
}

/// Where a stock firmware keeps its patch bank inside the USR area: `SLOTS` voices of 128
/// bytes (DX7 VMEM, a packed voice each) at `voices`, one per-slot settings record of
/// `settings_len` bytes at `settings` (all-zero settings play polyphonically, erased ones
/// (0xFF) left only one voice sounding on the FM-1), and `tail` more bytes the firmware reads
/// after the records, zeroed with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsrLayout {
    pub voices: usize,
    pub settings: usize,
    pub settings_len: usize,
    pub tail: usize,
}

impl UsrLayout {
    /// The FM-1's (dx7patch_load_or_store): voices at +0x4000, 59-byte settings at +0x10000.
    pub const FM1: UsrLayout = UsrLayout { voices: 0x4000, settings: 0x10000, settings_len: 59, tail: 0 };
    /// The SMK-37 Pro's (its patch load at 0x02005660 in V15): 4 banks of 32 voices at +0
    /// (bank * 0x1000 + patch * 0x80, so slot i at +0x80 i), a 163-byte record per patch at
    /// +0x4000 (index bank * 32 + patch; they end at +0x9180), then a 128-byte block it reads
    /// with its 9-byte state at +0x9200, which it writes itself (byte 8 = 8 marks it) and
    /// which the fill leaves alone. The area is 0xA000.
    pub const SMK37: UsrLayout = UsrLayout { voices: 0, settings: 0x4000, settings_len: 163, tail: 128 };

    pub fn of(board: crate::Board) -> UsrLayout {
        match board {
            crate::Board::Fm1 => UsrLayout::FM1,
            crate::Board::Smk37 => UsrLayout::SMK37,
        }
    }

    fn voices(&self, usr: usize) -> std::ops::Range<usize> {
        usr + self.voices..usr + self.voices + SLOTS * 128
    }

    /// The settings records and the tail after them.
    fn settings(&self, usr: usize) -> std::ops::Range<usize> {
        usr + self.settings..usr + self.settings + SLOTS * self.settings_len + self.tail
    }
}

const SLOTS: usize = 128;

/// One DX7 operator, packed (17 bytes): EG rates and levels, keyboard scaling off, detune 7
/// (none), key-velocity sensitivity, output level, frequency (ratio: coarse 0 = 0.5, fine).
fn op(level: u8, coarse: u8, fine: u8, detune: u8, rates: [u8; 4], levels: [u8; 4], vel: u8) -> [u8; 17] {
    let mut o = [0u8; 17];
    o[..4].copy_from_slice(&rates);
    o[4..8].copy_from_slice(&levels);
    o[8] = 39; // break point C3
    o[12] = detune << 3;
    o[13] = vel << 2;
    o[14] = level;
    o[15] = coarse << 1;
    o[16] = fine;
    o
}

const OFF: [u8; 17] = [99, 99, 99, 99, 99, 99, 99, 0, 39, 0, 0, 0, 7 << 3, 0, 0, 2, 0];

/// A packed voice from its operators (OP1 first), algorithm (1-32), feedback and name.
fn voice(ops: [[u8; 17]; 6], algorithm: u8, feedback: u8, name: &str) -> [u8; 128] {
    let mut v = [0u8; 128];
    for (i, o) in ops.iter().enumerate() {
        let at = (5 - i) * 17; // stored OP6 first
        v[at..at + 17].copy_from_slice(o);
    }
    v[102..110].copy_from_slice(&[99, 99, 99, 99, 50, 50, 50, 50]); // pitch EG: flat
    v[110] = algorithm - 1;
    v[111] = (1 << 3) | feedback; // oscillator key sync
    v[112..118].copy_from_slice(&[35, 0, 0, 0, 0, 24]); // LFO: off-ish; transpose C3
    v[116] = 3 << 4;
    let n = name.as_bytes();
    for i in 0..10 {
        v[118 + i] = *n.get(i).unwrap_or(&b' ');
    }
    v
}

/// This emulator's own bank (not Yamaha's): a few classic FM shapes, then init voices.
fn default_voices() -> Vec<[u8; 128]> {
    let pluck = [99, 30, 25, 60];
    let mut bank = vec![
        // two-operator stacks (algorithm 5: 2->1, 4->3, 6->5)
        voice([op(99, 1, 0, 7, [96, 28, 22, 65], [99, 80, 0, 0], 2), op(58, 14, 0, 7, [99, 60, 40, 70], [99, 0, 0, 0], 3),
               op(92, 1, 0, 9, [95, 30, 24, 65], [99, 78, 0, 0], 2), op(76, 1, 0, 5, [94, 40, 30, 70], [99, 60, 0, 0], 2), OFF, OFF],
              5, 0, "EMU E.PNO "),
        voice([op(99, 0, 0, 7, [99, 40, 30, 70], [99, 85, 70, 0], 1), op(82, 0, 0, 7, [99, 45, 35, 70], [99, 60, 40, 0], 2),
               OFF, OFF, OFF, OFF], 5, 5, "EMU BASS  "),
        voice([op(99, 1, 0, 7, [99, 22, 18, 30], [99, 60, 0, 0], 1), op(74, 3, 50, 7, [99, 20, 18, 30], [99, 50, 0, 0], 2),
               op(80, 1, 0, 10, [99, 25, 20, 30], [99, 55, 0, 0], 1), op(60, 4, 0, 7, [99, 25, 20, 30], [99, 40, 0, 0], 2), OFF, OFF],
              5, 0, "EMU BELL  "),
        voice([op(99, 1, 0, 7, [60, 40, 30, 55], [99, 90, 88, 0], 1), op(80, 1, 0, 7, [52, 35, 30, 55], [99, 85, 80, 0], 2),
               op(95, 1, 0, 9, [62, 40, 30, 55], [99, 90, 88, 0], 1), op(72, 1, 0, 5, [55, 35, 30, 55], [99, 85, 80, 0], 2), OFF, OFF],
              5, 6, "EMU BRASS "),
        voice([op(95, 1, 0, 4, [40, 30, 30, 45], [99, 95, 95, 0], 0), op(55, 1, 0, 7, [40, 30, 30, 45], [99, 90, 90, 0], 0),
               op(95, 1, 0, 10, [40, 30, 30, 45], [99, 95, 95, 0], 0), op(55, 1, 0, 7, [40, 30, 30, 45], [99, 90, 90, 0], 0),
               op(90, 2, 0, 7, [40, 30, 30, 45], [99, 95, 95, 0], 0), op(45, 1, 0, 7, [40, 30, 30, 45], [99, 90, 90, 0], 0)],
              5, 0, "EMU PAD   "),
        voice([op(99, 1, 0, 7, pluck, [99, 70, 0, 0], 3), op(78, 3, 0, 7, [99, 45, 35, 70], [99, 40, 0, 0], 3), OFF, OFF, OFF, OFF],
              5, 4, "EMU CLAV  "),
        voice([op(99, 1, 0, 7, [99, 40, 30, 50], [99, 0, 0, 0], 2), op(70, 4, 0, 7, [99, 60, 40, 60], [99, 0, 0, 0], 3),
               op(85, 4, 0, 7, [99, 55, 40, 55], [99, 0, 0, 0], 1), OFF, OFF, OFF], 5, 0, "EMU MARIMB"),
        voice([op(99, 1, 0, 7, [99, 99, 99, 70], [99, 99, 99, 0], 0), op(92, 2, 0, 7, [99, 99, 99, 70], [99, 99, 99, 0], 0),
               op(86, 4, 0, 7, [99, 99, 99, 70], [99, 99, 99, 0], 0), op(80, 8, 0, 7, [99, 99, 99, 70], [99, 99, 99, 0], 0), OFF, OFF],
              32, 0, "EMU ORGAN "),
    ];
    let init = voice([op(99, 1, 0, 7, [99, 99, 99, 99], [99, 99, 99, 0], 0), OFF, OFF, OFF, OFF, OFF], 1, 0, "INIT VOICE");
    while bank.len() < SLOTS {
        bank.push(init);
    }
    bank
}

/// The USR area's flash offset, from the app area's directory.
pub fn usr_offset(u: &Unpacked) -> Option<usize> {
    u.entry("USR").map(|h| h.offset)
}

/// Grow the flash image to the chip's size (what the update does not write reads erased).
pub fn full_flash(raw: &[u8], size: usize) -> Vec<u8> {
    let mut f = raw.to_vec();
    f.resize(size.max(raw.len()), 0xff);
    f
}

/// Put the default bank into the USR area (laid out as `layout`) if no voice there was ever
/// written. Returns whether it did.
pub fn usr_fill_defaults(flash: &mut [u8], usr: usize, layout: UsrLayout) -> bool {
    let voices = layout.voices(usr);
    let settings = layout.settings(usr);
    if settings.end > flash.len() || voices.end > flash.len() || flash[voices.clone()].iter().any(|&b| b != 0xff) {
        return false;
    }
    for (i, v) in default_voices().iter().enumerate() {
        flash[voices.start + i * 128..voices.start + (i + 1) * 128].copy_from_slice(v);
    }
    for b in &mut flash[settings] {
        *b = 0;
    }
    true
}

/// A DX7 32-voice bank (a 4104-byte bulk dump `F0 43 0n 09 20 00 .. F7`, or the bare 4096
/// bytes) into slots `first..first + 32` of the USR area laid out as `layout`. Returns how
/// many voices it wrote.
pub fn usr_load_syx(flash: &mut [u8], usr: usize, layout: UsrLayout, syx: &[u8], first: usize) -> Result<usize, String> {
    let data = if syx.len() >= 4104 && syx[0] == 0xf0 && syx[1] == 0x43 && syx[3] == 0x09 {
        &syx[6..6 + 4096]
    } else if syx.len() == 4096 {
        syx
    } else {
        return Err("not a DX7 32-voice bank (4104-byte sysex or 4096 bytes)".into());
    };
    let n = 32.min(SLOTS.saturating_sub(first));
    let at = usr + layout.voices + first * 128;
    let s = usr + layout.settings + first * layout.settings_len;
    if at + n * 128 > flash.len() || s + n * layout.settings_len > flash.len() {
        return Err("USR area past the flash".into());
    }
    flash[at..at + n * 128].copy_from_slice(&data[..n * 128]);
    for b in &mut flash[s..s + n * layout.settings_len] {
        if *b == 0xff {
            *b = 0;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header of `n` blocks whose markers spell `id` (then 0x7D), followed by data.
    fn header(id: &str, n: usize) -> Vec<u8> {
        let mut f = vec![0u8; n * 0x30 + 0x100];
        for i in 0..n {
            f[i * 0x30 + 0x2f] = id.as_bytes().get(i).map_or(0x7d, |&c| c.wrapping_add(i as u8 + 1));
        }
        // the byte after the header (block n's marker position) must not read as 0x7D
        f[n * 0x30 + 0x2f] = 0x12;
        f
    }

    /// A package of 20 header blocks with these entries (type, the word at +0x14, bytes), laid
    /// out as fm1-base's tools/fm1pkg_make.py does: data from 0x400 on 256-byte boundaries, offsets
    /// counted in the stream without the markers.
    fn package(entries: &[(u16, u32, Vec<u8>)]) -> Vec<u8> {
        let mut stream = vec![0u8; 20 * 0x2f];
        stream[8..10].copy_from_slice(&(entries.len() as u16).to_le_bytes());
        let mut off = 0x400;
        for (k, (typ, at, d)) in entries.iter().enumerate() {
            stream.resize(off, 0);
            let mut e = vec![0u8; 0x50];
            e[0..2].copy_from_slice(&typ.to_le_bytes());
            e[8..12].copy_from_slice(&(off as u32).to_le_bytes());
            e[12..16].copy_from_slice(&(d.len() as u32).to_le_bytes());
            e[0x14..0x18].copy_from_slice(&at.to_le_bytes());
            unmask(&mut e, 0xffff);
            stream[0x40 + k * 0x50..0x90 + k * 0x50].copy_from_slice(&e);
            stream.extend_from_slice(d);
            off = (stream.len() + 0xff) & !0xff;
        }
        let crc = crc16(&stream[2..0x40]);
        stream[0..2].copy_from_slice(&crc.to_le_bytes());
        unmask(&mut stream[..0x40], 0xffff);
        let mut file = Vec::new();
        for i in 0..20 {
            file.extend_from_slice(&stream[i * 0x2f..(i + 1) * 0x2f]);
            file.push(b"FM-1_015".get(i).map_or(0x7d, |&c| c.wrapping_add(i as u8 + 1)));
        }
        file.extend_from_slice(&stream[20 * 0x2f..]);
        file
    }

    /// A resource region as a package carries it: the commit record (magic, the payload's length
    /// and CRC-32, then the record's own CRC-32 at +28), the payload, erased to whole sectors.
    fn region(magic: &[u8; 4], payload: &[u8], sectors: usize) -> Vec<u8> {
        let mut r = vec![0xffu8; sectors * 0x1000];
        r[..4].copy_from_slice(magic);
        r[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        r[8..12].copy_from_slice(&crc32(payload).to_le_bytes());
        r[12..28].fill(0);
        let hcrc = crc32(&r[..28]);
        r[28..32].copy_from_slice(&hcrc.to_le_bytes());
        r[32..32 + payload.len()].copy_from_slice(payload);
        r
    }

    /// Where each update loader writes a package's resource region on a fresh install, and when it
    /// writes nothing: fm1-base's (RES1) where its entry's word says, Melodee's (MRS1) at its fixed
    /// 0xCB000 whatever the word; the record's CRC and the payload's length checked; the last entry
    /// only; a payload whose CRC does not check written without its record.
    #[test]
    fn resource_regions_as_the_update_loaders_write_them() {
        let flash = vec![0x11u8; 0x1000];
        let styles = region(b"RES1", &[7u8; 5000], 2);
        let polly = package(&[(0, 0, flash.clone()), (100, 0, vec![1; 0x80]), (RES_TYPE, 0xab000, styles.clone())]);
        assert_eq!(raw_flash_n(&polly, 20).unwrap(), flash, "the flash image is still found");
        assert_eq!(resources_n(&polly, 20), Some((0xab000, styles.clone())));
        let mut chip = vec![0xffu8; 1 << 20];
        lay_resources(&mut chip, &resources_n(&polly, 20));
        assert_eq!(&chip[0xab000..0xad000], &styles[..], "laid into the flash where the loader writes it");

        let sounds = region(b"MRS1", &[3u8; 9000], 3);
        for word in [0, 0xab000] {
            let melodee = package(&[(0, 0, flash.clone()), (RES_TYPE, word, sounds.clone())]);
            assert_eq!(resources_n(&melodee, 20), Some((0xcb000, sounds.clone())), "Melodee's, word {word:#x}");
        }

        let two = package(&[(0, 0, flash.clone()), (RES_TYPE, 0xab000, styles.clone()), (RES_TYPE, 0xb0000, region(b"RES1", &[9; 10], 1))]);
        assert_eq!(resources_n(&two, 20).map(|r| r.0), Some(0xb0000), "the last entry, as the loaders take it");

        let mut torn = region(b"RES1", &[7u8; 5000], 2);
        torn[100] ^= 1;
        let (_, laid) = resources_n(&package(&[(0, 0, flash.clone()), (RES_TYPE, 0xab000, torn.clone())]), 20).unwrap();
        assert!(laid[..32].iter().all(|&b| b == 0xff), "a payload that does not check: no record");
        assert_eq!(&laid[32..], &torn[32..]);

        let mut bad_record = styles.clone();
        bad_record[12] = 1;
        let mut too_long = styles.clone();
        too_long[4..8].copy_from_slice(&0x2000u32.to_le_bytes());
        let ok = |r: Vec<u8>, at: u32| resources_n(&package(&[(0, 0, flash.clone()), (RES_TYPE, at, r)]), 20);
        assert_eq!(ok(bad_record, 0xab000), None, "a record whose CRC does not check");
        assert_eq!(ok(too_long, 0xab000), None, "a payload longer than the region");
        assert_eq!(ok(region(b"XXX1", &[1], 1), 0xab000), None, "neither loader's magic");
        assert_eq!(ok(styles.clone(), 0), None, "fm1-base's with no address: its loader writes nothing");
        assert_eq!(ok(styles[..0x1800].to_vec(), 0xab000), None, "not whole sectors");
        assert_eq!(ok(styles.clone(), 0xab800), None, "not on a sector");
        assert_eq!(ok(styles.clone(), 0x4000), None, "the app area");
        assert_eq!(ok(styles.clone(), 0xdf000), None, "past the data area");
        assert_eq!(resources_n(&package(&[(0, 0, flash)]), 20), None, "no entry, no region");
    }

    #[test]
    fn container_counts_blocks_and_reads_the_identity() {
        assert_eq!(container(&header("FM-1_015", 20)), (20, "FM-1_015".to_string()));
        assert_eq!(container(&header("SMK-37 Pro_016", 36)), (36, "SMK-37 Pro_016".to_string()));
    }

    /// The default bank lands where each board's stock firmware reads it: the FM-1's voices at
    /// +0x4000 with 59-byte settings at +0x10000; the SMK-37's voices at +0 (bank 1's first at
    /// +0x1000), 163-byte records at +0x4000 up to +0x9180, the 128 bytes after them zeroed, its
    /// 9-byte state at +0x9200 and the rest of the area left erased.
    #[test]
    fn default_bank_in_each_boards_usr_layout() {
        let mut fm1 = vec![0xffu8; 1 << 20];
        let usr = 0xea000;
        assert!(usr_fill_defaults(&mut fm1, usr, UsrLayout::FM1));
        assert_eq!(&fm1[usr + 0x4000 + 118..usr + 0x4000 + 128], b"EMU E.PNO ");
        assert!(fm1[usr + 0x10000..usr + 0x10000 + 128 * 59].iter().all(|&b| b == 0));
        assert!(!usr_fill_defaults(&mut fm1, usr, UsrLayout::FM1), "a written bank stays");

        let mut smk = vec![0xffu8; 1 << 20];
        let usr = 0xf4000;
        assert!(usr_fill_defaults(&mut smk, usr, UsrLayout::SMK37));
        assert_eq!(&smk[usr + 118..usr + 128], b"EMU E.PNO ");
        assert_eq!(&smk[usr + 128 + 118..usr + 256], b"EMU BASS  ");
        assert_eq!(&smk[usr + 0x1000 + 118..usr + 0x1000 + 128], b"INIT VOICE", "bank 1, patch 0");
        assert!(smk[usr + 0x4000..usr + 0x9200].iter().all(|&b| b == 0), "163-byte records and the block after");
        assert!(smk[usr + 0x9200..usr + 0xa000].iter().all(|&b| b == 0xff), "the state and the rest: untouched");
        assert!(smk[..usr].iter().all(|&b| b == 0xff) && smk[usr + 0xa000..].iter().all(|&b| b == 0xff));

        let bank = vec![0x23u8; 4096];
        assert_eq!(usr_load_syx(&mut smk, usr, UsrLayout::SMK37, &bank, 0), Ok(32));
        assert!(smk[usr..usr + 4096].iter().all(|&b| b == 0x23));
        assert_eq!(&smk[usr + 4096 + 118..usr + 4096 + 128], b"INIT VOICE", "slot 32 kept");
    }
}
