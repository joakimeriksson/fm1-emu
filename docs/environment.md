# The `FM1_*` environment variables

The emulator's switches for testing and debugging are environment variables: how the machine
runs, what it logs, and what `fm1-live`'s headless runs (`--bench`, `--shot`) play and write.
`fm1-live --help` and `fm1-emu --help` describe their own; this is the whole list, with which
program honours which.

A variable is on when it is set, to anything (`FM1_STRICT=1`, `FM1_NO_JIT=1`), unless a value is
shown. Some are read by the programs themselves, the others by the libraries under them
(`fm1-soc`, `fm1-core`), and so by every native program built on those: `fm1-live`, `fm1-emu`
and the plugin, which gets its environment from the host that loads it. The browser build has no
environment: the page's switches are its URL parameters (the README lists them).

**†** marks a variable that the usage text of the program it is listed for does not mention.

## How the machine runs

| Variable | Honoured by | What it does |
|---|---|---|
| `FM1_STRICT` | fm1-live | Runs as an FM-1 is: a misaligned 16- or 32-bit RAM access faults, each instruction takes its measured cycles (both cores, the flash cache's misses), and the LCD is timed. The interpreter only, no JIT and no loop skip: slower. In fm1-emu and the plugin it turns on the timed LCD alone |
| `FM1_STRICT_LOG` | fm1-live, with `FM1_STRICT` | Lists the misaligned accesses on stderr |
| `FM1_LCD_TIMED` | fm1-live, fm1-emu, the plugin | The LCD's transfers take their time at the SPI clock, as on an FM-1 (by default they finish at once) |
| `FM1_LCD_INSTANT` | fm1-live, fm1-emu, the plugin | Keeps the LCD instant, also under `FM1_STRICT` or `FM1_LCD_TIMED` |
| `FM1_NO_JIT` | fm1-live | The interpreter instead of the JIT (the same audio, slower) |
| `FM1_NO_BLOCKS` | fm1-live | The reference loop: cpu0 an instruction at a time, without the second core |
| `FM1_NO_LOOPSKIP` | fm1-live, the plugin | Polling loops run round by round instead of being skipped (the same audio, slower). fm1-emu steps an instruction at a time and skips none |
| `FM1_NO_RAM_BLOCKS` | fm1-live, fm1-emu, the plugin | Code in RAM runs a step at a time (the reference) |
| `FM1_SUPERVISOR_START` | fm1-emu, fm1-live **†** | cpu0 starts in supervisor mode, with the one stack, instead of the user mode the loader hands it over in |
| `FM1_FLASH_CUT=N` | fm1-live, fm1-emu, the plugin | Cuts the power half way through the Nth flash erase or page program (with `fm1-live --flash FILE`, the file keeps what is left) |
| `FM1_USB_SIE_NS=N` | fm1-live `--usb` | Each access to the USB controller's registers takes N ns (1000 by default) |

## Logs

On stderr, except where said.

| Variable | Honoured by | What it logs |
|---|---|---|
| `FM1_TRACE` | fm1-live, fm1-emu, the plugin | Every instruction as it runs: its number, core, pc, class and registers. Turns the loop skip off |
| `FM1_TRACE_RANGE=FROM:TO` | fm1-emu | Every instruction from step FROM up to TO (on stdout, as the report) |
| `FM1_IRQ_TRACE` | fm1-emu | Each interrupt taken: its core, source, priority and handler (on stdout) |
| `FM1_DUAL_TRACE` | fm1-live, the plugin; fm1-emu for the resets | The second core's release (when, and its entry) and each chip reset |
| `FM1_RESET_TRACE` | fm1-live **†**, fm1-emu **†**, the plugin | Each chip reset: when, and whether the watchdog or the firmware (PR_PWR) asked for it |
| `FM1_FLASH_TRACE` | fm1-live, fm1-emu, the plugin | Each command to the flash chip: the command, its address, how many bytes |
| `FM1_LCD_TRACE` | fm1-live, fm1-emu, the plugin | The LCD's commands and the windows it is written in |
| `FM1_AUDIO_TRACE` | fm1-live, fm1-emu, the plugin | Each audio half buffer the I2S finished: the time and the cycle |
| `FM1_GPIO_TRACE` | fm1-emu | Counts the writes to each GPIO register; the report lists them (on stdout) |
| `FM1_USB_TRACE` | fm1-live `--usb` | The USB bus's transactions |
| `FM1_WATCH_LEDPUT=MS` | fm1-live **†** | A debugging aid for one build of Felucca: from MS ms on, the calls of the function at 0x0201AD60 (its `led_put`) that turn an LED on, 60 at most (on stdout). Runs the reference loop, as `FM1_NO_BLOCKS` |

## fm1-live's headless runs

`--bench MS` runs as fast as it goes and reports the speed; `--shot MS` runs and writes the panel
to `work/panel.ppm`.

| Variable | Honoured by | What it does |
|---|---|---|
| `FM1_BENCH_KEYS=SCRIPT` | `--bench`, `--shot` | The input script, times in ms: `T:PLAY`, `T:PLAY^` (release), `T:C4@100` (velocity), `T:e0:+3` (an encoder's clicks), `T:pot:512`, `T:mHEX` (MIDI bytes into the DIN jack); `@FILE` reads it from a file. In place of the default keys |
| `FM1_BENCH_NOKEYS` | `--bench` | No keys at all (by default a chord is pressed from 0.8 s) |
| `FM1_WARMUP_MS=N` | `--bench` | Where the steady speed is measured from (1000) |
| `FM1_BENCH_THREAD` | `--bench` | Runs the benchmark on a thread of its own, at the emulation thread's priority |
| `FM1_BENCH_JSON=FILE` | `--bench` | The speed and the audio halves the firmware did not refill, as JSON |
| `FM1_BENCH_WAV=FILE` | `--bench` | The audio (44.1 kHz stereo, 16-bit little endian, no header) |
| `FM1_BENCH_PANEL=FILE` | `--bench` | The panel at the end, as a PPM |
| `FM1_BENCH_FRAMES=DIR[,MS]` | `--bench` | A film: the panel every MS ms (40) as `DIR/000000.ppm`…, and `DIR/audio.raw` from the boot |
| `FM1_BENCH_TRACE` | `--bench` | A checkpoint (cycle, pc, registers) on stderr every 100 000 instructions, to diff two runs |
| `FM1_BENCH_PCS` | `--bench` | Samples cpu0's pc over the second half of the run and prints the 25 most frequent |
| `FM1_BENCH_ROUTES` | `--bench` | The JIT's profile: what compiled code left to the slow path, by route and by instruction |
| `FM1_DUMPRAM=FILE` | `--bench`, `--shot` | The 1 MiB of RAM as the run left it |
| `FM1_KEY_AT=MS`, `FM1_KEY_UP=MS` | `--shot` | When the default chord goes down (800) and, if given, up |
| `FM1_NO_PLAY` | `--shot` | Leaves out the default press of PLAY at 1.2 s |
| `FM1_NO_SKIP` | `--shot` | No idle skipping: a firmware waiting on a timer is run through the wait |
| `FM1_SKIP_FROM=MS` | `--shot` | Idle skipping only from MS ms on |
| `FM1_DUMP=ADDR` | `--shot` | Prints four words of memory at ADDR (hex) at the end |
| `FM1_TPROF=PATH,FROM,TO` | `--shot` | Both cores' pcs weighted by time and the flash cache's misses by line, from FROM to TO ms, into PATH |
| `FM1_ISA_HIST=FILE` **†** | `--shot` | The instructions executed, counted by ISA form, as JSON into FILE (with `FM1_NO_BLOCKS` every step is counted) |
| `FM1_SKIP_LOG` **†** | `--shot`, with `FM1_NO_BLOCKS` | Prints the 12 pcs where idle skips happened most |
| `FM1_USB_SEND='MS:HEX;…'` | `--shot --usb` | MIDI messages over USB at those times, and what comes back printed; `@PATH` reads them from a file |
| `FM1_USB_WAV=FILE` | `--bench`, `--shot`, with `--usb` | Records the device's USB audio as a computer would (16-bit little endian, no header) |
| `FM1_USB_RATE=HZ` | with `FM1_USB_WAV` | Asks the device for that rate if it lists it (44100) |

## fm1-emu's report

| Variable | What it does |
|---|---|
| `FM1_BREAK_PCS=ADDR,ADDR` | More breakpoints, reported as `--break-pc`'s (hex) |
| `FM1_BREAK_MAX=N` | Stops reporting breakpoints after N hits (8) |
| `FM1_HIST_TOP=N` | The lines of the instruction-class histogram (12) |
| `FM1_LIST_END=ADDR:LEN,…` | Lists memory as it is after the run, disassembled (hex; RAM code too) |
| `FM1_FWSC_XIP=FILE` | With `--fwsc`: saves the package's XIP window (the app area unscrambled), for a disassembler |
| `FM1_FWSC_DIR` | With `--fwsc`: prints the package's directory |

## Tests and the build

| Variable | Where | What it does |
|---|---|---|
| `FM1_TEST_FW=ID` | the plugin's `tests/engine.rs` | Another firmware from `reference/firmwares` for the test that runs one with no audio taken (felucca) |
| `FM1_TEST_SAVE_INPUT=FILE` | the plugin's `tests/portable.rs` | A `.fm1save` (or a `.fwsc`) to run its tests on, in place of Felucca |
| `FM1_TEST_SAVE_OUTPUT=FILE` | the plugin's `tests/portable.rs` | Where the round-trip test writes the save it made |
| `FM1_GIT`, `FM1_GIT_DATE` | `crates/fm1-emu/build.rs` | Not read at run time: the build script sets them to the commit a binary is built from, for `--version` and the halt report |
