//! A tiny x86-64 encoder: only the instructions the block compiler (jit/x64.rs) emits. Every
//! encoding is checked against LLVM's assembler in `tests::encodings_match_llvm_mc`.
//! Branches to labels are always rel32; 32-bit operations unless the name says 64.
#![allow(dead_code)]

pub type Reg = u8;
pub const RAX: Reg = 0;
pub const RCX: Reg = 1;
pub const RDX: Reg = 2;
pub const RBX: Reg = 3;
pub const RSP: Reg = 4;
pub const RBP: Reg = 5;
pub const RSI: Reg = 6;
pub const RDI: Reg = 7;
pub const R8: Reg = 8;
pub const R9: Reg = 9;
pub const R10: Reg = 10;
pub const R11: Reg = 11;
pub const R12: Reg = 12;
pub const R13: Reg = 13;
pub const R14: Reg = 14;
pub const R15: Reg = 15;

/// Condition codes (the low nibble of `jcc` / `cmovcc`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cc { O = 0, No = 1, B = 2, Ae = 3, E = 4, Ne = 5, Be = 6, A = 7, S = 8, Ns = 9, P = 10, Np = 11, L = 12, Ge = 13, Le = 14, G = 15 }

/// `[base + index * scale + disp]`
#[derive(Clone, Copy)]
pub struct Mem { pub base: Reg, pub index: Option<(Reg, u8)>, pub disp: i32 }
pub fn m(base: Reg, disp: i32) -> Mem { Mem { base, index: None, disp } }
pub fn mx(base: Reg, index: Reg) -> Mem { Mem { base, index: Some((index, 1)), disp: 0 } }

/// The two-operand ALU group: `op r/m, r` is `0x01 + 8 * n`, `op r/m, imm` is `81 /n`.
#[derive(Clone, Copy)]
pub enum Alu { Add = 0, Or = 1, And = 4, Sub = 5, Xor = 6, Cmp = 7 }
/// Shifts: `D3 /n` by cl, `C1 /n ib` by an immediate.
#[derive(Clone, Copy)]
pub enum Sh { Shl = 4, Shr = 5, Sar = 7 }
/// Scalar single SSE ops (`F3 0F op`).
#[derive(Clone, Copy)]
pub enum Fop { Add = 0x58, Mul = 0x59, Sub = 0x5c, Div = 0x5e }

#[derive(Clone, Copy)]
pub struct Label(pub usize);

pub struct Asm {
    pub code: Vec<u8>,
    labels: Vec<Option<usize>>,
    /// (offset of a rel32 field, label)
    fixups: Vec<(usize, usize)>,
}

#[allow(clippy::new_without_default, reason = "assembler construction reserves its expected code capacity")]
impl Asm {
    pub fn new() -> Self { Asm { code: Vec::with_capacity(1024), labels: Vec::new(), fixups: Vec::new() } }
    #[allow(clippy::len_without_is_empty, reason = "the length is exposed as a code offset, not as a collection API")]
    pub fn len(&self) -> usize { self.code.len() }
    pub fn here(&self) -> usize { self.code.len() }

    pub fn label(&mut self) -> Label { self.labels.push(None); Label(self.labels.len() - 1) }
    pub fn bind(&mut self, l: Label) { self.labels[l.0] = Some(self.code.len()); }

    /// Resolve every branch to its label; every label must be bound.
    pub fn finish(mut self) -> Vec<u8> {
        for (at, l) in std::mem::take(&mut self.fixups) {
            let target = self.labels[l].expect("unbound label");
            let rel = (target as i64 - (at as i64 + 4)) as i32;
            self.code[at..at + 4].copy_from_slice(&rel.to_le_bytes());
        }
        self.code
    }

