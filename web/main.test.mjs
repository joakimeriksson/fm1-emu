// Page lifecycle regression tests with deterministic DOM, storage and audio boundaries.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import * as persistence from './persistence.js';
import * as tryout from './tryout.js';
const settle = () => new Promise(resolve => setImmediate(resolve));
class Element {
  constructor(text = '', value = '') {
    this.text = this.textContent = text; this.value = String(value); this.options = []; this.listeners = {};
    this.dataset = {}; this.style = { setProperty() {} }; this.classList = { add() {}, remove() {}, toggle() {} };
    this.parentElement = this;
  }
  add(e, at = this.options.length) { this.options.splice(at, 0, e); }
  addEventListener(type, fn) { this.listeners[type] = fn; }
  append() {} replaceChildren() {} setAttribute() {} blur() {} click() {}
  hasAttribute() { return false; }
  querySelector() { return new Element(); }
  getBoundingClientRect() { return { top: 0 }; }
  getContext() { return {}; }
}
function storage() {
  return { records: new Map(), entries: new Map(), writes: [], fail: false,
    async entry(key) { return this.load(this.entries.get(key)); },
    async load(hash) { return structuredClone(this.records.get(hash) || null); },
    async get() { return null; },
    async remember(hash, entry) { this.entries.set(entry, hash); },
    async list() { return structuredClone([...this.records.values()]); },
    async save(record, entry) {
      if (this.fail) throw new Error('quota exceeded');
      assert.equal(record.revision, this.records.get(record.hash)?.revision || 0, 'stale write');
      const revision = record.revision + 1;
      this.records.set(record.hash, structuredClone({ ...record, revision }));
      this.entries.set(entry, record.hash); this.writes.push({ entry, record: structuredClone(record) });
      return revision;
    } };
}
async function page(disk = storage(), initialSearch = '', selected = '', manualBootTimeout = false, extra = {}) {
  let bootTimer;
  const elements = new Map();
  const document = { documentElement: new Element(),
    getElementById(id) { if (!elements.has(id)) elements.set(id, new Element()); return elements.get(id); },
    createElement() { return new Element(); }, querySelector() { return new Element(); }, addEventListener() {} };
  const cat = { default: 'a', firmwares: ['a', 'b'].map(id => ({ id, name: id, version: '1',
    package: `https://example.test/${id}.fwsc`, author: 'test', license: 'MIT', about: '' })) };
  let audio;
  class AudioContext {
    constructor() { this.state = 'running'; this.audioWorklet = { addModule: async () => {} }; }
    async resume() { this.state = 'running'; } async close() { this.state = 'closed'; }
  }
  class AudioWorkletNode {
    constructor() {
      audio = this; this.messages = [];
      this.port = { postMessage: message => {
        this.messages.push(message);
        queueMicrotask(() => {
          if (message.type === 'boot') {
            if (this.hangBoot) return;
            this.seq = message.seq;
            this.base = new Uint8Array(persistence.FLASH_SIZE).fill(170);
            this.flash = this.base.slice();
            const packed = new Uint8Array(message.sectors), view = new DataView(packed.buffer);
            for (let o = 0; o < packed.length; o += 4100) this.flash.set(packed.subarray(o + 4, o + 4100), view.getUint32(o, true));
            if (message.syx) this.flash.set(new Uint8Array(message.syx), 0x90000);
            this.port.onmessage({ data: { type: 'booted', seq: this.seq, base: this.base, flash: this.flash.slice(), board: 0 } });
          } else if (message.type === 'snapshot') {
            this.port.onmessage({ data: { type: 'snapshot', seq: this.seq, request: message.request, flash: this.flash.slice() } });
          }
        });
      } };
    }
    connect() {} disconnect() {}
  }
  const local = new Map(selected ? [['fm1.fw', selected]] : []);
  const location = { search: initialSearch, origin: 'http://localhost', pathname: '/', href: `http://localhost/${initialSearch}` };
  const environment = {
    setTimeout(fn, ms) { if (manualBootTimeout && ms === 30000) { bootTimer = fn; return 'boot-timer'; } return setTimeout(fn, ms); },
    clearTimeout(id) { if (id !== 'boot-timer') clearTimeout(id); },
    document, location, history: { replaceState(_a, _b, url) { location.search = url; } },
    localStorage: { getItem: key => local.get(key), setItem: (key, value) => local.set(key, value) },
    ResizeObserver: class { observe() {} }, scrollY: 0, Option: Element, navigator: {},
    fetch: async url => ({ json: async () => url === 'smk37/firmwares.json' ? { firmwares: [] } : cat,
      arrayBuffer: async () => new ArrayBuffer(0) }),
    WebAssembly: { instantiate: async () => ({ instance: { exports: { panel_board() {}, panel_width: () => 100, panel_height: () => 100 } } }) },
    requestAnimationFrame() {}, addEventListener() {}, AudioContext, AudioWorkletNode,
    matchMedia: () => ({ matches: false }), confirm: () => true,
    fetchPackage: async f => ({ bytes: new Uint8Array(960).fill(f.id === 'a' ? 1 : 2), url: f.package }),
    ...persistence, ...tryout, SaveStore: class { constructor() { return disk; } }, ...extra };
  const source = readFileSync(new URL('./main.js', import.meta.url), 'utf8').replace(/^import .*;$/gm, '');
  const run = new Function(...Object.keys(environment), `return (async () => { ${source}\nreturn {
    startAudio, boot, flushRunning, setLatency, get running() { return running; },
    setFirmware(id) { fw = byId.get(id); fwSel.value = id; }, setBank(bytes) { bank = bytes; },
    get fw() { return fw; }, perf, setMhz(c) { mhz = c; }, get bootSeq() { return bootSeq; }
  }; })()`);
  const api = await run(...Object.values(environment));
  return { api, disk, elements, location, expireBoot: () => bootTimer(), get audio() { return audio; } };
}
test('firmware switch, clock reboot and audio-context replacement retain the latest flash', async () => {
  const p = await page(); await p.api.startAudio(); await p.api.boot(); await settle();
  p.audio.flash[0x74000] = 19;
  p.api.setFirmware('b'); await p.api.boot();
  assert.equal(p.disk.writes.find(w => w.entry === 'fm1.a' && w.record.sectors.length).record.sectors[0].data[0], 19);
  p.api.setFirmware('a'); await p.api.boot();
  assert.equal(p.audio.flash[0x74000], 19);
  p.audio.flash[0x74001] = 20;
  await p.api.boot(); assert.equal(p.audio.flash[0x74001], 20);
  await p.api.setLatency('playback'); assert.equal(p.audio.flash[0x74001], 20);
});
test('DX7 seed is consumed once; later edits to imported voices survive a reboot', async () => {
  const p = await page(); await p.api.startAudio();
  p.api.setBank(new Uint8Array([13, 14])); await p.api.boot();
  assert.equal(p.audio.flash[0x90000], 13);
  p.audio.flash[0x90000] = 27;
  await p.api.boot();
  assert.equal(p.audio.messages.at(-1).syx, null);
  assert.equal(p.audio.flash[0x90000], 27);
});
test('backup import replaces the same running hash without a stale revision and restores local selection', async () => {
  const p = await page(); await p.api.startAudio(); await p.api.boot(); await settle();
  const record = { ...p.api.running.record, sectors: [{ at: 0x74000, data: new Uint8Array(4096).fill(53) }] };
  const input = p.elements.get('import-save');
  input.files = [{ size: 1000, text: async () => persistence.exportSave(record) }];
  await input.listeners.change(); await settle();
  assert.equal(p.audio.flash[0x74000], 53);
  assert.equal(p.elements.get('save-status').textContent, 'Saved in this browser');
  const reloaded = await page(p.disk, '?fw=local', 'local');
  assert.equal(reloaded.api.fw.id, 'local');
  await reloaded.api.startAudio(); await reloaded.api.boot();
  assert.equal(reloaded.audio.flash[0x74000], 53);
});
test('failed storage retains changes in memory across a reboot and exposes an error', async () => {
  const p = await page(); await p.api.startAudio(); await p.api.boot(); await settle();
  p.disk.fail = true; p.audio.flash[0x74000] = 83;
  await p.api.boot(); await settle();
  assert.equal(p.audio.flash[0x74000], 83);
  assert.match(p.elements.get('save-status').textContent, /quota exceeded/);
});

