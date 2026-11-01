# FM-1 SoC Emulator — Plan

Goal: a full instruction-set emulator of the **M-VAVE FM-1** pocket synth —
**JieLi AC791N / WL82 SoC** with a **pi32v2** CPU (JieLi's proprietary 32-bit
ISA) — CPU core(s) + SoC peripherals, accurate enough to boot the stock
firmware and run custom firmwares (Felucca, SLOOP, X0X, …) unmodified.

**Phase 0 recon is done** — see `docs/recon.md`. Headline findings:

- pi32v2 is a *known* proprietary ISA: the official AC79 AIoT SDK (Gitee,
  Apache-2.0) ships the toolchain, and kagaimiq's **ghidra-jieli** plus
  AL-255/FM-1-RE's pi32v2 Ghidra pipelines already disassemble it.
- XIP flash at `0x02000000`, 1 MB, single bank; stock engine is msfa/Dexed.
- Multiple GPL C firmwares built with the same toolchain = free golden
  references for differential testing.

---

## 0. Strategy overview

Custom firmwares exist, which means a working compiler/assembler for the target
already exists. That gives us three ground-truth sources, in order of
reliability:

1. **The toolchain + existing RE**: the AC79 SDK's pi32v2 assembler/disassembler
   tables, kagaimiq's ghidra-jieli SLEIGH spec, and AL-255's V13 disassembly
   give us instruction encodings directly.
2. **Compiled binaries**: the SDK + GPL firmwares let us compile known C/asm
   input and confirm encodings/semantics by correlating input and output —
   used here as *validation* of the mined ISA, and to fill gaps the tools miss.
3. **The real device**: deliberate probe code via the SysEx update path (or
   USB_KEY mask-ROM recovery mode) to observe semantics binaries can't reveal —
   flags behavior, cycle counts, undocumented opcodes, MMIO behavior. Optional
   until late phases; zero hardware risk before that.

We use all three: toolchain mining to bootstrap, binary diffing to validate and
fill the matrix, hardware probing to confirm semantics.

---

## 1. Phase 0 — Recon — ✅ DONE (2026-10-08)

Deliverable: `docs/recon.md`. Device, SoC, ISA family, toolchain, update
protocol, and the existing RE landscape are all established there. Remaining
recon actions (tracked in recon.md): clone reference repos, obtain the AC79
SDK and a stock V15 `.fwsc`, confirm physical device availability.

## 2. Phase 1 — ISA extraction (pi32v2)

Deliverable: `isa/fm1.yaml` — machine-readable instruction table:
mnemonic, encoding fields, operands, affected flags, cycle counts, notes.

**Status (2026-10-08): mask level DONE.** 1067 classes from ~421k
instructions across 5 corpora (V13, V14, uboot, uboot-debug, ota);
check_isa streaming decode = 0 mismatches / 0 undecodable on all.
Findings in `docs/isa-notes.md`.

**Operand level (2026-10-09): solved for most of the ISA.**
`tools/solve_slots.py` (exact linear fits over the varying bits, every
sample verified) covers 764 of 1067 classes = 86.8% of corpus
instructions (`derive.py` had 206 / 54%); `fm1-emu --coverage` lists the
remainder. 99.2% of corpus instructions execute. What worked on 2026-10-09:
**family-level fitting** — take every corpus line of one printed shape
(e.g. all 5302 `if (rN op #i) goto` lines), propose the bit fields, and
verify against all of them; five families (compare-immediate branches,
composed immediates, 4-byte ALU reg/imm/memory forms) went to 0
mismatches in one session and cover tens of thousands of instructions.
Continue that for the next families the boot hits. For sparse classes
and bits the corpus never varies, use the **assembler oracle** (2.2):
the vendor assembler is available and authoritative, so enumerate the
operand combinations per syntax and read the bytes back.

### 2.1 Toolchain mining (primary path)
- **ghidra-jieli SLEIGH spec**: if complete, this is nearly the whole ISA —
  SLEIGH semantics translate well into both our YAML and emulator pseudocode.
- **AC79 AIoT SDK toolchain**: read the pi32v2 assembler/disassembler
  (binutils/LLVM fork) opcode tables directly. Also gives relocations,
  ABI, calling convention — needed later for the GDB stub and for compiling
  probes.
- **AL-255's disassembly of V13**: a full real-firmware decode to
  cross-validate coverage: every opcode byte pattern in the stock firmware
  must decode with our table.

### 2.2 Assembler oracle + compiler probing (validation + gap filling)

**Assembler oracle (primary operand-level path).** For each syntax in
`isa/fm1.yaml`, generate assembly text sweeping each operand (every
register, immediates over the plausible range, each register-list shape),
assemble with the vendor clang (`tools/probe.sh --asm`, docker amd64,
batched thousands of lines per run), disassemble, and record
(text → bytes). One-variable-at-a-time diffs give exact bit fields,
sign-extension and scaling per syntax — no fitting ambiguity. Output
feeds `work/semantics_raw.json` → `tools/gen_isa.py` slots.

**Compiler probing.** Write minimal C/assembly snippets that each exercise
exactly one construct (compiled with the SDK toolchain):

- integer add/sub/mul/div, shifts, rotates (8/16/32-bit, signed/unsigned)
- logic ops, compares, all branch conditions, calls/returns
- loads/stores: every width × every addressing mode × offset ranges
- immediates: sweep values to find field widths and sign-extension behavior
- stack ops, special registers, supervisor/system instructions

Method:

1. Compile each snippet to a raw object/flat binary.
2. Diff binaries pairwise (one parameter changed at a time) to locate which
   bits encode the operand/opcode — the classic "one-variable-at-a-time"
   encoding extraction.
3. Record each discovered encoding row in `isa/fm1.yaml` with the probe that
   produced it (each row links to `probes/<name>.c` + expected bytes).

Automation: `tools/probe.py` compiles the whole `probes/` corpus and regenerates
a candidate encoding table; mismatches against the current YAML fail loudly.

### 2.3 Semantic confirmation on hardware
Encodings ≠ semantics. For each instruction class, build a probe firmware that:

- executes the instruction with crafted inputs,
- writes results/flags to a known memory location or UART,
- and lets us diff hardware truth vs. our assumptions.

Focus on: flag update rules (carry/overflow/zero/negative), edge cases
(shift-by-0, div-by-0, misaligned access), multiply/divide cycle counts, and any
opcode the toolchain never emits (fill the unused encoding space by executing
candidate bytes inside a fault-trapping harness and observing behavior — classic
"Opcode logger" / sandsifter-style approach, adapted to whatever fault handling
the SoC exposes).

### 2.4 Cross-check against stock firmware
Disassemble the stock firmware with the discovered ISA. A correct ISA table
yields clean, sensible disassembly: functions with proper prologues, no long
runs of unknown opcodes, branch targets landing on instruction boundaries.
Residual unknown regions = either data (find literal pools) or undiscovered
instructions (back to 2.3).

## 3. Phase 2 — Tooling

Deliverables in `tools/`:

- **Disassembler** driven by `isa/fm1.yaml` (single source of truth).

**Status (2026-10-08): started.** `fm1-isa` now contains the codegen'd
table (`tools/gen_isa.py` → `crates/fm1-isa/src/generated_isa.rs`,
regenerate after every `build_isa.py`) with a streaming decoder that
matches the Python checker's priority rules (len/popcount/count, sample
tiebreak). `fm1-core::step` fetches a 6-byte window and uses it. Pipeline:
`objdump corpora → tools/build_isa.py → isa/fm1.yaml → tools/gen_isa.py →
crates/fm1-isa/src/generated_isa.rs`. (2026-10-10: the corpora are listed in
`isa/corpora.txt`; after a re-mine run `tools/isa_stable_names.py` against the previous
yaml, `tools/solve_slots.py work/slots.json $(cat isa/corpora.txt)`, then gen_isa, and
`tools/audit_decode.py` on every listing.)

Operand layer (Python prototype first): `tools/derive.py` fits per-class
operand encodings against all corpora (437 classes fully solved, 115
partial; see `docs/isa-notes.md`); `tools/disasm.py` renders
vendor-comparable text — ~55% of all corpus instructions byte-exact,
0 undecodable. Next: exhaustive samples for sibling-mask classes, shared
formula table between fitter and renderer, +1/-1 immediates — then port
the operand spec into `gen_isa.py` + Rust formatting.
- **Assembler** — reuse the existing toolchain if redistributable; otherwise a
  small one from the same YAML, so tests can self-assemble.
- **Ghidra processor module** (SLEIGH spec) if the ISA is complex enough to
  warrant it — hugely helpful for navigating stock firmware.

## 4. Phase 3 — CPU core emulator

Deliverable: `crates/fm1-core` — an interpreter core, correctness before speed.

- Decode driven by `isa/fm1.yaml` (codegen the decoder from the YAML; never
  hand-maintain a second table).
- Interpreting fetch/decode/execute loop; exact flag semantics per Phase 1.3.
- Memory bus as a trait/interface: the core never touches peripherals directly —
  everything goes through the bus so the SoC layer owns the memory map.
- **Golden test vectors**: each probe from Phase 1 becomes a test — run the same
  bytes in the emulator, assert the hardware-observed result. CI runs the full
  corpus.
- Debugger hooks from day one: breakpoints, register/memory inspect, single
  step, and an execution trace log format we can diff against hardware traces.

## 5. Phase 4 — SoC peripherals

Deliverable: `crates/fm1-soc` — memory map + devices, added in boot-order priority.

1. Interrupt controller + reset/vector logic (needed before anything boots).
2. Clock/reset & timers (firmwares usually spin on these early).
3. UART/console — gives firmware a voice; debug output appears, boot progresses.
4. GPIO + the front-panel/display/storage devices as required by the firmware's
   boot sequence — implement lazily: log every unknown MMIO access, stub it,
   implement when the firmware actually depends on the behavior.
5. DMA, and any audio/DSP path, last (needed for full function, not for boot).

Every peripheral gets: a register map doc (`docs/peripherals/`), MMIO access
tracing (our primary reverse-engineering tool — watch what the stock firmware
touches), and unit tests where hardware behavior is known.

Unknown-access policy: log + return benign default, never silently swallow.
The access log *is* the reverse-engineering roadmap.

## 5.5 Phase 5 status — RTOS boot, dual core (2026-10-09, evening)

**The emulator runs the stock V13 firmware from reset through RTOS
start on two cores.** `fm1-emu --bin app.bin --flash decrypted.bin`:
CRT → RAM early init → clock/board init → interrupt tables → RTOS init
(dlmalloc heap, queues, mutexes) → cpu1 released (second `Cpu`, core id 1,
enters its render loop and acknowledges) → boot task created → `os_start`
enters the first task; ~370k instructions on cpu0. The task wrapper then
asserts in `xTaskResumeAll` (`uxSchedulerSuspended` is 0 — see
docs/isa-notes.md); the **interrupt controller + timer tick**
(0x1EEE000 region, IRQ vectors at 0x020000B0, `irq_c_dispatch`) is the
next piece, and the likely missing context for that assert. Verified semantics and
encodings: `docs/isa-notes.md`.

Core coverage: `fm1-emu --coverage` → 99.2% of corpus instructions by
weight execute (672 classes). Operand layouts: `tools/solve_slots.py`
(exact linear fits, corpus-verified) solves 764 classes = 86.8% of
instructions; semantics are keyed by printed shape (`exec_syntax`) with
raw-bit decoders where a class mixes encodings. Every hand-written
per-class arm that the solver covers was removed — three of them were
silently wrong.

Earlier in the day (kept for the record): the boot entry was wrong
(0x02000120 instead of the reset vector), the `0xFFC0xxxx` long-call
targets are RAM-resident `.data` code (not a mask ROM), and the 2-byte
shift / `rep` / post-increment decodes were corrupting RAM.

The SoC layer has RAM, the SRAM bank at 0x01F00000, the XIP window, the
interrupt-controller ready bit, a logged unknown-MMIO stub, and a first
ST7789-style LCD model on SPI1 (`crates/fm1-soc/src/lcd.rs`, frames
dumped to `work/lcd.bmp`) — nothing in the boot path has reached the LCD
yet.

## 5.6 Felucca runs: screen, keys, knobs, audio (2026-10-10)