    // ---------------------------------------------------------------- operand encoding
    fn b(&mut self, x: u8) { self.code.push(x); }
    fn d(&mut self, x: u32) { self.code.extend_from_slice(&x.to_le_bytes()); }
    /// A REX prefix when one is needed (`byte`: a byte register operand `reg` of 4..7 needs one
    /// to mean spl..dil rather than ah..bh).
    fn rex(&mut self, w: bool, reg: Reg, index: Reg, base: Reg, byte: bool) {
        let r = 0x40 | (w as u8) << 3 | (reg >> 3) << 2 | (index >> 3) << 1 | (base >> 3);
        if r != 0x40 || (byte && (4..8).contains(&reg)) {
            self.b(r);
        }
    }
    /// `pre` (a mandatory prefix or none), REX, `op`, ModRM with a register operand.
    fn rr(&mut self, pre: Option<u8>, w: bool, op: &[u8], reg: Reg, rm: Reg, byte: bool) {
        if let Some(p) = pre { self.b(p); }
        self.rex(w, reg, 0, rm, byte);
        self.code.extend_from_slice(op);
        self.b(0xc0 | (reg & 7) << 3 | (rm & 7));
    }
    /// `pre`, REX, `op`, ModRM (+ SIB, displacement) with a memory operand.
    fn rm(&mut self, pre: Option<u8>, w: bool, op: &[u8], reg: Reg, mem: Mem, byte: bool) {
        if let Some(p) = pre { self.b(p); }
        let index = mem.index.map_or(0, |(i, _)| i);
        self.rex(w, reg, index, mem.base, byte);
        self.code.extend_from_slice(op);
        let md = if mem.disp == 0 && mem.base & 7 != 5 { 0 } else if (-128..128).contains(&mem.disp) { 1 } else { 2 };
        let sib = mem.index.is_some() || mem.base & 7 == 4;
        self.b(md << 6 | (reg & 7) << 3 | if sib { 4 } else { mem.base & 7 });
        if sib {
            let (i, scale) = mem.index.unwrap_or((RSP, 1));
            let ss = match scale { 1 => 0, 2 => 1, 4 => 2, _ => 3 };
            self.b(ss << 6 | (i & 7) << 3 | (mem.base & 7));
        }
        match md {
            1 => self.b(mem.disp as i8 as u8),
            2 => self.d(mem.disp as u32),
            _ => {}
        }
    }

    // ---------------------------------------------------------------- moves
    /// r32 = imm (zero-extended into the 64-bit register)
    pub fn mov_ri(&mut self, r: Reg, imm: u32) {
        if imm == 0 { return self.alu_rr(Alu::Xor, r, r); }
        self.rex(false, 0, 0, r, false);
        self.b(0xb8 + (r & 7));
        self.d(imm);
    }
    pub fn mov_ri64(&mut self, r: Reg, imm: u64) {
        if imm >> 32 == 0 { return self.mov_ri(r, imm as u32); }
        self.rex(true, 0, 0, r, false);
        self.b(0xb8 + (r & 7));
        self.code.extend_from_slice(&imm.to_le_bytes());
    }
    pub fn mov_rr(&mut self, d: Reg, s: Reg) { self.rr(None, false, &[0x89], s, d, false); }
    pub fn mov_rr64(&mut self, d: Reg, s: Reg) { self.rr(None, true, &[0x89], s, d, false); }
    pub fn ld(&mut self, d: Reg, mem: Mem) { self.rm(None, false, &[0x8b], d, mem, false); }
    pub fn st(&mut self, mem: Mem, s: Reg) { self.rm(None, false, &[0x89], s, mem, false); }
    pub fn ld64(&mut self, d: Reg, mem: Mem) { self.rm(None, true, &[0x8b], d, mem, false); }
    pub fn st64(&mut self, mem: Mem, s: Reg) { self.rm(None, true, &[0x89], s, mem, false); }
    pub fn st_imm(&mut self, mem: Mem, imm: u32) { self.rm(None, false, &[0xc7], 0, mem, false); self.d(imm); }
    pub fn st16(&mut self, mem: Mem, s: Reg) { self.rm(Some(0x66), false, &[0x89], s, mem, false); }
    pub fn st8(&mut self, mem: Mem, s: Reg) { self.rm(None, false, &[0x88], s, mem, true); }
    pub fn ldzx8(&mut self, d: Reg, mem: Mem) { self.rm(None, false, &[0x0f, 0xb6], d, mem, false); }
    pub fn ldsx8(&mut self, d: Reg, mem: Mem) { self.rm(None, false, &[0x0f, 0xbe], d, mem, false); }
    pub fn ldzx16(&mut self, d: Reg, mem: Mem) { self.rm(None, false, &[0x0f, 0xb7], d, mem, false); }
    pub fn ldsx16(&mut self, d: Reg, mem: Mem) { self.rm(None, false, &[0x0f, 0xbf], d, mem, false); }
    /// d = zero / sign extension of the low byte / halfword of s (s one of eax..ebx)
    pub fn movzx8(&mut self, d: Reg, s: Reg) { assert!(s < 4); self.rr(None, false, &[0x0f, 0xb6], d, s, false); }
    pub fn movsx8(&mut self, d: Reg, s: Reg) { assert!(s < 4); self.rr(None, false, &[0x0f, 0xbe], d, s, false); }
    pub fn movzx16(&mut self, d: Reg, s: Reg) { self.rr(None, false, &[0x0f, 0xb7], d, s, false); }
    pub fn movsx16(&mut self, d: Reg, s: Reg) { self.rr(None, false, &[0x0f, 0xbf], d, s, false); }
    /// d = low 32 bits of base + disp (a 64-bit base)
    pub fn lea(&mut self, d: Reg, mem: Mem) { self.rm(None, false, &[0x8d], d, mem, false); }

