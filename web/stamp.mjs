// The built page's own files under one build's name: every reference from a page or a script to
// another file of the page (the scripts, the stylesheet, the wasm module, the worklet, the
// catalogs) gets ?v=<build>. GitHub Pages lets a browser keep each file 10 minutes, each on its
// own, so right after a deploy one visit could run a new worklet.js against the old fm1_web.wasm
// (the WebAssembly JIT then fails on every block, #142); with the build in every URL a page only
// ever loads files of one build. web/build.sh runs it over the built output:
//   node web/stamp.mjs BUILD DIR...      (each DIR's .html and .js, rewritten in place)
import { readFileSync, writeFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

// a file of the page: a plain relative name (no scheme, no leading slash, no query yet)
const OWN = String.raw`((?:\./)?(?:smk37/)?[\w.-]+\.(?:js|mjs|css|wasm|json))`;
const RULES = [
  // module imports: import ... from "./x.js", import("./x.js")
  new RegExp(String.raw`(\bfrom\s*|\bimport\s*\(\s*)(["'])${OWN}\2`, "g"),
  // fetch("x.wasm"), addModule("worklet.js")
  new RegExp(String.raw`(\bfetch\(\s*|\baddModule\(\s*)(["'])${OWN}\2`, "g"),
  // <script src="main.js">, <link rel="stylesheet" href="style.css">
  new RegExp(String.raw`(<script\b[^>]*\bsrc=|<link\b[^>]*\brel="stylesheet"[^>]*\bhref=)(["'])${OWN}\2`, "g"),
];

export function stamp(text, build) {
  if (!/^[\w.-]+$/.test(build)) throw new Error(`a build name of letters, digits, . and -: ${build}`);
  let out = text;
  for (const re of RULES) out = out.replace(re, (_, before, q, file) => `${before}${q}${file}?v=${build}${q}`);
  return out;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [build, ...dirs] = process.argv.slice(2);
  if (!build || !dirs.length) throw new Error("usage: node web/stamp.mjs BUILD DIR...");
  let n = 0;
  for (const dir of dirs) {
    for (const f of readdirSync(dir).filter((f) => /\.(html|js)$/.test(f))) {
      const path = join(dir, f), before = readFileSync(path, "utf8"), after = stamp(before, build);
      if (after !== before) { writeFileSync(path, after); n++; }
    }
  }
  console.log(`stamp ${build}: ${n} files in ${dirs.join(" ")}`);
}
