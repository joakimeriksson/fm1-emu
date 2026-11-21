// fetchPackage: a Hub package that has gone (the Hub removes a firmware's old package when a new
// version comes) is found again through the Hub's catalog.json.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fetchPackage, hubNewcomers, httpsUrl } from './fwsc.js';

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
  // the package fallback takes no other scheme from the Hub either
  await assert.rejects(fetchPackage({ id: 'e', package: 'https://thefirmwarehub.com/fw/e-0.fwsc', hub: 'e' },
    async (url) => (url.endsWith('catalog.json') ? new Response(JSON.stringify(hub)) : new Response('', { status: 404 }))), /e-0\.fwsc: 404/);
  assert.equal(httpsUrl('fw/x.fwsc', 'https://thefirmwarehub.com/'), 'https://thefirmwarehub.com/fw/x.fwsc');
  assert.equal(httpsUrl(undefined), null);
});
