# pi32v2 ISA notes

Working notes from Phase 1 (mask-level ISA extraction). Machine-readable
table: `isa/fm1.yaml` (auto-generated — do not hand-edit; edit
`tools/build_isa.py` / probe sources instead).

## Bit numbering convention

The instruction's bytes b0..bn form a little-endian integer V
(b0 = bits 0..7, b1 = bits 8..15, ...). `mask`/`match`/field lsb/msb all
refer to V. A 2-byte instruction `75 04` therefore has V = 0x0475 — the
hex literal in YAML `samples` is the byte string, and its LE value differs
from `int(s,16)`; always convert via `int.from_bytes(bytes.fromhex(s),
"little")` (this exact bug has bitten once already).

## What the table is

- 1067 classes, built from ~421k instructions across 5 vendor-objdump
  corpora (V13 app, V14 app, uboot, uboot-debug, usb-hid-ota).
- Each class: `mask`/`match` (constant bits across all observed samples),
  `syntax` (normalized form), optional `alt` (same-mask spelling families),
  `samples` (hex byte strings, capped at 64; decode tiebreak).
- Validation (`tools/check_isa.py`, streaming decode, sample priority):
  V13 99.98%, V14 99.98%, uboot 100%, ota 99.99% — 0 mismatches,
  0 undecodable on all. Mask-level coverage of real firmware is complete.

## Form normalization (tools/normlib.py)

- Vendor annotation `<_fw+0x... : ... >` stripped anchored at END of line.
  Un-anchored, it eats `ifs (r6 < 3) goto ...` through the `<` operator —
  this produced phantom forms `ifs (R` / `if (R` (3% of the corpus!).
- Hex immediates → `#h`, decimals → `#i`, r0..r15 → `R`.
- Register lists canonicalized: `{rets, r6-r4}` and `{rets, r5, r4}` are
  two objdump spellings over a variable field, so abstract runs become
  `R*k` (`R-R` counts as 2). Named regs (`rets`, `psr`, `icfg`, ...) kept.

## Register-block encoding (open puzzle, needs field-level work)

The pop/push list families encode a block of registers in the low nibble
of byte0 with an r3/r4 symmetry around the block bottom:

| bytes   | objdump text            |
|---------|-------------------------|
| `70 04` | `{rets, r3-r0}`         |
| `71 04` | `{rets, r3-r1}`         |
| `73 04` | `{rets, r3}`            |
| `74 04` | `{rets, r4}`            |
| `75 04` | `{rets, r5, r4}`        |
| `76 04` | `{rets, r6-r4}`         |
| `7a 04` | `{rets, r10-r4}`        |

Hypothesis: field = block size `|n-3.5|+0.5` (n = low nibble), block spans
down to r4 for n>=4, up to r3 for n<=3. *Not yet hand-verified* — needs a
targeted probe (Phase 1.3), and the YAML keeps `R*k` variants merged as one
mask family with `alt` spellings in the meantime.

## Decode semantics of the check tool

Priority = (longest, then mask popcount, then corpus count). Among mask
matches, an entry whose `samples` contain the observed byte string wins.
This is what separates sibling forms the masks alone cannot (e.g.
`[--sp] = {rets, r3}` vs `[--sp] = {rets, r3-r0}` differ only in bits the
V13-only masks left variable). For non-corpus bytes the first candidate
wins — pure-emulator decoding should never rely on that tiebreak; fields
must be refined first.

## Status of entries

Everything is `status: auto` — masks are corpus-derived, semantics (flag
updates, cycle counts, immediates sign-extension) unverified until
probes/hardware (Phase 1.3, device ordered 2026-10-08).

## Known undecoded space

None in the 5 corpora. Expected unknowns beyond them: fpu/coprocessor,
audio-DSP specifics, anything the stock/uboot/ota never emits.

## Operand derivation (Phase 1.2.5, 2026-10-08)

`tools/derive.py` fits per-slot operand encodings (template + `operands`
list per class, output `work/derived.yaml`); `tools/disasm.py` renders
vendor-comparable text and diffs against the corpora.

Current state: ~1067 classes; 437 fully solved + 115 partial (slots);
~55% of all instructions render byte-exact vendor text (incl. branch
offsets and hex constants). 0 undecodable; mask layer unaffected.

### Encoding discoveries (from fitting, all corpus-verified)

- Register operands are not always one span: e.g. `R = #i` packs
  `reg = bits(0,2) | bit7<<3` (byte0 0xc0-family carries reg+8):
  layered concat fitting found it.
- **`R = #i` hides ≥3 opcode families under one normalized form**:
  0x4N-family (imm from bits(0,5)*(0x38-rotations) unclear), 0xCN-family
  prints `field - 0x14`, etc. The fitter's single-formula model cannot
  express per-family bias — flagged for Phase 1.3 hardware probes.
- Immediate formulas found so far: identity, sign-extended, pc-relative
  (×1/×2, ±), bitwise-NOT over 32/16 bits, ×4/×8 shifts. Off-by-one
  variants (+1/-1) exist (`[R++=#i]` seems N-1 encoded) — pending.
- Block lists: `[max(n,hi) .. min(n,lo)]` with class constants (e.g.
  {rets, rN…r4} → hi=3, lo=4) — verified 7129/7129 for the byte1=0x04
  push/pop family.

### Known renderer/fit gaps (next steps)