**Felucca (GPL firmware, `reference/Felucca`) runs end to end.** It boots,
reads its fonts from the SPI-flash model, draws its UI on the LCD, takes
key presses and encoder turns from the modelled front panel, renders audio
in its ALNK0 interrupt, and runs its main loop with TIMER5 nesting into the
audio ISR. A scripted 7 s session (notes, a knob-driven filter sweep, PLAY)
comes out as `work/lcd_<ms>.bmp` snapshots and a 44.1 kHz WAV:

    fm1-emu --bin work/felucca_xip.bin --entry 0x02000120 --ms 7000 \
      --keys "800-1500:n7,3300:K1:-2,5200:PLAY" --wav work/demo.wav --snap 400,500,...
    tools/demo_page.py out.html     # snapshots + WAV + script -> one page

Peripherals added (`crates/fm1-soc/src/periph.rs`, `spiflash.rs`), all from
Felucca's HAL headers: SPI0 NOR flash, TIMER4 (24 MHz base) and TIMER5
(10 kHz tick) on a 240 MHz cycle clock, ALNK0 I2S double-buffered DMA,
SARADC (MASTER pot, battery), the interrupt controller (enable/priority
nibbles at 0x1EEF100, software latch 0x1EEF1A0/4, RAM vectors 0x01C7FE00,
nesting by priority), and the 2×74HC595 key matrix with 7 quadrature
encoders. The core gained interrupt entry, `sti`/`cli` and an `icfg` that
reports the active ISR levels.

CPU bugs found on the way, each pinned with a differential probe
(`probes/*.c`, built by `tools/probe_build.sh` with the vendor clang and
checked against compile-time constants): halfword reads at word offset 2,
`R = R.b0 (u) #`, 12-bit compare immediates (unsigned families
zero-extend), 4-byte `R = R << #n` executing as `>>>`, `[R+#i] <<= #n`,
`fX e0` decoded as a multiply, the flag-setting `smin/smax` forms. Probes
pass: arith 121/121, huff 139/139 (Felucca's own Huffman font decoder on
data from its own packer), lcdtest (SPI1/DMA stripes).

Speed: a decode cache plus cheap string matching took the runner from 1 M to
~17 M instructions/s, so one emulated second takes ~14 s. Live play
(host keyboard in, speakers out) needs ~1×: next steps are a per-class
pre-decoded dispatch instead of matching the printed syntax every step, and
fast-forwarding the clock through known busy-wait loops.

## 5.7 Live play at full speed (2026-10-10)

`cargo run --release -p fm1-live` plays Felucca live: a native window drawn after
Felucca's controls diagram (LCD, knobs, buttons, the 27 keys, LEDs from the firmware's LED
lines), mouse and computer keyboard input, audio through cpal, paced by the audio buffer. It
runs the FM-1's real 240 MHz at ~1.4x real time:

- interpreter: decoded-instruction cache, micro-ops for the common forms (operands and branch
  targets resolved once), straight-line blocks, interrupt delivery exact at block granularity
  (blocks stop at due ticks, peripheral writes, and the end of pairs / predicated blocks / rep
  loops), RAM/XIP fast paths, idle skipping of TIMER4 polling loops (capped at 20 us)
- JIT (`crates/fm1-core/src/jit.rs`, AArch64 macOS): hot blocks compiled to native code with
  chaining, guest state in the Cpu, inline RAM/XIP loads and stores, helpers for everything
  else that sync the clock to the exact instruction; assembler vendored from esp32sim
- verification: `fm1-live --bench MS --mhz N` with `FM1_BENCH_WAV=out.raw` must give
  byte-identical audio with the JIT, the block interpreter and `FM1_NO_BLOCKS=1` (per step);
  `tools/audit_decode.py OBJDUMP` compares every decoded instruction with the vendor
  disassembly (0 real mismatches: Felucca 92% covered, stock V13/V14 80%)

Bugs found on the way: the `d0 e9` sp pair store offset (>= 512 went to the wrong slot: stale
LED arrays, keys stayed lit), interrupt nesting (a tick between an ISR's `reti` pop and its
`rti` made the rti return to itself: now nesting only while reti is on the stack, the Blackfin
rule), the 4-byte register-mask push/pop, 2-byte shifts by 32, the `{pc, r3-rN}` pop range.

## 5.8 A selection of firmwares, and the public demo (2026-10-10)

All fifteen downloadable community firmwares from fm1-editor.com/firmware run: Felucca and
its forks, AMB-1, ChoralRoot, FoMni, FuMi-1, GHOULBOX, Hortator, Jangada, Melodee,
PurpleMonkey, FM1 Quest, SLOOP (+ALG), X0X. `web/firmwares.json` is the catalog.

- The page is published by `tools/build_site.sh` and `.github/workflows/pages.yml` (until
  2026-10-12 with ai-smarthome's other demos, where this project started; CI builds the wasm and runs `web/smoke.mjs`, which boots Felucca to audio and a screen).
  It hosts no firmware: it fetches each package from its author's GitHub Pages (all allow
  cross-origin reads) and unpacks it with `web/fwsc.js` (our own unpacker, byte-equal to
  Felucca's package code on every package). Authors move on fast: when the catalog's
  version URL is gone, the page takes the package the author's installer page names now
  (four had moved the same day). `node web/check_firmwares.mjs` checks the whole list.
- Native: `node tools/fetch_firmwares.mjs` (same code) -> reference/firmwares/<id>.xip.bin;
  `fm1-live --fw x0x`, `--fw list`.
- Picker: firmware and clock (`?fw=x0x&mhz=120`, remembered); a change reboots the
  worklet. The worklet gets the wasm bytes (a compiled Module is a messageerror in the
  AudioWorklet scope: the page never booted in Chrome before) and reports traps.
- What it took: the FPU and float compares, three decoder bugs (pair modes, the extract,
  `66 e8`), a fallback class for the whole `5x ec` pair family, an unknown-access log that
  shifted 8192 entries per access (FuMi polls P33), a JIT fall-through for branches run
  through the interpreter, `[++R=-#i]` routing, and the ISA re-mined from 25 listings
  (the newest versions included). The exercise (`FM1_BENCH_KEYS`: every button, every
  encoder, notes) runs 40 s on all fifteen; JIT == per-step audio on Felucca, X0X (playing
  a pattern), AMB-1, FoMni, FuMi, Melodee, ChoralRoot.
- Speed at 240 MHz native: 0.7x (GHOULBOX) to 2.5x (Melodee). Web at 100 MHz: 1.0x to 2x.
- Melodee and X0X hand audio work to core 1 when it answers; it does not here (cpu1 is not
  modelled), and both fall back to core 0. The stock M-VAVE firmware still stops early in
  its RTOS (no interrupts, no LCD): not in the picker.

## 5.9 Speed pass: every firmware at 240 MHz, native and in the browser (2026-10-11)

Real-time factor at 240 MHz (8 s bench native, 4 s in node for the wasm build), before -> after:

| | slowest | Felucca | fastest |
|---|---|---|---|
| native (AArch64 JIT) | X0X 1.14x -> 2.65x | 1.63x -> 4.4x | Melodee 3.1x -> 5.9x (Hortator 6.3x) |
| wasm | X0X 0.53x -> 1.62x | 0.66x -> 2.3x | Melodee 0.97x -> 3.5x (Hortator 3.7x) |

Every firmware's bench audio is byte-identical to before (the length can differ by an audio
half: the run loop checks its budget per block); JIT == block interpreter == per step.

- The slow path was 1.7-3.8% of instructions and half the time: each one also ended a block
  (JIT exit, re-entry). Now 0.1% (ISR prologues, `rti`). New micro-ops: execute's raw-decoded
  routes (16 pair loads / stores, 18/19 pre / post-increment, 21 64-bit ops, 22 rev8, 27 rets
  pops) through one shared `exec_raw`, so the micro-op and the route cannot drift; `{rets} =
  [sp++]`, `tbb` / `tbh`, pair moves, `~ - abs rev8 clz`, `/ %`, `& ~R`, shifts by register,
  `[++R=R]`-style accesses. `[--sp] = rets` had a micro-op that could never be built (the store
  matcher returned first).
- Dual-issue pairs whose second instruction reads and writes no register the first writes run
  as two plain ops (`BOp::glue`), compiled too; a block cut between them holds interrupts back
  (`Cpu::hold_irq`) as a pair in flight does.
- A pc whose first instruction needs `step` is remembered (an empty block) instead of rebuilt.
- wasm: the hot match is `#[inline(always)]` with the rare ops out of line (it had stopped being
  inlined: a call per instruction); one codegen unit and `-inline-threshold=2000` (+3-4%).
- WebAssembly JIT (`crates/fm1-core/src/jit_wasm.rs`, after esp32sim's): a hot block's region
  (blocks reachable over fixed edges, <= 32 blocks / 384 ops) becomes a small module whose
  `run` the host installs in the exported function table (`web/jit.js`, a copy in worklet.js);
  Rust calls it by index, helpers are `call_indirect` targets. Same timing rules as jit.rs.
  Per-block modules gained nothing (blocks average 3.5 instructions); regions doubled the speed.
  Chrome compiles them synchronously in the AudioWorklet (0 failures; ~25 us each; at most 16
  per audio quantum). `?jit=0` turns it off; the status line's tooltip counts the regions.
- Tools: `tools/bench_all.sh` (speed table, `WASM=1`, audio identity with `WAV=`/`REF=`),
  `tools/bench_ab.sh` (interleaved A/B: back-to-back runs drift 6-8%), `tools/check_modes.sh`.
- Tried and dropped: tick accounting per block in the interpreter (owing the ticks and paying
  them before any non-RAM access): 5-7% slower native and wasm (blocks are short).
- Next: the AArch64 JIT compiles natively what still goes through `h_op` (8.8% on X0X: the new
  micro-ops), the careful path (pairs that cannot be glued, if/else blocks: ~9%), regions for
  the native JIT too; the browser default clock can move to 240 MHz.

## 5.10 MIDI in (2026-10-12)

The MIDI IN jack works: UART1 (`0x12100`, IRQ 20) in `periph.rs`, the JieLi layout from the
SDK driver and Felucca's `hal/fm1_uart.h` (RX DMA into the ring RXSADR..RXEADR, written CON0
bit 7 latches the bytes since the last latch into HRXCNT, RX pending at RXCNT bytes, timeout
pending after OTCNT UART clocks of silence, TX DMA done at once). Bytes arrive one per 31250-baud
byte time (320 us); at most a second's worth wait (a USB keyboard can send faster than DIN).