test('reset removes only the selected firmware differences', async () => {
  const p = await page(); await p.api.startAudio(); await p.api.boot();
  p.audio.flash[0x74000] = 10;
  p.api.setFirmware('b'); await p.api.boot();
  p.audio.flash[0x74000] = 11;
  p.api.setFirmware('a'); await p.api.boot();
  await p.api.boot({ reset: true });
  assert.equal(p.audio.flash[0x74000], 170);
  p.api.setFirmware('b'); await p.api.boot();
  assert.equal(p.audio.flash[0x74000], 11);
});

test('opening an unchanged second tab does not invalidate the first tab revision', async () => {
  const disk = storage();
  const first = await page(disk); await first.api.startAudio(); await first.api.boot(); await settle();
  const count = disk.writes.length;
  const second = await page(disk); await second.api.startAudio(); await second.api.boot(); await settle();
  assert.equal(disk.writes.length, count);
  first.audio.flash[0x74000] = 91;
  await first.api.flushRunning();
  assert.equal(disk.writes.at(-1).record.sectors[0].data[0], 91);
});
test('a render failure clears the running session and another firmware boots immediately', async () => {
  const p = await page(); await p.api.startAudio(); await p.api.boot(); await settle();
  p.audio.port.onmessage({ data: { type: 'error', seq: p.audio.seq, fatal: true, message: 'render: trap' } });
  assert.equal(p.api.running, null);
  p.api.setFirmware('b'); assert.equal(await p.api.boot(), true);
  assert.equal(p.audio.messages.filter(m => m.type === 'snapshot').length, 0);
  assert.equal(p.api.running.record.name, 'b');
});