1. Sibling-mask classes decode wrong without exhaustive samples: YAML
   cap is 64; sibling classes (same len+mask+match across families) need
   their full distinct-encoding set stored.
2. Operand renderers must be formula-versioned identically to the fitter
   (single shared table violation; disasm.py re-declares the table).
3. +1/-1 immediate formula variants; restrict NOT-formulas to classes
   where the printed target is a bitmask-typed operand.

## Stock firmware boot in the emulator (2026-10-09, V13 app.bin)

`fm1-emu --bin app.bin` boots from the **reset vector at 0x02000000** (the
default entry). The CRT there sets `sp = 0x01C14BB4` / `ssp = 0x01C15BB4`,
calls `boot_hwinfo_save` (r0 = SPL boot-param pointer; the runner hands it
an all-zero block at 0x01C7FC00), zero-fills `.bss` (0x17380 bytes at
0x01C09E7C) and copies `.data` (0x9E7C bytes, flash 0x02084820 → RAM
0x01C00000), both with `rep` blocks — now executed with the right counts
(23776 / 10143 iterations).

**Correction of an earlier note.** The "calls into a JieLi mask ROM at
0x1FC05C4C" diagnosis was wrong on two counts: the earlier run entered at
0x02000120 (inside `pll_clock_init`, skipping the CRT) so sp was 0 and RAM
was empty; and the long-call targets vendor objdump prints as
`0xFFC0xxxx` (`_fw` = 0) are `0x01C0xxxx` at the real load address —
**RAM-resident .data code** (flash LMA = 0x02084820 + (target − 0x01C00000);
e.g. 0x01C05C4C is a delay loop at flash 0x0208A46C, 0x01C00D68 is the
RAM early-init at 0x02085588). FM-1-RE's docs call these "mask-ROM
services" in one table and "RAM-resident code" in another; the latter is
right. No mask-ROM dump is needed for boot, and the ROM-stub / zero-insn
stub hacks were removed.

### Encoding conventions verified against the corpus

- `R = #i` (2-byte): reg = bits(0,2)|bit7<<3; imm = (bits 8,12) |
  (bits 3,5)<<5 — 9-bit across the byte boundary (0xC?14 family is the
  separate `rN = 0` alias; 0x4N/0x6N/0x68.. behave differently per family).
- `R = #h` (4-byte): reg = (b3>>4)&0xf; value = f(b3&0xf, b2) = a
  composed-constant builder (IEEE float constants etc). Table of 70
  observed (b3lo, b2)->value pairs in fm1-core MOV_H_TABLE.
- block push/pop pairs: prologue `[--sp] = {rets, rN..rB}` ↔ epilogue
  `{pc, rN..rB} = [sp++]` (identical reg sets, verified on 8 pairs).
  Push: --sp then store, list in printed order (first item highest).
  Pop: read ascending, assign reversed printed list, pc last.
- `r_i_or_h`-family ([R+off] |=/&= const): base = bits(28,31);
  off = bits(0,4)*4; const = bits(16,31) (AND inverts). `r_i_or_r`:
  base = b3>>4, src = b3&0xf, off = b1 (8-bit).
- long call (6-byte `80 ff <imm32-le>`): offset sext32 from pc_next.
- 2-byte shifts `R = R <<|>>|>>> #i` (mask e088): dst = bits(0,2),
  src = bits(4,6), shift = bits(8,12); bit7 = right, bit3 = arithmetic
  (`a2 a2` r2 = r2 >> 2, `9a a2` r2 = r1 >>> 2). Bit 7 is NOT part of the
  register number — the first implementation thought so and silently
  shifted r10 instead of r2, so the CRT loop counts were 4× too big.
- `R = R + R` (2-byte `1c`/`1d`): dst = bits(0,2), a = bits(4,6),
  b = bit3 | bits(7,8)<<1 (`93 1c` r3 = r1 + r2, `29 1d` r1 = r2 + r5).
- `rep N rR {` (`03 xx`): reg = bits(0,3), N = block length in bytes =
  2*(bits(4,7)+1). `rep N #k {` (`8x xx`): same N, k = bits(8,12)+1.
  The block is the N bytes after the rep; it runs k / rR times. Count 0
  skips the block (the CRT runs `r2 = 0; rep 2 r2 {…}` unguarded for the
  empty overlay). **Measured on an FM-1 ("Hardware facts" below):
  a count of 0 does skip the body, and one `rep` runs at most 32 times**: a
  count c of 32 or more runs (c & 31) + 1 times and leaves c minus that in
  rR, and the compiler's `if (rR != 0) goto rep` after every register-count
  rep makes up the rest. The emulator latches the count at the rep (rR holds
  what will be left while the body runs: the stock memmove tail reuses it).

### 4-byte families fitted 2026-10-09 (0 mismatches over all V13 samples)

Method: pull every corpus line of one printed shape, propose bit fields,
verify against *all* samples (scratch fitters; the hypotheses are now the
comments in `crates/fm1-core/src/lib.rs`). Verified:

- **Compare-immediate branches** `if[s] (rN <op> #imm) goto #off`
  (5302 samples): reg = bits(0,3); byte1 = family, bit7 = second op:
  f8 ==/!=, f9 >=/< unsigned, fc >/<= unsigned, fd >=/< signed,
  fe >/<= signed. imm = 10 bits = bits(25,31) | bits(4,6)<<7, printed
  sign-extended for ==/!= and the signed families (`r14 != -1`), zero-
  extended for the unsigned ones. off = 2*sext9(bits(16,23) | bit24<<8)
  from pc+4.
