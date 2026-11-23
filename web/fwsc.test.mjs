// fetchPackage: a Hub package that has gone (the Hub removes a firmware's old package when a new
// version comes) is found again through the Hub's catalog.json.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fetchPackage, hubNewcomers, httpsUrl } from './fwsc.js';
import { MAX_PACKAGE } from './persistence.js';

test('a Hub package that has gone is fetched as the one the Hub lists now', async () => {
  const asked = [];
  const files = {
    'https://thefirmwarehub.com/catalog.json': JSON.stringify({ firmwares: [{ id: 'sloop', version: '2.7.1', pkg: 'fw/sloop-2.7.1.fwsc' }] }),
    'https://thefirmwarehub.com/fw/sloop-2.7.1.fwsc': 'new',
  };
  const fetchFn = async (url) => {
    asked.push(url);
    const body = files[url];
    return body == null ? new Response('', { status: 404 }) : new Response(body);
  };
  const entry = { id: 'sloop', package: 'https://thefirmwarehub.com/fw/sloop-2.6.fwsc', hub: 'sloop' };
  const got = await fetchPackage(entry, fetchFn);
  assert.equal(got.url, 'https://thefirmwarehub.com/fw/sloop-2.7.1.fwsc');
  assert.equal(new TextDecoder().decode(got.bytes), 'new');
  assert.equal(got.version, '2.7.1', "the version that runs, not the entry's");
  await assert.rejects(fetchPackage({ id: 'x', package: 'https://thefirmwarehub.com/fw/x-1.fwsc', hub: 'x' }, fetchFn), /x-1\.fwsc: 404/);
  // the Hub's catalog is read once
  assert.equal(asked.filter((u) => u.endsWith('catalog.json')).length, 1);
});

test('a failed Hub catalog is asked again', async () => {
  let up = false;
  const fetchFn = async (url) => {
    if (url.endsWith('catalog.json')) {
      return up ? new Response(JSON.stringify({ firmwares: [{ id: 'skiff', version: '1.4', pkg: 'fw/skiff-1.4.fwsc' }] }))
        : new Response('', { status: 503 });
    }
    return url.endsWith('skiff-1.4.fwsc') ? new Response('ok') : new Response('', { status: 404 });
  };
  const entry = { id: 'skiff', package: 'https://thefirmwarehub.com/fw/skiff-1.3.3.fwsc', hub: 'skiff' };
  await assert.rejects(fetchPackage(entry, fetchFn), /skiff-1\.3\.3\.fwsc: 404/);
  up = true;
  assert.equal((await fetchPackage(entry, fetchFn)).version, '1.4');
});

test("the Hub's newcomers: listed with a package, not in the catalog by either id, not skipped", async () => {
  const hub = { firmwares: [
    { id: 'sloop', version: '2.8', pkg: 'fw/sloop-2.8.fwsc' },                  // ours
    { id: 'fm1sid', version: '0.3', pkg: 'fw/fm1sid-0.3.fwsc' },               // ours under another id
    { id: 'lsdjburrito', version: '1', pkg: 'fw/lsdjburrito-1.fwsc' },         // skipped
    { id: 'cosmos', version: '0.1' },                                           // no package yet
    { id: 'caravel', version: '0.12.0', pkg: 'fw/caravel-0.11.0.fwsc', name: 'Caravel', author: 'BonszaiMusic',
      repo: 'bonszaimusic/caravel', lic: 'GPL-3.0', what: 'A multi-engine synth.' },
  ] };
  let asked = 0;
  const fetchFn = async (url) => { asked++; return url.endsWith('catalog.json') ? new Response(JSON.stringify(hub)) : new Response('', { status: 404 }); };
  const catalog = { skipped: { lsdjburrito: 'no audio until a song is sent' }, firmwares: [
    { id: 'sloop', hub: 'sloop' }, { id: 'sid', hub: 'fm1sid' }, { id: 'mvave' },
  ] };
  const got = await hubNewcomers(catalog, fetchFn);
  assert.deepEqual(got.map((f) => f.id), ['caravel']);
  assert.equal(got[0].version, '0.11.0', "the package's version, not the Hub's release tag");
  assert.equal(got[0].package, 'https://thefirmwarehub.com/fw/caravel-0.11.0.fwsc');
  assert.equal(got[0].repo, 'https://github.com/bonszaimusic/caravel');
  assert.equal(got[0].untested, true);
  // the same read serves a later package fallback
  await fetchPackage({ id: 'sloop', package: 'https://thefirmwarehub.com/fw/sloop-2.7.fwsc', hub: 'sloop' }, fetchFn).catch(() => {});
  assert.equal(asked, 1 + 2, 'one catalog read, then the old and the new package');
  // a Hub that cannot be read: nothing new, the page as it is
  assert.deepEqual(await hubNewcomers(catalog, async () => new Response('', { status: 503 })), []);
});

