// The wasm JIT's inline FPU ops against the interpreter, in this engine (jit_wasm.rs
// `fop_selftest`): every op on the edge values and random words. Needs the wasm build
// (web/build.sh, or FM1_WASM=another build); skipped without one.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { jitImports } from './jit.js';

const wasm = process.env.FM1_WASM || new URL('../target/wasm/wasm32-unknown-unknown/release/fm1_web.wasm', import.meta.url);

test('the inline FPU ops leave what the interpreter leaves', { skip: !existsSync(wasm) && 'no wasm build' }, async () => {
  let x;
  const stats = { regions: 0, failed: 0, compileMs: 0, compileMaxMs: 0, bytes: 0 };
  const { instance } = await WebAssembly.instantiate(readFileSync(wasm), { env: jitImports(() => x, stats) });
  x = instance.exports;
  const fails = x.fm1_jit_selftest();
  const p = x.fm1_jit_selftest_detail(), bytes = new Uint8Array(x.memory.buffer);
  const detail = new TextDecoder().decode(bytes.subarray(p, bytes.indexOf(0, p)));
  assert.equal(stats.failed, 0, 'every module compiles');
  assert.equal(fails, 0, detail);
});