- **Composed 12-bit immediate** (code = bits(24,27), m = bits(16,23)),
  shared by `R = #h`, `R = R op #h`, `[R+#i] op= #h`: code 0 literal m;
  1 m per halfword; 2 (m<<8) per halfword; 3 m per byte; 4..15 float-like
  with exponent E = code<<1 | m>>7 and mantissa 0x80|(m&0x7f), value =
  mantissa << (32-E). Reproduces the whole 70-entry observed `R = #h`
  table plus 1519 ALU-immediate and 129 mov-immediate samples. The
  `&~` forms print the inverted value.
- **4-byte ALU** (byte1 e0/e1/e8/eb/ef; 6878 samples): three-register
  forms `b4 e0` (+,-), `90 e1` (|,^,&), `f0 e1` (*) with d = bits(28,31),
  a = bits(20,23), b = bits(24,27), sub-op bits(16,19); register-immediate
  forms with d = bits(0,3), s = bits(28,31), op nibble bits(4,7):
  e1 0..3 `R = R + sext14(bits(16,27) | nibble<<12)`, e1 4/5/6/7 |,^,&,&~
  composed, e1 e `*` composed, e0 e/f +/- composed, e0 a `composed - R`;
  memory RMW `[R+off] op= R` (e8: base bits(28,31), src bits(24,27),
  off = 4*sext6(bits(18,23)), op bits(16,17); byte0 64 logic / 68 arith)
  and `[R+off] op= imm` (ef/eb: off = 4*sext6(bits(0,5)), bits(6,7)
  select |,^,&,&~ (ef) or += composed / += sext12 (eb 2/3)).

### Operand solver and the syntax-driven executor (2026-10-09, later)

`tools/solve_slots.py` replaced `solve_hot.py`: per class it fits every
printed operand as an exact linear model over the varying bits (rational
Gaussian elimination; collinear bits resolved by a power-of-two search),
with kinds imm / rel (sign-extended, scaled) / scaled(+adj) / adj /
zero32 / bit / nbit / composed / const / blk and a few-sample single-field
fallback. Result: 764 of 1067 classes fully solved, 86.8% of corpus
instructions (was 206 / 54%). `gen_isa.py` emits the slots into
`generated_isa.rs` (new SlotKinds: Scaled, Const, Zero32, Bit, NBit,
Composed, NComposed); `fm1-emu --coverage` executes one sample of every
class on a scratch SoC and lists the classes still lacking semantics or
slots. The core now executes **99.2% of corpus instructions by weight**:
semantics are keyed by the printed shape (`Cpu::exec_syntax`, operands
from the slots in printed order), with raw-bit decoders only where one
class mixes encodings (4-byte ALU, compare/bit-test branches, predicated
blocks, register pairs, post-increment and indexed memory forms).

Hand-written per-class arms were removed wherever the solver covers the
class: three of them were wrong and corrupted the boot silently
(`[R+#i] = R` took bit 7 as part of the source register and stored r9
instead of r1; the 2-byte `if (R == #i) goto` sign-extended a 5-bit
offset; the register-pair store used base = bits(24,27)).

### Semantics discovered while booting (all corpus-verified)

