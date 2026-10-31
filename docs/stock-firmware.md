# M-VAVE's stock firmware, and the second core

The emulator runs M-VAVE's own firmware (V13, V15 and the current V16 tested) straight from its
update package, with both cores:

    fm1-live --fwsc FM-1.fwsc [--syx bank.syx]
    fm1-emu  --fwsc FM-1.fwsc --ms 5000 --keys "2500-4000:n7" --wav out.wav
    web page: "Open .fwsc…" (M-VAVE's server sends no CORS header, so the page cannot fetch it)

The package is not in the repository and the page hosts nothing: users bring their own copy.

## Booting from the package (`fm1_soc::fwsc`, `stock_from_package`)

The stock app needs more than its `app.bin`:

- **XIP** shows the app area from flash `0x4000` (directory heads at `0x02000000`, the app at
  `0x02000120`), unscrambled. Past the image the window continues into the flash chip
  (`Soc::xip_flash`, set only for package boots): the patch bank is read through it.
- **The flash chip** holds the image as stored, grown to 1 MiB.
- **r0** points at the SPL's boot parameters (`write_boot_info`): the NOR file system finds the
  flash through them.
- **The USR area** (flash `0xEA000`–`0xFC000`) holds the patches: 128 DX7 VMEM voices at
  `+0x4000`, 128 per-slot settings of 59 bytes at `+0x10000`. Updates never write it; real units
  carry the factory bank (Yamaha's ROM1A) there. A blank area gets the emulator's own small bank
  (`EMU E.PNO`, `EMU BASS`, … then `INIT VOICE`) with zeroed settings; erased settings (0xFF) leave
  only one voice sounding. `--syx` loads a DX7 32-voice bank into slots 1-32.
  The SMK-37 Pro's firmware (and the mods built on it) keeps the same synth, with its USR area at
  `0xF4000`–`0xFE000` laid out its own way (`fwsc::UsrLayout::SMK37`): 4 banks of 32 voices from
  `+0`, a 163-byte record per patch from `+0x4000`, a 128-byte block at `+0x9180` and its 9-byte
  state at `+0x9200`, which it writes itself. Left erased, it loads patch 0 as it is and plays
  nothing (2026-10-31: three SMK-37 Pro mods silent in the bench); it gets the same bank. Its
  keys play the synth only with **PATCH** on (the button's LED): a bench script presses PATCH first.

Every community package boots the same way with byte-identical audio, so the page boots all
firmwares from their whole package.

## What the stock firmware needed

| Missing piece | Symptom | Now |
|---|---|---|
| SPI2 (`0x11E00`) + IRQ 37 | keys ignored | the matrix columns are shifted into the 74HC595s by SPI2 DMA, one column per interrupt, at the SPI's own pace |
| `lockset` / `lockclr` | a task queued twice on a wait list, then a hang | a real lock between the cores (`Bus::hw_lock`); held by the other core, the instruction retries |
| latch clear word `0x1EEF1A4` | a pending context switch lost, a task ran on after blocking | reads 0 (the set word reads the pending bits) |
| CRC-16 unit (`0x13500`) | the USR area did not open: every directory entry failed its CRC | `[+4]` register, writes to `[+0]` feed bytes (CCITT, MSB first) |
| `idle` | 0.44x real time | waits for an interrupt: with `skip_idle`, time jumps to the next event |
| random number generator (`0x13B00`) | ~8 s after boot the UI froze and keys were ignored | xorshift from a fixed seed; a routine in the audio driver parks its task when a random word equals a fixed descrambled one, which a constant 0 always did |
| two stacks | interrupts clobbered task stacks or tasks never switched | see below |
| the protected image decrypted to its end (V16) | no screen, keys ignored: the synth engine's start returned early | the package is decrypted up to its `PRCT` entry's end, not the app area's: right after the area is the `EXT_RESERVED` directory (`USER` at flash 0xED000, 0x12000), found by walking from the area's head by its size; without it `mnt/sdfile/EXT_RESERVED/user` does not open and neither the engine nor the UI task starts |
| FPU roundings, `rev16` (V16) | halts on `3f e5 0f 33`, then `70 f0 40 33` | `ftoi` / `ftou` / `ftof` with (even), (trunc), (ceil), (floor); `rev16`, `popcnt` (see isa-notes) |
| SPI1's interrupt after the transfer (V16) | the UI froze under an interrupt storm | V16's LCD driver enables the interrupt, starts the DMA and then marks it running: an interrupt at the start found the handler idle, which returned without clearing it. It now comes after the bytes' time on the wire, as on the SMK-37 (the done bit still reads set at once). V15 plays the same audio; it idles less (its flush waits as on hardware) |
| SPI1's interrupt enable clears (V16) | ~0.1x real time from a held key or a swept knob on: the UI froze under an interrupt storm | CON kept every bit ever written, so the LCD driver could not turn IRQ 16 off (bit 13) before a transfer's interrupt was due; that interrupt found the handler idle, which returned without clearing it. CON now holds the last write (not bits 14 and 15), and bit 13 off drops IRQ 16, raised or still to come |

## The stack model (`Cpu::user`)

`sp` is the stack in use; `usp` and `ssp` are registers of their own. In user mode an interrupt
saves `sp` in `usp` and continues on `ssp`; the `rti` that returns to user mode keeps `sp` in `ssp`
and continues on `usp`. In supervisor mode an interrupt stays on its stack. The SPL hands cpu0 over
in user mode (every crt0 sets `sp`, then a different `ssp`); cpu1 starts in supervisor mode (the
stock stub leaves it with `usp = ..; sp = ..; reti = ..; rti`). The stock RTOS's context switch
relies on it (`sp = usp`, save the frame, load the next task, `usp = sp`, `sp = ssp`, `rti`).
The old shortcut that made `ssp = #imm` also move `sp` gave single-stack firmwares one stack by
accident; with the old "switch at task level" rule it put their ISRs on top of the task stack.

## The second core (`fm1_soc::machine`)

cpu1 starts when cpu0 sets `0x10008` bit 3 (latched at the write: X0X and Melodee hold the bit
only briefly), at the word in `0x01C7FFF8`. It has no clock of its own: cpu0 runs in slices of
1024 instructions, then cpu1 may run as many instructions as cycles passed (idle skips included),
through `Core1Bus`, whose tick budget stops interpreted and compiled code alike. It stops early
when it is back at the same pc with the same registers (a job loop with nothing to do). A core 1
that runs `idle` sleeps until an interrupt from its own bank (`0x1EEF300`) is pending, as the
chip's does (before, `idle` did nothing on core 1, and a core that idles and feeds the watchdog
each round, Bubba Machine's, cost as much as a busy one). Compiled
code is capped to a slice (`Periph::room_cap`): unbounded, a JIT loop waiting on cpu1 ran for up
to 2^30 instructions before cpu1 ran at all. `fm1-live` and the browser build share this code;
`fm1-emu` interleaves the cores instruction by instruction.

Users: the stock firmware (synth rendering on cpu1), X0X, Melodee 1.0.1, ChoralRoot. With two
cores the JIT and the interpreter interleave them at slightly different points, so their audio is
no longer byte-identical for those firmwares (each is deterministic); single-core firmwares are.

### A flaky core 1 (`--flaky-core1`, `fm1_soc::flaky`)

On some FM-1s core 1 reads words that are not in RAM: Jangada measured 0x00200000 where core 0
reads 0, idle now and then and more often under load (fm1-x0x#10; Felucca's and X0X's
`fm1_cpu1.h` describe it and the check word they guard each job with). Firmwares that use core 1
fall back to one core when they see it. `--flaky-core1 RATE [--seed N]` (fm1-emu and fm1-live)
makes the emulated core 1 such a unit, so that fallback can be tried on any machine:

- each load core 1 makes from RAM returns its value with bit 21 (0x00200000) flipped, with
  probability RATE (`1e-6`, `0.001`, …), from a generator seeded with N (default 1): the same
  seed, the same misreads (fm1-emu runs are deterministic; fm1-live is paced by the wall clock,
  so its runs differ anyway). A byte or halfword load sees the flip only when the lane it
  returns holds bit 21 (a byte at offset 2; a halfword at offset 2 or 3, which reads the upper
  half), and only then counts as a misread. `--seed` without `--flaky-core1` is an error.
- Not touched: core 0, instruction fetches (`Bus::fetch32`), flash and peripherals. A write
  stores what core 1 gives it, so a read-modify-write on core 1 (a bit set or clear in memory,
  `testset`) that misread writes the wrong value back, and core 0 sees it: a corrupted read that
  is written back plausibly is stored on the hardware too.
- "More often under load" is not modelled on its own: the rate is per load, so a core 1 that
  reads more misreads more.
- With the option, core 1 runs interpreted (`Core1Bus::jit_mem` is None: compiled code reads RAM
  without the bus), so it is slower. Without it the output is the same as before; core 1's
  interpreted RAM loads only pay a check that the option is off.

fm1-emu prints `flaky core 1 (seed N): M misreads in L RAM loads` at the end of a run.

Checked with Felucca 1.6.1, MENU > SYSTEM > 2ND CORE ON (its settings sector, flash 0xFC000, in
`--raw-flash`) and notes on four tracks so it hands core 1 parts: with `--flaky-core1 1e-5
--seed 7` it shows `2ND CORE MISREAD: OFF` as the notes start and plays on with one core; without
the option its audio and screens are byte-identical to before.

## The watchdog and chip resets (`fm1_soc::p33`, `Soc::chip_reset`)

The P33 port's watchdog runs as on the chip (#65), so a firmware can test that a hang recovers:
Felucca's `fm1_sys.h` arms it (`fm1_wdt_arm`), feeds it (`fm1_wdt_feed`) and reboots through it
(`fm1_reboot`).

- **`WDT_CON` (0x80):** bit 4 runs it, bits 0..3 set the period (1 ms doubling to 512 ms, then
  1 s doubling to 32 s), and a write of bit 6 feeds it (that bit reads 0). Not fed within its
  period, it resets the chip.
- **`PR_PWR` (0xA0):** bit 4 resets the chip.
- **After a reset:**
  - cpu0 starts again where the loader started it, and core 1 is held until it is released
    again.
  - The chip's registers go back to how the loader left them, and the loader's boot parameters
    are written again.
  - `RST_SRC` (P33 0x12) holds the cause (bit 2 the watchdog, bit 6 `PR_PWR`), and the watchdog
    is stopped.
  - SPI0 is off, and a flash command under way (chip select low) is abandoned: a page program's
    bytes are not programmed.
  - What isn't the chip's stays: RAM (the mailboxes firmwares leave themselves, harbor's
    hand-over word), the flash, the screen, the audio played so far, the keys, the script and a
    USB cable (the host enumerates the device again).
- **Both cores:** the reset comes while core 1 holds core 0 (`C0_CON` bit 2) too, within one
  of cpu0's slices of the deadline. Core 1 stops at its `PR_PWR` write: it stores nothing after
  it. Every runner resets: the block loop, fm1-live's reference loop (`FM1_NO_BLOCKS`) at the
  same cycles, and fm1-emu.
- **Polling loops:** a loop that feeds the watchdog each round is still skipped, to the same
  cycle.
- **The boot state** is cpu0's before its first instruction (`Soc::take_boot`, which the runners
  call). A runner that let cpu0 run before calling it gets no reset at all, and a warning.
- **Not modelled:**
  - the watchdog's exception mode (`VLD_KEEP` bit 6), which resets here too;
  - `RST_SRC`'s power-on bit, which reads 0 at power-on as before, so firmwares boot as they did;
  - the core reset through `PWR_CON` (0x10000 bit 4), which the update path uses;
  - a reboot into a new image: the boot parameters and cpu0's entry are the first boot's, not
    read again from the flash as the loader would (a firmware that rewrites its app area and
    reboots starts the old entry with the old header);
  - USB at the reset: device-to-host messages the host has not taken yet are dropped (as the
    device's own buffers are), and the host sees no detach before the new enumeration.
- **Keeping it up to date:** `Periph::chip_reset` names the fields that survive a reset (the
  board's and the host's: keys, script, audio, clock); a field added to `Periph` is reset unless
  it is added there.
- `FM1_RESET_TRACE` prints each reset.

## Verifying

- Felucca: `fm1-live --bench 6000 --mhz 240` with `FM1_BENCH_WAV`, byte-identical to before.
- Every community firmware: bench audio against a build of the last commit (a worktree), plus a
  package boot (`--fwsc reference/firmwares/<id>.fwsc`) against the image boot.
- Stock: `fm1-emu --fwsc … --keys` with `--snap` (the LCD shows `001 EMU E.PNO`, the scope the
  wave) and the WAV's spectrum (the chord's three notes).
- Debug aids added on the way: `FM1_BREAK_PCS`, `FM1_TRACE_RANGE=a:b` (fm1-emu),
  `FM1_FWSC_DIR` (print the package directory), `FM1_FLASH_TRACE`, `FM1_AUDIO_TRACE`,
  `FM1_DUAL_TRACE`, `FM1_BENCH_TRACE` (checkpoints to diff JIT against interpreter), `FM1_RESET_TRACE`.