    // ---------------------------------------------------------------- arithmetic
    pub fn alu_rr(&mut self, op: Alu, d: Reg, s: Reg) { self.rr(None, false, &[0x01 + 8 * op as u8], s, d, false); }
    pub fn alu_ri(&mut self, op: Alu, d: Reg, imm: u32) {
        if (imm as i32 as i64) == (imm as i8 as i64) {
            self.rr(None, false, &[0x83], op as u8, d, false);
            self.b(imm as u8);
        } else {
            self.rr(None, false, &[0x81], op as u8, d, false);
            self.d(imm);
        }
    }
    pub fn alu_ri64(&mut self, op: Alu, d: Reg, imm: i8) { self.rr(None, true, &[0x83], op as u8, d, false); self.b(imm as u8); }
    /// cmp r32, [mem]
    pub fn cmp_rm(&mut self, r: Reg, mem: Mem) { self.rm(None, false, &[0x3b], r, mem, false); }
    pub fn cmp_rm64(&mut self, r: Reg, mem: Mem) { self.rm(None, true, &[0x3b], r, mem, false); }
    /// cmp dword [mem], imm8
    pub fn cmp_m_i8(&mut self, mem: Mem, imm: i8) { self.rm(None, false, &[0x83], 7, mem, false); self.b(imm as u8); }
    /// cmp byte [mem], imm8
    pub fn cmp_m8_i8(&mut self, mem: Mem, imm: i8) { self.rm(None, false, &[0x80], 7, mem, false); self.b(imm as u8); }
    pub fn test_rr(&mut self, a: Reg, b: Reg) { self.rr(None, false, &[0x85], b, a, false); }
    pub fn test_rr64(&mut self, a: Reg, b: Reg) { self.rr(None, true, &[0x85], b, a, false); }
    pub fn test_ri(&mut self, r: Reg, imm: u32) { self.rr(None, false, &[0xf7], 0, r, false); self.d(imm); }
    pub fn imul(&mut self, d: Reg, s: Reg) { self.rr(None, false, &[0x0f, 0xaf], d, s, false); }
    pub fn not(&mut self, r: Reg) { self.rr(None, false, &[0xf7], 2, r, false); }
    pub fn sh_cl(&mut self, op: Sh, r: Reg) { self.rr(None, false, &[0xd3], op as u8, r, false); }
    pub fn sh_i(&mut self, op: Sh, r: Reg, n: u8) { self.rr(None, false, &[0xc1], op as u8, r, false); self.b(n); }
    pub fn sh_i64(&mut self, op: Sh, r: Reg, n: u8) { self.rr(None, true, &[0xc1], op as u8, r, false); self.b(n); }
    pub fn cmov(&mut self, cc: Cc, d: Reg, s: Reg) { self.rr(None, false, &[0x0f, 0x40 + cc as u8], d, s, false); }
    /// bt r64, imm8 (CF = the bit)
    pub fn bt64(&mut self, r: Reg, bit: u8) { self.rr(None, true, &[0x0f, 0xba], 4, r, false); self.b(bit); }

