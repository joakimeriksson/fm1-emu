// Page lifecycle regression tests with deterministic DOM, storage and audio boundaries.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import * as persistence from './persistence.js';
import * as tryout from './tryout.js';
const settle = () => new Promise(resolve => setImmediate(resolve));
// What a pick sets going has come to rest. Its boot hashes the package on a thread of its own and takes a snapshot
// from the worklet: turns of the loop alone do not wait for those on a loaded machine, so some real time is given too.
const quiet = async () => {
  for (let i = 0; i < 20; i++) await settle();
  await new Promise(resolve => setTimeout(resolve, 50));
  for (let i = 0; i < 20; i++) await settle();
};
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
            if (this.failBoot) return this.port.onmessage({ data: { type: 'error', seq: message.seq, message: 'boot: not a package' } });
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
    localStorage: { getItem: key => local.get(key), setItem: (key, value) => local.set(key, value), removeItem: key => local.delete(key) },
    ResizeObserver: class { observe() {} }, scrollY: 0, Option: Element, navigator: {},
    fetch: async url => ({ ok: true, json: async () => url === 'smk37/firmwares.json' ? { firmwares: [] } : cat,
      arrayBuffer: async () => new ArrayBuffer(0) }),
    WebAssembly: { instantiate: async () => ({ instance: { exports: { panel_board() {}, panel_width: () => 100, panel_height: () => 100, panel_theme_count: () => 1, panel_theme_name: () => 0, panel_theme() {}, memory: { buffer: new ArrayBuffer(256) } } } }) },
    requestAnimationFrame() {}, addEventListener() {}, AudioContext, AudioWorkletNode,
    matchMedia: () => ({ matches: false }), confirm: () => true,
    fetchPackage: async f => ({ bytes: new Uint8Array(960).fill(f.id === 'a' ? 1 : 2), url: f.package }),
    hubNewcomers: async () => [], hubCardUrl: id => `https://thefirmwarehub.com/#fw-${id}`,
    ...persistence, ...tryout, SaveStore: class { constructor() { return disk; } }, ...extra };
  const source = readFileSync(new URL('./main.js', import.meta.url), 'utf8').replace(/^import .*;$/gm, '');
  const run = new Function(...Object.keys(environment), `return (async () => { ${source}\nreturn {
    startAudio, boot, flushRunning, setLatency, get running() { return running; },
    setFirmware(id) { fw = byId.get(id); fwSel.value = id; }, setBank(bytes) { bank = bytes; },
    get fw() { return fw; }, perf, setMhz(c) { pickedMhz = mhz = c; }, setPot, get bootSeq() { return bootSeq; }, get bootQueue() { return bootQueue; }, screen: { lcdRaw, showLcd }
  }; })()`);
  // (a page that could not come up: what it shows, with the error)
  const api = await run(...Object.values(environment)).catch(error => { throw Object.assign(error, { elements }); });
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

test('a boot that fails leaves the page with the firmware that still runs: its frames shown, the pick back to it', async () => {
  const fetchPackage = async f => {
    if (f.id === 'b') throw new Error('b.fwsc: 503');
    return { bytes: new Uint8Array(960).fill(1), url: f.package };
  };
  const kept = new Map([['fm1.fw', 'a'], ['fm1.mhz', '100']]);
  const localStorage = { getItem: k => kept.get(k), setItem: (k, v) => kept.set(k, String(v)), removeItem: k => kept.delete(k) };
  const p = await page(storage(), '', '', false, { fetchPackage, localStorage });
  await p.api.startAudio(); await p.api.boot(); await settle();
  const first = p.api.running, status = p.elements.get('status');
  const frame = () => p.audio.port.onmessage({ data: { type: 'frame', seq: first.seq, lcd: null, leds: new Float32Array(41), ms: 5000,
    short: 0, halted: false, jit: null, playing: true, busy: 400, worst: 1, emulated: 1000, idle: 0, quanta: 345, frames: 128 } });
  // its package does not come: the machine was never touched
  p.api.setFirmware('b'); p.elements.get('fw').listeners.change();
  await p.api.bootQueue; await quiet();
  assert.equal(status.textContent, 'b: b.fwsc: 503');
  assert.equal(p.api.running, first);
  assert.equal(p.api.fw.id, 'a');
  assert.equal(p.elements.get('fw').value, 'a');
  assert.match(p.location.search, /fw=a\b/);
  assert.equal(kept.get('fm1.fw'), 'a');          // the next visit opens what plays, not the pick that failed
  frame();
  assert.match(status.textContent, /^b: b\.fwsc: 503 · a · 100 MHz · 2\.5x real time/);
  // the worklet refuses the new machine (the old one paused for it, then resumed), at another clock too
  p.audio.failBoot = true; p.api.setMhz(160);
  assert.equal(await p.api.boot(), false);
  assert.equal(p.audio.messages.at(-1).type, 'resume');
  assert.equal(p.api.running, first);
  assert.equal(p.elements.get('mhz').value, 100);
  frame();
  assert.match(status.textContent, /^a: boot: not a package · a · 100 MHz · /);
  // a clock picked from the list is stored as it is picked: a machine that then does not boot takes it back
  p.elements.get('mhz').value = 160; p.elements.get('mhz').listeners.change();
  await p.api.bootQueue; await quiet();
  assert.equal(p.api.running, first);
  assert.equal(kept.get('fm1.mhz'), '100');
  // the next boot that works clears what the failed one said
  p.audio.failBoot = false;
  assert.equal(await p.api.boot(), true);
  p.audio.port.onmessage({ data: { type: 'frame', seq: p.api.bootSeq, lcd: null, leds: new Float32Array(41), ms: 100,
    short: 0, halted: false, jit: null, playing: true, busy: 1, worst: 1, emulated: 1, idle: 0, quanta: 1, frames: 128 } });
  assert.match(status.textContent, /^a · 100 MHz · /);
});

