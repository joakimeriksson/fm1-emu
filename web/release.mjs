// The page's release blocks, from one source: the version in Cargo.toml ([workspace.package]) and
// its notes, docs/releases/v<version>.md (the lead paragraph, the "## Downloads" table and the
// "## What's new" bullets). web/build.sh runs it over the built pages; the committed index.html
// holds the last output, so a page served without a build still says something true.
//   node web/release.mjs web/index.html web/smk37/index.html     (rewrites the marked blocks)
// A page marks each block with <!-- release:NAME --> ... <!-- /release:NAME -->: "pill" (the
// version in the header) and "section" (what's new, the downloads, the announcement text).
// The release film: the first YouTube link in the notes (youtube.com/watch?v=ID or youtu.be/ID)
// shows as a video in the section; without one the section has no video.
import { readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

export const REPO = "https://github.com/joakimeriksson/fm1-emu";
export const PAGE = "https://joakimeriksson.github.io/fm1-emu/";
const NOTES_BASE = `${REPO}/blob/master/docs/releases/`;

export function workspaceVersion(cargoToml) {
  const m = /^\[workspace\.package\][^[]*?^version\s*=\s*"([^"]+)"/ms.exec(cargoToml);
  if (!m) throw new Error("Cargo.toml: no version in [workspace.package]");
  return m[1];
}

const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