test("a newcomer's links are https: another scheme in the Hub's catalog is no link, and no package", async () => {
  const hub = { firmwares: [
    { id: 'a', pkg: 'fw/a-1.fwsc', site: 'javascript:alert(document.domain)' },
    { id: 'b', pkg: 'fw/b-1.fwsc', site: 'http://b.example/' },
    { id: 'c', pkg: 'fw/c-1.fwsc', site: 'https://c.example/synth' },
    { id: 'd', pkg: 'fw/d-1.fwsc', site: 'not a url' },
    { id: 'e', pkg: 'javascript:alert(1)//e-1.fwsc', site: 'https://e.example/' },
    { id: 'f', pkg: 'http://[', site: 'https://f.example/' },
    { id: 'g', pkg: 'https://cdn.example/g-1.fwsc' },
  ] };
  const fetchFn = async () => new Response(JSON.stringify(hub));
  const got = await hubNewcomers({ firmwares: [] }, fetchFn);
  assert.deepEqual(got.map((f) => [f.id, f.repo]), [
    ['a', 'https://thefirmwarehub.com/#fw-a'], ['b', 'https://thefirmwarehub.com/#fw-b'], ['c', 'https://c.example/synth'],
    ['d', 'https://thefirmwarehub.com/#fw-d'], ['g', 'https://thefirmwarehub.com/#fw-g']]);
  assert.equal(got.at(-1).package, 'https://cdn.example/g-1.fwsc');
  // the package fallback takes no other scheme from the Hub either: it is not asked for
  const asked = [];
  await assert.rejects(fetchPackage({ id: 'e', package: 'https://thefirmwarehub.com/fw/e-0.fwsc', hub: 'e' },
    async (url) => { asked.push(url); return url.endsWith('catalog.json') ? new Response(JSON.stringify(hub)) : new Response('', { status: 404 }); }), /e-0\.fwsc: 404/);
  assert.deepEqual(asked, ['https://thefirmwarehub.com/fw/e-0.fwsc', 'https://thefirmwarehub.com/catalog.json']);
  assert.equal(httpsUrl('fw/x.fwsc', 'https://thefirmwarehub.com/'), 'https://thefirmwarehub.com/fw/x.fwsc');
  assert.equal(httpsUrl(undefined), null);
});

test("a Hub catalog of another shape gives the newcomers it can, or none: the page waits for them while loading", async () => {
  const from = (hub) => hubNewcomers({ firmwares: [] }, async () => new Response(JSON.stringify(hub)));
  for (const hub of [null, 7, [], {}, { firmwares: null }, { firmwares: 'none' }, { firmwares: { a: { id: 'a', pkg: 'fw/a-1.fwsc' } } }]) {
    assert.deepEqual(await from(hub), [], JSON.stringify(hub));
  }
  // entries that are no firmware (not an object, no id to pick it by) are left out; the others are listed
  const got = await from({ firmwares: [null, 'x', 3, [], { pkg: 'fw/y-1.fwsc' }, { id: 5, pkg: 'fw/5-1.fwsc' }, { id: 'ok', pkg: 'fw/ok-1.fwsc' }] });
  assert.deepEqual(got.map((f) => [f.id, f.package]), [['ok', 'https://thefirmwarehub.com/fw/ok-1.fwsc']]);
});

test('a package larger than a package can be is refused: by its stated length, else while it is read', async () => {
  const entry = { id: 'x', package: 'https://example.test/x.fwsc' };
  // the server says how long: nothing is read, and the response is let go of (its connection would stay open)
  let read = 0, dropped = false;
  const stated = new Response(new ReadableStream({ pull(c) { read++; c.enqueue(new Uint8Array(1)); c.close(); }, cancel() { dropped = true; } }, { highWaterMark: 0 }),
    { headers: { 'content-length': String(MAX_PACKAGE + 1) } });
  await assert.rejects(fetchPackage(entry, async () => stated), /x\.fwsc: larger than 8 MB/);
  assert.equal(read, 0);
  await new Promise(setImmediate);
  assert.equal(dropped, true);
  await assert.rejects(fetchPackage(entry, async () => ({ ok: true, headers: new Headers({ 'content-length': String(MAX_PACKAGE + 1) }) })), /larger than 8 MB/);
  // it does not say: the reading stops once past the limit (64 KiB at a time here, without end)
  let sent = 0, cancelled = false;
  const endless = new Response(new ReadableStream({ pull(c) { sent += 65536; c.enqueue(new Uint8Array(65536)); }, cancel() { cancelled = true; } },
    { highWaterMark: 0 }));
  await assert.rejects(fetchPackage(entry, async () => endless), /larger than 8 MB/);
  assert.ok(sent > MAX_PACKAGE && sent <= MAX_PACKAGE + 2 * 65536, `${sent} bytes read`);
  await new Promise(setImmediate);
  assert.equal(cancelled, true);
  // one of the limit's size comes whole, in the order it was sent; a response with no stream to read too
  const whole = new Uint8Array(MAX_PACKAGE).map((_, i) => i >> 16);
  let at = 0;
  const chunks = new Response(new ReadableStream({ pull(c) { at < whole.length ? c.enqueue(whole.slice(at, at += 1 << 20)) : c.close(); } }));
  assert.deepEqual((await fetchPackage(entry, async () => chunks)).bytes, whole);
  assert.deepEqual((await fetchPackage(entry, async () => ({ ok: true, arrayBuffer: async () => new Uint8Array([1, 2, 3]).buffer }))).bytes, new Uint8Array([1, 2, 3]));
  await assert.rejects(fetchPackage(entry, async () => ({ ok: true, arrayBuffer: async () => new ArrayBuffer(MAX_PACKAGE + 1) })), /larger than 8 MB/);
});
