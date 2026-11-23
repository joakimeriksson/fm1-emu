// Actual WASM round trip, with a local .fwsc or the same Felucca fetch used by smoke.mjs.
// node web/persistence.wasm.mjs [web/dist] [firmware.fwsc]
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
const dir = new URL(`${process.argv[2] || 'web/dist'}/`, `file://${process.cwd()}/`);
const { fetchPackage } = await import(new URL('fwsc.js', dir));
const { jitImports } = await import(new URL('jit.js', dir));
const { changedSectors, packSectors, exportSave, importSave, packageHash } = await import(new URL('persistence.js', dir));
const cat = JSON.parse(readFileSync(new URL('firmwares.json', dir)));
const bytes = process.argv[3] ? new Uint8Array(readFileSync(process.argv[3]))
  : (await fetchPackage(cat.firmwares.find(f => f.id === 'felucca'))).bytes;
const module = await WebAssembly.compile(readFileSync(new URL('fm1_web.wasm', dir)));
async function boot(sectors = []) {
  let x;
  x = (await WebAssembly.instantiate(module, { env: jitImports(() => x) })).exports;
  x.fm1_jit(1, 16);
  const copy = bytes => {
    const ptr = x.fm1_alloc(bytes.length);
    new Uint8Array(x.memory.buffer, ptr, bytes.length).set(bytes);
    return ptr;
  };
  assert.equal(x.fm1_boot_fwsc(copy(bytes), bytes.length, 0, 0, 100), 0);
  const packed = packSectors(sectors);
  if (packed.length) assert.equal(x.fm1_flash_restore(copy(packed), packed.length), 0);
  return { x, copy, flash: () => new Uint8Array(x.memory.buffer, x.fm1_flash(), x.fm1_flash_len()).slice(),
    base: () => new Uint8Array(x.memory.buffer, x.fm1_flash_base(), x.fm1_flash_len()).slice() };
}
const first = await boot();
const pristineXip = new Uint8Array(first.x.memory.buffer, first.x.fm1_xip(), first.x.fm1_xip_len()).slice();
const run = seconds => {
  for (let frame = 0; frame < seconds * 44100; frame += 128) first.x.fm1_render(128, 50_000_000);
};
run(2);
assert.equal(first.x.fm1_halted(), 0);
const current = first.flash();
// Also include a known change and erased sector, independent of the firmware's UI behavior.
current[0x74000] ^= 0x5a;
current.fill(0xff, 0x75000, 0x76000);
const record = { name: 'round-trip', hash: await packageHash(bytes), package: bytes,
  sectors: changedSectors(first.base(), current) };
const restored = await importSave(exportSave(record));
const second = await boot(restored.sectors);
assert.deepEqual(second.flash(), current);
assert.deepEqual(second.base(), first.base());
// A saved sector represents a runtime write: every overlapping XIP byte must match
// raw flash, even when that byte equals the original encrypted package byte.
const expectedXip = pristineXip.slice();
for (const s of restored.sectors) {
  const from = Math.max(s.at, 0x4000), end = Math.min(s.at + 4096, 0x4000 + expectedXip.length);
  if (from < end) expectedXip.set(s.data.subarray(from - s.at, end - s.at), from - 0x4000);
}
assert.deepEqual(new Uint8Array(second.x.memory.buffer, second.x.fm1_xip(), second.x.fm1_xip_len()), expectedXip);
// Invalid input is rejected atomically, including duplicate sectors and late restores.
const invalid = new Uint8Array(4100); new DataView(invalid.buffer).setUint32(0, 1, true);
assert.equal(second.x.fm1_flash_restore(second.copy(invalid), invalid.length), 1);
assert.deepEqual(second.flash(), current);
second.x.fm1_render(128, 50_000_000);
const packed = packSectors(restored.sectors);
assert.equal(second.x.fm1_flash_restore(second.copy(packed), packed.length), 1);
console.log(`WASM flash round trip passed: ${restored.sectors.length} sectors; rendered firmware and restored raw flash plus XIP byte-for-byte`);
