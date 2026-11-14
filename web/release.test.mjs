// The page's release blocks (web/release.mjs): the version in Cargo.toml has its notes, and the
// notes give the page what it shows (a release whose notes the page cannot read fails here, before
// web/build.sh would).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { readRelease, parseNotes, render, apply, announcement, inline, workspaceVersion, PAGE } from './release.mjs';

const root = new URL('../', import.meta.url);
const read = (path) => readFileSync(new URL(path, root), 'utf8');

test('the version in Cargo.toml has notes the page can show', () => {
  const r = readRelease(root);
  assert.equal(r.version, workspaceVersion(read('Cargo.toml')));
  assert.equal(r.version, workspaceVersion(read('crates/fm1-plugin/Cargo.toml').replace('[package]', '[workspace.package]')));
  assert.ok(r.highlights.length >= 1, 'a "## What\'s new" list');
  assert.ok(r.downloads.some((d) => d.kind === 'plugin'), 'the plugin in "## Downloads"');
  assert.ok(r.downloads.some((d) => d.kind === 'live'), 'fm1-live in "## Downloads"');
  for (const d of r.downloads) {
    assert.ok(d.file.includes(`-${r.version}-`), `${d.file} is ${r.version}'s`);
    assert.equal(d.url, `https://github.com/joakimeriksson/fm1-emu/releases/download/v${r.version}/${d.file}`);
    assert.notEqual(d.os, 'other', `${d.file}: a platform the page can name`);
  }
});

test('every release\'s notes read cleanly', () => {
  for (const f of readdirSync(new URL('docs/releases/', root)).filter((f) => /^v[\d.]+\.md$/.test(f))) {
    const version = f.slice(1, -3);
    const r = parseNotes(read(`docs/releases/${f}`), version);
    assert.ok(r.highlights.length >= 1 && r.downloads.length >= 1, f);
    for (const h of r.highlights) {
      assert.doesNotMatch(h.text, /\*\*|`|\]\(|\(#\d+/, `${f}: plain text: ${h.text}`);
      assert.doesNotMatch(h.html, /\*\*|`|\]\(/, `${f}: html: ${h.html}`);
    }
    assert.doesNotMatch(r.leadHtml, /joakimeriksson\.github\.io/, `${f}: the lead does not send the page to itself`);
  }
});

test('0.5.1 as the page shows it', () => {
  const r = parseNotes(read('docs/releases/v0.5.1.md'), '0.5.1');
  assert.deepEqual(r.thanks, ['bask209', 'eubide', 'pcace']);
  // the short "## Highlights" list, not the longer what's new
  assert.match(r.highlights[0].html, /^<b>fm1-live for Windows<\/b>: double-click it and pick a firmware; F4 opens another\.$/);
  assert.equal(r.highlights.length, 7);
  assert.deepEqual(r.downloads.map((d) => `${d.kind} ${d.os} ${d.arch}${d.isNew ? ' new' : ''}`), [
    'plugin macos arm64', 'plugin windows x86_64', 'live windows x86_64 new', 'live macos arm64', 'live linux arm64', 'live linux x86_64']);
  const text = announcement({ ...r, version: '0.5.1', url: 'https://github.com/joakimeriksson/fm1-emu/releases/tag/v0.5.1' });
  const lines = text.split('\n');
  assert.match(lines[0], /^FM-1 Emulator 0\.5\.1: /);
  assert.equal(lines.filter((l) => l.startsWith('• ')).length, r.highlights.length);
  assert.ok(lines.includes(`Play it in the browser: ${PAGE}`));
  assert.ok(lines.includes('Downloads and release notes: https://github.com/joakimeriksson/fm1-emu/releases/tag/v0.5.1'));
  assert.equal(lines.at(-1), 'Thanks to bask209, eubide and pcace.');
});

test('the downloads: plugin macOS, Windows; fm1-live Windows, macOS, Linux x86-64, Linux ARM64', () => {
  const html = render(readRelease(root)).section;
  const order = [...html.matchAll(/class="dl-btn" href="[^"]*\/(fm1-[^"/]+)"/g)].map((m) => m[1].replace(/-[\d.]+-/, ' ').replace(/\..*/, ''));
  assert.deepEqual(order, ['fm1-plugin macos-arm64', 'fm1-plugin windows-x86_64', 'fm1-emu windows-x86_64', 'fm1-emu macos-arm64',
    'fm1-emu linux-x86_64', 'fm1-emu linux-arm64']);
});

test('the film shows once the notes link it, and not before', () => {
  const md = read('docs/releases/v0.5.1.md');
  const base = { version: '0.5.1', tag: 'v0.5.1', url: 'u', notesPath: 'docs/releases/v0.5.1.md' };
  const without = render({ ...base, ...parseNotes(md, '0.5.1') }).section;
  assert.doesNotMatch(without, /data-yt|<iframe|ytimg/);
  const withFilm = render({ ...base, ...parseNotes(md + '\nThe release film: https://youtu.be/AbC_dEf-123\n', '0.5.1') }).section;
  assert.match(withFilm, /<a class="film" href="https:\/\/www\.youtube\.com\/watch\?v=AbC_dEf-123" data-yt="AbC_dEf-123"/);
  assert.equal(parseNotes('x\n\n[film](https://www.youtube.com/watch?v=Zz9_-Zz9_-Z&t=3)', '1').film, 'Zz9_-Zz9_-Z');
});

test('without a Highlights list, what\'s new is the list', () => {
  const md = 'Lead.\n\n## What\'s new\n\n- **One**: a thing (#1).\n- **Two**: another.\n- **Contributors:** a, b and c.\n';
  const r = parseNotes(md, '9.9.9');
  assert.deepEqual(r.highlights.map((h) => h.text), ['One: a thing.', 'Two: another.']);
  assert.deepEqual(r.thanks, ['a', 'b', 'c']);
});

test('inline Markdown: asides and pull requests go, code and links stay', () => {
  assert.equal(inline('**A** (#1, x): `?a=(b)` and [d](../D.md) (e (f)).', 'text'), 'A: ?a=(b) and d.');
  assert.equal(inline('**A** (#1): `<x>` [d](../D.md) & e'),
    '<b>A</b>: <code>&lt;x&gt;</code> <a href="https://github.com/joakimeriksson/fm1-emu/blob/master/docs/D.md">d</a> &amp; e');
});

test('the pages carry the release blocks, and a rerun changes nothing', () => {
  const blocks = render(readRelease(root));
  const index = read('web/index.html');
  assert.match(index, /<!-- release:pill -->[\s\S]*<!-- \/release:pill -->/);
  assert.match(index, /<section id="release"[^>]*><!-- release:section -->[\s\S]*<!-- \/release:section --><\/section>/);
  assert.match(read('web/smk37/index.html'), /<!-- release:pill -->[\s\S]*<!-- \/release:pill -->/);
  const once = apply(index, blocks);
  assert.equal(apply(once, blocks), once);
  assert.throws(() => apply('<p>no blocks</p>', blocks), /no <!-- release/);
});

test('the page has no sidebar on the left, and an embed shows no release', () => {
  const css = read('web/style.css');
  assert.doesNotMatch(css, /position:\s*sticky/, 'nothing pinned beside the text');
  assert.doesNotMatch(css, /\.guide\s*\{[^}]*grid-template-columns:\s*\d+px/, 'the guide is one column');
  assert.match(css, /\[data-embed\][^{]*\.release[,\s]/);
});