test("M-VAVE's entry picked without its file stays picked while the machine before it plays on, named in the status line", async () => {
  const entry = id => ({ id, name: id, version: '1', package: `https://example.test/${id}.fwsc`, author: 't', license: 'MIT', about: '' });
  const cat = { default: 'a', firmwares: [entry('a'), entry('b'),
    { id: 'mv', name: 'M-VAVE', version: '', byo: true, download: 'https://m.example/', repo: 'https://r.example/', author: 'M-VAVE', license: '', about: '' }] };
  const fetch = async url => ({ ok: true, json: async () => (url === 'smk37/firmwares.json' ? { firmwares: [] } : cat), arrayBuffer: async () => new ArrayBuffer(0) });
  const kept = new Map([['fm1.fw', 'a'], ['fm1.mhz', '100']]);
  const localStorage = { getItem: k => kept.get(k), setItem: (k, v) => kept.set(k, String(v)), removeItem: k => kept.delete(k) };
  let t = 0;
  const p = await page(storage(), '', '', false, { fetch, localStorage, performance: { now: () => t } });
  await p.api.startAudio(); await p.api.boot(); await settle();
  const first = p.api.running, status = p.elements.get('status'), warning = p.elements.get('perfwarn');
  // a second of frames in which the firmware ran out of time (some of those in a row bring up the warning)
  const second = () => { t += 1001; p.audio.port.onmessage({ data: { type: 'frame', seq: p.api.running.seq, lcd: null, leds: new Float32Array(41), ms: t,
    short: 0, halted: false, jit: null, playing: true, busy: 400, worst: 1, emulated: 1000, idle: 0, starved: 5, quanta: 345, frames: 128 } }); };
  const pick = async (id, value) => { p.elements.get(id).value = value; p.elements.get(id).listeners.change(); await quiet(); };
  let about = [];
  p.elements.get('about').replaceChildren = (...parts) => { about = parts.map(e => e.textContent ?? e); };
  await pick('fw', 'mv');
  assert.equal(status.textContent, 'M-VAVE: open your FM-1.fwsc to boot it');
  assert.deepEqual([p.api.running, p.api.fw.id, p.elements.get('fw').value], [first, 'mv', 'mv']);
  assert.ok(about.includes('download FM-1.fwsc from M-VAVE'), 'where its file is found stays shown');
  assert.match(p.location.search, /fw=mv\b/);
  assert.equal(kept.get('fm1.fw'), 'mv');            // (the pick, as before a Start: the next visit asks for the file)
  // the machine that plays is shown and named; no warning is made of it under the picked entry's name
  for (let i = 0; i < 8; i++) second();
  assert.match(status.textContent, /^M-VAVE: open your FM-1\.fwsc to boot it · a · 100 MHz · 2\.5x real time/);
  assert.equal(warning.hidden, true);
  // another clock is the picked entry's too, for when its file comes; Reset is of the picked entry, which does not run
  await pick('mhz', 160);
  assert.deepEqual([p.api.running, p.api.fw.id, p.elements.get('mhz').value, kept.get('fm1.mhz')], [first, 'mv', 160, '160']);
  second();
  assert.match(status.textContent, /^M-VAVE: open your FM-1\.fwsc to boot it · a · 100 MHz · /);
  await p.elements.get('reset-save').listeners.click();
  assert.match(p.elements.get('save-status').textContent, /start the selected firmware first/);
  assert.equal(p.api.running, first);
  // the file opened next, whatever its name, is the picked entry's copy
  const open = p.elements.get('open');
  open.files = [{ name: 'my copy.fwsc', arrayBuffer: async () => new Uint8Array(960).fill(7).buffer }];
  open.listeners.change();
  await quiet();
  assert.deepEqual([p.api.running.fw.id, p.api.running.mhz, p.api.running.record.byo, p.api.running.record.package[0]], ['mv', 160, true, 7]);
  for (let i = 0; i < 8; i++) second();
  assert.match(status.textContent, /^M-VAVE · 160 MHz · /);
  assert.equal(warning.hidden, false);               // (the same seconds, of the machine that is picked)
  assert.match(p.elements.get('perfwarn-text').textContent, /^M-VAVE runs out of time at 160 MHz/);
  // a pick that does not boot for another reason (the worklet refuses it) still goes back to the machine that plays
  p.audio.failBoot = true;
  await pick('fw', 'b');
  assert.deepEqual([p.api.fw.id, p.elements.get('fw').value, kept.get('fm1.fw')], ['mv', 'mv', 'mv']);
});