- **Dual-issue pairs.** An instruction printed with a trailing `#` and the
  indented instruction after it execute as a pair: the second reads the
  register state from *before* the first (`r5 = r3 #; b[r4+6] = r5`
  stores the old r5). Implemented as deferred register commit in
  `Cpu::step`. A pair counts as **one unit** in the block-size nibbles of
  `if (...) {` (memset's `{ r2 -= 1 #; b[r3++=1] = r1; goto }` is 2 units).
- **Predicated blocks** `if[s] (rA <op> x) {` (4-byte, byte1 e8..ee):
  rA = bits(0,3); byte0 high nibble: 1 reg (rB = bits(24,27)), 2 composed
  immediate, 3 sext12; bit 7 flips the op (bit-test family ea: h=2 ==0,
  h=3 !=0, reg form byte2 bit7). then = bits(30,31)+1 units, else =
  bits(28,29) units (2057 blocks); nested blocks and `rep` headers count
  as one unit including their bodies.
- **Reg-reg compare branches**: 4-byte e8/e9/ec/ed/ee (bit7 alt, A =
  bits(28,31), B = bits(0,3), off = 2*sext9(bits(16,23)|bit24<<8)) and
  6-byte `4x ff` (fam bits(1,3), bit0 alt, A bits(28,31), B bits(24,27),
  off 2*sext16(bits(32,47))). The family code byte1&7 is shared by every
  compare form: 0 ==, 1 >= u, 4 > u, 5 >= s, 6 > s.
- **6-byte compare-immediate** `xx ff`: byte0 bits(1,3) family, bit0 alt,
  bit5 = composed (else sext12(bits(16,27))), reg bits(28,31), off
  2*sext16(bits(32,47)). (1323 samples)
- **Bit-test branches** `if ((rN & m) ==/!= 0) goto`: 6-byte `60/61 ff`
  (m composed, off sext16×2), 4-byte `5x e8` (m = 1<<bits(27,31), bit25 =
  !=, off sext9×2), 4-byte `fa/fb` (m = r[bits(4,7)], off sext16×2).
- **Register pairs** `rH_rL = d[...]`: pair = bits(29,31) (regs 2p+1, 2p),
  bit16 = store; `5x ec` base = bits(20,23), off = 4*(bits(18,19) |
  bits(24,28)<<2 | bit0<<6 | bit1<<7); `d0 e9` sp-relative, off =
  bits(17,23)<<1 | bit24<<8. Low register at the lower address.
- **Post-increment memory** (4-byte `dx ec/ed/ee`): data bits(28,31),
  base bits(20,23), f = bits(16,19)|bits(24,27)<<4; word/half: store =
  bit16, inc = sext10(f&~1 | bit0<<8 | bit1<<9), bit2 = signed (half);
  byte: store = bit1, signed = bit2, inc = bit0 ? sext8(f) : f. The
  indexed `d8 ec/ed/ee` forms share byte0 d8 with word post-increment —
  bit 17 set means indexed. 2-byte forms: byte1 05 word (±4, bit3 sign,
  bit7 store), see the core.
- **Block moves with a register base** `{..} = [rN+]` / `[rN+] = {..}`
  (00/20 eb): base bits(0,3), 16-bit register mask at bits(16,31), lowest
  register at the lowest address; `[rN++]` also advances the base.
- Shift counts: a zero field prints as 32. `testset b[R]`: cc = (old != 0)
  and byte |= 0x80 (SDK spins `testset; ifeq goto retry`). **Wrong on an FM-1
  (below): the byte becomes 0xFF and psr bits 3..0 take the old byte's
  bits 3..0**, so a held lock (0xFF) sets Z and `ifeq` retries. `tbb/tbh [R]`:
  pc = next_pc + 2*table[R] with the table right after the instruction
  (tbh entries are halfwords, R pre-scaled). `cnum` = core id.

### Dual core

The AC791N is dual-core and the firmware uses it: cpu0 runs the RTOS,
cpu1 is the audio render core (`0x01C022B6`, flash 0x02086AD6: sets the
ack byte `b[0x01C1FF08] = 1`, then polls a state byte and calls
`dx7note_compute_block`). `cpu1_boot_start` releases it with
`0x10008 |= 8` and spins on the ack. The runner starts a second `Cpu`
(core id 1, usp/sp = 0x01C15EB4/0x01C16EB4) when that bit is set and
interleaves the cores 1:1. **Open:** the vector at 0x02000098 `rti`s into
0x01C023D6, which is 2 bytes into a 6-byte instruction of
cpu_ipc_call_sync in the RAM image, and the mailbox word 0x020001B8 is
mid-function too — so the real cpu1 entry chain (ROM → ?) is unknown;
the runner enters cpu1 directly at 0x01C022B6. Cross-core IPC goes
through the mailbox at 0x01C06EA0/4 and the doorbell IRQ; interrupts are
not modelled yet.

### Where boot stops now

cpu0 runs the CRT, the RAM early init (interrupt controller, SRAM
clears), clock/board init (`0x02001C24`, 100k-iteration delay loop),
`request_irq` table setup, RTOS init (dlmalloc mspace, queues, mutexes),
releases cpu1, creates the boot task and reaches `os_start`, where the
first task is entered through `{psr, rets, reti} = [sp++]; rti`.
The first task's wrapper (0x0205BFCE → 0x0205BF62 →
`__os_sched_process` = xTaskResumeAll) asserts because
`uxSchedulerSuspended` (0x01C09758) is 0 — `os_start` zeroes it just
before the switch, so either the wrapper is entered differently on
hardware (interrupt context, which we do not model yet) or something on
the switch path is still wrong. Next items: the interrupt controller +
timer tick model (0x1EEE000 region, vector 0x020000B0,
`irq_c_dispatch`), then this assert. `--flash decrypted.bin` backs
the XIP window with the whole flash image (app.bin starts at flash
0x4120) so the `.data` tail past app.bin reads real bytes.

### Fixed by differential probes (2026-10-10)

Probes are small C programs built with the vendor clang
(`tools/probe_build.sh`); each check compares a value computed at run time
from `volatile` inputs with the same expression folded by the compiler, so a
mismatch is an emulator semantic bug. Found this way:

- `read16` at word offset 2 returned bytes 1–2 (sheared every 16-bit canvas).
- `R = R.b0 (u) #` parsed its part name at the wrong index.
- 12-bit compare immediates (6-byte `xx ff` branches and `if (R op #i) {`
  blocks): sign-extended for `==`/`!=` and the signed families, but
  **zero-extended for the unsigned families** `>=u` (1) and `>u` (4) —
  corpus: `if (r2 < 2111)` encodes 0x83F.
- 4-byte `R = R << #n` / `>> #n` read the operator at the wrong index and
  all ran as arithmetic right shifts.
- `[R+#i] <<= #n` (`6c e8`): base bits 28–31, word offset bits 18–23,
  shift bits 24–27, op bits 16–17 as in the e1 shifts.
- `fX e0` is `R = R - #h`; only `f0 e1` is the three-register multiply.
- `R = smin/smax/umin/umax(R, R) #` (`f4`) share the `e4` layout: d bits
  28–31, a bits 20–23, b bits 24–27.

