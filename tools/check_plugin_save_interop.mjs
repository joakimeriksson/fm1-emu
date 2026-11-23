// Test the real browser codec against the real plugin engine in both directions.
// node tools/check_plugin_save_interop.mjs [browser-persistence-module] [firmware.fwsc]
// (the module: web/persistence.js, or another build's copy of it)
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';
const root = fileURLToPath(new URL('../', import.meta.url));
const codec = resolve(process.argv[2] || join(root, 'web/persistence.js'));
const firmware = resolve(process.argv[3] || join(root, 'reference/firmwares/felucca.fwsc'));
const { packageHash, exportSave, importSave } = await import(pathToFileURL(codec));
const dir = mkdtempSync(join(tmpdir(), 'fm1-save-interop-'));
try {
  const pkg = new Uint8Array(readFileSync(firmware));
  const original = { name: 'Browser to plugin test', package: pkg, hash: await packageHash(pkg),
    sectors: [{ at: 0xff000, data: new Uint8Array(4096).fill(0x56) }] };
  const input = join(dir, 'browser.fm1save'), output = join(dir, 'plugin.fm1save');
  writeFileSync(input, exportSave(original));
  const run = spawnSync('cargo', ['test', '--manifest-path', join(root, 'crates/fm1-plugin/Cargo.toml'),
    '--release', '--locked', '--test', 'portable', 'portable_save_round_trip_and_same_package_replacement', '--', '--exact'], {
    cwd: root, encoding: 'utf8', timeout: 180000,
    env: { ...process.env, FM1_TEST_SAVE_INPUT: input, FM1_TEST_SAVE_OUTPUT: output },
  });
  if (run.error || run.status !== 0) throw new Error(`${run.error || 'plugin test failed'}\n${run.stdout}\n${run.stderr}`);
  const restored = await importSave(readFileSync(output, 'utf8'));
  assert.equal(restored.hash, original.hash);
  assert.equal(restored.name, original.name);
  assert.deepEqual(restored.package, original.package);
  assert.deepEqual(restored.sectors, original.sectors);
  console.log('PASS: browser export → plugin engine load/snapshot/export → browser import; firmware and saved sectors match byte-for-byte.');
} finally {
  rmSync(dir, { recursive: true, force: true });
}