test("going back to a machine from a ?package= link puts the link back in the address and in the full page's", async () => {
  const fetchPackage = async f => {
    if (f.id === 'b') throw new Error('b.fwsc: 503');
    return { bytes: new Uint8Array(960).fill(3), url: f.package };
  };
  const p = await page(storage(), '?package=https://x.example/l-2.fwsc&name=L&jit=0', '', false, { fetchPackage });
  await p.api.startAudio(); await p.api.boot(); await settle();
  const address = () => Object.fromEntries(new URLSearchParams(p.location.search));
  const linked = { package: 'https://x.example/l-2.fwsc', name: 'L', jit: '0', fw: 'link', mhz: '100' };
  assert.deepEqual(address(), linked);
  p.elements.get('fw').value = 'b'; p.elements.get('fw').listeners.change();
  assert.deepEqual(address(), { jit: '0', fw: 'b', mhz: '100' });       // (the pick: no link's)
  await quiet();
  assert.equal(p.elements.get('status').textContent, 'b: b.fwsc: 503');
  assert.equal(p.api.fw.id, 'link');
  assert.deepEqual(address(), linked);
  assert.equal(p.elements.get('fullpage').href, './?fw=link&package=https%3A%2F%2Fx.example%2Fl-2.fwsc&name=L');
});

test("a file the emulator refuses in place of M-VAVE's copy is not the entry's package at the next boot", async () => {
  const cat = { default: 'mv', firmwares: [{ id: 'mv', name: 'M-VAVE', version: '', byo: true, download: 'https://m.example/', repo: 'https://r.example/', author: 'M-VAVE', license: '', about: '' }] };
  const fetch = async url => ({ ok: true, json: async () => (url === 'smk37/firmwares.json' ? { firmwares: [] } : cat), arrayBuffer: async () => new ArrayBuffer(0) });
  const p = await page(storage(), '', '', false, { fetch });
  await p.api.startAudio();
  const open = async (name, fill) => {
    p.elements.get('open').files = [{ name, arrayBuffer: async () => new Uint8Array(960).fill(fill).buffer }];
    p.elements.get('open').listeners.change();
    await quiet();
  };
  const booted = () => new Uint8Array(p.audio.messages.filter(m => m.type === 'boot').at(-1).package)[0];
  await open('FM-1.fwsc', 5);
  const first = p.api.running;
  assert.deepEqual([first.fw.id, booted()], ['mv', 5]);
  p.audio.failBoot = true;
  await open('FM-1 (1).fwsc', 6);
  assert.equal(p.elements.get('status').textContent, 'M-VAVE: boot: not a package');
  assert.deepEqual([p.api.running, booted()], [first, 6]);
  // another clock boots the copy that plays, not the file refused
  p.audio.failBoot = false;
  p.elements.get('mhz').value = 160; p.elements.get('mhz').listeners.change();
  await quiet();
  assert.deepEqual([p.api.running.mhz, booted(), p.api.running.record.package[0]], [160, 5, 5]);
});