test('saved firmware library recovers earlier local files after importing another backup', async () => {
  const p = await page(); await p.api.startAudio(); await p.api.boot();
  p.audio.flash[0x74000] = 29; await p.api.flushRunning();
  const firstHash = p.api.running.record.hash;
  p.api.setFirmware('b'); await p.api.boot();
  await p.elements.get('browse-saves').listeners.click();
  const select = p.elements.get('saved-versions');
  assert.equal(select.options.length, 2);
  select.value = firstHash;
  await p.elements.get('open-saved-version').listeners.click();
  assert.equal(p.api.running.record.hash, firstHash);
  assert.equal(p.audio.flash[0x74000], 29);
  await settle();
  const reloaded = await page(p.disk, '?fw=local', 'local');
  await reloaded.api.startAudio(); await reloaded.api.boot();
  assert.equal(reloaded.audio.flash[0x74000], 29);
});

test('a boot timeout cancels the machine and the next firmware selection recovers', async () => {
  const p = await page(storage(), '', '', true);
  await p.api.startAudio(); await p.api.boot(); await settle();
  p.audio.hangBoot = true;
  const sent = p.audio.messages.length;
  const booting = p.api.boot();
  // the hung boot's timer is set as it posts its boot message: until then, the timer is the last boot's
  while (!p.audio.messages.slice(sent).some(m => m.type === 'boot')) await settle();
  p.expireBoot();
  assert.equal(await booting, false);
  assert.equal(p.api.running, null);
  assert.ok(p.audio.messages.some(m => m.type === 'cancel-boot'));
  p.audio.hangBoot = false; p.api.setFirmware('b');
  assert.equal(await p.api.boot(), true);
});

test('the status line and the report give the emulation speed and the JIT; idle firmwares get no lower clock', async () => {
  const p = await page(); await p.api.startAudio(); await p.api.boot(); await settle();
  const jit = { regions: 812, failed: 1, error: 'CompileError: x', compileMs: 40, compileMaxMs: 1.5 };
  // a second's worth of quanta: 345 of them, 2.9 ms of audio each; 400 ms busy for 1000 ms emulated
  p.audio.port.onmessage({ data: { type: 'frame', seq: p.api.bootSeq, lcd: null, leds: new Float32Array(41), ms: 5000,
    short: 3, halted: false, jit, playing: true, busy: 400, worst: 2.5, emulated: 1000, idle: 830, quanta: 345, frames: 128 } });
  const status = p.elements.get('status').textContent;
  assert.match(status, /2\.5x real time · load 40%/);
  assert.doesNotMatch(status, /instructions/);
  const report = p.api.perf.report();
  assert.match(report, /^speed \(emulated time per time spent emulating, last 1 s\): 2\.50x real time$/m);
  assert.match(report, /^jit: 812 regions compiled, 1 failed \(CompileError: x\) · compiling took 40 ms, the longest 1\.5 ms$/m);
  assert.match(report, /idle skipped: 83% of the time/);
  assert.match(report, /worst quantum since the audio started 2\.5 ms/);
  assert.doesNotMatch(report, /instructions/);
  // M-VAVE's firmware (83% idle at 240 MHz): its work is the same at 60 MHz, so no lower clock
  p.api.setMhz(240);
  assert.equal(p.api.perf.lowerClock(1.3), null);
  // a community firmware (10% idle): a lower clock helps, the highest that brings the load to ~80%
  p.api.perf.idleShare = 0.1;
  assert.equal(p.api.perf.lowerClock(1.2), 120);
  assert.equal(p.api.perf.lowerClock(0.9), 160);
  // too slow even at 60 MHz: still the lowest, it needs much less there
  assert.equal(p.api.perf.lowerClock(3), 60);
  p.api.setMhz(80);
  assert.equal(p.api.perf.lowerClock(1.2), null);    // (60 MHz would save only ~13%)
});

