// FM1_FWSC=package.fwsc node web/bench.mjs [mhz=240] [seconds=6] [out.raw]
// FM1_BENCH_KEYS uses the native timestamped key/encoder/MIDI script syntax.
// FM1_BENCH_NOKEYS=1 disables the default chord; FM1_WARMUP_MS=1000 separates warm-up.
// FM1_WASM selects an A/B build. FM1_BENCH_JSON writes machine-readable results.
// FM1_LCD_TIMED=1: the LCD's transfers take their time, as on an FM-1 (fm1_lcd_timed; default at once).
// The page's audio schedule (web/worklet.js): FM1_AHEAD=frames run ahead (fm1_ahead; default as
// the page, 0 off), FM1_CAP=0 keeps the boot budget of 20 ms of emulated time per quantum after
// audio starts (the page caps it to 1.5 quanta when it runs ahead). "over" counts steady quanta that took longer
// than the 2.9 ms they stand for: at 1x those are where a device of this speed drops out.
import { readFileSync, writeFileSync } from "node:fs";
import { jitImports } from "./jit.js";
const mhz = +(process.argv[2] || 240), secs = +(process.argv[3] || 6);
const warmup = +(process.env.FM1_WARMUP_MS || 1000);
if (![mhz, secs].every(v => Number.isFinite(v) && v > 0) || !Number.isFinite(warmup) || warmup < 0 || warmup >= secs * 1000) {
  throw new Error("positive MHz/seconds required; warm-up must be within the run");
}
const wasm = readFileSync(process.env.FM1_WASM || new URL("../target/wasm/wasm32-unknown-unknown/release/fm1_web.wasm", import.meta.url));
let x;
const stats = { regions: 0, failed: 0, compileMs: 0, compileMaxMs: 0, bytes: 0 };
const { instance } = await WebAssembly.instantiate(wasm, { env: jitImports(() => x, stats) });
x = instance.exports;
if (!process.env.FM1_NO_JIT) x.fm1_jit(1, +(process.env.FM1_JIT_PER || 16));
if (process.env.FM1_LCD_TIMED) {
  if (!x.fm1_lcd_timed) throw new Error("this build has no fm1_lcd_timed");
  x.fm1_lcd_timed(1);
}
const ahead = +(process.env.FM1_AHEAD ?? 1024);
if (x.fm1_ahead) x.fm1_ahead(ahead);
else if (ahead) console.error("this build has no fm1_ahead: quanta run only for their own frames");
// as the worklet: 20 ms of emulated time per quantum while booting, 1.5 quanta once audio plays
const bootBudget = Math.ceil(mhz * 1e6 * 0.02), playBudget = Math.ceil(mhz * 1e6 * 1.5 * 128 / 44100);
const cap = process.env.FM1_CAP !== "0" && !!x.fm1_ahead && ahead > 0;
let budget = bootBudget;
const fw = readFileSync(process.env.FM1_FWSC || process.env.FM1_FW || new URL("../work/felucca_xip.bin", import.meta.url));
const p = x.fm1_alloc(fw.length);
new Uint8Array(x.memory.buffer, p, fw.length).set(fw);
if (process.env.FM1_FWSC) {
  if (x.fm1_boot_fwsc(p, fw.length, 0, 0, mhz) !== 0) throw new Error("firmware boot failed");
} else x.fm1_boot(p, fw.length, 0x02000120, mhz);
const script = process.env.FM1_BENCH_NOKEYS ? "" : (process.env.FM1_BENCH_KEYS ?? "800:21,900:25,1000:28");
if (x.fm1_bench_script) {
  const bytes = new TextEncoder().encode(script), ptr = x.fm1_alloc(bytes.length);
  new Uint8Array(x.memory.buffer, ptr, bytes.length).set(bytes);
  if (x.fm1_bench_script(ptr, bytes.length)) throw new Error("input script failed");
} else if (script) throw new Error("this build lacks scheduled inputs; use FM1_BENCH_NOKEYS=1 for old builds");
if (process.env.FM1_PROFILE) x.fm1_profile(1);
const out = process.argv[4] ? [] : null;
const durations = [], steady = [];
const t0 = performance.now(), memoryStart = x.memory.buffer.byteLength;
let real = 0, short = 0, steadyStart, steadyMs, steadyFrames = 0, warmupJit;
// Stop on emulated time, as native does. Boot may produce no audio; never count silence
// inserted by fm1_render as useful audio or claim the requested duration was completed.
while (x.fm1_ms() < secs * 1000) {
  const beforeMs = x.fm1_ms();
  if (steadyStart === undefined && beforeMs >= warmup) {
    steadyStart = performance.now(); steadyMs = beforeMs; steadyFrames = real;
    warmupJit = { ...stats, tableSlots: x.__indirect_function_table.length, memoryBytes: x.memory.buffer.byteLength };
  }
  const start = performance.now();
  const have = x.fm1_render(128, budget);
  if (have && cap) budget = playBudget;
  const elapsed = performance.now() - start;
  durations.push(elapsed);
  if (steadyStart !== undefined) steady.push(elapsed);
  if (have < 128) short++;
  if (out && have) {
    const l = new Float32Array(x.memory.buffer, x.fm1_out_l(), have), r = new Float32Array(x.memory.buffer, x.fm1_out_r(), have);
    for (let i = 0; i < have; i++) out.push(Math.round(l[i] * 32768), Math.round(r[i] * 32768));
  }
  real += have;
  if (x.fm1_halted()) throw new Error(`halted at ${x.fm1_ms()} ms`);
  if (x.fm1_ms() === beforeMs && have === 0) throw new Error("emulation made no progress");
  if (performance.now() - t0 > Math.max(60_000, secs * 1000 * 100)) throw new Error("benchmark timed out");
}
const end = performance.now(), wall = (end - t0) / 1000, emulatedMs = x.fm1_ms();
const percentiles = values => {
  const a = [...values].sort((a, b) => a - b);
  const p = n => a[Math.max(0, Math.ceil(a.length * n) - 1)] ?? null;
  return { p50: p(.5), p95: p(.95), p99: p(.99), max: p(1) };
};
const idleMs = x.fm1_idle_ms ? x.fm1_idle_ms() : null;
const quantumMs = 128 / 44.1, over = steady.filter(d => d > quantumMs).length;
const result = { mhz, ahead: x.fm1_ahead ? ahead : 0, cap, requestedMs: secs * 1000, emulatedMs, wallSeconds: wall,
  realtime: emulatedMs / 1000 / wall, audioFrames: real, audioRealtime: real / 44100 / wall,
  shortQuanta: short, renderMs: percentiles(durations), steadyRenderMs: percentiles(steady),
  steadyQuanta: steady.length, steadyOver: over, idleShare: idleMs === null ? null : idleMs / emulatedMs,
  steadyRealtime: steadyStart === undefined ? null : (emulatedMs - steadyMs) / (end - steadyStart),
  steadyAudioFrames: real - steadyFrames, jit: stats, warmupJit, memoryStart, memoryEnd: x.memory.buffer.byteLength,
  tableSlots: x.__indirect_function_table.length, script };