test('?embed=1 leaves the firmware the full page starts with as it is, also when a pick there does not boot', async t => {
  const has = Element.prototype.hasAttribute;
  Element.prototype.hasAttribute = name => name === 'data-embed';        // (what index.html's inline script sets for ?embed=1)
  t.after(() => { Element.prototype.hasAttribute = has; });
  const fetchPackage = async f => {
    if (f.id === 'b') throw new Error('b.fwsc: 503');
    return { bytes: new Uint8Array(960).fill(1), url: f.package };
  };
  const kept = new Map([['fm1.fw', 'a'], ['fm1.mhz', '100']]);
  const localStorage = { getItem: k => kept.get(k), setItem: (k, v) => kept.set(k, String(v)), removeItem: k => kept.delete(k) };
  const p = await page(storage(), '?embed=1', '', false, { fetchPackage, localStorage });
  await p.api.startAudio(); await p.api.boot(); await settle();
  kept.set('fm1.fw', 'z');                           // (picked on the full page, in another tab, meanwhile)
  p.elements.get('mhz').value = 160; p.elements.get('mhz').listeners.change();
  p.elements.get('fw').value = 'b'; p.elements.get('fw').listeners.change();
  await quiet();
  assert.equal(p.elements.get('status').textContent, 'b: b.fwsc: 503');
  assert.equal(p.api.fw.id, 'a');
  assert.deepEqual([kept.get('fm1.fw'), kept.get('fm1.mhz')], ['z', '100']);
});

test('a machine that booted while a later pick waited is gone back to with its own stored pick, board and MASTER', async () => {
  const cat = { default: 'a', firmwares: ['a', 'b', 'c'].map(id => ({ id, name: id, version: '1', package: `https://example.test/${id}.fwsc`, author: 't', license: 'MIT', about: '' })) };
  const fetch = async url => ({ ok: true, json: async () => (url === 'smk37/firmwares.json' ? { firmwares: [] } : cat), arrayBuffer: async () => new ArrayBuffer(0) });
  const fetchPackage = async f => {
    if (f.id === 'c') throw new Error('c.fwsc: 503');
    return { bytes: new Uint8Array(960).fill(1), url: f.package };
  };
  const kept = new Map([['fm1.fw', 'a'], ['fm1.mhz', '100']]);
  const localStorage = { getItem: k => kept.get(k), setItem: (k, v) => kept.set(k, String(v)), removeItem: k => kept.delete(k) };
  const boards = [];
  const exports = { panel_board(b) { boards.push(b); }, panel_width: () => 100, panel_height: () => 100, panel_theme_count: () => 1,
    panel_theme_name: () => 0, panel_theme() {}, memory: { buffer: new ArrayBuffer(256) } };
  const p = await page(storage(), '', '', false, { fetch, fetchPackage, localStorage, WebAssembly: { instantiate: async () => ({ instance: { exports } }) } });
  await p.api.startAudio(); await p.api.boot(); await settle();
  p.api.setPot(300);
  const pick = id => { p.elements.get('fw').value = id; p.elements.get('fw').listeners.change(); };
  // b is in the worklet when c is picked; b boots (as an SMK-37's package here), then c's package does not come
  p.audio.hangBoot = true;
  pick('b');
  await quiet();
  const asked = p.audio.messages.at(-1);
  assert.equal(asked.type, 'boot');
  pick('c');
  const base = new Uint8Array(persistence.FLASH_SIZE).fill(170);
  Object.assign(p.audio, { seq: asked.seq, base, flash: base.slice() });
  p.audio.port.onmessage({ data: { type: 'booted', seq: asked.seq, base, flash: base.slice(), board: 1 } });
  await quiet();
  assert.equal(p.elements.get('status').textContent, 'c: c.fwsc: 503');
  assert.deepEqual([p.api.running.fw.id, p.api.fw.id, p.elements.get('fw').value], ['b', 'b', 'b']);
  assert.equal(kept.get('fm1.fw'), 'b');             // (what asked for the machine that plays; c was stored when b booted)
  assert.equal(boards.at(-1), 1);
  assert.deepEqual(p.audio.messages.filter(m => m.type === 'analog').map(m => m.idx), [16, 17, 18, 19, 20, 21]);
  // and once: another pick that fails sends them no second time; an FM-1 machine gets MASTER as it was turned
  pick('c');
  await quiet();
  assert.equal(p.audio.messages.filter(m => m.type === 'analog').length, 6);
  p.audio.hangBoot = false;
  pick('a');
  await quiet();
  assert.deepEqual([p.api.running.fw.id, boards.at(-1), p.audio.messages.at(-1)], ['a', 0, { type: 'pot', value: 300 }]);
});