    // ---------------------------------------------------------------- SSE (single precision)
    /// xmm = r32 (movd)
    pub fn movd_xr(&mut self, x: Reg, r: Reg) { self.rr(Some(0x66), false, &[0x0f, 0x6e], x, r, false); }
    /// r32 = xmm (movd)
    pub fn movd_rx(&mut self, r: Reg, x: Reg) { self.rr(Some(0x66), false, &[0x0f, 0x7e], x, r, false); }
    pub fn fop(&mut self, op: Fop, d: Reg, s: Reg) { self.rr(Some(0xf3), false, &[0x0f, op as u8], d, s, false); }
    pub fn ucomiss(&mut self, a: Reg, b: Reg) { self.rr(None, false, &[0x0f, 0x2e], a, b, false); }
    /// r = truncate(xmm), 32 or 64-bit result
    pub fn cvttss2si(&mut self, r: Reg, x: Reg, w: bool) { self.rr(Some(0xf3), w, &[0x0f, 0x2c], r, x, false); }
    /// xmm = (float) r, r a signed 32 or 64-bit integer
    pub fn cvtsi2ss(&mut self, x: Reg, r: Reg, w: bool) { self.rr(Some(0xf3), w, &[0x0f, 0x2a], x, r, false); }

    // ---------------------------------------------------------------- control flow
    pub fn jmp(&mut self, l: Label) { self.b(0xe9); self.fixups.push((self.code.len(), l.0)); self.d(0); }
    pub fn jcc(&mut self, cc: Cc, l: Label) { self.b(0x0f); self.b(0x80 + cc as u8); self.fixups.push((self.code.len(), l.0)); self.d(0); }
    pub fn jmp_r(&mut self, r: Reg) { self.rr(None, false, &[0xff], 4, r, false); }
    pub fn call_r(&mut self, r: Reg) { self.rr(None, false, &[0xff], 2, r, false); }
    pub fn push(&mut self, r: Reg) { self.rex(false, 0, 0, r, false); self.b(0x50 + (r & 7)); }
    pub fn pop(&mut self, r: Reg) { self.rex(false, 0, 0, r, false); self.b(0x58 + (r & 7)); }
    pub fn ret(&mut self) { self.b(0xc3); }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each encoding against `llvm-mc -triple=x86_64 -show-encoding` (Intel syntax), run once
    /// and pasted here.
    #[test]
    fn encodings_match_llvm_mc() {
        let cases: Vec<(&str, Box<dyn Fn(&mut Asm)>, &[u8])> = vec![
            ("mov eax, 0x12345678", Box::new(|a| a.mov_ri(RAX, 0x1234_5678)), &[0xb8, 0x78, 0x56, 0x34, 0x12]),
            ("mov r11d, 1", Box::new(|a| a.mov_ri(R11, 1)), &[0x41, 0xbb, 0x01, 0, 0, 0]),
            ("xor ecx, ecx", Box::new(|a| a.mov_ri(RCX, 0)), &[0x31, 0xc9]),
            ("movabs r11, 0x123456789a", Box::new(|a| a.mov_ri64(R11, 0x12_3456_789a)), &[0x49, 0xbb, 0x9a, 0x78, 0x56, 0x34, 0x12, 0, 0, 0]),
            ("mov ecx, eax", Box::new(|a| a.mov_rr(RCX, RAX)), &[0x89, 0xc1]),
            ("mov rbx, rdi", Box::new(|a| a.mov_rr64(RBX, RDI)), &[0x48, 0x89, 0xfb]),
            ("mov r15, r9", Box::new(|a| a.mov_rr64(R15, R9)), &[0x4d, 0x89, 0xcf]),
            ("mov eax, dword ptr [rbx + 0x40]", Box::new(|a| a.ld(RAX, m(RBX, 0x40))), &[0x8b, 0x43, 0x40]),
            ("mov eax, dword ptr [rbx + 0x200]", Box::new(|a| a.ld(RAX, m(RBX, 0x200))), &[0x8b, 0x83, 0, 2, 0, 0]),
            ("mov dword ptr [rbx + 0x44], ecx", Box::new(|a| a.st(m(RBX, 0x44), RCX)), &[0x89, 0x4b, 0x44]),
            ("mov dword ptr [rsp], r8d", Box::new(|a| a.st(m(RSP, 0), R8)), &[0x44, 0x89, 0x04, 0x24]),
            ("mov eax, dword ptr [rsp + 8]", Box::new(|a| a.ld(RAX, m(RSP, 8))), &[0x8b, 0x44, 0x24, 0x08]),
            ("mov r11, qword ptr [r10]", Box::new(|a| a.ld64(R11, m(R10, 0))), &[0x4d, 0x8b, 0x1a]),
            ("mov qword ptr [rbp + 0x20], r10", Box::new(|a| a.st64(m(RBP, 0x20), R10)), &[0x4c, 0x89, 0x55, 0x20]),
            ("mov dword ptr [rbx + 0x50], 0x2000010", Box::new(|a| a.st_imm(m(RBX, 0x50), 0x0200_0010)), &[0xc7, 0x43, 0x50, 0x10, 0, 0, 2]),
            ("mov eax, dword ptr [r12 + rsi]", Box::new(|a| a.ld(RAX, mx(R12, RSI))), &[0x41, 0x8b, 0x04, 0x34]),
            ("mov eax, dword ptr [r13 + rsi]", Box::new(|a| a.ld(RAX, mx(R13, RSI))), &[0x41, 0x8b, 0x44, 0x35, 0x00]),
            ("movzx eax, byte ptr [r12 + rsi]", Box::new(|a| a.ldzx8(RAX, mx(R12, RSI))), &[0x41, 0x0f, 0xb6, 0x04, 0x34]),
            ("movsx eax, byte ptr [r13 + rsi]", Box::new(|a| a.ldsx8(RAX, mx(R13, RSI))), &[0x41, 0x0f, 0xbe, 0x44, 0x35, 0x00]),
            ("movzx eax, word ptr [r12 + rsi]", Box::new(|a| a.ldzx16(RAX, mx(R12, RSI))), &[0x41, 0x0f, 0xb7, 0x04, 0x34]),
            ("movsx eax, word ptr [r12 + rsi]", Box::new(|a| a.ldsx16(RAX, mx(R12, RSI))), &[0x41, 0x0f, 0xbf, 0x04, 0x34]),
            ("mov dword ptr [r12 + rsi], ecx", Box::new(|a| a.st(mx(R12, RSI), RCX)), &[0x41, 0x89, 0x0c, 0x34]),
            ("mov word ptr [r12 + rsi], cx", Box::new(|a| a.st16(mx(R12, RSI), RCX)), &[0x66, 0x41, 0x89, 0x0c, 0x34]),
            ("mov byte ptr [r12 + rsi], cl", Box::new(|a| a.st8(mx(R12, RSI), RCX)), &[0x41, 0x88, 0x0c, 0x34]),
            ("mov byte ptr [r12 + rsi], sil", Box::new(|a| a.st8(mx(R12, RSI), RSI)), &[0x41, 0x88, 0x34, 0x34]),
            ("cmp byte ptr [r15 + rdx], 0", Box::new(|a| a.cmp_m8_i8(mx(R15, RDX), 0)), &[0x41, 0x80, 0x3c, 0x17, 0x00]),
            ("cmp dword ptr [rbp + 0x18], 0", Box::new(|a| a.cmp_m_i8(m(RBP, 0x18), 0)), &[0x83, 0x7d, 0x18, 0x00]),
            ("cmp r11d, dword ptr [rsp]", Box::new(|a| a.cmp_rm(R11, m(RSP, 0))), &[0x44, 0x3b, 0x1c, 0x24]),
            ("movzx eax, al", Box::new(|a| a.movzx8(RAX, RAX)), &[0x0f, 0xb6, 0xc0]),
            ("movsx eax, al", Box::new(|a| a.movsx8(RAX, RAX)), &[0x0f, 0xbe, 0xc0]),
            ("movzx eax, ax", Box::new(|a| a.movzx16(RAX, RAX)), &[0x0f, 0xb7, 0xc0]),
            ("movsx eax, ax", Box::new(|a| a.movsx16(RAX, RAX)), &[0x0f, 0xbf, 0xc0]),
            ("lea eax, [r14 + 3]", Box::new(|a| a.lea(RAX, m(R14, 3))), &[0x41, 0x8d, 0x46, 0x03]),
            ("lea edx, [r14 + 0x100]", Box::new(|a| a.lea(RDX, m(R14, 0x100))), &[0x41, 0x8d, 0x96, 0, 1, 0, 0]),
            ("lea eax, [r13]", Box::new(|a| a.lea(RAX, m(R13, 0))), &[0x41, 0x8d, 0x45, 0x00]),
            ("add eax, ecx", Box::new(|a| a.alu_rr(Alu::Add, RAX, RCX)), &[0x01, 0xc8]),
            ("sub ecx, eax", Box::new(|a| a.alu_rr(Alu::Sub, RCX, RAX)), &[0x29, 0xc1]),
            ("and eax, ecx", Box::new(|a| a.alu_rr(Alu::And, RAX, RCX)), &[0x21, 0xc8]),
            ("or eax, ecx", Box::new(|a| a.alu_rr(Alu::Or, RAX, RCX)), &[0x09, 0xc8]),
            ("xor eax, ecx", Box::new(|a| a.alu_rr(Alu::Xor, RAX, RCX)), &[0x31, 0xc8]),
            ("cmp eax, ecx", Box::new(|a| a.alu_rr(Alu::Cmp, RAX, RCX)), &[0x39, 0xc8]),
            ("add r14d, 5", Box::new(|a| a.alu_ri(Alu::Add, R14, 5)), &[0x41, 0x83, 0xc6, 0x05]),
            ("sub esi, 0x2000000", Box::new(|a| a.alu_ri(Alu::Sub, RSI, 0x0200_0000)), &[0x81, 0xee, 0, 0, 0, 2]),
            ("and esi, 0xfffffffe", Box::new(|a| a.alu_ri(Alu::And, RSI, !1)), &[0x83, 0xe6, 0xfe]),
            ("cmp esi, 0x7fffc", Box::new(|a| a.alu_ri(Alu::Cmp, RSI, 0x7fffc)), &[0x81, 0xfe, 0xfc, 0xff, 0x07, 0x00]),
            ("add eax, 0xffffff80", Box::new(|a| a.alu_ri(Alu::Add, RAX, 0xffff_ff80)), &[0x83, 0xc0, 0x80]),
            ("or ecx, 0x80", Box::new(|a| a.alu_ri(Alu::Or, RCX, 0x80)), &[0x81, 0xc9, 0x80, 0, 0, 0]),
            ("xor ecx, 0x7fffffff", Box::new(|a| a.alu_ri(Alu::Xor, RCX, 0x7fff_ffff)), &[0x81, 0xf1, 0xff, 0xff, 0xff, 0x7f]),
            ("sub rsp, 0x18", Box::new(|a| a.alu_ri64(Alu::Sub, RSP, 0x18)), &[0x48, 0x83, 0xec, 0x18]),
            ("add rsp, 0x18", Box::new(|a| a.alu_ri64(Alu::Add, RSP, 0x18)), &[0x48, 0x83, 0xc4, 0x18]),
            ("test eax, ecx", Box::new(|a| a.test_rr(RAX, RCX)), &[0x85, 0xc8]),
            ("test r11, r11", Box::new(|a| a.test_rr64(R11, R11)), &[0x4d, 0x85, 0xdb]),
            ("test ecx, 1", Box::new(|a| a.test_ri(RCX, 1)), &[0xf7, 0xc1, 1, 0, 0, 0]),
            ("imul eax, ecx", Box::new(|a| a.imul(RAX, RCX)), &[0x0f, 0xaf, 0xc1]),
            ("not ecx", Box::new(|a| a.not(RCX)), &[0xf7, 0xd1]),
            ("shl eax, cl", Box::new(|a| a.sh_cl(Sh::Shl, RAX)), &[0xd3, 0xe0]),
            ("shr eax, cl", Box::new(|a| a.sh_cl(Sh::Shr, RAX)), &[0xd3, 0xe8]),
            ("sar eax, cl", Box::new(|a| a.sh_cl(Sh::Sar, RAX)), &[0xd3, 0xf8]),
            ("shl edx, 7", Box::new(|a| a.sh_i(Sh::Shl, RDX, 7)), &[0xc1, 0xe2, 0x07]),
            ("shr edx, 2", Box::new(|a| a.sh_i(Sh::Shr, RDX, 2)), &[0xc1, 0xea, 0x02]),
            ("sar eax, 16", Box::new(|a| a.sh_i(Sh::Sar, RAX, 16)), &[0xc1, 0xf8, 0x10]),
            ("shr rcx, 32", Box::new(|a| a.sh_i64(Sh::Shr, RCX, 32)), &[0x48, 0xc1, 0xe9, 0x20]),
            ("cmovge eax, ecx", Box::new(|a| a.cmov(Cc::Ge, RAX, RCX)), &[0x0f, 0x4d, 0xc1]),
            ("cmovbe eax, ecx", Box::new(|a| a.cmov(Cc::Be, RAX, RCX)), &[0x0f, 0x46, 0xc1]),
            ("bt rax, 32", Box::new(|a| a.bt64(RAX, 32)), &[0x48, 0x0f, 0xba, 0xe0, 0x20]),
            ("movd xmm0, eax", Box::new(|a| a.movd_xr(0, RAX)), &[0x66, 0x0f, 0x6e, 0xc0]),
            ("movd xmm1, ecx", Box::new(|a| a.movd_xr(1, RCX)), &[0x66, 0x0f, 0x6e, 0xc9]),
            ("movd eax, xmm0", Box::new(|a| a.movd_rx(RAX, 0)), &[0x66, 0x0f, 0x7e, 0xc0]),
            ("movd ecx, xmm0", Box::new(|a| a.movd_rx(RCX, 0)), &[0x66, 0x0f, 0x7e, 0xc1]),
            ("addss xmm0, xmm1", Box::new(|a| a.fop(Fop::Add, 0, 1)), &[0xf3, 0x0f, 0x58, 0xc1]),
            ("subss xmm0, xmm1", Box::new(|a| a.fop(Fop::Sub, 0, 1)), &[0xf3, 0x0f, 0x5c, 0xc1]),
            ("mulss xmm0, xmm1", Box::new(|a| a.fop(Fop::Mul, 0, 1)), &[0xf3, 0x0f, 0x59, 0xc1]),
            ("divss xmm0, xmm1", Box::new(|a| a.fop(Fop::Div, 0, 1)), &[0xf3, 0x0f, 0x5e, 0xc1]),
            ("ucomiss xmm0, xmm1", Box::new(|a| a.ucomiss(0, 1)), &[0x0f, 0x2e, 0xc1]),
            ("ucomiss xmm1, xmm0", Box::new(|a| a.ucomiss(1, 0)), &[0x0f, 0x2e, 0xc8]),
            ("cvttss2si eax, xmm0", Box::new(|a| a.cvttss2si(RAX, 0, false)), &[0xf3, 0x0f, 0x2c, 0xc0]),
            ("cvttss2si rax, xmm0", Box::new(|a| a.cvttss2si(RAX, 0, true)), &[0xf3, 0x48, 0x0f, 0x2c, 0xc0]),
            ("cvtsi2ss xmm0, eax", Box::new(|a| a.cvtsi2ss(0, RAX, false)), &[0xf3, 0x0f, 0x2a, 0xc0]),
            ("cvtsi2ss xmm0, rax", Box::new(|a| a.cvtsi2ss(0, RAX, true)), &[0xf3, 0x48, 0x0f, 0x2a, 0xc0]),
            ("jmp r11", Box::new(|a| a.jmp_r(R11)), &[0x41, 0xff, 0xe3]),
            ("call r11", Box::new(|a| a.call_r(R11)), &[0x41, 0xff, 0xd3]),
            ("cmp rax, qword ptr [rcx]", Box::new(|a| a.cmp_rm64(RAX, m(RCX, 0))), &[0x48, 0x3b, 0x01]),
            ("cmp eax, dword ptr [r10 + rcx]", Box::new(|a| a.cmp_rm(RAX, mx(R10, RCX))), &[0x41, 0x3b, 0x04, 0x0a]),
            ("mov r11, qword ptr [r10 + rcx + 8]", Box::new(|a| a.ld64(R11, Mem { base: R10, index: Some((RCX, 1)), disp: 8 })),
                &[0x4d, 0x8b, 0x5c, 0x0a, 0x08]),
            ("cmp rax, qword ptr [rbp + 40]", Box::new(|a| a.cmp_rm64(RAX, m(RBP, 40))), &[0x48, 0x3b, 0x45, 0x28]),
            ("push rbx", Box::new(|a| a.push(RBX)), &[0x53]),
            ("push r15", Box::new(|a| a.push(R15)), &[0x41, 0x57]),
            ("pop r12", Box::new(|a| a.pop(R12)), &[0x41, 0x5c]),
            ("pop rbp", Box::new(|a| a.pop(RBP)), &[0x5d]),
            ("ret", Box::new(|a| a.ret()), &[0xc3]),
        ];
        for (text, f, want) in cases {
            let mut a = Asm::new();
            f(&mut a);
            assert_eq!(a.finish(), want, "{text}");
        }
        // rel32 branches: forward and back
        let mut a = Asm::new();
        let (back, fwd) = (a.label(), a.label());
        a.bind(back);
        a.jcc(Cc::Ne, fwd);
        a.jmp(back);
        a.bind(fwd);
        assert_eq!(a.finish(), [0x0f, 0x85, 0x05, 0, 0, 0, 0xe9, 0xf5, 0xff, 0xff, 0xff]);
    }
}