if (process.env.FM1_PROFILE) {
  const ptr = x.fm1_profile_report(), bytes = new Uint8Array(x.memory.buffer);
  result.profile = JSON.parse(new TextDecoder().decode(bytes.subarray(ptr, bytes.indexOf(0, ptr))));
}
if (out) {
  const bytes = Buffer.alloc(out.length * 2);
  out.forEach((v, i) => bytes.writeInt16LE(v, i * 2));
  writeFileSync(process.argv[4], bytes);
}
if (process.env.FM1_BENCH_JSON) writeFileSync(process.env.FM1_BENCH_JSON, JSON.stringify(result, null, 2) + "\n");
const ms = v => v === null ? "?" : v.toFixed(2);
console.log(`${(emulatedMs / 1000).toFixed(3)} s emulated at ${mhz} MHz in ${wall.toFixed(2)} s: ${result.realtime.toFixed(2)}x real time ` +
  `(${real} real frames, ${short} short quanta); steady quanta p50 ${ms(result.steadyRenderMs.p50)} p99 ${ms(result.steadyRenderMs.p99)} ` +
  `max ${ms(result.steadyRenderMs.max)} ms, ${over} of ${steady.length} over ${quantumMs.toFixed(1)} ms` +
  (idleMs === null ? "" : `; ${Math.round(100 * result.idleShare)}% of the time idle skipped`));
console.log(JSON.stringify(result));