test('a page that is not on https or localhost says so at Start: the browser gives it no audio worklet', async () => {
  class AudioContext { constructor() { this.state = 'running'; } async resume() {} async close() { this.state = 'closed'; } }
  const p = await page(storage(), '', '', false, { AudioContext });
  const start = p.elements.get('start');
  await start.listeners.click();
  assert.equal(p.elements.get('status').textContent,
    'the audio did not start: this page is not on https or localhost, and the browser gives such a page no audio worklet to run the emulator in');
  assert.equal(start.disabled, false);
});

test('Start says what kept the audio from starting and is there to try again', async () => {
  let fail = true;
  const contexts = [];
  class AudioContext {
    constructor(options) { contexts.push(this); this.hint = options.latencyHint; this.state = 'running'; this.audioWorklet = { addModule: async () => { if (fail) throw new Error('worklet.js: 404'); } }; }
    async resume() { this.state = 'running'; } async close() { this.state = 'closed'; }
  }
  const kept = new Map();
  const localStorage = { getItem: k => kept.get(k), setItem: (k, v) => kept.set(k, String(v)), removeItem: k => kept.delete(k) };
  const p = await page(storage(), '', '', false, { AudioContext, localStorage });
  const start = p.elements.get('start'), status = p.elements.get('status');
  await start.listeners.click();
  assert.equal(start.disabled, false);
  assert.equal(status.textContent, 'the audio did not start: worklet.js: 404');
  assert.equal(contexts[0].state, 'closed');       // (the next Start makes its own)
  fail = false;
  await start.listeners.click(); await settle();
  assert.equal(p.api.running.record.name, 'a');
  assert.equal(start.hidden, true);
  // the larger buffer's context refused: the page says so, and Start is back to make a graph again
  fail = true;
  await p.api.setLatency('playback');
  assert.equal(p.api.running, null);
  assert.equal(status.textContent, 'the audio buffer was not changed: worklet.js: 404');
  assert.equal(start.hidden, false);
  assert.equal(start.disabled, false);
  // the buffer is the one it was, now and at the next visit, and the context that got no worklet is closed
  assert.deepEqual([contexts.at(-1).hint, contexts.at(-1).state], ['playback', 'closed']);
  assert.equal(kept.has('fm1.latency'), false);
  fail = false;
  await start.listeners.click(); await settle();
  assert.equal(p.api.running.record.name, 'a');
  assert.equal(contexts.at(-1).hint, 'interactive');
  // a change that works is kept
  await p.api.setLatency('playback'); await settle();
  assert.deepEqual([contexts.at(-1).hint, kept.get('fm1.latency'), p.api.running.record.name], ['playback', 'playback', 'a']);
  assert.deepEqual(contexts.map(c => c.state), ['closed', 'closed', 'closed', 'closed', 'running']);
});

test('?latency= is one of the audio buffers an AudioContext knows; any other word is not passed to it', async () => {
  const hints = [];
  class AudioContext {
    constructor(options) { hints.push(options.latencyHint); this.state = 'running'; this.audioWorklet = { addModule: async () => {} }; }
    async resume() {} async close() {}
  }
  for (const q of ['?latency=foo', '?latency=0.5', '', '?latency=playback', '?latency=balanced']) {
    const p = await page(storage(), q, '', false, { AudioContext });
    await p.api.startAudio();
  }
  assert.deepEqual(hints, ['interactive', 'interactive', 'interactive', 'playback', 'balanced']);
});

test('a page whose list of firmwares or emulator does not load says so, with Start off', async () => {
  const cat = { default: 'a', firmwares: [{ id: 'a', name: 'a', version: '1', package: 'https://example.test/a.fwsc', author: 't', license: 'MIT', about: '' }] };
  const fetch = (missing) => async url => (url === missing ? { ok: false, status: 404 }
    : { ok: true, json: async () => (url === 'smk37/firmwares.json' ? { firmwares: [] } : cat), arrayBuffer: async () => new ArrayBuffer(0) });
  const down = async (extra) => { let shown; await assert.rejects(page(storage(), '', '', false, extra), e => (shown = e.elements, true)); return shown; };
  let e = await down({ fetch: fetch('firmwares.json') });
  assert.equal(e.get('status').textContent, 'the list of firmwares did not load (HTTP 404): reload the page to try again');
  assert.equal(e.get('start').disabled, true);
  e = await down({ fetch: fetch('fm1_web.wasm') });
  assert.equal(e.get('status').textContent, 'the emulator did not load (HTTP 404): reload the page to try again');
  e = await down({ WebAssembly: { instantiate: async () => { throw new Error('CompileError: bad magic'); } } });
  assert.equal(e.get('status').textContent, 'the emulator did not load (CompileError: bad magic): reload the page to try again');
  assert.equal(e.get('start').disabled, true);
});

