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

/// The flash image, the header taken as `blocks` blocks.
fn raw_flash_n(file: &[u8], blocks: usize) -> Result<Vec<u8>, String> {
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
    for k in 0..u16le(&hdr, 8) as usize {
        let at = 0x40 + k * 0x50;
        if at + 0x50 > hdr.len() {
            break;
        }
        let mut e = hdr[at..at + 0x50].to_vec();
        unmask(&mut e, 0xffff);
        if u16le(&e, 0) == 0 {
            // offsets count in the file without its markers; 36 blocks (0x6C0 bytes) reach into
            // the image at 0x400, so the markers come out of the image's start too
            let off = u32le(&e, 8) as usize;
            let len = u32le(&e, 12) as usize;
            let mut stream = hdr.clone();
            stream.extend_from_slice(&file[blocks * 0x30..]);
            return stream.get(off..off + len).map(|s| s.to_vec()).ok_or("truncated package".into());
        }
    }
    Err("no flash image in the package".into())
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
    Ok(Unpacked { identity: String::new(), raw, xip: flash[area..].to_vec(), area, size, end, dir, key })
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
