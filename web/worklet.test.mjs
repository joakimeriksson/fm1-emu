// Deterministic AudioWorklet harness: no audio device or browser required.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
function harness() {
  let Processor, time = 0, revision = 0, flashRevision = 0, busy = false;
  const messages = [];
  const memory = new WebAssembly.Memory({ initial: 4 });
  const context = vm.createContext({ WebAssembly, Float32Array, Uint16Array, Uint32Array, Uint8Array,
    sampleRate: 44100, performance: { now: () => time },
    AudioWorkletProcessor: class { constructor() { this.port = { postMessage(m) { messages.push(m); time += 2; } }; } },
    registerProcessor(_, cls) { Processor = cls; } });
  vm.runInContext(readFileSync(new URL('./worklet.js', import.meta.url), 'utf8'), context);
  const p = new Processor();
  Object.assign(p, { x: { memory, fm1_render(n) { time += .1; return n; },
    fm1_out_l: () => 0, fm1_out_r: () => 4096, fm1_lcd: () => 8192,
    fm1_lcd_revision: () => revision, fm1_leds: () => 131072,
    fm1_flash_revision: () => flashRevision, fm1_flash_busy: () => busy,
    fm1_flash: () => 140000, fm1_flash_len: () => 4096,
    fm1_ms: () => 1000, fm1_minsns: () => 10, fm1_halted: () => false },
    budget: 100000, board: 0, short: 0, reportFrames: 0, reportCalls: 0,
    busy: 0, worst: 0, lcdRevision: null, seq: 1, flashRevision: 0, observedRevision: 0,
    flashChangedAt: 0, lastSnapshot: 0 });
  const render = () => p.process([], [[new Float32Array(128), new Float32Array(128)]]);
  return { p, context, messages, memory, render, change: () => revision++, advance: ms => time += ms,
    flashChange: () => flashRevision++, flashBusy: value => busy = value };
}
test('unchanged LCD is omitted; changed partial frame is sent at capped rate', () => {
  const h = harness();
  for (let i = 0; i < 24; i++) h.render();
  assert.equal(h.messages.length, 2);
  assert.equal(h.messages[0].lcd.length, 57600);
  assert.equal(h.messages[1].lcd, null);
  h.change();
  for (let i = 0; i < 12; i++) h.render();
  assert.equal(h.messages[2].lcd.length, 57600);
});
test('timing includes postMessage; audio views reused and refreshed on memory growth', () => {
  const h = harness(); h.render(); const views = h.p.views;
  h.render(); assert.equal(h.p.views, views);
  h.memory.grow(1); h.render(); assert.notEqual(h.p.views, views);
  for (let i = 3; i < 24; i++) h.render();
  assert.ok(Math.abs(h.messages[1].busy - 3.2) < 1e-8);
  assert.ok(Math.abs(h.messages[1].worst - 2.1) < 1e-8);
  assert.equal(h.messages[1].quanta, 12);
});

test('snapshot copies flash, preserves boot identity and waits for an SPI command', async () => {
  const h = harness();
  new Uint8Array(h.memory.buffer, 140000, 4096).fill(42);
  h.advance(500);
  h.flashBusy(true);
  await h.p.port.onmessage({ data: { type: 'snapshot', seq: 1, request: 7, pause: true } });
  assert.equal(h.messages.length, 0);
  h.flashBusy(false);
  h.p.flushSnapshots();
  assert.equal(h.messages[0].type, 'snapshot');
  assert.equal(h.messages[0].seq, 1);
  assert.equal(h.messages[0].request, 7);
  assert.equal(h.messages[0].flash[0], 42);
  new Uint8Array(h.memory.buffer, 140000, 4096).fill(13);
  assert.equal(h.messages[0].flash[0], 42);
  assert.equal(h.p.paused, true);
  await h.p.port.onmessage({ data: { type: 'resume' } });
  assert.equal(h.p.paused, false);
  await h.p.port.onmessage({ data: { type: 'snapshot', seq: 0, request: 8 } });
  assert.equal(h.messages.length, 1);
});
test('autosave waits for quiet flash and does not resend unchanged flash', () => {
  const h = harness();
  h.flashChange(); h.advance(1100);
  for (let i = 0; i < 12; i++) h.render();
  assert.equal(h.messages.filter(m => m.type === 'snapshot').length, 0);
  h.advance(300);
  for (let i = 0; i < 12; i++) h.render();
  assert.equal(h.messages.filter(m => m.type === 'snapshot').length, 1);
  h.advance(2000);
  for (let i = 0; i < 12; i++) h.render();
  assert.equal(h.messages.filter(m => m.type === 'snapshot').length, 1);
});