test('Shift+Space is REC, Space PLAY, and R presses nothing (a slip among the black keys must not record)', async () => {
  const on = {};
  const p = await page(storage(), '', '', false, { addEventListener(type, fn) { on[type] = fn; }, HTMLSelectElement: class {} });
  await p.api.startAudio(); await p.api.boot(); await settle();
  const key = (type, key, code, shiftKey = false) => on[type]({ type, key, code, shiftKey, repeat: false, target: {}, preventDefault() {} });
  const sent = () => {   // the key messages since the last call: id, v down, ^ up
    const k = p.audio.messages.filter(m => m.type === 'key').map(m => `${m.id}${m.down ? 'v' : '^'}`);
    p.audio.messages = p.audio.messages.filter(m => m.type !== 'key');
    return k.join(' ');
  };
  key('keydown', 'r', 'KeyR'); key('keyup', 'r', 'KeyR');
  assert.equal(sent(), '');
  key('keydown', ' ', 'Space', true); key('keyup', ' ', 'Space');          // (Shift let go first)
  assert.equal(sent(), '13v 13^');
  key('keydown', ' ', 'Space'); key('keyup', ' ', 'Space', true);
  assert.equal(sent(), '12v 12^');
});

test('MASTER stays where it was turned to when the machine is replaced', async () => {
  const p = await page(); await p.api.startAudio(); await p.api.boot(); await settle();
  const pots = () => p.audio.messages.filter(m => m.type === 'pot').map(m => m.value);
  assert.deepEqual(pots(), []);                      // (untouched: the machine's own, which the page draws)
  p.api.setPot(300);
  p.api.setFirmware('b'); await p.api.boot(); await settle();
  assert.deepEqual(pots(), [300, 300]);
  assert.equal(p.audio.messages.at(-1).type, 'pot', 'after the boot, to the new machine');
});

test('?mhz= is a clock within those offered; anything else leaves the clock as it was, and the address says which runs', async () => {
  const clock = async (q, kept) => {
    const p = await page(storage(), q, '', false, kept ? { localStorage: { getItem: k => (k === 'fm1.mhz' ? kept : undefined), setItem() {} } } : {});
    await p.api.startAudio(); await p.api.boot();
    const mhz = p.audio.messages.find(m => m.type === 'boot').mhz;
    assert.equal(new URLSearchParams(p.location.search).get('mhz'), String(mhz), q);
    assert.ok(p.elements.get('mhz').options.some(o => o.value === String(mhz)), q);
    return mhz;
  };
  assert.equal(await clock('?mhz=160'), 160);
  assert.equal(await clock('?mhz=96'), 96);            // (between two of them: listed too)
  for (const bad of ['abc', '0', '-120', '59', '241', '700', '1e9', '100.5', 'Infinity', '']) assert.equal(await clock(`?mhz=${bad}`), 100, bad);
  assert.equal(await clock('?mhz=abc', '200'), 200);   // (the clock of the last visit)
  assert.equal(await clock('', 'NaN'), 100);
});