// One line of the notes' Markdown as HTML or as plain text, shortened for a summary: every
// parenthetical goes (the pull requests and names, the asides), code spans and links stay.
export function inline(md, mode = "html") {
  const keep = [];
  const hold = (s) => `\u0000${keep.push(s) - 1}\u0000`;
  let s = md.replace(/`([^`]*)`/g, (_, code) => hold(mode === "html" ? `<code>${esc(code)}</code>` : code));
  s = s.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, text, href) => {
    const url = new URL(href, NOTES_BASE).href;
    return hold(mode === "html" ? `<a href="${esc(url)}">${esc(text)}</a>` : text);
  });
  for (let prev; prev !== s;) { prev = s; s = s.replace(/\s*\([^()]*\)/g, ""); }
  s = s.replace(/\s+([,.;:])/g, "$1").replace(/\s+/g, " ").trim();
  if (mode === "html") s = esc(s).replace(/\*\*(.+?)\*\*/g, "<b>$1</b>");
  else s = s.replace(/\*\*(.+?)\*\*/g, "$1");
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => keep[i]);
}

// the notes, read into what the page shows
export function parseNotes(md, version) {
  const lines = md.replace(/\r\n/g, "\n").split("\n");
  const at = (re) => lines.findIndex((l) => re.test(l));
  const sectionOf = (re) => {
    const i = at(re);
    if (i < 0) return [];
    const end = lines.findIndex((l, j) => j > i && /^## /.test(l));
    return lines.slice(i + 1, end < 0 ? lines.length : end);
  };

  // the lead: the first paragraph, less the sentence that sends readers to this page
  const firstHeading = at(/^## /);
  const lead = lines.slice(0, firstHeading < 0 ? lines.length : firstHeading).join("\n").trim().split(/\n\s*\n/)
    .find((p) => !p.startsWith("#")) || "";
  const leadMd = lead.replace(/\s+/g, " ").split(/(?<=\.)\s+(?=[A-Z*[])/)
    .filter((s) => !s.includes(PAGE.replace(/^https:\/\//, "").replace(/\/$/, ""))).join(" ");

  // the downloads: `file` | what it is
  const downloads = [];
  for (const l of sectionOf(/^## Downloads/)) {
    const m = /^\|\s*`([^`]+)`\s*\|(.*)\|\s*$/.exec(l);
    if (!m) continue;
    const file = m[1];
    const p = /-(macos|windows|linux)-(arm64|x86_64)\./.exec(file);
    downloads.push({
      file, url: `${REPO}/releases/download/v${version}/${file}`,
      kind: file.startsWith("fm1-plugin-") ? "plugin" : file.startsWith("fm1-emu-") ? "live" : "other",
      os: p ? p[1] : "other", arch: p ? p[2] : "",
      isNew: /(^|[.:]\s+)New\.?(\s|$)/.test(m[2].trim()),
    });
  }

  // a section's bullets, one item each (continuation lines joined)
  const bullets = (re) => {
    const items = [];
    for (const l of sectionOf(re)) {
      if (/^- /.test(l)) items.push(l.slice(2));
      else if (/^\s+\S/.test(l) && items.length) items[items.length - 1] += " " + l.trim();
      else if (l.trim() && !/^\s/.test(l)) break;            // a paragraph after the list
    }
    return items;
  };
  // the highlights: "## Highlights" (short lines, for the page and an announcement) when the notes
  // have it, else what's new; the contributors' bullet (in either) becomes the thanks
  const short = bullets(/^## Highlights/);
  const highlights = [];
  let thanks = [];
  for (const item of [...(short.length ? short : []), ...bullets(/^## What's new/)]) {
    const text = inline(item, "text");
    const c = /^(Contributors[^:]*|Thanks[^:]*):\s*(.*)$/i.exec(text);
    if (c) thanks = c[2].replace(/\.$/, "").split(/,\s*|\s+and\s+/).map((n) => n.trim()).filter(Boolean);
    else if (!short.length || short.includes(item)) highlights.push({ html: inline(item, "html"), text });
  }

  const film = /https:\/\/(?:www\.)?(?:youtube\.com\/watch\?v=|youtu\.be\/)([\w-]{6,})/.exec(md);
  return { leadHtml: inline(leadMd, "html"), highlights, thanks, downloads, film: film ? film[1] : null };
}

export function readRelease(root) {
  const version = workspaceVersion(readFileSync(new URL("Cargo.toml", root), "utf8"));
  const notesPath = `docs/releases/v${version}.md`;
  let md;
  try { md = readFileSync(new URL(notesPath, root), "utf8"); }
  catch { throw new Error(`no ${notesPath} for the version in Cargo.toml (${version})`); }
  return { version, notesPath, tag: `v${version}`, url: `${REPO}/releases/tag/v${version}`, ...parseNotes(md, version) };
}

const OS = { windows: "Windows", macos: "macOS", linux: "Linux" };
const ARCH = { macos: { arm64: "Apple silicon", x86_64: "Intel" }, any: { arm64: "ARM64", x86_64: "x86-64" } };
const GROUPS = [
  ["plugin", "DAW plugin", "AU, VST3 and CLAP"],
  ["live", "fm1-live, standalone", "a firmware in a window, with audio and MIDI in"],
  ["other", "More", ""],
];
const names = (list) => list.length < 2 ? list.join("") : `${list.slice(0, -1).join(", ")} and ${list.at(-1)}`;

// the plain-text announcement: for a video's description or a post
export function announcement(r) {
  return [
    `FM-1 Emulator ${r.version}: the M-VAVE FM-1 pocket synth, emulated in the browser, in your DAW and on the desktop.`,
    "",
    ...r.highlights.map((h) => `• ${h.text}`),
    "",
    `Play it in the browser: ${PAGE}`,
    `Downloads and release notes: ${r.url}`,
    ...(r.thanks.length ? ["", `Thanks to ${names(r.thanks)}.`] : []),
  ].join("\n");
}

export function render(r) {
  const pill = `<a class="ver" href="${r.url}" title="Release notes and downloads for ${r.version}">${esc(r.tag)}</a>`;

  const groups = GROUPS.map(([kind, title, sub]) => {
    const files = r.downloads.map((d, i) => ({ ...d, i })).filter((d) => d.kind === kind);
    if (!files.length) return "";
    // the order of the notes' table, an OS's x86-64 build before its ARM one
    const first = (os) => files.find((d) => d.os === os).i;
    files.sort((a, b) => first(a.os) - first(b.os) || (a.arch === "arm64") - (b.arch === "arm64"));
    const links = files.map((d) => {
      const arch = (ARCH[d.os] || ARCH.any)[d.arch] || d.arch;
      return `<a class="dl-btn" href="${esc(d.url)}" title="${esc(d.file)}"><span>${esc(OS[d.os] || d.file)}</span>` +
        (arch && d.os !== "other" ? `<small>${esc(arch)}</small>` : "") + (d.isNew ? `<em>new</em>` : "") + `</a>`;
    }).join("\n          ");
    return `
      <div class="dl-group">
        <p><b>${esc(title)}</b>${sub ? ` <span>${esc(sub)}</span>` : ""}</p>
        <div class="dl-links">
          ${links}
        </div>
      </div>`;
  }).join("");

  const film = r.film ? `
    <a class="film" href="https://www.youtube.com/watch?v=${esc(r.film)}" data-yt="${esc(r.film)}" aria-label="Play the ${esc(r.version)} release film (YouTube)">
      <img src="https://i.ytimg.com/vi/${esc(r.film)}/hqdefault.jpg" alt="" loading="lazy" width="480" height="360">
      <span class="film-play" aria-hidden="true"></span><span class="film-cap">The ${esc(r.version)} film ↗</span>
    </a>` : `
    <!-- the release film: none yet (a YouTube link in ${esc(r.notesPath)} shows it here) -->`;

  const text = announcement(r);
  const section = `
  <div class="rel-top">
    <div>
      <p class="eyebrow">Latest release · <a href="${r.url}">${esc(r.tag)}</a></p>
      <h2 id="h-release">What's new in ${esc(r.version)}</h2>
      ${r.leadHtml ? `<p class="rel-lead">${r.leadHtml}</p>` : ""}
    </div>
    <div class="rel-acts">
      <button type="button" class="copy" data-copy="rel-text">Copy announcement</button>
      <a href="${r.url}">Full release notes ↗</a>
    </div>
  </div>
  <div class="rel-body">
    <div class="rel-news">
      <ul class="rel-list">
${r.highlights.map((h) => `        <li>${h.html}</li>`).join("\n")}
      </ul>
      ${r.thanks.length ? `<p class="rel-thanks">Thanks to ${esc(names(r.thanks))}.</p>` : ""}
    </div>
    <div class="rel-side">${film}
    <div class="dl">
      <h3>Downloads</h3>${groups}
      <p class="dl-note">The downloads are not signed: <a href="${r.url}">the release notes</a> say how to
        open them on macOS and Windows.</p>
    </div>
    </div>
  </div>
  <details class="rel-copy">
    <summary>The announcement as text</summary>
    <textarea id="rel-text" readonly rows="${text.split("\n").length + 2}" aria-label="Announcement text for ${esc(r.version)}">${esc(text)}</textarea>
  </details>
  `;
  return { pill, section };
}

// the page with its marked blocks replaced
export function apply(html, blocks) {
  let found = 0;
  const out = html.replace(/(<!-- release:(\w+) -->)[\s\S]*?(<!-- \/release:\2 -->)/g, (all, open, name, close) => {
    if (!(name in blocks)) throw new Error(`unknown release block ${name}`);
    found++;
    return open + blocks[name] + close;
  });
  if (!found) throw new Error("no <!-- release:... --> blocks in the page");
  return out;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const r = readRelease(new URL("../", import.meta.url));
  const blocks = render(r);
  for (const file of process.argv.slice(2)) writeFileSync(file, apply(readFileSync(file, "utf8"), blocks));
  console.log(`release ${r.version}: ${r.highlights.length} highlights, ${r.downloads.length} downloads, ` +
    `film ${r.film || "none"} -> ${process.argv.slice(2).join(" ")}`);
}
