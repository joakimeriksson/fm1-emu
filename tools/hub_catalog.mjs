// The FM-1 page's catalog (web/firmwares.json) from the FM-1 Firmware Hub's (thefirmwarehub.com,
// zvenson's): every firmware it lists that runs here, its package fetched from the Hub (whose
// packages never change under their name, sent with Access-Control-Allow-Origin: *; an old one is
// removed when a new version comes, and web/fwsc.js then asks the Hub's catalog), then our
// own list, web/firmwares-own.json (hand-edited: FM-1 SID, M-VAVE's own, ... the firmwares the Hub
// does not list). For a firmware the Hub lists, what is ours is kept (the page's description and
// the installer link, from our own list, else from the last web/firmwares.json; a licence from our
// own list, else the Hub's, else the last file's) and the rest comes from the Hub. The Hub's catalog changes every few hours: run this when it has.
//   node tools/hub_catalog.mjs [--check] [--catalog=hub.json]
// (--check: only say whether the file is current; --catalog: the Hub's catalog from a file)
import { readFileSync, writeFileSync } from "node:fs";
import { packageVersion } from "../web/fwsc.js";

const HUB = "https://thefirmwarehub.com/";
const OUT = new URL("../web/firmwares.json", import.meta.url);
const OWN = new URL("../web/firmwares-own.json", import.meta.url);
// our ids where the Hub's differ (the page takes either in ?fw=; see ourId for the rest)
const OURS = { salt: "felucca-salt", floyd: "sloop-floyd", sloopbanks: "sloop-banks" };
// listed on the Hub, not run here yet
const SKIP = {
  lsdjburrito: "boots to its instructions, but makes no audio until an LSDJ song is sent to it over USB from its web page, which the emulator's page cannot do",
};
// first in the list (the page's default first), then by id
const FIRST = ["mvave", "felucca", "felucca-salt"];

const arg = process.argv.find((a) => a.startsWith("--catalog="));
const hub = arg ? JSON.parse(readFileSync(arg.slice(10))) : await (await fetch(HUB + "catalog.json")).json();
const cur = JSON.parse(readFileSync(OUT));
const ownFile = JSON.parse(readFileSync(OWN));
const own = ownFile.firmwares;
const old = new Map([...cur.firmwares, ...own].map((f) => [f.id, f]));