test('Big screen shows the screen again; Screen colours recolour it by brightness, on the panel too', async () => {
  const memory = { buffer: new ArrayBuffer(240 * 240 * 2) };
  const exports = { panel_board() {}, panel_width: () => 100, panel_height: () => 100, panel_theme_count: () => 1,
    panel_theme_name: () => 0, panel_theme() {}, panel_lcd: () => 0, memory };
  const p = await page(storage(), '', '', false, { WebAssembly: { instantiate: async () => ({ instance: { exports } }) } });
  const big = p.elements.get('big'), button = p.elements.get('bigscreen'), colours = p.elements.get('colours');
  assert.equal(big.hidden, true);
  button.listeners.click(); assert.equal(big.hidden, false);
  button.listeners.click(); assert.equal(big.hidden, true);
  assert.deepEqual(colours.options.map(o => o.value), ['', 'yellow', 'white', 'paper']);
  const { lcdRaw, showLcd } = p.api.screen, lcd = new Uint16Array(memory.buffer);
  lcdRaw.set([0xffff, 0x0000, 0x07e0, 0x0841]);   // white, black, full green, a near-black grey
  const show = (v) => { colours.value = v; colours.listeners.change(); showLcd(); return [...lcd.subarray(0, 4)]; };
  assert.deepEqual(show(''), [0xffff, 0x0000, 0x07e0, 0x0841]);
  assert.deepEqual(show('paper'), [0x0000, 0xffff, 0x0000, 0xffff]);
  assert.deepEqual(show('yellow'), [0xfee0, 0x0000, 0xfee0, 0x0000]);
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

test('a firmware out of time at its clock: the warning offers a higher one, and the advice never suggests that clock again', async () => {
  const p = await page(); await p.api.startAudio(); await p.api.boot(); await settle();
  // a second of audio: load 40% (busy 1000 for 100%), `starved` audio blocks the firmware left unfilled
  const second = (starved, busy = 400) => {
    p.api.perf.add({ busy, quanta: 345, frames: 128, emulated: 1000, idle: 0, worst: 1, starved });
    p.api.perf.second({ halted: false });
  };
  const warning = () => p.elements.get('perfwarn').hidden ? null : p.elements.get('perfwarn-text').textContent;
  for (let i = 0; i < 3; i++) second(9);             // (booting: not judged)
  second(9); second(0); second(9);
  assert.equal(warning(), null);                     // (seconds apart are not enough)
  second(9);
  assert.match(warning(), /^a runs out of time at 100 MHz: .* on any device\. The real FM-1 runs at 240 MHz\./);
  assert.equal(p.elements.get('perfwarn-fix').hidden, false);
  assert.equal(p.elements.get('perfwarn-fix').textContent, 'Use 120 MHz');
  assert.match(p.api.perf.report(), /^firmware out of time: 3 s, 27 audio blocks not computed in time/m);
  // later, on a device too slow at 240 MHz: the advice stays above the clock it ran out of time at
  await p.api.boot(); await settle();
  p.api.setMhz(240); p.api.perf.idleShare = 0.1;
  assert.equal(p.api.perf.lowerClock(3), 120);
  p.api.setFirmware('b');
  assert.equal(p.api.perf.lowerClock(3), 60);        // (another firmware: its own clocks)
  // out of time and the device overloaded too: no clock to offer
  p.api.setFirmware('a'); p.api.setMhz(100); await p.api.boot(); await settle();
  for (let i = 0; i < 5; i++) second(9, 1000);
  assert.match(warning(), /^a runs out of time at 100 MHz: .* A faster device or the desktop version will play it\./);
  assert.equal(p.elements.get('perfwarn-fix').hidden, true);
  // out of time at the top clock: no clock gives it enough, on any device
  p.api.setMhz(240); await p.api.boot(); await settle();
  for (let i = 0; i < 5; i++) second(9);
  assert.match(warning(), /^a runs out of time at 240 MHz: .* No higher clock gives it enough, the desktop version too\.$/);
  assert.equal(p.elements.get('perfwarn-fix').hidden, true);
  // two packages from this computer: each its own clocks
  const open = async (name) => {
    p.elements.get('open').files = [{ name, arrayBuffer: async () => new ArrayBuffer(960) }];
    p.elements.get('open').listeners.change(); await settle(); await settle();
  };
  await open('x.fwsc'); p.api.setMhz(100); await p.api.boot(); await settle();
  for (let i = 0; i < 5; i++) second(9);
  await open('y.fwsc'); p.api.setMhz(240);
  assert.equal(p.api.fw.name, 'y');
  assert.equal(p.api.perf.lowerClock(3), 60);
});

test("the catalog's minimum clock: the advice offers no clock under it, and only on the package it was measured on", async () => {
  const p = await page(); await p.api.startAudio();
  p.api.fw.minMhz = 160;
  p.api.fw.minMhzPackage = (await persistence.packageHash(new Uint8Array(960).fill(1))).slice(0, 16);   // (a's package)
  p.api.setMhz(240); await p.api.boot(); await settle();
  p.api.perf.idleShare = 0.1;
  assert.equal(p.api.perf.lowerClock(3), 160);
  // another build of the firmware: its own minimum is not known
  p.api.fw.minMhzPackage = 'ffffffffffffffff';
  assert.equal(p.api.perf.lowerClock(3), 60);
});

test("a firmware boots at the catalog's minimum for its package, and the next one at the clock the user picked", async () => {
  const p = await page(); await p.api.startAudio();
  const booted = () => p.audio.messages.filter(m => m.type === 'boot').at(-1).mhz;
  p.api.fw.minMhz = 160;
  p.api.fw.minMhzPackage = (await persistence.packageHash(new Uint8Array(960).fill(1))).slice(0, 16);   // (a's package)
  await p.api.boot(); await settle();
  assert.equal(booted(), 160);
  assert.equal(Number(p.elements.get('mhz').value), 160);
  assert.match(p.location.search, /mhz=100/);
  p.api.setFirmware('b'); await p.api.boot(); await settle();
  assert.equal(booted(), 100);
  assert.equal(Number(p.elements.get('mhz').value), 100);
  p.api.setFirmware('a'); p.api.setMhz(240); await p.api.boot(); await settle();
  assert.equal(booted(), 240);
  // another build of the firmware: its minimum is not known
  p.api.setMhz(100); p.api.fw.minMhzPackage = 'ffffffffffffffff'; await p.api.boot(); await settle();
  assert.equal(booted(), 100);
});

test("a try-out boots with its sectors and script from this origin, starts by itself and saves nothing", async () => {
  const disk = storage(), fetched = [], told = [];
  const sector = new Uint8Array(4100); new DataView(sector.buffer).setUint32(0, 0x96000, true); sector.fill(7, 4);
  const files = { 'http://localhost/dock/tryit/m.flash': sector, 'http://localhost/dock/tryit/m.txt': '1500:mf07d4842f7\n' };
  const fetch = async (url, init) => {
    fetched.push([url, init]);
    if (url in files) return { ok: true, arrayBuffer: async () => files[url].buffer, text: async () => files[url] };
    return { ok: true, json: async () => url === 'smk37/firmwares.json' ? { firmwares: [] } : { default: 'a', firmwares: ['a', 'b'].map(id => ({ id, name: id, version: '1', package: `https://example.test/${id}.fwsc`, author: 't', license: 'MIT', about: '' })) },
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
  const fetch = async (url) => { fetched.push(url); return { ok: true, json: async () => (url === 'smk37/firmwares.json' ? { firmwares: [] } : { default: 'a', firmwares: [{ id: 'a', name: 'a', version: '1', package: 'https://example.test/a.fwsc', author: 't', license: 'MIT', about: '' }] }), arrayBuffer: async () => new ArrayBuffer(0) }; };
  for (const q of ['?flash=https://evil.test/m.flash', '?script=//evil.test/m.txt', '?flash=data:,x']) {
    const p = await page(storage(), q, '', false, { fetch });
    await settle();
    assert.equal(p.audio, base.audio, q);          // (no audio graph made: nothing started)
    assert.match(p.elements.get('status').textContent, /^try-out: only files from http:\/\/localhost/, q);
  }
  assert.ok(fetched.every(u => !/evil|data:/.test(u)));
});

test("the Hub's newcomers come last, marked as not yet run here, and ?fw= picks one", async () => {
  const c = { id: 'c', name: 'Newcomer', version: '2', package: 'https://thefirmwarehub.com/fw/c-2.fwsc', hub: 'c',
    author: 't', license: 'GPL-3.0', about: 'new', untested: true };
  const p = await page(storage(), '?fw=c', '', false, { hubNewcomers: async () => [c] });
  assert.equal(p.api.fw.id, 'c');
  assert.equal(p.api.fw.untested, true);
  // the selector's plain options are the catalog's; the newcomer is in its group, not among them
  assert.deepEqual(p.elements.get('fw').options.map(o => o.value), ['a', 'b']);
  // a Hub that did not answer: the page as before
  const q = await page(storage(), '?fw=c', '', false, { hubNewcomers: async () => [] });
  assert.equal(q.api.fw.id, 'a', 'the default');
});
test('?package= plays a package from a link, named by &name=, and a reload fetches the link again', async () => {
  const fetched = [];
  const fetchPackage = async f => { fetched.push(f.package); return { bytes: new Uint8Array(960).fill(f.package.length % 250), url: f.package }; };
  const url = 'https://mutant.cloud/dl/fomajam/fomajam-1.0.fwsc';
  const p = await page(storage(), `?package=${encodeURIComponent(url)}&name=Fomajam`, '', false, { fetchPackage });
  assert.equal(p.api.fw.id, 'link');
  assert.equal(p.api.fw.name, 'Fomajam');
  assert.equal(p.elements.get('fw').options[0].value, 'link');
  assert.match(p.location.search, /package=/);
  await p.api.startAudio(); await p.api.boot(); await settle();
  assert.deepEqual(fetched, [url]);
  // another link on the same browser: its own package, not the last link's kept one
  const other = 'https://example.test/other.fwsc';
  const q = await page(p.disk, `?package=${encodeURIComponent(other)}`, '', false, { fetchPackage });
  assert.equal(q.api.fw.name, 'other');
  await q.api.startAudio(); await q.api.boot();
  assert.deepEqual(fetched, [url, other]);
  // not https: ignored, the page as without it
  const r = await page(storage(), '?package=http%3A%2F%2Fexample.test%2Fx.fwsc');
  assert.equal(r.api.fw.id, 'a');
});
