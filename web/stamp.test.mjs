// The built page's files under one build's name (web/stamp.mjs): what the pages and scripts load
// of their own gets ?v=<build>, and nothing else changes.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stamp } from './stamp.mjs';

const read = (f) => readFileSync(new URL(f, import.meta.url), 'utf8');

test('the page, its scripts and the worklet ask for one build', () => {
  const page = stamp(read('index.html'), 'b1');
  assert.match(page, /<link rel="stylesheet" href="style\.css\?v=b1">/);
  assert.match(page, /<script type="module" src="main\.js\?v=b1"><\/script>/);
  assert.match(stamp(read('smk37/index.html'), 'b1'), /src="main\.js\?v=b1"/);
  const main = stamp(read('main.js'), 'b1');
  for (const s of ['from "./fwsc.js?v=b1"', 'from "./persistence.js?v=b1"', 'from "./tryout.js?v=b1"',
    'fetch("fm1_web.wasm?v=b1")', 'fetch("firmwares.json?v=b1")', 'addModule("worklet.js?v=b1")'])
    assert.ok(main.includes(s), s);
  // persistence.js the same module from main.js and tryout.js: one URL
  assert.ok(stamp(read('tryout.js'), 'b1').includes("from './persistence.js?v=b1'"));
});

test('every own reference is stamped, and only those', () => {
  for (const f of ['index.html', 'smk37/index.html', 'main.js', 'tryout.js', 'persistence.js', 'fwsc.js', 'worklet.js']) {
    const out = stamp(read(f), 'b1');
    // nothing left that loads a file of the page without the build
    assert.doesNotMatch(out, /\b(?:from|import\(|fetch\(|addModule\()\s*["'](?:\.\/)?[\w./-]+\.(?:js|wasm|json|css)["']/, f);
    assert.doesNotMatch(out, /<script\b[^>]*\bsrc="[\w.-]+\.js"/, f);
    // remove the stamps: the file as it was
    assert.equal(out.replaceAll('?v=b1', ''), read(f), f);
  }
  // other sites' files, and URLs built at run time, stay as they are
  const other = 'fetch("https://thefirmwarehub.com/fw/x.json"); fetch(url); import("https://x/y.js"); <a href="x.json">';
  assert.equal(stamp(other, 'b1'), other);
  assert.throws(() => stamp('', 'a b'), /build name/);
});