test('manual snapshot acknowledges a revision before the next frame report', async () => {
  const h = harness();
  h.flashChange();
  await h.p.port.onmessage({ data: { type: 'snapshot', seq: 1, request: 9 } });
  assert.equal(h.p.observedRevision, 1);
  for (let i = 0; i < 12; i++) h.render();
  assert.equal(h.messages.filter(m => m.type === 'flash-changed').length, 1);
});
test('an invalid replacement package leaves the previous machine available', async () => {
  const h = harness(), previous = h.p.x;
  h.p.paused = true;
  await h.p.port.onmessage({ data: { type: 'boot', seq: 2, wasm: new Uint8Array([0]).buffer } });
  assert.equal(h.p.x, previous);
  assert.equal(h.p.paused, false);
  assert.equal(h.messages[0].type, 'error');
  assert.equal(h.messages[0].seq, 2);
});

test('replacement snapshots wait for a quiet interval and timed-out requests can be cancelled', async () => {
  const h = harness();
  h.flashChange();
  await h.p.port.onmessage({ data: { type: 'snapshot', seq: 1, request: 10, pause: true } });
  assert.equal(h.messages.filter(m => m.type === 'snapshot').length, 0);
  h.advance(300); h.p.flushSnapshots();
  assert.equal(h.messages.filter(m => m.type === 'snapshot').length, 1);
  const pending = harness(); pending.flashBusy(true);
  await pending.p.port.onmessage({ data: { type: 'snapshot', seq: 1, request: 11, pause: true } });
  await pending.p.port.onmessage({ data: { type: 'cancel-snapshot', request: 11 } });
  pending.flashBusy(false); pending.advance(500); pending.p.flushSnapshots();
  assert.equal(pending.p.paused, undefined);
  assert.equal(pending.messages.length, 0);
});

test('render traps acknowledge pending and future snapshots, allowing a replacement boot', async () => {
  const h = harness(); h.flashBusy(true);
  await h.p.port.onmessage({ data: { type: 'snapshot', seq: 1, request: 20 } });
  h.p.x.fm1_render = () => { throw new Error('trap'); };
  h.render();
  assert.equal(h.p.x, null);
  assert.ok(h.messages.some(m => m.request === 20 && m.unavailable));
  await h.p.port.onmessage({ data: { type: 'snapshot', seq: 1, request: 21 } });
  assert.ok(h.messages.some(m => m.request === 21 && m.unavailable));
  let booted = false; h.p.boot = async () => { booted = true; };
  await h.p.port.onmessage({ data: { type: 'boot', seq: 2 } });
  assert.equal(booted, true);
});
test('manual backups wait for quiet; continuous writes bound switching without saving partial flash', async () => {
  const h = harness(); h.flashChange();
  await h.p.port.onmessage({ data: { type: 'snapshot', seq: 1, request: 22 } });
  assert.equal(h.messages.filter(m => m.type === 'snapshot').length, 0);
  h.advance(300); h.p.flushSnapshots();
  assert.ok(h.messages.some(m => m.request === 22 && m.flash));
  const busy = harness(); busy.flashBusy(true);
  await busy.p.port.onmessage({ data: { type: 'snapshot', seq: 1, request: 23, pause: true } });
  busy.advance(1600); busy.p.flushSnapshots();
  assert.ok(busy.messages.some(m => m.request === 23 && m.unavailable && !m.flash));
  assert.equal(busy.p.paused, true);
  const exporting = harness(); exporting.flashBusy(true);
  await exporting.p.port.onmessage({ data: { type: 'snapshot', seq: 1, request: 24 } });
  exporting.advance(1600); exporting.p.flushSnapshots();
  assert.ok(exporting.messages.some(m => m.request === 24 && m.error));
  assert.notEqual(exporting.p.paused, true);
});
test('cancelled boot invalidates an in-flight replacement and clears the old machine', async () => {
  const h = harness(); const token = h.p.bootToken;
  await h.p.port.onmessage({ data: { type: 'cancel-boot', seq: 2 } });
  assert.equal(h.p.bootToken, token + 1); assert.equal(h.p.x, null);
});

test('a firmware without a user bank still boots when a DX7 bank is selected', async () => {
  const h = harness(), x = h.p.x;
  Object.assign(x, { fm1_alloc: () => 1000, fm1_jit() {}, fm1_boot_fwsc: () => 0,
    fm1_load_syx: () => 2, fm1_board: () => 0, fm1_flash_base: () => 140000 });
  h.p.module = {};
  h.context.WebAssembly = { instantiate: async () => ({ exports: x }) };
  await h.p.port.onmessage({ data: { type: 'boot', seq: 2, mhz: 100,
    package: new Uint8Array(960).buffer, syx: new Uint8Array(4096).buffer } });
  assert.ok(h.messages.some(m => m.type === 'booted' && m.seq === 2));
  assert.equal(h.messages.some(m => m.type === 'error'), false);
});