Felucca's I2S words hold a 24-bit sample in the **low** bits (Q15 << 7,
`audio.c OUT_SHIFT`), not left-justified as the HAL comment says.

### Found by the decoder audit (2026-10-10, `tools/audit_decode.py`)

- `d0 e9` sp-relative pair load/store: off = bits(17,27) << 1 (bit16 = store); all 810
  corpus samples. The V13-only fit (bits(17,23)<<1 | bit24<<8) broke offsets >= 512.
- 4-byte push/pop with a register mask (`d9 e8` / `d5 e8`): registers = bits(16,31) as a
  mask; push rets first, then the registers from the highest down; pop ascending, then pc.
- 2-byte shift-by-immediate (`xx a0..`): a count field of 0 is a shift by 32.
- 2-byte `{pc, rN..} = [sp++]`: the range is min(n,4)..max(n,3), as the push's
  (`50 04` = {pc, r3-r0}).
- Interrupt nesting follows the Blackfin rule: an ISR can be interrupted only while its
  `reti` is saved on the stack.

### The FPU and the community firmwares (2026-10-10)

Fifteen community firmwares (web/firmwares.json, `tools/fetch_firmwares.mjs`) were added as
corpora: the ISA is mined from 25 listings now (`isa/corpora.txt`, newest versions included; the app listings come from
`tools/disasm_app.sh`, which wraps a raw app in an ELF for JieLi's objdump;
`node tools/fwsc_extract.mjs PACKAGE.fwsc OUT.bin` takes the raw app out of an update package). 2765 classes;
`tools/isa_stable_names.py` keeps the previous class names (the core keys some decoders on
them); `tools/gen_isa.py` lets a class the solver could not finish borrow a twin's slot only
when it reproduces every (raw, value) example the solver kept for it.

- **FPU** (`-mcpu=r3`; objdump prints `<unknown instruction>`): `3f e5 b2 b3` on the ordinary
  registers. Binary `R = R op R (f)`: op = b2 & 15 (0 +, 1 -, 2 *, 3 /, 5 fmin, 6 fmax),
  a = b2 >> 4, d = b3 >> 4, b = b3 & 15. Unary: b2 = K f, K 0..3 ftoi, 4..7 ftou, c..f ftof
  (to a whole float), rounding by K & 3: 0 (even), 1 (trunc), 2 (ceil), 3 (floor); 8f itof,
  9f utof; d = b3 >> 4, s = b3 & 15 (the roundings from the vendor assembler and
  `objdump --mcpu=r3`, which prints the FPU; FM-1 V16 uses ftoi (even)). `3f f5` is the same op as a dual-issue pair
  head (bit 12, as e0/e1 vs f0/f1; `-mfprev1` makes the compiler pair them). The stock app
  also has ops 7 and 8: `R += R * R (f)` and `R -= R * R (f)` (the vendor assembler). Encodings from the vendor assembler
  (`work/probe/fpu_ops.S`, `fcmp_*.S`); `probes/fpu.c` checks 1464 cases against LLVM's folding.
- **Float compares** `iff`: the integer `if (R op R) goto` (4-byte: flag bit 27; 6-byte
  `4x ff`: bit 23) and `if (R op R) {` (bit 23) with a float flag. Families: e8 ordered ==,
  e9 u>=, ec u>, ed >=, ee >; the alt bit gives !=(unordered), <, <=, u<, u<=.
- **Pair accesses `5x ec`** had only `d[R+#]` with off >= 0. All modes now
  (`decode_pair_mem`): off = 4*sext9(bits18-19 | bits24-27<<2 | bit0<<6 | bit1<<7 | bit2<<8);
  50..57 bit17 = pre-increment; 58..5f bit17 clear = post-increment, set = index register
  bits 24-27 (`58` [R+R], `5c` [++R=R]). X0X's `d[++r1=r0] = r5_r4` had overwritten the
  909 BD's settings with another voice's. The audit compares all five modes now.
