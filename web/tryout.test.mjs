// ?flash= / ?script= (web/tryout.js): this origin's files only, sector files checked as saves are.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { sameOrigin, tryOutOf, parseSectors, scriptEnd, loadTryOut, MAX_SCRIPT } from './tryout.js';

const PAGE = 'https://joakimeriksson.github.io/fm1-emu/?embed=1&fw=fm1dock';
const record = (at, fill) => {
  const b = new Uint8Array(4100);
  new DataView(b.buffer).setUint32(0, at, true);
  b.fill(fill, 4);
  return b;
};
const join = (...parts) => {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let o = 0;
  for (const p of parts) { out.set(p, o); o += p.length; }
  return out;
};

test('paths and URLs on the page origin are taken; other sites and schemes are not', () => {
  assert.equal(sameOrigin('/fm1-dock/tryit/monk.flash', PAGE).href, 'https://joakimeriksson.github.io/fm1-dock/tryit/monk.flash');
  assert.equal(sameOrigin('../fm1-dock/tryit/monk.txt', PAGE).pathname, '/fm1-dock/tryit/monk.txt');
  assert.equal(sameOrigin('https://joakimeriksson.github.io/x', PAGE).pathname, '/x');
  for (const bad of ['https://example.com/m.flash', '//example.com/m.flash', 'http://joakimeriksson.github.io/m.flash',
    'https://joakimeriksson.github.io.evil.test/m.flash', 'data:application/octet-stream,AAAA', 'javascript:alert(1)',
    'blob:https://joakimeriksson.github.io/0b0f', 'file:///etc/passwd']) {
    assert.throws(() => sameOrigin(bad, PAGE), /only files from https:\/\/joakimeriksson\.github\.io/, bad);
  }
});

test('a page without the parameters has no try-out; either parameter makes one', () => {
  assert.equal(tryOutOf(new URLSearchParams('fw=fm1dock'), PAGE), null);
  const t = tryOutOf(new URLSearchParams('flash=/a.flash'), PAGE);
  assert.equal(t.flash.pathname, '/a.flash');
  assert.equal(t.script, null);
  assert.throws(() => tryOutOf(new URLSearchParams('flash=/a.flash&script=https://example.com/x'), PAGE));
});

test('sector files: whole records, aligned, inside the flash, no sector twice', () => {
  const s = parseSectors(join(record(0x96000, 1), record(0x93000, 2)));
  assert.deepEqual(s.map((x) => [x.at, x.data[0], x.data.length]), [[0x96000, 1, 4096], [0x93000, 2, 4096]]);
  for (const bad of [new Uint8Array(0), new Uint8Array(4099), record(0x96001, 0), record(1 << 20, 0),
    join(record(0x1000, 0), record(0x1000, 1)), new Uint8Array(257 * 4100)]) {
    assert.throws(() => parseSectors(bad));
  }
});

test('the script end is its latest time', () => {
  assert.equal(scriptEnd('1500:mf07d4842220001f7, 200:21,3000:21^'), 3000);
  assert.equal(scriptEnd(''), 0);
});

test('loading fetches without credentials elsewhere or redirects, and refuses what is wrong', async () => {
  const seen = [];
  const files = { '/t.flash': record(0x96000, 5), '/t.txt': ' 1500:mf0f7 \n' };
  const fetchFn = async (url, init) => {
    seen.push(init);
    const f = files[new URL(url).pathname];
    return f === undefined ? { ok: false, status: 404 } : { ok: true, arrayBuffer: async () => f.buffer, text: async () => f };
  };
  const got = await loadTryOut(tryOutOf(new URLSearchParams('flash=/t.flash&script=/t.txt'), PAGE), fetchFn);
  assert.equal(got.sectors[0].at, 0x96000);
  assert.equal(got.script, '1500:mf0f7');
  assert.equal(got.end, 1500);
  assert.ok(seen.every((i) => i.redirect === 'error' && i.credentials === 'same-origin'));
  await assert.rejects(loadTryOut(tryOutOf(new URLSearchParams('flash=/none.flash'), PAGE), fetchFn), /none\.flash: HTTP 404/);
  files['/big.txt'] = 'x'.repeat(MAX_SCRIPT + 1);
  await assert.rejects(loadTryOut(tryOutOf(new URLSearchParams('script=/big.txt'), PAGE), fetchFn), /larger than/);
});