- Felucca polls the count from its TIMER5 ISR; M-VAVE's firmware takes the timeout IRQ (CON0
  0x6d) into its `midi_route` task. Notes play on Felucca, Felucca Salt, FoMni, FuMi, GHOULBOX,
  Jangada, Melodee, PurpleMonkey, Quest, SLOOP, AMB and stock; ChoralRoot, Hortator, SLOOP ALG
  and X0X gave no note from a plain note-on (X0X's driver says "untested on hardware").
- **Stock listens on channel 2** here: boot code at `0x0200431A` writes `[0, 1, 2, 12]` to
  `ENG+4780..4783`, and only `0x91` notes sound. To check on the real FM-1.
- Front ends: the page has MIDI in (Web MIDI: off / all inputs / one device, a channel remap,
  an activity dot; access asked only once an input is picked) and `fm1_midi(byte)` in the wasm;
  `fm1-live` connects every MIDI input (`--midi none|NAME`, `--midi-ch N`, midir).
- Tests: `FM1_BENCH_KEYS=1000:m903c64,...` (fm1-live) and `--keys 1000:midi:903c64` (fm1-emu)
  script bytes; `web/smoke.mjs` checks a note sounds through the wasm; a unit test covers the
  DMA, the latch and the timeout. Without MIDI every firmware's audio is byte-identical to
  before; with it JIT == interpreter == per step.

## 5.11 A repository of its own; FM-1 SID as a synth with effects (2026-10-12)

- The emulator and fm1-sid moved from ai-smarthome (`audio/fm-1`, `audio/fm1-sid`, history kept)
  to github.com/joakimeriksson/fm1-emu; Pages: the emulator at the top, the SID player under
  `fm1-sid/` (`tools/build_site.sh`, `.github/workflows/pages.yml`: unit tests, both smoke tests,
  deploy). The old ai-smarthome addresses forward here with their query.
- The page got a guide below the synth (what it is, playing, MIDI in, a card per firmware from
  `web/firmwares.json`, M-VAVE's own, limits, how it works, credits).
- FM-1 SID's update package boots into the synth (pack flag 8 on a pack of its own tune), and the
  firmware has effects (`fm1-sid/firmware/fx.c`, after Felucca's): stereo chorus, tempo delay,
  room reverb on an FX page. The player stays dry by default and its output is bit for bit the
  firmware's before; the audio interrupt goes from ~10 % to ~14 %, RAM to 425 of 432 KB.
- The SID firmware builds on macOS with the JieLi toolchain in Docker (`linux/amd64`), the
  image byte-identical to the one built on Linux.

## 5.12 Every firmware's newest version, and five more (2026-10-13)

- Felucca 1.6 halted on a word store with a negative offset (`docs/isa-notes.md`); the catalog
  was then refreshed: ChoralRoot 0.21, FuMi 1.0.7, GHOULBOX 2.1, Hortator 0.16.0, Jangada 0.9.9,
  Melodee 1.3.3, Quest 0.21, SLOOP 2.6 and X0X 1.0.6, and five new: April OS (beta 1.1), FiMba-1
  0.6.1, ORBIT 0.7.0, Rainbow mode 1.1.5-rb and WaveLoop Kit. SLICE64-FM is on the list too, but
  its firmware is not published.
- Five of the fourteen halted on table gaps (sreg moves, if-blocks with else, rev8, sp pairs,
  register-increment accesses), and April OS waited for its sample flash: all fixed, see
  `docs/isa-notes.md`. Each was tested with keys and MIDI, screens checked, JIT == block
  interpreter == per-step audio, natively and in the wasm build; every firmware that ran before is
  byte-identical. Jangada 0.9.9 now starts the second core, as Melodee does.
- The catalog's packages can be found by a `manifest` (ORBIT's manifest.json, FiMba-1's
  latest.json) or `embedded` in the installer page (April OS's base64 `DEFAULT_FIRMWARE`).

## 5.13 Speed pass 2: RAM code in the browser; the page says when a device can't keep up (2026-10-14)

Android users found the page too slow. Melodee was the one firmware under real time in wasm
(0.95x at 240 MHz): its audio code runs from RAM, and compiled code ran 3.3 instructions per
entry there (others 16-21): a wasm region took a RAM block only as its start, every RAM block
was compared with memory on every entry, and wasm's memcmp goes a byte at a time.

Real-time factor at 240 MHz in the wasm build (node), before -> after (interleaved A/B):

| Melodee | stock V13 | X0X | Felucca | GHOULBOX | others |
|---|---|---|---|---|---|
| 0.90x -> 2.47x | ~3.0x -> 3.7x | 1.58x -> 1.68x | +4% | ~0 | within noise (+-3%) |

In Chrome, Melodee at 240 MHz went from ~122% of the audio thread (dropouts) to ~39%.

- RAM keeps a flag per 4-byte word of code blocks were built from (`Ram::mark_code`, through
  `Bus::code_mark`) and counts writes to flagged words (`Bus::code_writes`). In the wasm build a
  RAM block is compared with memory only when that count moved since it was last found current
  (`Cpu::ram_block_ok`); the comparison itself goes 8 bytes at a time there (natively the
  platform memcmp is faster than that loop). Natively nothing changes: the AArch64 JIT's inline
  stores do not look at the flags, so the count is not used there.
- wasm regions now include RAM blocks: each region keeps its RAM blocks and the count they were
  checked at, and is checked again on entry when it moved (`wasm_region_ok`; a changed one is
  dropped and its blocks compile anew). Inline stores to a flagged word take the bus, and a write
  that moved the count leaves the region right after the instruction.
- Words, not 256-byte pages: no difference measured, but the stock firmware's cpu1 keeps data
  near its RAM code. What made stock 2x slower at first was cpu1's idle probe: it runs a block
  at a time to find its polling loop's start again, and compiled code, limited only by the tick
  horizon, now ran past it through the region. Compiled code now also keeps the caller's
  instruction budget (whole blocks: the first always, more while they fit), as the interpreter.
- Compiled in wasm instead of the helper (`h_op`): `PushRets` / `PopPc` (function prologue and
  epilogue: ~20% of stock cpu1's instructions went through the helper), `MemA4`, `BitExt`,
  `BitIns`, `Mov2`, `Mov2I`, `cli`. The helper calls left: `sti` / `icfg` (the interrupt
  window), FOp (NaN payloads must match), Tb, Raw routes.
- Tried and dropped: wasm SIMD (`+simd128`) and binaryen `wasm-opt -O3`: both within noise.
- Checks: every firmware's wasm audio byte-identical to before (21 + stock, 4-6 s at 240 MHz),
  native audio byte-identical (`tools/bench_all.sh` REF), JIT == interpreter == per step.
  (Melodee's per-step run differs as before this pass: it runs both cores.)
- `web/bench.mjs` takes `FM1_WASM=file.wasm` for A/B runs of two builds.
- The page measures how it keeps up: the worklet times each quantum's emulation, the status
  line shows the load (its share of the audio thread), and the audio clock's drift against the
  wall clock (`getOutputTimestamp`) shows dropouts. Three seconds of either after warm-up bring
  up a warning with the fix: the highest clock that fits (load x 1.25), or a larger audio
  buffer (`latencyHint: "playback"`, remembered) when it drops out with time to spare; "Copy a
  report" gives device, browser, audio setup and load for a bug report. Chrome's CPU throttling
  does not reach the audio thread; `?mhz=700` overloads it for a test.
- Next: Melodee is still the slowest natively (1.4x: the AArch64 JIT does not chain into RAM
  blocks; the same count would let it, with the flag check in its inline stores); careful pairs
  (a pair head whose second instruction reads its result) stay interpreted in both JITs.

## 5.14 The SMK-37 board: its community ports run (2026-10-14)

M-VAVE's SMK-37 (Pro / Elite, MKE-P37, Donner Starrykey 37 Play) has the FM-1's chip, flash layout,
audio and update path; only the panel's wiring differs. genkigenki's fm1-to-smk37 kit documents it
(docs/01-hardware.md: each fact verified on a unit or read from the stock firmware) and has ported
14 firmwares (genkigenki.github.io/smk37). `Board::{Fm1, Smk37}` picks the wiring, from the package's
identity (`SMK-37 Pro_016`); every FM-1 path is unchanged (all firmwares byte-identical, native and
wasm).

- `fm1-soc/src/smk.rs`: the 16 x 8 matrix (four 595s by SPI2, a 32-bit word latched on PA1; rows on
  PA2, PA5-7, PA9, PH6, PH8, PH9): 37 keys with two contacts (the second closes `smk_vel` after the
  first: the velocity), 18 buttons, 8 encoders; three 8:1 analog muxes on ADC 1 / 8 / 2 (pads,
  faders, wheels, pedal, battery) addressed by the word's bits 5..3; the 16 RGB pad LEDs (WS2812
  frames on SPI1 group B, GRB). SPI1 group A is the LCD with D/C on PB5. Flash id EB 60 14 and a
  default calibration record (USRTRIM, `trimdat`).
- The map's matrix codes are in the reference port's column numbering: it reads a column's rows
  before latching the next word, so code column c is the word selecting c - 1. Taken literally the
  keys came out scrambled (k0 played A#3: its contacts read as k1's first and k10's second).
- Packages: the header has as many blocks as its markers say (36 for SMK-37 Pro v016) and spells
  the identity; 36 blocks reach into the image at 0x400, whose start then loses its markers too.
  Stock SMK packages store wrong CRCs for their small files: an unchecked isd_config.ini is taken.
- Three decoder gaps (the corpora's offsets / masks): `4x ff 00 AB oo oo` register-compare branches
  with a 16-bit offset (route 13; 3004 corpus samples), `d5 e8` pops of pc and a register mask
  (route 28; 54), `d4 e9` word loads / stores at sp + off (new route 33; 20620).
- All 14 ports boot, draw and play (Snoop37, X0X, FoMni, GHOULBOX, ORBIT, AMB, ChoralRoot, Rainbow,
  MAKU, Makkuro37, ObervaVe, Polly, Vector37, Winamp); 37 keys checked as a chromatic scale on
  GHOULBOX; Snoop37's splash animates the pads. Snoop37's idle sound is its boot jingle.
- M-VAVE's own SMK-37 firmware unpacks and runs but stays black (90% of its time in one interrupt):
  its drivers want more of the board than the ports do.
- Next: the front panel (native and the page), the catalog, button LEDs, MIDI out (UART1 TX on PC7).

## 5.15 The SMK-37's page and panel (2026-10-14)

- `fm1-live/src/panel_smk.rs` (shared with the page, as panel.rs): the SMK-37's front panel after
  the kit's panel map: the wheels and six small buttons, the screen at 2x, K1..K8 over the 16 RGB
  pads, four faders and twelve buttons, the 37 keys C3..C6 (1920 x 900). panel.rs's primitives
  draw on any canvas width (`canvas`). The 19 button LEDs: the latched word's low bits while their
  group's common (0x16A0C bits 17 / 18, mapped for the SMK-37 only: the FM-1's stock firmware
  touches that block too) is on.
- The page: one `main.js` for both devices (`<html data-device>`): its catalog, words, panel, and
  a package's board switching the panel (an SMK-37 package opened on the FM-1 page gets the
  SMK-37's). `web/smk37/` is the SMK-37's page (its own guide, preview card, catalog), the shared
  files copied in by web/build.sh; `style.css` shared. On the SMK-37: a key hit lower plays harder
  (2..80 ms between its contacts), pads, faders, wheels (the pitch wheel springs back), a MIDI
  keyboard on its keys and wheels (it has no MIDI in jack). Cards link "also on the FM-1 / SMK-37".
- `tools/smk37_catalog.mjs` builds `web/smk37/firmwares.json` from genkigenki's install page (its
  FW list and cards), FM-1 twins found by upstream repository; `tools/readme_firmwares.mjs` keeps
  both README tables. `web/smoke_smk37.mjs` in CI: GHOULBOX's SMK-37 build, a key sounds.
- fm1-live opens an SMK-37 package in an SMK-37 window (`window_smk`; not tried by hand yet);
  `Action::Vel` carries a key velocity.

## 5.16 M-VAVE's own SMK-37 firmware runs (2026-10-15)

It booted to a dark screen. Five things, in the order they showed up (all SMK-37-only or gaps no
FM-1 firmware reaches; every FM-1 firmware byte-identical, native and wasm):

- An interrupt storm, 2.4 M a second: its ADC scan chains conversions through IRQ 24, each handler
  starting the next, and conversions finished at once. On the SMK-37 a conversion now takes 40 us
  (the hardware map's SMK timing) and raises IRQ 24 at its end (`Periph::adc_next`).
- Its Bluetooth controller task reset the chip ("btctrler"): its buffers are checked against the BT
  window base at 0x2FC44, which the emulator served as V13's .bd_base. The firmware writes that
  register (V13 writes 0x01C09E7C, the SMK-37's v016 0x01C09A18): it is stored now.
- The float multiply-accumulates `3f e5` op 7 / 8: `rD += rA * rB (f)` / `rD -= ...` (vendor
  assembler; not fused: JieLi's clang calls a library fmaf). Native JIT: through the helper.
- `78..7b e0 0S DS`: rD = sat16(rS) (s / u), now a micro-op (the table's class had no semantics).
- Core 0 waited at boot for the second USB PHY (0x16800 bit 4, after powering it up): the block
  is stored registers on the SMK-37, bit 4 reading set.

It runs as a MIDI controller: the main screen, buttons (ARP toggles the arpeggiator, its LED
lights), pads lit, and the keys: PATCH, then C-E-G held, and it shows "C Maj" with those keys
marked, which also confirms the matrix's column numbering independently of the ports. No sound:
its notes go to USB / Bluetooth MIDI. SPI1 transfers take their time on the wire on the SMK-37
(60 MHz / (BAUD + 1), done bit and IRQ 16 at the end).

Speed: ~150 k interrupts a second (its key scan from SPI2's interrupt), and the ISR wrappers'
`{psr, rets, reti}` pushes / pops went through `exec_syntax`'s string matching with an allocation
each: now first in exec_syntax, allocation-free (native 1.17x -> 1.42x at 240 MHz); the SoC's
register map hashes with a cheap multiplicative hash instead of SipHash. In the browser it runs at
~0.6x real time whatever the clock: the cost is per interrupt and peripheral event. Next for it:
the interrupt path (ISR entry / exit as micro-ops, fewer compiled-code exits).
`fm1-emu` prints interrupts by source; `FM1_FWSC_XIP=file` saves a package's XIP window.

## 5.17 The catalog follows the FM-1 Firmware Hub too (2026-10-15)

thefirmwarehub.com (designburg's hub, `/catalog.json`, 37 firmwares) lists more than
fm1-editor.com. Its own package copies (`fw/<name>.fwsc`) have no CORS header, so the page
fetches each one from its author: github.io pages, or raw.githubusercontent.com for a repo's
`docs/` (both send `Access-Control-Allow-Origin: *`). GitHub release assets don't, and the
firmwares published only that way stay out (Dinghy, fm1-chord, MAKU, FM-106, Fart Machine One,
Felucca 8 Patterns); DUELL and MYCELIUM have no published package.

Added: CTL-1, ETCH, FM-1 Doom, JIANT FM, OberVave, Polly, Skiff, SLOOP DX7 Banks, SLOOP Floyd FM,
sloopDX, TRIBE, winampFM, zp12 (35 entries, 33 community). Bumped: Felucca 1.6.1, Jangada 1.0,
Melodee 1.3.6, X0X 1.0.9. Two decoder gaps: `5x ed` halfword loads / stores (route 35, the
classes fit only the corpus offsets; JIANT) and `80 e1|f1 00 DS` clz (route 36; Bubba Machine).
Not added yet: Bubba Machine (stops on `rts` decoded as data at 0x0201d5d4, no audio) and Doom
E1M1 (spins on a timer at 0x020035dc waiting for `[r10+4]` to advance, blue screen).

## 5.18 The plugin: AU, VST3 and CLAP (2026-10-15)

`crates/fm1-plugin` (its own cargo workspace, so its GUI dependencies stay out of the emulator's
builds): one CLAP instrument written with nih-plug, its VST3 feature off since that brings GPL
bindings. clap-wrapper v0.16 (`crates/fm1-plugin/wrap`, CMake + Ninja) makes the AUv2 and the VST3
of it, with Apple's AudioUnitSDK (Apache-2.0) and the VST3 SDK 3.8 (MIT); each bundle carries the
CLAP in Contents/PlugIns, where the wrapper looks first. `tools/build_plugin.sh` builds, signs ad
hoc, installs, runs `auval` (passes: render tests at 11.025-192 kHz, latency, state, UI).
Under Ninja clap-wrapper's PRE_BUILD copy of the generated Info.plist is overwritten by the
bundle's default one (no AudioComponents: "didn't find the component"); the script copies it in.

The machine runs on its own thread, a ring of 44.1 kHz frames ahead of the host (a host block +
1024), host MIDI stamped with its frame (the host's position + the lead), applied when the machine
reaches it; the lead is the latency reported. A test (`tests/engine.rs`) runs Felucca and finds a
stamped note sounding 312 frames after its frame (the DMA half then running, and the attack).
The project keeps the package (base64) and the flash sectors the firmware wrote (checked every
2 s), so patches come back. The window draws panel.rs / panel_smk.rs, played as on the page;
MASTER is a host parameter, so is a MIDI channel override (M-VAVE's firmware listens on 2).

## 5.19 Optimization pass: measurements, audio worklet, and native RAM chains (2026-10-15)

Native AArch64, 240 MHz, three interleaved A/B rounds of 4 s with the same default
scheduled chord (total emulated time / wall time, including boot):

| Firmware | Before | After | Ratio |
|---|---:|---:|---:|
| Melodee | 1.96x | 3.63x | 1.85 |
| Felucca | 4.80x | 4.83x | 1.01 |
| X0X | 3.03x | 3.07x | 1.01 |
| stock V13 | 3.47x | 4.46x | 1.29 |

WASM in Node, three interleaved rounds at 240 MHz with identical inputs, measured after
1 s of warm-up: Melodee 1.63x -> 1.62x, Felucca 2.68x -> 2.69x, X0X 1.60x -> 1.65x,
stock 2.65x -> 2.60x. Only X0X showed an improvement (~3%); the other differences are
within the previously observed measurement noise. These are local host results, not
Android or browser AudioWorklet measurements.

- Measurement: native and WASM use `fm1_soc::script::queue` for the same timestamped keys,
  encoder and MIDI events (`FM1_BENCH_KEYS`). WASM now defaults to the native benchmark's
  chord, stops at actual emulated time, checks boot/halt/progress, and reports real audio
  frames separately. `FM1_BENCH_NOKEYS=1` disables the script. Old binaries without the
  script export require that option rather than silently testing different workloads.
- `FM1_BENCH_JSON=file` records results for both hosts; `FM1_WARMUP_MS` separates steady
  playback from boot. WASM also reports render p50/p95/p99/max, short quanta, compilation
  time/bytes/count, table slots and linear-memory size. Optional `FM1_PROFILE=1` (WASM) or
  `FM1_BENCH_ROUTES=1` (native) reports JIT entries/instructions and helper counts per core.
  Profiling allocates and slows execution: do not enable it for speed comparisons.
- `node tools/bench_wasm_ab.mjs A.wasm B.wasm 3 [firmware ids...]` runs alternating A/B and
  B/A pairs and reports median steady-state factors plus all samples. `MS` and `MHZ`
  select duration/clock. Native comparison remains `tools/bench_ab.sh`. Comparison scripts
  now fail on execution errors or audio mismatches; mode checks explicitly skip per-step
  execution when a second core ran.
- Worklet: cache output views (refresh on memory growth, pointer or quantum-size change),
  send LCD only when pixels changed, cap reports at 30 Hz using audio frames, transfer pad
  buffers too. LCD revisions include partial writes and reset; an identical redraw does
  not need a transfer. The page consumes each LCD update once and preserves pending updates
  across status-only messages. Load accounting includes copying and `postMessage`, reported
  one interval later so the message's own delivery cost is included.
- Native RAM chaining: inline stores consult live code-word flags, helpers leave after code
  writes, and each compiled RAM body validates its own immutable snapshot against the
  shared bus write version. This also guards old incoming links after code replacement.
  Snapshots are owned by the JIT and freed with its cache. Chained execution respects the
  caller's instruction budget, as WASM already did, to preserve cpu1 idle-loop discovery.
  Untracked buses do not get RAM chain links.
- The first chaining prototype slowed flash-heavy firmware ~6-8%. Helper profiling then
  identified millions of X0X calls for stack/bitfield operations. AArch64 now directly emits
  SpAdd, RFromSp, Mov2/Mov2I, BitExt/BitIns, PushRets/PopPc, PopRets and table branches.
  WASM directly emits PopRets and table branches. The final native A/B run above no longer
  shows the flash-heavy regression. Floating-point/NaN handling stays on its existing path.
- Validation: 40 core/SoC tests, including native generated-code comparisons against the
  interpreter for aliasing and edge bitfields, budget limits, stale RAM chain targets, and
  stores into executing code. Two Node worklet tests cover LCD throttling, partial updates,
  view reuse/memory growth and full callback timing. Native and WASM audio agree with their
  respective baselines on all 35 locally available FM-1 firmwares (4 s at 240 MHz; compare
  common lengths because a runner may stop at a different DMA half). JIT == block interpreter
  for Felucca, X0X, GHOULBOX, stock and Melodee; per-step checks pass where applicable.
  The local WASM smoke test boots, draws 57,600 lit pixels and responds to MIDI.
- Review (Claude): the WASM "within noise" above came from four firmwares that change little.
  Over all 35 (three interleaved rounds, no input script) the pass made WASM 4% slower, AMB
  -12%, TRIBE -10%, GHOULBOX -6%: `h_op` called `profile_helper` for every helper op, with
  profiling off too (not inlined there, the op copied). Bisected by building WASM variants
  (no LCD revision compare: still slow; no PopRets/Tb: still slow; no profile call: level).
  Now each JIT picks `h_op::<B, PROF>` when compiling, from `Cpu::profile` when the JIT was
  made, so plain code pays nothing (turn profiling on before the first render). After: WASM
  0.99-1.01x of the committed build on AMB, TRIBE, Felucca, GHOULBOX, X0X, Melodee, stock;
  native as above (Melodee 1.98x -> 3.66x, stock 3.50x -> 4.47x, AMB / TRIBE level). Audio
  identical to the previous build for all 35 firmwares, native and WASM; JIT == interpreter
  (and per-step where single-core) for Felucca, X0X, GHOULBOX, stock, Melodee, AMB, TRIBE.
- Remaining experiments require evidence before changing defaults: browser/mobile compilation
  budgets and callback percentiles, register caching, region size/hotness, careful pairs,
  and shared scheduler/peripheral hotspots. No browser was connected during this pass, so
  actual browser/Android callback behavior and native DAW playback remain to be verified.
  Node throughput and the worklet harness do not substitute for those device checks.

## 5.20 WASM: fewer exits from compiled code (geomean +14%, X0X +57%) (2026-10-16)

Node's CPU profile (`node --cpu-prof web/bench.mjs`) put about a third of WASM time in
`run_fast` itself: the dispatcher between compiled regions. FM1_PROFILE counts showed X0X
leaving compiled code every 15 instructions (38 M times in 3 emulated s). Three causes, fixed:
- Run-time jumps (returns, popped pcs, table branches, helper jumps) always left the region.
  Each region now has a collision-free (pc >> 1) & mask table of its blocks in memory; such
  a jump looks its target up and continues inside. Regions also queue a call's return point.
- An if-else block was careful, never compiled; X0X's hot string compare starts with one. The
  region takes the then-part in tow when plain; an exit inside it (a tick after a bus access)
  hands `run` the skip for `pred_skips`, as the interpreter has it.
- Every peripheral write set next_due = 0 (a SoC service, leaving compiled code). The FM-1's
  instant ADC needs one only when IRQ 24's state changes; OberVave converts ~1 M times / s.

WASM, three interleaved rounds against 1e11413: geomean 1.137 over 35 firmwares; X0X 1.72x ->
2.70x, OberVave 1.25x -> 1.78x, CTL-1 +33%, FuMi +31%, Polly +28%, zp12 +18%, GHOULBOX +17%,
FM-1 Doom 1.18x -> 1.33x, stock +11%, SMK-37 stock 1.41x -> 1.47x; none slower beyond noise
(FM-1 SID -1%). Native (the ADC change): OberVave +21%, X0X +4%. Audio identical natively
(35) and in WASM (36 with and without input, SMK-37 stock and 14 ports), also at 120 MHz and
against the WASM interpreter; JIT == interpreter == single steps natively.

Tried and dropped (measured):
- Skipping cpu1's idle rounds: record a round (interpreted), and while its state, the words it
  read and its stores (idempotent RAM bytes, a no-op latch clear) are unchanged and no IRQ is
  due, skip the slice. X0X: 570 k of its idle slices skipped, yet only +4% WASM, ~0 native,
  stock FM-1 -2%. The SMK-37's idle loop reads TIMER5's count (it changes every slice), so
  it can never be skipped. The cost was in the probing calls' fixed overhead, not the rounds.
- Regions of 64 blocks / 768 ops: Doom +3%, Felucca +4%, but zp12 -7%, X0X / AMB -5%.
Next for Doom (slowest, 1.33x): its frame wait calls through ~6 regions per round.

## 5.21 The catalog follows the FM-1 Firmware Hub (2026-10-16)

zvenson opened the Hub to other pages (Access-Control-Allow-Origin: * on /fw/*.fwsc and
/catalog.json; packages never change under their name; the catalog updates every 3 hours).
`tools/hub_catalog.mjs` builds web/firmwares.json from it: every Hub firmware that runs here,
its package from the Hub (manifest / author URLs dropped for those), ours kept where we have it
(description, licence, installer), the rest from the Hub (new entries' licences from GitHub),
plus the entries the Hub lacks (stock, SID, April, Purple Monkey, Quest, SLOOP ALG, FM-1 Doom).
Each entry records its Hub id (`hub`); the layout stays fm1-live-readable (`{ "id": ...`).
49 entries, 47 community (42 from the Hub, 14 new: Dinghy, fm1-chord, MAKU, FM-106, Fart
Machine One, Felucca 8 Patterns, DUELL, MYCELIUM, Pigeon, CS-1, HYP-1, Tonitor, Noiz Demons,
Coral Flower). Left out: Bubba Machine (black screen). Following the Hub moved SLOOP Floyd FM
back to 2.4 (its author's site had 2.4.1); Hortator's and Melodee's Hub packages are other
builds under the same version (their audio changed); Rainbow's reference image too, though its
package is the same (the build before this gives the same new audio on the new files).
Page: `?fw=` takes the Hub's ids (salt, floyd, sloopbanks); `?embed=1` shows only the device
and its controls (inline <head> script sets data-embed before the first paint; nothing stored),
for the Hub's "Try in browser" window; each card links to its Hub card (#fw-<id>). Checked in
headless Chrome over the DevTools protocol (node + built-in WebSocket).

## 5.22 Decoder fuzzing against the vendor objdump (2026-10-19)

Issue #16 (edco): github.com/edco/ghidra-jieli, a SLEIGH description of the whole pi32v2 ISA (834
constructors; our corpus classes match 328: the rest are mostly DSP / SIMD / MAC forms clang never
emits, in vendor libraries byte-identical in every firmware). `tools/fuzz_decode.py` makes test
instructions (every corpus class's samples with each bit flipped, and N random encodings per
SLEIGH constructor with `--sleigh DIR`), has the vendor toolchain's objdump decode them (Docker,
as probe_build.sh) and the emulator (`fm1-emu --check-decode`: length, class, one step's
outcome), and reports gaps (objdump decodes, we don't), length disagreements, decodes without
semantics, junk we accept, then runs audit_decode.py over the listing. 161774 instructions:
no length disagreements. Fixed, each with the vendor's layout (`vendor_encodings` test):
- `d8..df ec` word post-increments: inc = sext11 (byte0 bit 2 the sign), route 19 had sext10:
  `dc/dd` (-1024..-513) went positive, `da/db` (+512..+1023) negative. In Felucca, DUELL, JIANT,
  ChoralRoot, Melodee, April, Bubba, fm1doom (`r11 = [r12++=-820]`, `r0 = [r1++=516]`); none on
  the 8 s bench paths (all 49 firmwares' audio byte-identical before / after).
- `r_mov_b_sp_u` (`dc e9 00 R0`): its register slot, solved from r0, r1, r2, r12, swapped bits 30
  and 31 (r4 <-> r8). `d8|d9|dc|dd e9` now decode by `sp_mem_raw` (route 37) before the table;
  `d8 e9` (h[sp+off] (u) loads and stores, bit 0) was missing.
- `5e ec` `d[R++=R]`: bits 1-2 of byte 0 are the mode (00 index, 10 pre, 11 post); bit 2 alone
  had made it a pre-increment.
- `h[R+off] = R.h` at an offset the corpora lacked (route 35, no micro-op) panicked the step
  (`unreachable!`): now Fast::Raw.
- fallbacks widened to the vendor ranges: `d0..d7 ec` (route 18), `d0..d7 ed`, `d0..d5 ee`,
  `d8..df ec` bit 17 clear (route 19).
Left: shift-immediate field 0 is `<< 32` to objdump, 0 to us (clang never emits it, the hardware
unconfirmed; the JITs would need a special case); 6-byte call targets beyond +-128 MB; the
flag-condition branches (`ifcs/ifle/... goto`) have no semantics (rare, in hand-written code).
audit_decode.py now compares the pre- / post-increment accesses too (`describe`'s `Q`, from the
same `decode_inc_mem` routes 18 / 19 run): +7610 compared in the stock app's listing, all agree;
with route 19's old sext10 put back it reports 135 disagreements in the fuzz listing.

## 5.23 The Hub's old packages go away (2026-10-20)

A user reported firmwares failing with 404: the Hub removes a firmware's old package when a
new version comes, so 18 of web/firmwares.json's 47 Hub packages (written 2026-10-16) were gone.
`fetchPackage` (web/fwsc.js) now asks the Hub's catalog.json (once) for an entry with `hub`
whose package has gone, and fetches the one listed now. The list is rebuilt from the Hub: 73
entries, 71 community, 24 new. Each was booted 8 s in fm1-live (a note on the panel and over
MIDI, then PLAY): all but five play or draw their screen. Left out (SKIP in hub_catalog.mjs):
Melodee 1.4.5 (black screen, interrupts off; 1.3.6 ran), OberVave 1.5.3, 386DX, MC-1 and Shine
On (each halts on a decode not known yet: `64 e8 5f bc`, `64 f0 00 ae`, `64 f0 00 0e`,
`64 e8 5f b9` ...).

## 5.24 The JIT on x86-64 Linux (2026-10-21)

`jit/x64.rs` compiles the same block prefixes as the AArch64 compiler, op for op, sharing
everything else in jit.rs (code cache, chaining, the helpers that keep timing exact, the RAM
guards); `jit_x64.rs` is its encoder (86 encodings checked against LLVM's assembler). System V:
rbx / rbp / r12..r15 hold Cpu, Ctx, RAM, XIP, the count and the code flags; the limit and the two
values kept across a helper call live in the frame. Floats in SSE (what Rust's f32 gives on the
host); conversions to integers saturate as Rust's `as` (cvttss2si alone does not); fmin / fmax
through the interpreter. One cfg, `native_jit` (fm1-core's build.rs), says where a JIT is built.
A new test compiles every inline micro-op alone and runs it from random registers (NaN, both
zeros, infinities, out-of-range floats) against the interpreter: both backends agree. (It found
two places where the AArch64 code differs from the interpreter, neither decodable: a float
condition on `BrI`, FPU op 4.)
Measured on an x86-64 Linux box (Core Ultra 7 155H, Docker capped at 1.5 CPUs): all 49
firmwares and the stock firmware's audio the same with the JIT and interpreted, and the same as
on Apple silicon; 2.41x the interpreter (geomean), every firmware faster than real time
(slowest Doom 1.19x, zp12 1.26x). CI runs the Linux arm64 job's checks on an x86-64 runner too.

## 5.25 The catalog a day later: 83 firmwares (2026-10-21)

A day after 5.23 the Hub had five new firmwares (Bytemania, Chipbox, Flyfish, Kawa, Mutable) and
19 new versions (SLOOP 2.8, FMini 1.8.0, Beetle 1.3.8, CTL 1.0, ...). With #33 / #34 merged,
Melodee 1.4.5, OberVave 1.5.3 and Shine On come off the skip list. Two decodes the vendor objdump
names: `64 f0 00|80 XY`, the special-register moves as a dual-issue head (`r2 = rets  #`: CTL 1.0,
386DX and MC-1 halted on it; `sreg_move_raw` took `64 e0` only), and `60 ff` bit-test branches
(`if ((r0 & 0x1) == 0) goto`: Kawa; the table's class had `61 ff` with a forward offset only;
`decode_bittest` already knew both). Of 131584 words of the two families, the only ones whose
decode changes either halted before or (two) run the same. Booted 8 s each (a note, then PLAY): all
82 packages run, no halts; the 78 that ran before give the same audio. Kawa, a tracker, is silent
under the script but draws its screen and plays. Only Bubba stays out (black screen).
The page now shows the version that runs when a package has gone and the Hub's newer one came
instead (fetchPackage returns its `version`); a failed Hub catalog is asked again rather than
turning the fallback off.

## 5.26 The JIT on Windows (2026-10-21)

The x86-64 JIT (5.24) on Windows too, with its code unchanged: the generated code's entry and
the helpers it calls are `extern "sysv64"` on x86-64 (`code_abi!`, `CodeFn` in jit.rs), so it
keeps the System V register plan on Windows as well, and Rust saves around the call what Win64
wants kept (rdi, rsi, xmm6..15). The code cache comes from VirtualAlloc (read-write-execute, as
the Linux mapping; after luginf's fork, issue #3, whose JIT took the Win64 convention into its
code generator instead). CI runs the core tests (the random-state test of every compiled op
included) and SID with the JIT against the interpreter on a Windows runner. (Issue #21 is the
browser page on Windows, not fm1-live: this gives such users a fast native player, it does not
change the page.)

## 5.27 The peripheral window by table (2026-10-22)

A profile review (four reviewers, one per area: interpreter, native JIT, SoC, browser) found
`Soc::region` the top function of FM1-DOOM and ZP-12 (about a quarter of their time): a chain of
some 30 compares, with P33 (0x13e08) near its end, which their SDK's main loop touches every
round (a watchdog kick). Now 0x10000..0x14000 is a table of 8-byte cells, the rest below
0x0100_0000 a short match (no memory region lies there), the memory regions as before; a test
compares it with the old chain for every address below 0x60000 on both boards. All 82 catalog
firmwares, the stock one and FM1 Open, JIT and interpreted: the same audio, byte for byte. JIT
geomean +6% (FM1-DOOM 1.55x -> 2.23x, Etch +37%, ZP-12 +26%, Beetle +23%, OberVave +16%),
interpreted +2.4%; in the browser +3-5% on those (its bigger costs are elsewhere).

## 5.28 Run budgets to the instruction: the JIT gives the interpreter's output (2026-10-22)

`run_fast` checked its budget per block, so a call ran up to a block past it, and the JITs were
handed `max(budget left, block length)`: where a call stopped depended on where blocks began, and
compiled code (which leaves at the end of its prefix, before a careful op) stopped elsewhere than
the interpreter. Cores interleave and interrupts are taken where calls stop, so on master 10 of 84
firmwares (Felucca, FMel, Frodhes, Ketch, Melodee, OPN1, X0X, X0XRMX, the stock firmware, FM1 Open)
played differently with the JIT than interpreted. Now the budget holds to the instruction (checked
per op; the JITs get exactly what is left, and compiled code runs a block only while it fits), so
both stop at the same one: all 84 the same, JIT and interpreted. cpu1's idle probe, which ran a
block per call (budget 1), now runs the block at pc up to where the probe began if that lies inside
it (`Cpu::block_len_to`): with calls ending mid-block, the place to come back to need not start a
block, and the probe missed it (NMSFM's core 1 ran 100 M instructions a second instead of 2.4 M).
Against master: 20 firmwares play with interrupts a few instructions earlier or later (where a
budget now ends), the same levels (peaks within 2 counts); speed geomean -1.3% JIT, -0.8%
interpreted (noise). This also unblocks changes that move block boundaries (5.27's review: the
polling-loop skip without its 4096 cap, untimed peripheral writes).

## 5.29 The browser's audio schedule, its speed readout and its clock advice (2026-10-22)

From 5.27's browser review and issue #21. Half the catalog hands out audio in 256- or 512-frame DMA
half buffers, and `fm1_render` ran the machine only when the queue was short of a quantum's 128
frames: every second or fourth quantum emulated 5.8 or 11.6 ms at once and the others nothing, so a
device needed 2-4x real time not to drop out. `fm1_ahead` (fm1-web's `Pace`) makes each quantum
also run its share of emulated time (the credit carried over, so the pace is the audio clock's)
while the queue holds less than one half buffer beyond the quantum. Time that a short queue forced
is not owed back, so the pace moves ahead of the half buffers until they come before they are
needed. Half buffers no larger than a quantum (Felucca, the stock firmware) run as before. The same
20k chunks run in the same order, so the audio is the same: all 98 packages byte for byte over the
common length. Cost: input lands up to one half buffer later (X0X mean 7.3 ms, max 11.6; FM1GM max
5.8 ms; Felucca and stock 0). In Node at 240 MHz, steady quanta over 2.9 ms: X0X 421 -> 21 (playing
a pattern 420 -> 74), FM1GM 145 -> 2, FOMAJAM 836 -> 7, CS-1 840 -> 145, Tonitor 140 -> 4; all 98:
14109 -> 3027, with the speed unchanged (geomean 2.32x). What remains on X0X and CS-1 is their own
uneven work per emulated millisecond (X0X's slow quanta cost twice its mean), not the schedule.

The worklet's budget was 20 ms of emulated time per quantum throughout. Now that holds while a
firmware boots (Tonitor boots 3.8 s silent), and once audio plays it is 1.5 quanta (with the pace
on), so a stalled firmware no longer holds the audio thread ~7 quanta' worth at a time.
`?ahead=0` turns the pace off and keeps the 20 ms. The 1.5 quanta suffice only when the pace may
run a whole half buffer ahead: with less (`?ahead=256` on X0X's 512-frame halves, or halves larger
than the default 1024), the quantum that empties the queue must also run the rest of the half
buffer, and X0X dropped every fourth quantum (458 in 6 s at 240 MHz). `fm1_render` raises the cap
by that rest (`Pace::least`); `simulate` checks no quantum is short for halves of 128-2048 frames.

The status line's "M instructions/s" was cpu0's run budget in 20k chunks. It is now the speed:
emulated time per time spent emulating, as `bench.mjs` and `fm1-live --bench` print it. The copied
report gives it per second, the idle-skipped share (`fm1_idle_ms`), the JIT's regions, failures and
compile time, and a worst quantum that leaves out boot quanta. The warning's advice to lower the
clock assumed the cost scales with it. The stock firmware idles 43/69/83% of the time at 60/120/240
MHz, idle skipping removes that, and it runs at 2.62x real time at 60 MHz and 2.39x at 240. Felucca
runs at 4.96x and 2.01x, X0X at 4.17x and 1.74x, CS-1 at 2.96x and 1.32x: about a quarter fixed
cost plus the cycles run. The warning now estimates the load at a lower clock from the measured idle
share. It offers a lower clock only where that brings the load down, and otherwise says why it
would not help. It also says to close the developer tools (with a debugger attached the wasm runs
3-4x slower). Left open: a work-based lead (running further ahead in cheap quanta) would absorb X0X's
own spikes at the cost of more latency; moving the emulation to a Worker (P-core placement on
hybrid CPUs) is 5.27's estimate, not tried.

## 5.30 Micro-ops for what still went through the executor or step; pairs that run second first (2026-10-22)

From the interpreter and JIT reviews of 5.27, the items that need no new generated code in either
native backend (the generated-code ones — returns through a pc table, compiled swapped pairs, the
RAM guard's version compared inline, predicated prefixes — follow on top):

- `call R` / `goto R` are micro-ops (`Fast::CallR`, `Fast::GotoR`; compiled code calls the
  executor for them). Blocks no longer end at them. Few are left on core 0 now (Melodee about
  60 K a second), so the gain is small; with 5.28's budgets it moves no audio.
- Routes 18 / 19, loads and stores with an increment, are the micro-ops every backend compiles
  inline: `LdPost` / `StPost`, `Ld` / `St` for a word at an offset (`inc_mem_fast`). A load into
  its own base register and the pre-increments stay raw (exec_raw's rule differs there). JIT:
  Grimm 2.8x -> 5.6x, FM-106 +70%, OberVave +34%, and a dozen catalog firmwares +80-165%.
- RAM blocks are checked by the bus's code-write count natively too (was the wasm build's only;
  the native JIT's RAM guards already relied on it): every write that can reach a block's bytes is
  counted, compiled code's inline stores included. Melodee interpreted +12%.
- `Fast::Swap`: a dual-issue pair whose second instruction reads what a register-only head writes
  (so it cannot be glued), where the head reads and writes nothing the second writes and not both
  set the flags, is one op of two instructions that runs the second first, then the head — the
  pair rule's result. `run_fast` runs it whole when nothing is in progress, both fit the budget and
  the second one's tick is not due; else as the two careful steps. Careful: compiled code still
  stops before it (the follow-up can compile it). Plain ops in an if-else then-part apply
  `finish`'s skip rule without the general careful path. FM1-DOOM +15% JIT, +10% interpreted.
- `cli` and `R = icfg`: compiled code calls a quiet helper (CPU state only: no tick sync, nothing
  due after) with the same arguments as `h_op`; icfg is read without a string match. GhoulBox +5%.

Exactness: all 82 catalog firmwares, the stock one and FM1 Open, JIT and interpreted, 8 s with
keys: byte-identical to master, and JIT == interpreter on every one. The browser build gives
master's audio on Felucca, FM1-DOOM, Melodee and the stock firmware, JIT and not. New tests: route
18 / 19 micro-ops against exec_raw's arm (20000 random encodings), swapped pairs against the pair
rule (random heads and second instructions: registers, flags, sp, rets, pc, memory), a real pair
in a block cut by the budget, and the JIT random-state test with call R, goto R, cli, sti and both
icfg moves (interrupt state compared). Speed (this Mac, loaded; one run per firmware): JIT
geomean +16%, interpreted +4%.

## 5.31 Polling loops skipped to the cycle (2026-10-22)

The rest of 5.27's SoC review, now that budgets hold to the instruction (5.28). FM1-DOOM, ZP-12,
OberVave, Etch, Beetle and fmini spend much of their time in a main loop with nothing to do: a
watchdog kick through the P33 port and a look at flags their interrupts set, round after round
until the next interrupt. Such a round leaves the machine as it found it, and until the next
timed event nothing it reads can change, so every round runs the same. `machine::probe_loop`
runs cpu0 on in the interpreter through `ProbeBus` (at a block boundary, outside interrupts,
with nothing pending, room before the next event and enough budget left). Loads must only look
(`Soc::pure_read`, region by region from `read32_slow`: no clock, no counter, no log); stores go
to RAM (logged with the word they replace; not into code words) or to the P33 port and the FM-1's
instant ADC, whose state is compared. Back at the pc it began at with the same core
(`Cpu::plain_state`), the same port and ADC, and every RAM word the round stored to as it was when
the round began, the rounds whose ticks all fall before `next_due`, and within the budget, are
added as k * L cycles: the machine as running them would leave it, at the same cycle. A probe
that finds nothing (an impure access, refused rather than made; an event; 2048 steps without a
round) undoes all it ran, core, RAM, port, ADC and clock, so the run goes on exactly as without
it. Probes run a step at a time, so they back off after one that failed and spend a credit (one
per step, earned back at 1/16 of a skipped instruction with the JIT, 1/4 interpreted, and
1/2048 a cycle): without it Ctl and Polly, whose ~640-instruction rounds alternate with real
work, ran 10% slower with the JIT. `FM1_NO_LOOPSKIP` turns it off (A/B; FM1_TRACE does too).

Two smaller ones with it. Port-pin writes (but PD0, the flash chip select) and the counters'
write-ignored words (0x10808, 0x10904) no longer force a service: nothing timed hangs on them,
and Felucca's matrix scan wrote the port half a million times a second (+9%). And cpu0 alone
runs in one stretch instead of 4096-instruction ones (a skip was cut at 4096): the block loop
stops at the release of cpu1 and says how far it got, and the rest of the 4096-chunk the
release fell in is still not run, so cpu1 starts where it did. (That forfeit is master's
accounting kept for the same output; dropping it would move every dual-core firmware's timeline
by up to 4095 instructions, once.) No probe while a release is latched (`Soc::cpu1_release`):
the block loop then breaks after every `run_fast` return, and a skip would carry cpu0 past where
it stops without one. Master latches it again on any later 0 -> 1 of 0x10008 bit 3, with cpu1
already running, and nothing takes it (cpu0 then runs one instruction a slice); no firmware
tested does that, and the latch is for a change of its own.

All 82 catalog firmwares, the stock one and FM1 Open, JIT and interpreted, --bench 8000 with
keys: the same audio as before, byte for byte, JIT = interpreter, also with FM1_NO_LOOPSKIP;
cpu1's instruction counts unchanged. Unit tests drive a key-polling loop against the same
machine without the skip, a skip one cycle late fails them. Speed, best of 3 against master on a
shared machine (JIT / interpreted): FM1-DOOM 2.21 -> 5.26x / 1.34 -> 2.94x (a 3 s run, its menu
only: ~20x), ZP-12 3.42 -> 7.68x / 1.28 -> 2.01x, OberVave 2.87 -> 4.35x / 1.28 -> 1.59x, Felucca
4.92 -> 5.34x, stock +3%, Grimm, Melodee, Jiant +4-5% (JIT); over all 84 a geomean of +12.5%
JIT, +11.6% interpreted, none below 0.98x. The dual-core firmwares (stock, Open, X0X, Jiant, ...)
skip nothing: within cpu0's 1024-instruction slices their probes find no round worth it.

## 5.32 Generated code that stays in generated code: returns through a pc table, swapped pairs, RAM guards inline, then-parts (2026-10-23)

The generated-code items of 5.27's JIT review, on top of 5.30, in both native backends (AArch64
and x86-64, op for op):

- Returns and computed jumps (rts, `call R`, `goto R`, a pop into pc, a table jump, a jump
  through the executor) look their target up in a direct-mapped table of compiled bodies
  (`Jit::itab`, 16 K entries by pc >> 1) and jump straight in, instead of always leaving for
  Rust. The body checks what a chain target checks (its whole prefix fits the call's limit, its
  RAM snapshot); after a bus access brk is checked first. `call R` / `goto R` are compiled
  inline. A pop with pc in its mask that falls through the executor now checks brk before it
  chains on. FM1-DOOM +15%, Felucca +19%.
  - A body goes in when it compiles, and again when an exit to it comes back to Rust after a
    later body on the same entry took its place (markterm's #47 fills its dynamic-jump table
    that way, `set_dyn`). Without the refill an evicted hot return target stayed out for good:
    58% of GhoulBox's entries from Rust, 35% of Felucca's. GhoulBox +7-12%, Felucca +4-6%.
  - An empty entry's tag is the pc of another entry (`(i ^ 1) << 1`), so no target matches it.
    It was all ones, and a computed jump to 0xffff_ffff (an erased-flash function pointer, a
    trashed rets) branched the host to address 0 (SIGSEGV) where the interpreter goes on.
- `Fast::Swap` compiled: the second instruction (both ticks taken), then the head, then one
  check. After the pair the careful path stops for a pending interrupt; compiled code does too
  (`h_pair`, called only when brk is set or `Ctx::irq` is not known to be clear: helpers mark it
  unknown, since only the bus changes it; asking the bus in every helper cost ZP-12 10%).
  FM1-DOOM +18%.
- RAM bodies compare the bus's code-write count (`Ctx::ver`, `RamGuard::ver`) inline and call
  `h_ram_ok` only when it changed: a code write sets brk, so the count cannot change while
  compiled code runs. Melodee +23%.
- Compiled prefixes inside an if-else then-part or a rep loop (one of the two in progress,
  nothing else), when no instruction but the last can end it: then-part ends at or past the
  prefix's end, the rep end not in it or at its end. Capped at that prefix; run_fast applies
  `finish`'s rule for the last instruction and the careful path's stop; the chain slot is
  linked before the skip. Pairs inside: no stop after a swapped pair there (`Ctx::inside`), no
  glued head that may use the bus. GhoulBox +27%.
- If-else blocks compiled with their then-part (plain ops), as the wasm JIT does: not taken, on
  at the else part; taken, the then-part and past the else part, stopping for a pending
  interrupt there; an exit inside the then-part leaves the skip for `pred_skips` (`Ctx::pred`).
  Inside another construct the ops before the if run instead (a second compiled variant).
  FM1-DOOM +21%, ZP-12 +16%.
- markterm's #46: the rest of routes 18 / 19 / 32 (loads and stores that update their base) as
  micro-ops: route 18's pre-increments as `LdPre` / `StPre`, route 32 as `LdPre` / `StPre` /
  `LdPostR` / `StPostR` (raw: a high-half store, an access of the base register itself), with
  inline lowerings in both native backends (the wasm JIT reaches them through `h_op`, as it did
  the raw route). 5.30's rule stays: a post-increment load into its own base is raw. The random
  test against exec_raw's arm now covers all three routes and requires each form to come up.
  Melodee and Felucca about +2%.

Exactness: all 82 catalog firmwares, the stock one and FM1 Open, JIT and interpreted, 8 s with
keys: byte-identical to master before 5.30, and JIT == interpreter on all 84, with every change
above; after the rebase onto 5.31, FM1-DOOM, ZP-12, Felucca, Melodee, GhoulBox and the stock one
again. x86-64: the fm1-core / fm1-soc / fm1-isa tests in an amd64 Linux container (the encoder
test with the new encodings checked against llvm-mc), and before the refill and #46 an audio
comparison under qemu-x86_64 on twelve firmwares (x86 JIT == x86 interpreter == AArch64);
`cargo check` for wasm32.
New tests: the table (rts, goto R, call R, a pop: into the body, not past the limit, nothing
there, a due tick, changed code, a jump to all ones, an evicted body listed again), compiled
swapped pairs against the pair rule, run_fast differential tests with a clock and interrupts
(swapped pairs; prefixes inside then-parts and rep loops), the inline RAM guard, if-else blocks
against the careful path, routes 18 / 19 / 32 against the raw arm; each fails with the rule it
checks removed. Speed (this Mac, shared with other benchmarks): the whole catalog, one
interleaved run each, against master before 5.30: JIT geomean +36% (5.30 and this together),
interpreted +5%. Against master with 5.31, best of 2 (JIT / interpreted): FM1-DOOM 6.19 -> 9.20x
/ 2.97 -> 2.98x, GhoulBox 3.58 -> 5.71x / 1.35 -> 1.39x, Felucca 5.52 -> 7.55x / 1.40 -> 1.42x,
Melodee 4.23 -> 5.76x / 1.43 -> 1.32x, ZP-12 8.72 -> 10.23x / 1.95 -> 2.00x, stock 6.42 -> 7.25x
/ 4.44 -> 4.49x. Melodee interpreted moves between 1.23x and 1.46x from one commit of this
branch to the next (it is at master's speed on some), a code layout effect in run_fast.

## 5.33 markterm's core 1 stretches, guest registers in host registers, and more ops compiled (2026-10-23)

Three PRs from markterm (Mark White), #48, #49 and #50, ported onto 5.32 with his authorship kept:

- #48, core 1's idle probe in stretches. While `run_cpu1` probes core 1's polling loop it makes
  one `run_fast` call per block and decides between them. With nothing pending for core 1 those
  calls now run back to back in one (`Cpu::stop_pc`). That call returns only where one of them
  ends and the loop could decide otherwise: back at the probe's pc, after anything that may have
  touched a peripheral (compiled code's helpers set `stop_io`; interpreted code counts too), or
  with the probe's or the slice's count spent. Compiled code runs on from block to block only
  through blocks compiled whole and checks those points at each body's entry. A block left
  inside (a code write) says how much of it was left (`stop_rest`). Same calls, same points.
- #49 (AArch64): guest registers kept in eight caller-saved host registers within a block.
  `ld_reg` loads at first use and `st_reg` marks the register changed; exits, chains and helper
  calls write the changed ones back, and a call reloads them after. There is a 3000-block random
  test against the interpreter.
- #50 (AArch64): register-pair loads and stores without a base update (`LdPair` / `StPair`,
  route 16), route 41's min / max as `MinMax`, `Un`, `Div` and `cli` compiled inline. A
  dual-issue pair's head of route 16 or 41 stays raw: glued to the next instruction it moved
  interrupts in Skiff and NMSFM.

Porting, beyond conflict resolution (itab for his dyn_tab, emit_op, Prefix, ind_brk flushing for
#49, the flag lists for #50's new ops), #48 needed four changes on this base (their own commit):
- calls counted in instructions, a swapped pair being two, with a stop at its second running the
  head alone, as `block_len_to` does;
- the stretch's room raised from 32 to 64 instructions (`STRETCH_ROOM`, a block's most with
  swapped pairs);
- a prefix with an if-else then-part in tow is not "whole";
- `check_pair`'s exit after a compiled swapped pair stores `stop_rest`.

The stretch's bookkeeping in run_fast's loop cost every interpreted run (Melodee -15%, Felucca
-6%, stock -4%). run_fast is now generic over STRETCH: the normal copy has none of it, and
`run_stretch` (not inlined) runs the other copy only while `stop_pc` is set.

Exactness:
- All 82 catalog firmwares, the stock one and FM1 Open, JIT and interpreted, 8 s with keys:
  byte-identical to master (db8d472), and JIT == interpreter on all 84.
- With the stretch on and off (`stop_checks`) on stock, FM1 Open and MC-1: identical.
- The fm1-core / fm1-soc / fm1-isa tests pass at each commit on AArch64 and x86-64 (amd64
  container), and `cargo check` passes for wasm32.

Speed against 5.32 (this Mac, shared, best of 3 interleaved; JIT / interpreted):

| Firmware | JIT before | JIT after | Interp. before | Interp. after |
|---|---|---|---|---|
| MC-1 | 3.11x | 3.50x | 1.08x | 1.06x |
| GhoulBox | 5.67x | 6.13x | 1.39x | 1.39x |
| OberVave | 8.80x | 9.31x | 1.72x | 1.80x |
| FM1-DOOM | 9.17x | 9.42x | 3.00x | 3.10x |
| Felucca | 7.53x | 7.72x | 1.43x | 1.42x |
| Melodee | 5.86x | 5.99x | 1.31x | 1.39x |

- Stock, FM1 Open, ZP-12, Grimm and X0X are within ±2%.
- Geomean over the 11: JIT +3.1%, interpreted +1.1%.
- The stretch alone, on and off in the same build (JIT): MC-1 +12%, stock +5%, FM1 Open +3.5%.
- The #48 measurement that showed FM1 Open +7% was against master, before 5.30-5.32; here
  FM1 Open gains nothing net against 5.32.
- On this Mac (Apple cores forward stores cheaply), #49's register cache is about neutral.
- Melodee's core 1 never starts in fm1-live's 8 s bench, so #48's Melodee gain (measured on a
  Pixel 7 Pro in Harbor) does not show here.

## 5.34 Speed pass 3: where the time goes with the JIT on, and seven exact changes (2026-10-27)

A profile (macOS `sample`, 4 s of Felucca, X0X and Melodee at 240 MHz with the JIT) split the
time four ways: generated code 30-42%, the Rust core (run_fast's loop, the careful path, cpu1's
probe) 22-40%, the JIT's helpers 5-12%, the SoC (peripheral reads and writes, interrupts)
16-36%. Temporary counters (an env-gated `perf_trace`, taken out again) then said what each
part was doing: which addresses the slow bus paths see, which ops run carefully with the JIT on
and why, which instructions still go through `exec_syntax`. The findings, each a change measured
alone (interleaved A/B, the CPU time of a 4 s bench at 240 MHz, median of 3; this Mac was shared
with a VM and other work at a load of 7-8 on 8 cores, so wall-clock factors were meaningless):

- SoC, three hot spots (Felucca -4%, AMB -2%, Melodee -1%): TIMER4's count was computed with a
  u128 division (a library call) on every read, and Melodee reads it 3 M times a second; now u64
  while the product fits, u128 past 53 minutes at 240 MHz (the same quotient). Every port write
  ran the LED accounting's 44-cell loop; Felucca writes the port 500 k times a second, nearly
  always in the same state: the time is now summed per state and charged when it changes. The
  plain SFR words lived in a HashMap written on every GPIO write; the aligned ones below 0x60000
  are an array.
- JIT: a chain exit was a slot loaded with mov64 (four instructions), a cbz and a `br`, on the
  way out of every block (3.5 guest instructions on average), for 11 host instructions of block
  overhead. On AArch64 the exit is now a `b` to its own exit code, patched by `jit::link` into
  `b body` once the target is compiled (the cache is one 64 MB mapping, within a branch's reach;
  macOS's write-protect toggle and an icache invalidate per link). Felucca -4.5%, Melodee -4%,
  X0X and GhoulBox -2%. x86-64 keeps its slot.
- JIT: 64-bit addresses (helpers, the itab, RAM guards, the op pointers for `h_op`) from a
  literal pool after the code, one `ldr` instead of four moves. Neutral on Apple silicon (within
  noise, GhoulBox -3%); kept for the smaller code.
- Pairs with a float-op head ran carefully: FOp was not in `reg_only`, so a head whose result the
  second instruction stores (`rD = fop #; [sp+k] = rD`, Melodee 225 k pairs a second, X0X 42 k)
  could not become a `Swap`, and both instructions went through `exec_fast_paired` with its three
  register-file copies. It is a register-only op; now swappable and compiled. Melodee -2.5%.
- The ISR wrappers' instructions still went through `exec_syntax`, parsing their syntax strings
  each time: `[--sp] = {psr, rets, reti}`, `{psr, rets, reti} = [sp++]`, `[--sp] = {R*2}`,
  `{R*2} = [sp++]` and `rti`, five per interrupt, 10 k interrupts a second on each of the three,
  plus `R = R + R + c` (Melodee 35 k / s), `[R+#i] &= ~R` (X0X 70 k / s) and `flushinv [R]`.
  They are micro-ops now: `PushSregs` / `PopSregs` (a list as a mask over `SREG_LIST`, the ISA's
  order), `PushRegs` / `PopRegs` (the 2-byte forms' [max(n,3) .. min(n,4)] and the 4-byte masks),
  `Rti`, `AddC`, `MemA4` with `AndNot`, `Nop`. The slow path returned to the run loop after every
  such instruction, where interrupts are taken and the SoC's "in an ISR" is updated (TIMER4's idle
  skip reads it): `PushSregs` / `PopSregs` open the bus's interrupt window as `sti` does, and
  `Rti` calls the new `Bus::service` (next_due = 0: the run loop looks before the next
  instruction, as it did). A differential test runs every encoding of each shape (thousands, the
  ISA table's masks and the carry ops' own decoder) from random state through `exec_syntax` and
  the micro-op and compares state and memory. Felucca -2.5%, X0X -2%, Melodee -0.5%.
- `R = rev8/rev16/revs(R)` (route 22, X0X 600 k / s through `h_op`) inline on AArch64, popcnt
  staying with the executor; `sti` (Felucca 700 k / s) through a helper of its own without the
  executor's dispatch. X0X -2%, Felucca -1%.

All of it together, master (8bc116b) against this branch, the CPU seconds (user + sys) of a 4 s bench
at 240 MHz with the default keys, boot included, median of three interleaved runs each
(`CPU=1 tools/bench_ab.sh`, new: on this loaded Mac the real-time factor swung 2x between runs, CPU
time a few percent). JIT: geomean 0.868 (every firmware faster, 0.77 to 0.94); interpreted 0.982.

| Firmware | JIT before | after | ratio | Interp. before | after | ratio |
|---|---:|---:|---:|---:|---:|---:|
| amb | 0.95 | 0.84 | 0.884 | 3.64 | 3.60 | 0.989 |
| choralroot | 0.67 | 0.59 | 0.881 | 3.57 | 3.56 | 0.997 |
| felucca | 0.92 | 0.81 | 0.880 | 4.29 | 4.26 | 0.993 |
| felucca-salt | 0.80 | 0.65 | 0.812 | 4.18 | 4.11 | 0.983 |
| fm106 | 1.72 | 1.34 | 0.779 | 5.55 | 5.09 | 0.917 |
| fomni | 0.91 | 0.82 | 0.901 | 3.47 | 3.46 | 0.997 |
| fumi | 0.97 | 0.86 | 0.887 | 3.89 | 3.87 | 0.995 |
| ghoulbox | 1.07 | 0.98 | 0.916 | 4.53 | 4.48 | 0.989 |
| hortator | 0.69 | 0.61 | 0.884 | 3.92 | 3.93 | 1.003 |
| jangada | 1.17 | 1.04 | 0.889 | 4.48 | 4.45 | 0.993 |
| melodee | 1.69 | 1.56 | 0.923 | 5.75 | 5.65 | 0.983 |
| purplemonkey | 1.23 | 1.15 | 0.935 | 4.13 | 4.08 | 0.988 |
| quest | 0.35 | 0.27 | 0.771 | 1.18 | 1.14 | 0.966 |
| sid | 0.41 | 0.37 | 0.902 | 1.11 | 1.07 | 0.964 |
| sloop | 0.72 | 0.61 | 0.847 | 3.47 | 3.44 | 0.991 |
| sloop-alg | 0.68 | 0.59 | 0.868 | 3.50 | 3.48 | 0.994 |
| tribe | 0.92 | 0.73 | 0.793 | 3.42 | 3.24 | 0.947 |
| x0x | 1.10 | 0.99 | 0.900 | 4.76 | 4.74 | 0.996 |

Exactness: all 18 locally available firmwares, JIT and interpreted, 4 s with keys: byte-identical
to master before this pass after each change; fm1-core's 61 tests and fm1-soc's 42 pass on
AArch64, fm1-core's 47 as an x86-64 binary under Rosetta; `cargo check` for wasm32 and x86-64.
CI's catalog check (every Hub firmware, 8 s, JIT against the interpreter and against the stored
hashes) agreed for all but 386DX, whose sound moved: its core 1 runs the carry ops (`R = R + R + c`,
`R = R - R - !c`: its 64-bit arithmetic) 400 k times a second, and with them micro-ops its blocks
no longer end at each one, so `run_cpu1`'s idle probe looks at other instruction boundaries and the
cores interleave a few instructions apart. The sound is the same (the loudness per 250 ms is), the
song after PLAY lands about 6.5 ms away; JIT and interpreter agree. tests/catalog.json carries the
new hash (`tools/catalog_check.py --update 386dx`). The review of this pass (#98) found "in an ISR"
set a loop iteration late once the handler's first instruction stayed in compiled code: set as the
interrupt is taken now.

What the counters say is left, by size (none done here):
- cpu1's idle probe: X0X's core 1 runs 2 instructions per `run_fast` call, 330 k calls a second,
  each through `run_stretch`, `block_at`, `jit::run`'s prologue and epilogue: ~8% of X0X's time,
  ~11% of Melodee's (`run_stretch` + `run_cpu1` + `jit::run<Core1Bus>` in the profile). 5.20
  tried skipping idle rounds and found the probing calls' fixed cost was the problem; a cheaper
  call (a remembered "idle at pc with these registers, nothing it reads changed" check before any
  of the machinery) is the thing to try.
- The peripheral read path: `h_read` -> `read32` -> `read32_slow` -> `region` -> `Periph::read`,
  27 ns a read, 1.5 M reads a second of TIMER4's count on Felucca, 3 M on Melodee (the firmware
  polls it): 19% of Felucca's time. A direct path for the count register from `h_read`, or a
  cheaper `next_event` (scanned on every polled read), would pay there.
- Pair heads of raw routes 21 (the 64-bit register-pair ops: Melodee 35 k / s) and 41 (qasl /
  cls / brev: X0X 43 k / s, Felucca 13 k / s) are careful, and their pairs run interpreted (two
  careful steps each): micro-ops for them, as `MinMax` was for route 41's min / max.
- Flag producers whose flags a later `+ c` reads are careful (`flags_live`): Melodee 35 k / s of
  `A4`. Compiled code could store the flags for those (`adds` and `mrs nzcv` on AArch64).
- If-else blocks interpreted carefully: X0X 80 k / s (`IfBlk careful`: blocks not at a prefix's
  end, or inside another construct).
- The register cache (#49) is flushed at every chain: guest registers live in memory between
  blocks. A fixed allocation across blocks is the large remaining item in generated code.
- Loads check RAM then XIP with two immediate pairs each; constants in the two spare callee-saved
  registers would save 2-4 instructions a load.

## 5.35 The wasm JIT's FPU inline (2026-11-01)

The float firmwares rendered below real time in the browser and clicked (Ravel 0.75x at 100 MHz)
while the native JIT ran them at 2-3x: every FPU op (`3f e5`, fm1-isa's fpu.rs) left the
generated wasm for the executor through `h_op` (269 M of Ravel's 1.93 G compiled instructions
in 20 s; LOOM-8 31 M in 12 s). Now `block` computes them with wasm's own f32 instructions, which
are the IEEE single precision the interpreter's Rust is in the same build, so the engine's own
`fadd` makes both results, NaN payloads included:

- `+ - * /`: `f32.add / sub / mul / div` on the registers reinterpreted (`f32.reinterpret_i32`,
  back with `i32.reinterpret_f32`). `+` and `*` take b first: LLVM commuted the interpreter's
  `x + y` and `x * y` for wasm, and when both operands are NaNs the engine passes the first
  one's on (the self-test below found it: 0x7fc00000 + 0xffc00000).
- `fmin` / `fmax`: not `f32.min` / `f32.max` (IEEE minimum / maximum, which propagate a NaN)
  but the sequence LLVM expands Rust's `f32::min` / `max` into for wasm: `a' = a != a ? b : a`,
  `b' = b != b ? a' : b`, `a' < b' ? a' : b'` (max: `>`). A NaN operand yields the other one;
  equal operands (the two zeros) yield b, as the interpreter's build does. (jit.rs's `fminnm` /
  `fmaxnm` are the same minNum semantics; on a tie of signed zeros they give the negative one.)
- `ftoi` / `ftou` in the four roundings: `f32.nearest` (even) / `ceil` / `floor` first, then
  `i32.trunc_sat_f32_s` / `_u` (0xfc 0 / 1), which truncates by itself (so the trunc rounding
  emits nothing before it), saturates on overflow and sends a NaN to 0, as Rust's `as i32` /
  `as u32`. `itof` / `utof`: `f32.convert_i32_s` / `_u`. `ftof`: nearest / trunc / ceil / floor.
- The multiply-accumulates 7 / 8 (`R += R * R (f)`) stay with the executor, as in jit.rs (none
  in the catalog's float firmwares: Ravel's and LOOM-8's FOp helper calls went to 0).
- `fop_selftest` (`fm1_jit_selftest`, web/jit.test.mjs; skipped without a wasm build): every
  inlined op alone in a block, on every pair of 44 edge values (NaNs with payloads, quiet and
  signalling, both zeros, denormals, infinities, the integers' bounds, halves) and random words,
  into a fresh, a source, a doubled and the second source register, must leave what `exec_fast`
  leaves, and so must `run_fast` on the instruction word (the interpreter proper, its own copy
  of the float code), in the engine at hand: the JIT's oracle test for wasm, as jit.rs's
  random-state test is for AArch64.

Ravel 0.74x -> 0.91x at 100 MHz (its quanta's p50 3.22 -> 2.46 ms), LOOM-8 2.87x -> 3.31x
(interleaved A/B against the unchanged build, 20 s); PT/FM 1.32x and Felucca 2.37x unchanged:
neither uses the FPU (PT/FM's helpers are `LdPre`, `StPre` and route 21's pair ops; Orange
Cat, DUELL and FIZZ have no FPU ops either). JIT and interpreter audio byte-identical for
Ravel, PT/FM, Felucca, Orange Cat, DUELL, LOOM-8 and FIZZ; the native catalog check unchanged
(56 ok, 39 rebuilt). Bubba Machine (282 M FPU ops in 20 s) gives the same JIT audio as before,
byte for byte, but its JIT and interpreter audio differ from 6.5 s on, with and without this
change: that came with the cores' lock handover (d70ed00; at 52e1cb7 they agree), not with the
FPU; its JIT also compiles 7500 regions in 20 s (RAM code dropped and rebuilt), 0.46x where the
interpreter runs 1.15x.

## 5.36 A countdown loop of one instruction runs its iterations in one go (2026-11-01)

fm1-dock ran at 0.76x real time natively and 0.6x in the browser, every other firmware's speed
notwithstanding, and its profile was unlike theirs: 76% of the browser's time in core 1's machinery
(`run_stretch`, `run_cpu1`, `Jit::lookup`), 283 M `run_fast` calls for 297 M of core 1's instructions.
Sampling both cores' pcs (fm1-live's `FM1_BENCH_PCS` samples core 0; a local patch added core 1)
put half of all samples on one RAM address: `if (--r0 != 0) goto self` after `r0 = 256`, a software
delay in core 1's polling loop. Each iteration is a block of one instruction that branches to its
own start, and in a runner's stretch that start is the stop pc, so compiled code left after every
iteration, on AArch64 as in wasm: one call per instruction, 60 M a second. The idle probe never
matched, the counter differing at every visit.

`run_fast` now runs such a block's iterations in one go, before the JIT or the interpreter sees it:
a block of one `DecBr` with `!=` whose target is its own pc, entered at a call's start, runs as many
iterations as fit the budget and the ticks before the next event (`tick_room`, as compiled code pays
them: `add_ticks` for all but the first instruction of a call), the register down by one per
iteration, the pc past the loop when the count ran out. Nothing else observes the iterations (one
register, no bus), so the state after any number of instructions is the interpreter's; a stretch
ends its call there and returns to the runner, as after a block run interpreted. In a stretch the
iterations stop `STRETCH_ROOM` short of the budget, where compiled code's check at a block's entry
would have stopped them (a review found the first version running up to 64 further than the
compiled path on a count longer than the stretch; fm1-dock's 256 never is). One shape only: a
two-instruction delay (`r -= 1; if r != 0 goto`) or another condition still pays a call per lap.

fm1-dock: 0.76x -> 6.05x native, 0.59x -> 2.98x in the browser (node), no audio quantum over its
2.9 ms where 1680 of 1723 were. Native CPU time unchanged to three digits on Felucca, X0X, Melodee
and 386DX. Exactness: fm1-core's 66 and fm1-soc's 70 tests; the native catalog check, 94 ok, fm1-dock
itself "rebuilt" (its 0.7 package is not the one hashed: JIT = interpreter, and master's audio over
8 s with keys byte for byte; tests/catalog.json carries the new package and audio hashes); the wasm
build's audio identical to master's on all 95.

## 5.37 The wasm JIT goes from region to region without a round of run_fast's loop (2026-11-01)

Counters on `jit_wasm::run`'s exits (a temporary build) said what ended a region run on Melodee:
of 41 M in 5 s, half were edges out of the region (a jump or fall-through to a pc in another
compiled region), each a round of `run_fast`'s loop: the block table, `Jit::lookup`, the region's
RAM check, the call's prologue. `Jit` keeps a direct-mapped cache by pc of what `lookup` found
(4096 entries; cleared with a dropped region and on `reset`), and after a region run that is not
a stretch's call, `run_fast` goes straight to the region at the new pc while the budget allows and
nothing careful is pending: `block_at` is pure, and what it checks of a RAM block `wasm_region_ok`
checks of all the region's, so the round of the loop would have decided the same. A region that
does not fit (k = 0) or was dropped goes round the loop as before, and nothing is tried once the
next tick is due. An empty entry is one without a function (table index 0 is never a region's: a
review found the first version's all-zero entry matching a jump to pc 0, a trap where `step`
reports the fetch); a dropped region takes only its own entries out of the cache.

On top of 5.35 and 5.36 (the wasm build of each, interleaved in node, the median of 3): Felucca
+7.8%, X0X +6.1%, 386DX +3.5%, Melodee +2.3%; byte-identical audio on all 95 catalog firmwares.
Also tried in this pass, with the FPU, the six ops and the stretches that 5.35 brought: guest
registers in wasm locals within a block (+1-5%, within what the rest moves; 5.35's author found
the same and dropped it), a helper of its own for `sti` (neutral), SRAM2 as a third inline range
(no gain, 3-6% code-size cost where it is unused). None kept.

## 6. Phase 5 — Boot & integration

- Boot stock firmware to a stable idle/main-loop state.
- Boot a known custom firmware; diff behavior against hardware (front panel
  state, UART output, displayed content).
- GDB stub over TCP (`crates/fm1-gdbstub`) so the existing toolchain's debugger works
  against the emulator — huge payoff for both using and validating the emu.

## 7. Phase 6 — Timing & validation

- Cycle counting per instruction (from Phase 1.3 measurements) — start
  instruction-accurate, refine toward cycle-accurate only where it matters
  (timer-driven code, DSP/audio loops).
- Long-running differential tests: feed identical inputs (UART scripts, panel
  events) to hardware and emulator, compare observable outputs.
- Performance pass only after correctness: decode caching / threaded dispatch
  if needed; keep the simple interpreter as the reference.

---

## Repo layout (target)

```
fm-1/
├── PLAN.md
├── docs/
│   ├── recon.md
│   ├── isa-notes.md
│   └── peripherals/
├── firmware/            # stock + custom images (gitignored)
├── isa/
│   └── fm1.yaml         # single source of truth for the ISA
├── probes/              # ISA-discovery probe sources + expected bytes
├── tools/               # probe runner, assembler, ghidra/ (Python)
└── crates/              # Rust workspace
    ├── fm1-isa/         # codegen'd decoder/disassembler from fm1.yaml
    ├── fm1-core/        # CPU interpreter + bus trait
    ├── fm1-soc/         # memory map + peripherals
    ├── fm1-gdbstub/     # GDB remote protocol
    └── fm1-emu/         # binary: runner + debugger CLI
```

## Tech choices

- **Emulator language: Rust** (decided). One Cargo workspace:
  - `crates/fm1-isa` — generated decoder/disassembler from `isa/fm1.yaml`
  - `crates/fm1-core` — CPU interpreter + bus trait
  - `crates/fm1-soc` — memory map + peripherals
  - `crates/fm1-emu` — binary: runner, debugger CLI, GDB stub
  - `crates/fm1-gdbstub` — GDB remote protocol integration
- Bus as a Rust trait (`read`/`write` by width); peripherals implement it,
  keeping the core SoC-agnostic and unit-testable.
- Rust also keeps the door open to `wasm32-unknown-unknown` later, if the
  emulator should ever run in the browser (WebAudio worklet).
- Python for tooling/probing scripts.

## Open questions

1. ~~What is the device / SoC / toolchain?~~ Answered in Phase 0 (see
   `docs/recon.md`): M-VAVE FM-1, JieLi AC791N/WL82, pi32v2, AC79 AIoT SDK.
2. ~~How complete is ghidra-jieli's pi32v2 SLEIGH?~~ "Very early stage";
   vendor objdump is the authority for encodings, and the vendor assembler
   is the oracle for operand fields (2.2). SLEIGH only as a semantics
   cross-check.
3. AC791N peripheral set: how much is documented in SDK headers vs. needs
   MMIO-log reverse engineering? (Dual-core: what runs on core 2?)
4. ~~Do we have a physical FM-1?~~ Ordered, not yet arrived (2026-10-08).
   Phase 1.3 hardware probing deferred until delivery.
5. Target accuracy: "boots and behaves right" vs. cycle-accurate audio/DSP?
   (Suggest: behavior-first, cycle counts only where the audio path needs it.)
6. Is there a real mask ROM in the boot path at all? Every "ROM" target
   seen so far is RAM-resident `.data` code. The SPL (`uboot.boot`) is the
   only code before the app; if the app never calls into
   `0xFFC0xxxx`-as-ROM, no dump is needed.
