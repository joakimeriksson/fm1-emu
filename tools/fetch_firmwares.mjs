// Fetch the firmwares in web/firmwares.json into reference/firmwares/<id>.fwsc, the package as
// published, for fm1-live --fw and tools/catalog_check.py, which boot it as the web page does.
// Same download as the web page; the app is unpacked only to say its size.
//   node tools/fetch_firmwares.mjs [--keep] [id ...]
// --keep: a package already here that is the one tests/catalog.json hashed is not fetched again.
// The catalog check in CI asks for that: once its author publishes a new build, the catalog's own
// address is gone, the download falls back to the newer build, and the audio can no longer be
// compared with the hash. (Not for tools/catalog_refresh.py, which is there to take the new build.)
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
const { appFromPackage, fetchPackage } = await import(new URL("../web/fwsc.js", import.meta.url));
// (a path, not URL.pathname: that keeps a leading slash before the drive letter on Windows)
const root = fileURLToPath(new URL("../", import.meta.url));
const cat = JSON.parse(readFileSync(root + "web/firmwares.json"));
const keep = process.argv.includes("--keep");
const want = new Set(process.argv.slice(2).filter((a) => !a.startsWith("--")));
const golden = keep && existsSync(root + "tests/catalog.json") ? JSON.parse(readFileSync(root + "tests/catalog.json")) : {};
mkdirSync(root + "reference/firmwares", { recursive: true });
// A site that is busy (429, 5xx) or drops the connection gets three more tries; what failed is
// said, as the download then may not be the catalog's own package (fetchPackage falls back).
const failed = [];
async function patient(url, opts) {
  for (let i = 0; ; i++) {
    try {
      const r = await fetch(url, opts);
      if (r.ok || (r.status !== 429 && r.status < 500) || i === 3) {
        if (!r.ok) failed.push(`${url}: ${r.status}`);
        return r;
      }
    } catch (e) {
      if (i === 3) { failed.push(`${url}: ${e.message}`); throw e; }
    }
    await new Promise((ok) => setTimeout(ok, 2000 << i));
  }
}
for (const f of cat.firmwares) {
  if (want.size && !want.has(f.id)) continue;
  if (f.byo) continue;                     // M-VAVE's own: fm1-live --fwsc FM-1.fwsc
  const file = root + "reference/firmwares/" + f.id + ".fwsc";
  if (golden[f.id] && existsSync(file) &&
      createHash("sha256").update(readFileSync(file)).digest("hex").startsWith(golden[f.id].package)) {
    console.log(`${f.id.padEnd(13)} kept: the package tests/catalog.json hashed`);
    continue;
  }
  try {
    failed.length = 0;
    const { bytes, url } = await fetchPackage(f, patient);
    const app = appFromPackage(bytes);
    writeFileSync(file, bytes);
    console.log(`${f.id.padEnd(13)} ${url.replace(/\/$/, "").split("/").pop().padEnd(36)} app ${app.length} bytes`);
    for (const why of failed) console.log(`${"".padEnd(13)} (${why})`);
  } catch (e) {
    console.log(`${f.id.padEnd(13)} FAILED ${e.message}`);
  }
}