// Our id for a Hub firmware: OURS, else the id of the entry the Hub's id was recorded on ("hub"),
// else one of our own list's by the same id or the same name (letters and digits only, so the Hub's
// "fm1-sid" or "FM1 SID" is our "FM-1 SID"); else the Hub's. One in our own list with the same
// repository but neither is reported, to be told apart by hand ("hub" on the entry).
const norm = (s) => String(s || "").toLowerCase().replace(/[^a-z0-9]/g, "");
const ghRepo = (u) => String(u || "").replace(/^https:\/\/github\.com\//, "").split("/").slice(0, 2).join("/").toLowerCase();
function ourId(h) {
  if (OURS[h.id]) return OURS[h.id];
  const rec = [...old.values()].find((f) => f.hub === h.id);
  if (rec) return rec.id;
  const m = own.find((f) => f.id === h.id) || own.find((f) => norm(f.name) === norm(h.name));
  if (m) {
    if (m.id !== h.id) console.log(`  (the Hub's ${h.id} is our ${m.id}: the same name)`);
    return m.id;
  }
  for (const f of own) {
    if (h.repo && ghRepo(f.repo) === h.repo.toLowerCase())
      console.log(`  CHECK: the Hub's ${h.id} ("${h.name}") has the repository of our ${f.id} ("${f.name}"); ` +
        `if it is the same firmware, add "hub": "${h.id}" to it in web/firmwares-own.json`);
  }
  return h.id;
}

// a repository's licence (GitHub's SPDX id), for a firmware new to the list whose Hub entry has
// none ("lic"); GITHUB_TOKEN when set, as the unauthenticated API allows 60 asks an hour
async function licence(repo) {
  try {
    const headers = { "User-Agent": "fm1-emu hub_catalog" };
    if (process.env.GITHUB_TOKEN) headers.Authorization = `Bearer ${process.env.GITHUB_TOKEN}`;
    const r = await fetch(`https://api.github.com/repos/${repo}`, { headers });
    if (!r.ok) {
      console.log(`  (GitHub did not answer for ${repo}: ${r.status}${r.status === 403 ? "; GITHUB_TOKEN raises the limit" : ""})`);
      return "see the repository";
    }
    const id = (await r.json()).license?.spdx_id;
    return id && id !== "NOASSERTION" ? id : "see the repository";
  } catch (e) {
    console.log(`  (GitHub did not answer for ${repo}: ${e.message})`);
    return "see the repository";
  }
}

// The package's version (web/fwsc.js's packageVersion: the Hub's "version" is its latest release
// tag, which runs ahead of the package it hosts for a release whose package it has not taken yet,
// FM1 Quest 43 with fm1quest-42.fwsc). A package not named <id>-<version>.fwsc is said.
function pkgVersion(h) {
  const v = packageVersion(h.id, h.pkg);
  if (v == null) console.log(`  CHECK: the Hub's ${h.id} has a package not named after it, ${h.pkg}: its version ${h.version} taken as the package's`);
  return v ?? String(h.version);
}
// A firmware's licence: one in our own list (o is that entry for an id of ours: the own list is
// last into old); else the Hub's; else the one an earlier run asked GitHub for, unless that ask
// failed; else GitHub's now.
async function licenceOf(h, o) {
  const kept = o.license && o.license !== "see the repository" ? o.license : null;
  return (own.some((f) => f.id === o.id) && o.license) || h.lic || kept || (h.repo ? await licence(h.repo) : "see the repository");
}

const firmwares = [];
const listed = new Set();
for (const h of hub.firmwares) {
  if (!h.pkg) continue;
  const id = ourId(h);
  listed.add(id);
  if (SKIP[h.id]) continue;
  const o = old.get(id) || {};
  firmwares.push({
    id,
    name: h.name,
    version: pkgVersion(h),
    author: o.author || h.author,
    license: await licenceOf(h, o),
    about: o.about || h.what,
    repo: o.repo || (h.repo ? `https://github.com/${h.repo}` : h.site),
    package: new URL(h.pkg, HUB).href,
    ...(o.installer ? { installer: o.installer } : {}),
    hub: h.id,
  });
}
// our own list, after the Hub's (an id the Hub lists too: the Hub's entry above, our texts)
// (a "hub" on one: the Hub's id it was listed under, gone from the Hub now: no card to link)
for (const f of own) {
  if (!listed.has(f.id)) {
    const { hub: _, ...rest } = f;
    firmwares.push(rest);
  }
}
const rank = (f) => (FIRST.includes(f.id) ? FIRST.indexOf(f.id) : FIRST.length);
firmwares.sort((a, b) => rank(a) - rank(b) || a.id.localeCompare(b.id));
// the page picks a firmware by its id: two of one (two Hub firmwares matched to one of ours, a
// Hub id that is one of our own list's for another firmware) would hide one of them
const dup = firmwares.map((f) => f.id).filter((id, i, a) => a.indexOf(id) !== i);
if (dup.length) {
  console.error(`two firmwares with the id ${[...new Set(dup)].join(", ")}: tell them apart in OURS or with "hub" in web/firmwares-own.json`);
  process.exit(2);
}

// The file's layout, as it was written by hand: an entry a few lines, `{ "id": ...` opening each
// (fm1-live's small reader scans for that, and `"key": "value"`), the description and the source
// on lines of their own.
const fields = (f, keys) => keys.filter((k) => k in f).map((k) => `"${k}": ${JSON.stringify(f[k])}`).join(", ");
const entry = (f) => {
  const head = ["id", "name", "version", "author", "license"], own = ["about", "repo"];
  const rest = Object.keys(f).filter((k) => !head.includes(k) && !own.includes(k));
  return [`    { ${fields(f, head)},`, ...own.filter((k) => k in f).map((k) => `      ${fields(f, [k])},`), `      ${fields(f, rest)} }`]
    .join("\n").replace(/,\n      \s*}$/, " }");
};
const note = "Community firmwares for the M-VAVE FM-1: the FM-1 Firmware Hub's list (thefirmwarehub.com, " +
  "tools/hub_catalog.mjs; its packages fetched from the Hub), then our own list (web/firmwares-own.json: " +
  "this repository's FM-1 SID, M-VAVE's own, a few from fm1-editor.com/firmware). Generated: edit " +
  "web/firmwares-own.json (the default too), not this file. The web page fetches each package when started and " +
  "unpacks it with web/fwsc.js; tools/fetch_firmwares.mjs does the same into reference/firmwares for " +
  "fm1-live. \"hub\": the firmware's id on the Hub (its card: thefirmwarehub.com/#fw-<hub>). \"skipped\": Hub " +
  "firmwares not run here, by the Hub's id, with why; the page lists the Hub's other firmwares not in this file " +
  "after these, marked as not yet run here (web/fwsc.js hubNewcomers).";
const json = `{\n  "note": ${JSON.stringify(note)},\n  "default": ${JSON.stringify(ownFile.default ?? cur.default)},\n` +
  `  "skipped": ${JSON.stringify(SKIP)},\n  "firmwares": [\n` +
  firmwares.map(entry).join(",\n") + "\n  ]\n}\n";
JSON.parse(json); // (still JSON)

// what the last list had and this one doesn't (gone from the Hub, or its package; skipped): its
// ?fw= links stop working
const ids = new Set(firmwares.map((f) => f.id));
for (const f of cur.firmwares) {
  if (!ids.has(f.id)) console.log(`  dropped: ${f.id} (${SKIP[f.hub] ? `skipped: ${SKIP[f.hub]}` : "no longer on the Hub with a package"})`);
}

if (process.argv.includes("--check")) {
  let now = "";
  try { now = readFileSync(OUT, "utf8"); } catch {}
  console.log(now === json ? "web/firmwares.json: current with the Hub" : "web/firmwares.json: the Hub has changed (node tools/hub_catalog.mjs)");
  process.exit(now === json ? 0 : 1);
}
writeFileSync(OUT, json);
console.log(`web/firmwares.json: ${firmwares.length} firmwares (Hub catalog of ${hub.updated})`);
for (const f of firmwares) {
  const o = old.get(f.id);
  const what = !o ? "new" : o.version !== f.version ? `${o.version} -> ${f.version}` : "";
  console.log(`  ${f.id.padEnd(14)} ${f.version.padEnd(20)} ${f.hub ? "hub" : "   "} ${what}`);
}
for (const [id, why] of Object.entries(SKIP)) console.log(`  (not listed: ${id}: ${why})`);
