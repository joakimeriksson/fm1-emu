// fetchPackage: a Hub package that has gone (the Hub removes a firmware's old package when a new
// version comes) is found again through the Hub's catalog.json.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fetchPackage, hubNewcomers } from './fwsc.js';

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