- **`b4 e1` is the bitfield extract** `R = u/sextra(R, p:#, l:#)` (294 samples); the 3-register
  ALU arm also took it as `R = R + R` (X0X's fm_log2f got an address as exponent). `b4` is
  add/sub only with e0.
- **`66 e8`** `[R+#i] op= 1 << R` (273 samples): the 64 e8 layout, ops {|, ^, &, &~}.
- `fm1-isa`'s FALLBACK class `pair_mem_any` takes any `5x ec` word no mined class matches
  (the corpora have few `58..5f` samples; Melodee 1.0.1 and ChoralRoot 0.15 use more).
- Both pair and extract bugs were found with `probes/x0x/d909.c`: X0X's own 909 code built
  for the device (its flags) and natively, compared word for word (`reference/fm1-x0x`).

### Flags, carry and soft double (2026-10-11)

Found by running reSID 0.16 on the emulator (a SID study, `fm1-sid`): its `double` math (compiler-rt
`__muldf3` / `__divdf3`, libm `sin`) came out wrong, so its sampling setup and filter
coefficients did. Probes `probes/softfloat/*.c` (`run.sh`; 64-bit add / sub / compare /
shift / multiply, soft double) compare the emulator with the host.

- **Flags** (`Cpu::flags`, psr bits 0..3): N, C (set = no borrow, as on ARM), Z (assumed),
  V. **Measured on an FM-1 (below): bit 3 is N, bit 2 Z, bit 1 C, bit 0 V; this note
  had N and V the other way round, and most ALU forms, not only add / subtract, write N and Z.** clang's 64-bit code reads them right after the low-word add / subtract: `R = R + R + c`,
  `R = R - R - !c`, and `rX = psr` then `slt` = N ^ V (bits 0, 3) / `ult` = !C (bit 1).
  psr used to read 0 and the carry forms assumed C clear, so every 64-bit add lost its
  carry and every 64-bit subtract borrowed. Every add / subtract form now sets the flags
  (which ones do on the real core is a probe item; compiled code only reads them right after
  the producer). A producer followed by a flag reader takes the careful path (the JIT does
  not model flags).
- **`b8 e0` / `b8 f0` add / subtract with carry**: rD = bits(28,31), rA = bits(20,23),
  rB = bits(24,27), op bits(16,19) 0 `+ c`, 2 `- !c`; f0 = the dual-issue form. The mined
  classes fitted only the corpus registers (`r_mov_r_r_c_l4` odd registers only,
  `r_mov_r_r_c_3` with bit 31 set); `carry_op_raw` decodes the family before the table.
- **`fc e1` 64-bit multiply-accumulate** `rP += rA * rB (u|s)`: the fields of `f8 e1`.
- **`d1 e8 <mask>` / `d9 e8 <mask>`**: pop / push of rets and a register mask for any mask
  (libm's `sin` pops `{rets, r11-r8, r6-r4}`; the classes matched only the corpus masks).
- **Table classes fitted to corpus operands** (found running floooh's m6502 in a SID player
  firmware, `fm1-sid/firmware`): when the table has no class, `fetch_at` now falls back
  to `decode_alu4` for every 4-byte ALU form it knows (`6c e8` `[r0+24] <<= 1`, `64 e8`
  `[r1+-4] &= r0`) and to route 19 for the post-increment family `d1..d7 ec/ed/ee`
  (`h[r4++=480] = r0`).

### Word accesses with negative offsets (2026-10-13)

Felucca 1.6 halted on `d7 ec 51 38` `[r5+-128] = r3`: class `r_i_mov_r_3` (19 corpus samples)
had no slots. The word family `d0..d7 ec` has one layout (27884 samples in the 25 listings,
0 mismatches): store = bit16, bit17 = pre-increment `[++rB=#i]` (clear: a plain offset, no
write-back), off = 4*sext9(bits(18,19) | bits(24,27)<<2 | bit0<<6 | bit1<<7 | bit2<<8), the
pair accesses' field. `d4..d7 ec` (byte0 bit 2: negative offsets) and table misses in `d1..d7 ec`
now go to route 18, which decodes it.

### New firmware versions: five table gaps (2026-10-13)

Nine catalog firmwares moved on and five were new on fm1-editor.com; five of the fourteen halted
on classes the corpora had fitted to the few register / size combinations they used. Each family
now has a raw decoder, checked on every corpus sample (the 25 listings and the 15 new ones), all
in `decode_any`, which `fetch_at`, `block_end`, `describe` and the flags lookahead share:

- `64 e0 00 XY` rX = sreg Y, `64 e0 80 XY` Y = rX (0 reti, 1 rete, 3 rets, 5 psr, 6 cnum,
  b icfg, c usp, d ssp, e sp; 40 pairs): ChoralRoot 0.21's `r2 = reti`.
- Predicated blocks the table had only without an else part (`decode_if_block` knew the layout;
  90440 block heads print as `if (...) {`; in family ea byte0 9x..bx are calls): Quest 0.21's
  `1d ea 80 a6` `if ((r13 & r6) != 0) {` with 3 + 2 units.
- `70 e0/f0 00 DS` rD = rev8(rS) (690 samples): Quest 0.21's `r8 = rev8(r0)`.
  `70 e0 40 DS` rev16 (each half's bytes swapped; FM-1 V16 swaps 16-bit samples with it),
  `70 e0 80 DS` revs(rS.l), `70 e0 c0 DS` popcnt (the vendor objdump --mcpu=r3).
- `d0 e9` register pairs at sp+off, `decode_pair_mem` (9701 samples): WaveLoop Kit's
  `d[sp+1044] = r13_r12`.
- `dc` `[++rB=rI]` / `de` `[rB++=rI]` with ec/ed/ee (3462 samples, route 32): base = bits(20,23),
  index = bits(24,27), data = bits(28,31); bit16 store, bit17 signed load (ec: the register
  form), a half store's bit17 the high half (`h[r9++=r0] = r0.h`). SLOOP 2.6 and ORBIT 0.7's
  `b[r2++=r0] = r1`. All of the family goes to route 32: the table's class for that `.h` store
  stepped the base by an immediate.

`block_end` had decoded with the table alone, so a block holding a fallback-only instruction
ended early; it counts through `decode_any` now. Every firmware that ran before is byte-identical
(bare app and whole package), and the decoder audit's mismatch counts did not change.

April OS was not a decoder gap: it keeps its samples at flash 0x74000 and waits at boot until it
finds them. Past the image the XIP window now shows the flash chip for every firmware (erased:
0xff), and a booted package's window, which covers flash 0x4000..0x93000 from the installed
image, takes the chip's contents wherever the firmware erases or programs (`Soc::sync_xip`).

### 64-bit multiply-subtract (2026-10-16)

ADD-1 (an additive synth built with the vendor clang 4.0.1) halted on `fe e1 40 61`
`r7_r6 -= r4 * r1 (u)`: clang emits it for `acc - (u64)a * b` (and `(s)` for signed factors),
and fuses `a * b - c * d` into a multiply and a multiply-subtract. The corpora have one sample
(`fe f1 00 00`), so the table has one class. The vendor assembler gives the `f8 e1` / `fc e1`
fields: pair = bits(29,31), signed = bit28, a = bits(20,23), b = bits(24,27) (`fe e1 f0 1e`
r1_r0 -= r15 * r14 (s), `fe e1 00 ed` r15_r14 -= r0 * r13 (u)). Route 21 runs it,
`rP = rP - a * b` mod 2^64, from the fallback for any operands. `probes/mul64.c` compares it with
LLVM's folding (borrows, wrap past zero, sign-extended factors, two pairs in flight; -O1 and -O2).

The pair-op fallback also returned the plain class for byte1 `f1`, so a head such as
`fc f1 ..` or `fe f1 ..` that the table didn't match was not taken as a dual-issue pair head (the
second instruction saw its results). `f1` words now get a class whose syntax ends in `#`.

The multiply by an immediate paired with a store that ADD-1 also reported (`ea f1 16 40 87 20`,
`r10 = r4 * 0x16 # [sp] = r7`) decodes and runs here, and ran before this change: the immediate is
the `composed_imm` 12-bit form (bits 16..27; the assembler calls it `timm12` and refuses 257 and
-2). Checked against the assembler for 22 encodings, and in ADD-1's own build with the pair
restored.

### M-VAVE's FM1 Open and the compiler's builtins (2026-10-19)

M-VAVE's open SDK (open.m-vave.com) links its app against `libfm1_engine.a`, whose mixers and
envelopes are hand-written DSP assembly, and the vendor clang has ~40 `__builtin_pi32v2_*`
functions any firmware can call. Encodings from the vendor assembler and compiler
(`work/probe/dsp.S`, `fam.S`, `mm.S`, `bi/*.c`); meanings from the compiler's own choices where it
makes one (`work/probe/sem.c`). `tools/sem_check.py` runs every register instruction of a listing
against objdump's text from random registers (`fm1-emu --sem-run`): the corpora, the engine and
the probes, ~175000 distinct instructions, agree except four shapes only data decodes to.

- **Pair extract** `de e1`: rD = rQ >> n (s|u); d = bits(28,31), q = bits(21,23), n = bits(16,19) |
  bits(24,25) << 4, signed = bit27. `df e1` by a register (bits 24-27; signed = bit16). Saturated
  into 32 bits (assumed: clang uses it only for `__builtin_pi32v2_shfms64` / `shfmu64`, never for
  a plain `(int)(x >> n)`, which it does with a pair shift). **Measured on an FM-1 (below): saturated, and
  the register count is its low 8 bits (64..255 shift everything out).**
- **`<<<` saturates** (`qasl`): the 2-byte `xx a0` / `SD 1a` with bit 3, `c0 e1` op 2 (bits 25-27),
  `c8 e1` sub-op 1. Plain C shifts compile to `<<`; it used to run as one.
- **Shift counts are 1..32**: 32 encoded as 0 in the 4-byte `c0 e1` forms too (the assembler's
  range), as in the 2-byte ones.
- `39 e4` rD = rA +|- rB (ssat), subtract = bit19; `34`/`35 e4` max / min, bit16 signed; `20 e0`
  rD.bN = #i (N = bits 26,27); `70 e0 80` revs(rS.l) = `(short)bswap16`; `d4 e1` / `dc e1` the
  64-bit rotate right (compiler's `(x >> n) | (x << (64 - n))`); `78 e0` sat16 and `94 e1`
  rD = rA & ~(1 << rB) on all registers.
- Assumed, no C equivalent: `80 e1 01` cls = leading bits equal to the sign, sign not counted;
  `b0 e0` rD += rS (brev), a bit-reversed add. `80 e1 02` zcmp is left undecoded (unknown).
  **Measured on an FM-1 (below): cls counts the sign bit too (cls(0) = 32); the brev add is
  rev(rev(a) + rev(b)) and writes all four flags; zcmp is the sign, -1 / 0 / 1.**
  The six 64-bit multiply builtins do not compile (the backend cannot select them).
- These decode before the table: its classes fit only corpus registers or had no semantics
  (`r_add_r_brev`, `r_b2_mov_i`), and they caught the dual-issue `f0` / `f1` heads.
- The ALNK0 receive channel pends too (bit 4 for channel 0): FM1 Open renders a block only then.

### Remaining exec gaps (known, traced)

- goto/if branch-offset fields: printed = word offset (delta = 2*printed)
  confirmed for goto_i, but the exact multi-piece bit map (over variable
  bits 0, 4..12) still unresolved — hand sweeps failed, needs a 3+-piece
  solver run.
- `&=`/`^=` register masks, `R += R`-with-saturate variants, `d[..]`
  DPRAM pairs, fpu/acc classes — all still Unsupported (halt with class
  name, which is the intended discovery path).

## Hardware facts (an FM-1)

Measured on an M-VAVE FM-1 (AC791N). The runs, their method and their raw records are in
[pi32v2-isa](https://github.com/jvitkauskas/pi32v2-isa)'s `hw/`:
- `hw/fm1-2026-10-24*` and `hw/fm1-2026-11-14-bit12`: about 14000 single instructions run from RAM, with
  every register, the psr flags, sp and a scratch area set, and everything read back.
  `tools/hw_replay.py --isa PATH` replays them through this core.
- `hw/fm1-2026-11-15-excprobe`: a RAM app with its own exception handler. It covers misaligned accesses,
  divide by zero and the FPU traps, `rep` counts and interrupts, the PC limits, cycle costs, and words
  only this decoder took (`45 00`..`48 00`, the `iff` gotos).
- `hw/fm1-2026-11-14-bootprobe`: the application's entry state under M-VAVE's loader and m-bootloader.

Where they disagree with notes above, these win.

- **psr**: bit 3 N, bit 2 Z, bit 1 C (1 = no borrow), bit 0 V. Reading psr keeps the flags; pushing it
  (`[--sp] = {psr}`) clears them. Signed less-than is still N ^ V.
- **Which forms write flags**: add / subtract (also with carry, and the brev add) write all four. The logic
  forms (`&`, `|`, `^`, `~`, `& ~`, the bit forms), extensions (`.b0`, `.l`), 32-bit multiplies and divides
  (for a divide bit 2 = the quotient is 0, bit 3 = its bit 31), `clz`, `cls`, `rev8` / `rev16` / `revs`,
  `insert`, `u/sextra`, `sat16`, `zcmp` and 32-bit rotates write N and Z and keep C and V. 32-bit shifts
  (`<<`, `>>`, `>>>`, `<<<`) write N, Z and C (the last bit shifted out; kept for a count of 0) and keep V;
  64-bit pair shifts write only C; 64-bit rotates, 64-bit products and divides, moves, constants, loads,
  `abs`, `min` / `max` and the FPU arithmetic write none. The 64-bit multiply-accumulates write only C (the
  carry out of the 64-bit add). `cc = 0/1` is psr bit 1.
- **Shift counts in a register** are its low 8 bits: 32..255 shift everything out (0, or the sign for `>>>`);
  a pair shift by 64 or more likewise. `<<<` saturates (signed), with the same count rule.
- **Parallel pairs**: B reads the registers as they were before A, as above; when A and B write the same
  register, A's value is the one that stays.
- **`rep`**: a count of 0 skips the body. One `rep` runs at most 32 times: below 32 it runs `count` times,
  from 32 up `(count & 31) + 1` times and leaves the register with its low five bits at 31. Interrupts wait
  until the `rep` has finished.
- **`if (...) {` lengths** count units: one instruction or one parallel pair.
- **`testset b[R]`**: the byte becomes 0xFF; psr bits 3..0 = bits 3..0 of the old byte.
- **Bit operations**: `popcnt rD = rS` is a nop on this chip (instruction revision 3 is not implemented as
  such), and the pair form copies the low halfword (`rD.l = rS_even.l`). `cls` counts the sign bit
  (cls(0) = 32). `insert` with l:0 changes nothing; `sextra` / `uextra` with l:0 give 0. `abs` saturates
  0x80000000 to 0x7fffffff. `sat16 (u)` treats its source as unsigned.
- **FPU**: a NaN operand, 0/0, inf/inf and inf - inf give 0x7f800001 (sign and payload dropped).
  `ftof` of a NaN is 0x7fc00000. `ftoi` / `ftou` clamp to their range; NaN and +inf give the maximum,
  -inf the minimum. `fmin` / `fmax` set psr as a compare (less 8, equal 6, greater 2, unordered 3) and
  prefer the number to a NaN.
- **Multiply / divide**: a zero divisor gives 0 (32- and 64-bit) without a trap unless EMU_CON
  (0x01EEF0D0) bit 2 is set; then integer and float division by zero raise the CPU exception with EMU_MSG
  bit 2. EMU_CON bits 16..20 enable FPU traps (inexact, overflow, underflow, divide by zero, invalid).
- **Memory**: a misaligned 16- or 32-bit access to RAM raises the CPU exception (EMU_MSG bit 0) always
  (EMU_CON bits 0 and 1 read 1 and cannot be cleared); the access itself still happens, at the aligned-down
  address. Pair accesses (`d[...]`) need only 4-byte alignment. The exception is taken about 7 instructions
  late; `reti` is the next instruction not yet run. A post-increment load into its own base register keeps the
  loaded value; a pre-increment store of the base register stores its value before the increment.
  `d[rB+rI<<3]` scales the index by 8. `h[...] = rX.h` stores the high half.
- **Second-word bit 12** is ignored by the `mul2` / `madd.mul2` / `msub.mul2` x2 forms and by the `d[...]`
  pair loads and stores (words the current vendor objdump does not decode).
- **Undecoded words**: `iff (rA_rB op rC_rD) goto` (the vendor objdump shows it only with `-mcpu=r4`) raises
  the illegal-instruction exception (EMU_MSG bit 1); words such as `45 00`..`48 00`, which the `R = #i` class
  takes, do nothing.
- **Boot**: there is a mask ROM (7056 bytes at 0x01E00000); the app is entered with r0 = 0x01C7FE08 (the boot
  object), r1 = the entry, icfg = 0x02050500, at 360 MHz, by both M-VAVE's loader and m-bootloader.
