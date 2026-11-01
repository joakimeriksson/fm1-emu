// The host side of the WebAssembly JIT (crates/fm1-core/src/jit_wasm.rs): a hot block arrives
// as a small module, which is compiled here synchronously and whose `run` goes into the
// emulator's exported function table; Rust then calls it by that index, with no JavaScript in
// between. Returns the index, or 0 when it cannot (the block stays interpreted).
// (worklet.js carries its own copy: an AudioWorklet does not import modules everywhere)
export function jitImports(exports, stats) {
  let next;
  return {
    fm1_jit_compile(ptr, len, hRead, hWrite, hOp) {
      const start = performance.now();
      try {
        const x = exports();
        const bytes = new Uint8Array(x.memory.buffer, ptr, len).slice();
        const table = x.__indirect_function_table;
        // the helpers as function imports, from the table (a module importing the table itself
        // gets a copy of it, grown and updated with every region: 40% of a two-core firmware's
        // time); the table grows by a chunk, the emulator's own instance being its only user
        const env = { memory: x.memory, h_read: table.get(hRead), h_write: table.get(hWrite), h_op: table.get(hOp) };
        const run = new WebAssembly.Instance(new WebAssembly.Module(bytes), { env }).exports.run;
        if (next === undefined || next >= table.length) { next = table.length; table.grow(256); }
        const slot = next++;
        table.set(slot, run);
        if (stats) { stats.regions++; stats.bytes += len; }
        return slot;
      } catch (e) {
        if (stats) stats.failed++;
        return 0;
      } finally {
        if (stats) {
          const elapsed = performance.now() - start;
          stats.compileMs += elapsed;
          stats.compileMaxMs = Math.max(stats.compileMaxMs, elapsed);
        }
      }
    },
  };
}
