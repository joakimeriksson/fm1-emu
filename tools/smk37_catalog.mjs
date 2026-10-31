// The SMK-37 page's catalog (web/smk37/firmwares.json) from genkigenki's install page
// (genkigenki.github.io/smk37): its FW list (id, name, version, the SMK-37 Pro package) and its
// cards (source, origin, the one-line description). Run it when that page has new versions.
//   node tools/smk37_catalog.mjs [--check]   (--check: only say whether the file is current)
import { readFileSync, writeFileSync } from "node:fs";

const SITE = "https://genkigenki.github.io/smk37/";
const OUT = new URL("../web/smk37/firmwares.json", import.meta.url);
const page = await (await fetch(SITE)).text();

const fwList = JSON.parse(page.match(/const FW = (\[.*?\]);\n/s)[1]);
const text = (s) => s.replace(/<[^>]+>/g, "").replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">")
  .replace(/&#x27;|&#39;/g, "'").replace(/&quot;/g, '"').replace(/\s+/g, " ").trim();
// a card's origin: "new firmware · AUTHOR", or "<a ORIGINAL>based on BASE by AUTHOR</a> · ... <br>
// SMK-37 port: <a PORT>owner/repo</a>" (or "X by AUTHOR ... <br>SMK-37 build: <a ..>")
const cards = new Map();
for (const m of page.matchAll(/<article class="card fw[^"]*" data-fw="([^"]+)"[^>]*>(.*?)<\/article>/gs)) {
  const body = m[2];
  const from = body.match(/<div class="from">(.*?)<\/div>/s)?.[1] || "";
  const [first, second = ""] = from.split(/<br\s*\/?>/);
  const orig = text(first.split(" · ")[0]);
  const by = orig.match(/^(?:new firmware|.*?\bby) (.+)$/)?.[1] || orig.replace(/^new firmware · /, "");
  const port = second.match(/<a href="([^"]+)">([^<]+)<\/a>/);
  cards.set(m[1], {
    repo: port?.[1] || body.match(/<h3><a href="([^"]+)"/)?.[1],
    upstream: first.match(/^<a href="([^"]+)">based on/)?.[1] || null,
    base: orig.match(/^based on (.+?) by /)?.[1] || null,
    author: first.startsWith("new firmware") ? text(first).split("·").pop().trim() : by,
    porter: port ? port[2].split("/")[0] : null,
    about: text(body.match(/<p>(.*?)<\/p>/s)?.[1] || ""),
  });
}
// the FM-1 firmware each is a build of, by its upstream repository
const fm1 = JSON.parse(readFileSync(new URL("../web/firmwares.json", import.meta.url))).firmwares;
const norm = (u) => (u || "").toLowerCase().replace(/\/$/, "").replace(/\/tree\/.*$/, "");

const firmwares = [{
  id: "stock", name: "M-VAVE stock firmware", version: "016", author: "M-VAVE", license: "M-VAVE's own", byo: true,
  about: "The firmware the SMK-37 Pro ships with: a MIDI controller. Its screen, buttons, pads and keys work (PATCH, " +
    "then a chord: it names it), and with PATCH on the keys play its DX7 synth (this emulator's own small bank: the " +
    "factory patches are not in the package; its notes also go to USB and Bluetooth MIDI, not emulated). It runs at " +
    "about 0.6x real time in the browser so far. This page does not host it and cannot download it: " +
    "download SMK-37 Pro_016.fwsc from M-VAVE, then open it here.",
  download: "https://yms-file-store.oss-cn-hongkong.aliyuncs.com/software/firmware/SMK-37Pro/SMK-37%20Pro_016.fwsc",
  repo: "https://github.com/genkigenki/fm1-to-smk37",
}];
for (const f of fwList) {
  const c = cards.get(f.id) || {};
  const twin = c.upstream && fm1.find((g) => norm(g.repo) === norm(c.upstream));
  firmwares.push({
    id: f.id, name: f.name.replace(/ — SMK-37 build$/, "").replace(/ \(SLOOP for the SMK-37\)$/, ""), version: f.version,
    author: c.author || "genkigenki", license: "GPL-3.0", about: c.about || "",
    ...(c.base ? { base: c.base } : {}), ...(c.porter && c.porter !== c.author ? { porter: c.porter } : {}),
    repo: c.repo || SITE, ...(c.upstream ? { upstream: c.upstream } : {}), ...(twin ? { fm1: twin.id } : {}),
    package: new URL(f.v.pro.pkg, SITE).href, installer: SITE,
  });
}
const json = JSON.stringify({
  note: `SMK-37 builds from ${SITE} (tools/smk37_catalog.mjs); each package is fetched from there when started`,
  default: "snoop37",
  firmwares,
}, null, 1) + "\n";
if (process.argv.includes("--check")) {
  let cur = "";
  try { cur = readFileSync(OUT, "utf8"); } catch {}
  console.log(cur === json ? "web/smk37/firmwares.json: current" : "web/smk37/firmwares.json: out of date (node tools/smk37_catalog.mjs)");
  process.exit(cur === json ? 0 : 1);
}
writeFileSync(OUT, json);
console.log(`web/smk37/firmwares.json: ${firmwares.length} firmwares`);
for (const f of firmwares) console.log(`  ${f.id.padEnd(12)} ${f.version.padEnd(7)} ${f.name} · ${f.author}${f.base ? ` (${f.base})` : ""}${f.porter ? ` · port ${f.porter}` : ""}${f.fm1 ? ` · FM-1: ${f.fm1}` : ""}`);