test('boot turns on the even pace and boots with 20 ms of emulated time per quantum', async () => {
  const h = harness(), x = h.p.x;
  let ahead = null;
  Object.assign(x, { fm1_alloc: () => 1000, fm1_jit() {}, fm1_ahead(f) { ahead = f; }, fm1_boot_fwsc: () => 0,
    fm1_board: () => 0, fm1_flash_base: () => 140000 });
  h.p.module = {};
  h.context.WebAssembly = { instantiate: async () => ({ exports: x }) };
  await h.p.port.onmessage({ data: { type: 'boot', seq: 2, mhz: 100, package: new Uint8Array(960).buffer } });
  assert.equal(ahead, 1024);
  assert.equal(h.p.budget, 100e6 * 0.02);
  assert.equal(h.p.playing, false);
  await h.p.port.onmessage({ data: { type: 'boot', seq: 3, mhz: 100, ahead: 0, package: new Uint8Array(960).buffer } });
  assert.equal(ahead, 0);
});

test('once audio plays a quantum may run 1.5 quanta; the worst quantum counts only those', () => {
  const h = harness(), budgets = [];
  let ms = 0, have = 0;
  Object.assign(h.p, { mhz: 240, ahead: 1024, budget: 240e6 * 0.02, playing: false, timedMs: 0 });
  // booting: no audio and slow quanta; then audio, quick quanta
  Object.assign(h.p.x, { fm1_ms: () => ms,
    fm1_render(n, budget) { budgets.push(budget); ms += have ? 3 : 20; h.advance(have ? 1 : 9); return have; } });
  for (let i = 0; i < 5; i++) h.render();
  have = 128;
  for (let i = 0; i < 30; i++) h.render();
  assert.deepEqual(budgets.slice(0, 6), Array(6).fill(240e6 * 0.02));
  assert.ok(Math.abs(budgets[6] - 240e6 * 1.5 * 128 / 44100) < 1e-6);
  assert.equal(budgets.at(-1), budgets[6]);
  const frames = h.messages.filter(m => m.type === 'frame');
  assert.equal(frames[0].playing, true);
  // the second report times the first interval: 5 boot quanta (9 ms, 20 ms emulated each), 7 playing
  assert.equal(frames[1].emulated, 5 * 20 + 7 * 3);
  assert.ok(frames[1].worst < 9, `worst ${frames[1].worst}`);
  assert.ok(frames[1].busy > 5 * 9);
  // without the pace (?ahead=0) a quantum may need a whole half buffer's time: no cap
  const off = harness(), seen = [];
  Object.assign(off.p, { mhz: 240, ahead: 0, budget: 240e6 * 0.02, playing: false });
  off.p.x.fm1_render = (n, budget) => { seen.push(budget); return n; };
  off.render(); off.render();
  assert.deepEqual(seen, [240e6 * 0.02, 240e6 * 0.02]);
});

test("a try-out's sectors and input script are installed before the first quantum", async () => {
  const h = harness(), x = h.p.x, calls = [];
  let next = 1000;
  Object.assign(x, { fm1_alloc: (n) => { const p = next; next += n + 8; return p; }, fm1_jit() {}, fm1_boot_fwsc: () => 0,
    fm1_board: () => 0, fm1_flash_base: () => 140000,
    fm1_flash_restore(p, n) { calls.push(['flash', n]); return 0; },
    fm1_bench_script(p, n) { calls.push(['script', new TextDecoder().decode(new Uint8Array(x.memory.buffer, p, n))]); return 0; } });
  h.p.module = {};
  h.context.WebAssembly = { instantiate: async () => ({ exports: x }) };
  const script = new TextEncoder().encode('1500:mf07d4842f7');
  await h.p.port.onmessage({ data: { type: 'boot', seq: 2, mhz: 100, package: new Uint8Array(960).buffer,
    sectors: new Uint8Array(4100).buffer, script: script.buffer } });
  assert.deepEqual(calls, [['flash', 4100], ['script', '1500:mf07d4842f7']]);
  assert.ok(h.messages.some(m => m.type === 'booted' && m.seq === 2));
  // a build without scheduled inputs refuses a script rather than booting without it
  delete x.fm1_bench_script;
  await h.p.port.onmessage({ data: { type: 'boot', seq: 3, mhz: 100, package: new Uint8Array(960).buffer, script: script.buffer } });
  assert.ok(h.messages.some(m => m.type === 'error' && m.seq === 3 && /input script/.test(m.message)));
});