test("a try-out boots with its sectors and script from this origin, starts by itself and saves nothing", async () => {
  const disk = storage(), fetched = [], told = [];
  const sector = new Uint8Array(4100); new DataView(sector.buffer).setUint32(0, 0x96000, true); sector.fill(7, 4);
  const files = { 'http://localhost/dock/tryit/m.flash': sector, 'http://localhost/dock/tryit/m.txt': '1500:mf07d4842f7\n' };
  const fetch = async (url, init) => {
    fetched.push([url, init]);
    if (url in files) return { ok: true, arrayBuffer: async () => files[url].buffer, text: async () => files[url] };
    return { json: async () => url === 'smk37/firmwares.json' ? { firmwares: [] } : { default: 'a', firmwares: ['a', 'b'].map(id => ({ id, name: id, version: '1', package: `https://example.test/${id}.fwsc`, author: 't', license: 'MIT', about: '' })) },
      arrayBuffer: async () => new ArrayBuffer(0) };
  };
  const window = { parent: { postMessage(m, origin) { told.push([m.state, origin]); } } };
  const p = await page(disk, '?fw=b&flash=/dock/tryit/m.flash&script=../dock/tryit/m.txt', '', false, { fetch, window });
  while (!p.api.running) await settle();
  await settle();
  const boot = p.audio.messages.find(m => m.type === 'boot');
  assert.equal(new TextDecoder().decode(boot.script), '1500:mf07d4842f7');
  assert.equal(new DataView(boot.sectors).getUint32(0, true), 0x96000);
  assert.equal(p.audio.flash[0x96000], 7);
  assert.equal(p.api.fw.id, 'b');
  assert.ok(fetched.filter(([u]) => u in files).every(([, init]) => init.redirect === 'error' && init.credentials === 'same-origin'));
  // nothing stored, the flash kept for an export; the embedding page (this origin only) hears how it goes
  p.audio.flash[0x96001] = 9; await p.api.flushRunning();
  assert.equal(disk.writes.length, 0);
  assert.equal(p.api.running.record.sectors.find(s => s.at === 0x96000).data[1], 9);
  assert.match(p.elements.get('save-status').textContent, /try-out: not saved/);
  assert.deepEqual(told.slice(0, 2), [['loading', 'http://localhost'], ['booting', 'http://localhost']]);
  p.audio.port.onmessage({ data: { type: 'frame', seq: p.api.bootSeq, lcd: null, leds: new Float32Array(41), ms: 2100,
    short: 0, halted: false, jit: null, playing: true, busy: 1, worst: 1, emulated: 1, idle: 0, quanta: 1, frames: 128 } });
  assert.deepEqual(told.at(-1), ['playing', 'http://localhost']);
  // another firmware ends the try-out: its own saved flash again, and saved
  p.api.setFirmware('a'); p.elements.get('fw').listeners.change(); await p.api.boot(); await settle();
  assert.doesNotMatch(p.location.search, /flash|script/);
  p.api.setFirmware('b'); await p.api.boot(); await settle();
  assert.notEqual(p.audio.flash[0x96000], 7);
  assert.ok(disk.writes.length > 0);
});

test('a try-out from another site is refused: nothing fetched, nothing started', async () => {
  const fetched = [];
  const base = await page();
  const fetch = async (url) => { fetched.push(url); return { json: async () => (url === 'smk37/firmwares.json' ? { firmwares: [] } : { default: 'a', firmwares: [{ id: 'a', name: 'a', version: '1', package: 'https://example.test/a.fwsc', author: 't', license: 'MIT', about: '' }] }), arrayBuffer: async () => new ArrayBuffer(0) }; };
  for (const q of ['?flash=https://evil.test/m.flash', '?script=//evil.test/m.txt', '?flash=data:,x']) {
    const p = await page(storage(), q, '', false, { fetch });
    await settle();
    assert.equal(p.audio, base.audio, q);          // (no audio graph made: nothing started)
    assert.match(p.elements.get('status').textContent, /^try-out: only files from http:\/\/localhost/, q);
  }
  assert.ok(fetched.every(u => !/evil|data:/.test(u)));
});
