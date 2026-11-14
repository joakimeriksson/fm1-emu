//! pi32v2-isa's table of the pi32v2 instruction set (https://github.com/jvitkauskas/pi32v2-isa), its forms
//! fitted to the vendor objdump and assembler and checked on an FM-1: `pi32v2.rs`, generated from its
//! `isa/pi32v2.toml` by tools/gen_pi32v2.py. fm1-core runs the forms it has semantics for from it
//! (`pi32v2_exec.rs`): the DSP and other forms fm1-isa's corpus classes do not have.
//!
//! Encodings are in the emulator's window: the instruction's bytes little-endian, its first 16-bit word in
//! bits 0..15. A field's bits are listed most significant first.

pub use crate::pi32v2::{id, ENTRIES, FORMS};

/// How a field's raw value reads (pi32v2-isa docs/TABLE-FORMAT.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// a register r0..r15 (`gpr_lo`: r0..r7)
    Gpr,
    GprLo,
    /// r8..r15, 3 bits
    GprHi,
    /// a register pair, its even register: 2 * raw
    Pair,
    /// two registers of a pair: (2 * (raw >> 2) + (raw >> 1 & 1), 2 * (raw >> 2) + (raw & 1))
    PairRegs,
    /// 1 << raw
    Bit,
    /// !(1 << raw)
    InvBit,
    /// a 12-bit modified immediate (`modimm`), inverted with `invert`
    Modimm,
    /// one of `values`, by raw index
    Enum,
    /// raw * scale + add, `zero_as` for 0
    Uimm,
    /// the same, raw sign-extended first
    Simm,
    /// a register mask, bit n = rn
    Rmask,
    /// 0..3: r3 down to r<raw>; 4..15: r<raw> down to r4
    Rlist4,
    /// a special register by number
    Sr,
    /// a special-register mask
    Srmask,
}

pub struct Field {
    pub name: &'static str,
    pub kind: Kind,
    /// window bits, most significant first
    pub bits: &'static [u8],
    pub scale: i32,
    pub add: i32,
    pub zero_as: Option<i32>,
    pub invert: bool,
    pub values: &'static [&'static str],
}

pub struct Form {
    /// pi32v2-isa's id (`add4.ppr`; a dual-issue pair's head ends in `.p`)
    pub id: &'static str,
    /// pi32v2-isa's syntax (`{r} = {s} +|+|+|+ {t}`; a pair's head ends in `  #`)
    pub syntax: &'static str,
    pub group: &'static str,
    /// bytes
    pub len: u8,
    pub mask: u64,
    pub value: u64,
    pub fields: &'static [Field],
}

impl Field {
    /// The field's raw value in an instruction window.
    #[inline]
    pub fn raw(&self, win: u64) -> u32 {
        self.bits.iter().fold(0, |v, &b| v << 1 | ((win >> b) & 1) as u32)
    }

    /// The field's value as pi32v2-isa's models take it (tools/hwtest/check.py `operands`): a register number
    /// (`gpr_hi` + 8, a pair's even register), a constant (bit, invbit, modimm), an immediate (scale, add,
    /// zero_as), an enum's index; the masks and lists raw.
    pub fn value(&self, win: u64) -> i64 {
        let r = self.raw(win);
        let width = self.bits.len() as u32;
        match self.kind {
            Kind::Gpr | Kind::GprLo => r as i64,
            Kind::GprHi => r as i64 + 8,
            Kind::Pair => 2 * r as i64,
            Kind::Bit => (1u32 << (r & 31)) as i64,
            Kind::InvBit => (!(1u32 << (r & 31))) as i64,
            Kind::Modimm => (modimm(r) ^ if self.invert { u32::MAX } else { 0 }) as i64,
            Kind::Uimm | Kind::Simm => {
                let v = if self.kind == Kind::Simm && width > 0 && r >> (width - 1) & 1 != 0 {
                    r as i64 - (1i64 << width)
                } else {
                    r as i64
                };
                match self.zero_as {
                    Some(z) if r == 0 => z as i64,
                    _ => v * self.scale as i64 + self.add as i64,
                }
            }
            Kind::Enum | Kind::PairRegs | Kind::Rmask | Kind::Rlist4 | Kind::Sr | Kind::Srmask => r as i64,
        }
    }

    /// An enum field's text.
    pub fn text(&self, win: u64) -> &'static str {
        self.values.get(self.raw(win) as usize).copied().unwrap_or("")
    }
}

impl Form {
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// A dual-issue pair's head (`#`).
    pub fn pair_head(&self) -> bool {
        self.syntax.trim_end().ends_with('#')
    }

    /// The id without a pair head's `.p`: the form whose semantics it has.
    pub fn base_id(&self) -> &'static str {
        self.id.strip_suffix(".p").unwrap_or(self.id)
    }
}

/// A 12-bit modified immediate: 8 bits repeated in a pattern, or rotated (as in ARM Thumb-2).
pub fn modimm(raw: u32) -> u32 {
    if raw >> 10 == 0 {
        let xy = raw & 0xff;
        [xy, xy * 0x0001_0001, xy * 0x0100_0100, xy * 0x0101_0101][(raw >> 8 & 3) as usize]
    } else {
        let (rot, byte) = (raw >> 7, 0x80 | (raw & 0x7f));
        byte.rotate_right(rot)
    }
}

/// The form the table decodes a window as: the first whose encoding matches, as pi32v2-isa decodes.
pub fn decode(win: u64) -> Option<usize> {
    FORMS.iter().position(|f| win & f.mask == f.value)
}

/// The form an entry of `ENTRIES` stands for (by its place in the array).
pub fn index_of(e: &crate::IsaEntry) -> Option<usize> {
    let (p, base) = (e as *const crate::IsaEntry as usize, ENTRIES.as_ptr() as usize);
    let k = p.wrapping_sub(base) / std::mem::size_of::<crate::IsaEntry>();
    (p >= base && k < ENTRIES.len()).then_some(k)
}

/// The form with this id.
pub fn by_id(id: &str) -> Option<usize> {
    FORMS.iter().position(|f| f.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_as_the_table_does() {
        // `20 e4 04 e0` r15_r14 = r1_r0 +|+|+|+ r0 (the table's add4.ppr example)
        let k = decode(0xe004_e420).unwrap();
        let f = &FORMS[k];
        assert_eq!(f.id, "add4.ppr");
        assert_eq!(k, id::ADD4_PPR);
        assert_eq!((f.field("r").unwrap().value(0xe004_e420), f.field("s").unwrap().value(0xe004_e420)), (14, 0));
        assert_eq!(modimm(0x1ff), 0x00ff_00ff);
    }
}
