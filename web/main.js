// The page: the front panel (drawn by the same Rust code as the native window, in its own wasm
// instance), the pointer and keyboard input, the firmware and clock pickers, and the audio graph
// that runs the emulator. Every firmware boots from its whole update package (.fwsc), unpacked
// in the wasm: a catalog entry's fetched from its author's site, or one opened from this
// computer (M-VAVE's own: their server does not let other pages fetch it).
import { fetchPackage } from "./fwsc.js";

const CLOCKS = [60, 80, 100, 120, 160, 200, 240];
// The device this page is for: the FM-1 (index.html) or the SMK-37 (smk37/index.html, <html
// data-device="smk37">): its catalog, words and panel. A package says which board it runs on, and
// the panel follows it (an SMK-37 package opened on the FM-1 page gets the SMK-37's).
const DEVICE = document.documentElement.dataset.device === "smk37" ? "smk37" : "fm1";
const DEV = DEVICE === "smk37"
  ? { product: "M-VAVE SMK-37", board: 1, key: "smk37.", stock: "SMK-37 Pro_016.fwsc", stockFile: /^SMK-37 (Pro|Elite)_\d+( \(\d+\))?\.fwsc$/i,
      other: { name: "FM-1", href: "../" } }
  : { product: "M-VAVE FM-1", board: 0, key: "fm1.", stock: "FM-1.fwsc", stockFile: /^FM-1( \(\d+\))?\.fwsc$/i,
      other: { name: "SMK-37", href: "smk37/" } };
const params = new URLSearchParams(location.search);
const stored = (k) => { try { return localStorage.getItem(k); } catch { return null; } };
const store = (k, v) => { try { localStorage.setItem(k, v); } catch { /* private mode */ } };

const catalog = await (await fetch("firmwares.json")).json();
const byId = new Map(catalog.firmwares.map((f) => [f.id, f]));
// the FM-1 Firmware Hub's ids too, where they differ (its cards link here with ?fw=<its id>)
for (const f of catalog.firmwares) if (f.hub && !byId.has(f.hub)) byId.set(f.hub, f);
// ?embed=1: only the device and its controls, for a window on another site (the inline script in
// <head> set data-embed before the first paint; style.css hides the rest). It does not change the
// firmware this browser starts with on the full page.
const EMBED = document.documentElement.hasAttribute("data-embed");
let fw = byId.get(params.get("fw")) || byId.get(stored(DEV.key + "fw")) || byId.get(catalog.default) || catalog.firmwares[0];
let mhz = Number(params.get("mhz") || stored("fm1.mhz") || 100);
// the audio buffer: "interactive" (small: keys answer quickly) unless this device needed a larger
// one ("playback", offered when the sound drops out with time to spare; ?latency= overrides)
let latency = params.get("latency") || stored("fm1.latency") || "interactive";

const canvas = document.getElementById("panel");
const ctx = canvas.getContext("2d");
const status = document.getElementById("status");
const startBtn = document.getElementById("start");
const fwSel = document.getElementById("fw");
const mhzSel = document.getElementById("mhz");
const about = document.getElementById("about");
const title = document.getElementById("title");
const openInput = document.getElementById("open");
const bankInput = document.getElementById("bank");
const bankName = document.getElementById("bankname");

const wasm = await (await fetch("fm1_web.wasm")).arrayBuffer();   // the worklet gets its own copy
// (the panel's instance runs no machine: its JIT import is never called)
const ui = (await WebAssembly.instantiate(wasm, { env: { fm1_jit_compile: () => 0 } })).instance.exports;
// the panel drawn: 0 the FM-1's, 1 the SMK-37's (setBoard; the canvas takes its size)
let W = 0, H = 0, board = -1;
const fitPanel = () => document.documentElement.style.setProperty("--above",
  `${Math.round(canvas.parentElement.getBoundingClientRect().top + scrollY)}px`);
{
  const ro = new ResizeObserver(fitPanel);
  ro.observe(document.querySelector("header"));
  ro.observe(document.querySelector(".bar"));
}
fitPanel();

let node = null;
let frame = null;            // the latest LCD + LEDs from the audio thread
const down = new Uint8Array(64);           // keys and buttons held (FM-1: matrix ids 0..40; SMK-37: keys 0..36, buttons 37..54)
const encClicks = new Int32Array(8);
const turning = new Uint32Array(9);
let pot = 900;
// SMK-37: its analog inputs as set (pads 0..15, faders 16..19, the pitch / mod wheel 20 / 21)
const PITCH_REST = 535, MOD_REST = 629;
const analog = new Uint16Array(23);
analog[20] = PITCH_REST;
analog[21] = MOD_REST;
function setBoard(b) {
  if (b === board) return;
  board = b;
  ui.panel_board(b);
  W = ui.panel_width();
  H = ui.panel_height();
  canvas.width = W;
  canvas.height = H;
  // the synth's width follows the room left below the header and the bar (index.html's canvas rule)
  document.documentElement.style.setProperty("--aspect", String(W / H));
  document.documentElement.dataset.board = b ? "smk37" : "fm1";
  down.fill(0);
  encClicks.fill(0);
}
setBoard(DEV.board);

// ---------------------------------------------------------------- firmware and clock
for (const f of catalog.firmwares) fwSel.add(new Option(`${f.name} ${f.version}`, f.id));
for (const c of new Set([...CLOCKS, mhz].sort((a, b) => a - b))) mhzSel.add(new Option(`${c} MHz`, c));
fwSel.value = fw.id;
mhzSel.value = mhz;

// the guide's firmware list, one card per catalog entry; "Play" picks it like the select does
const NO_NOTE = "MIDI: no sound from a plain note-on";
const MIDI_NOTES = {
  mvave: "MIDI: channel 2 in the emulator (not yet checked on a real FM-1)",
  felucca: "MIDI: its parts play on channels 1–4",
  choralroot: NO_NOTE, hortator: NO_NOTE, "sloop-alg": NO_NOTE, x0x: NO_NOTE,
};
const cards = new Map();
// the FM-1 page: which entries have an SMK-37 build (that catalog names them)
const twins = new Map();
if (DEVICE === "fm1") {
  try {
    for (const s of (await (await fetch("smk37/firmwares.json")).json()).firmwares) if (s.fm1) twins.set(s.fm1, s.id);
  } catch { /* (no SMK-37 catalog next to this page: no links) */ }
}
{
  const el = (tag, cls, ...kids) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    e.append(...kids);
    return e;
  };
  const link = (href, text) => Object.assign(document.createElement("a"), { href, textContent: text });
  const list = document.getElementById("fwlist");
  for (const f of catalog.firmwares) {
    const play = el("button", "", "Play");
    play.type = "button";
    play.setAttribute("aria-label", `Play ${f.name}`);
    play.addEventListener("click", () => {
      if (fw.id !== f.id) {
        fwSel.value = f.id;
        fwSel.dispatchEvent(new Event("change"));
      }
      scrollTo({ top: 0, behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    });
    const links = el("div", "links");
    if (f.byo) links.append(link(f.download, `download ${DEV.stock}`), link(f.repo, "notes"));
    else {
      links.append(link(f.repo, "source"));
      if (f.installer) links.append(link(f.installer, "install"));
      if (f.hub) links.append(link(`https://thefirmwarehub.com/#fw-${f.hub}`, "on the Hub"));
    }
    // the same firmware on the other device (an SMK-37 build's `fm1`, or an FM-1 entry it names)
    const twin = DEVICE === "smk37" ? f.fm1 : twins.get(f.id);
    if (twin) links.append(link(`${DEV.other.href}?fw=${twin}`, `also on the ${DEV.other.name}`));
    const by = [f.author + (f.base ? ` (${f.base})` : ""), f.porter && `SMK-37 port: ${f.porter}`, f.license].filter(Boolean).join(" · ");
    const card = el("li", "",
      el("div", "head", el("b", "", f.name), el("span", "dim", f.version), play),
      el("div", "by", by),
      el("p", "what", f.byo && DEVICE === "fm1" ? "The firmware the FM-1 ships with: a DX7-style FM synth, both cores running. " +
        "Bring your own copy (see below)." : f.about));
    if (DEVICE === "fm1" && MIDI_NOTES[f.id]) card.append(el("div", "midi", MIDI_NOTES[f.id]));
    card.append(links);
    list.append(card);
    cards.set(f.id, card);
  }
}

// the package of catalog entry f, fetched from its author's site (nothing is hosted with this
// page); kept per session. A package opened from this computer is the entry `local`. A `byo`
// entry (M-VAVE's own: their server does not let pages fetch it) is the copy the visitor opened,
// kept in this browser (IndexedDB) for the next visit.
const packages = new Map();
class NeedFile extends Error {}
async function packageOf(f) {
  if (f.id === "local") return { bytes: f.bytes, url: null };
  if (f.byo) {
    if (!packages.has(f.id)) {
      const kept = await idb("get", f.id);
      if (!kept) throw new NeedFile();
      packages.set(f.id, { bytes: kept, url: null });
    }
    return packages.get(f.id);
  }
  if (!packages.has(f.id)) packages.set(f.id, await fetchPackage(f));
  return packages.get(f.id);
}
// a tiny key-value store in IndexedDB; any failure (private mode, blocked storage) is "nothing kept"
function idb(op, key, value) {
  return new Promise((done) => {
    try {
      const open = indexedDB.open("fm1-emulator", 1);
      open.onupgradeneeded = () => open.result.createObjectStore("packages");
      open.onerror = () => done(null);
      open.onsuccess = () => {
        try {
          const tx = open.result.transaction("packages", op === "get" ? "readonly" : "readwrite");
          const st = tx.objectStore("packages");
          const r = op === "get" ? st.get(key) : op === "put" ? st.put(value, key) : st.delete(key);
          r.onsuccess = () => done(op === "get" ? r.result || null : true);
          r.onerror = () => done(null);
        } catch { done(null); }
      };
    } catch { done(null); }
  });
}
// a kept copy of a byo entry: known at once, so the page can say so before Start
for (const f of catalog.firmwares.filter((f) => f.byo)) {
  const kept = await idb("get", f.id);
  if (kept) packages.set(f.id, { bytes: kept, url: null });
}
let bank = null;                           // a DX7 32-voice bank for the stock firmware, or null

let loadedUrl = null;                      // the package the running machine came from

function showFirmware() {
  title.textContent = `${DEV.product} · ${fw.name} (emulated)`;
  for (const [id, card] of cards) {
    card.classList.toggle("on", id === fw.id);
    card.querySelector("button").textContent = id === fw.id ? "Selected" : "Play";
  }
  const b = document.createElement("b");
  b.textContent = `${fw.name} ${fw.version}`.trim();
  let parts;
  if (fw.id === "local") {
    parts = [b, " · opened from this computer; it stays in this page."];
  } else if (fw.byo) {
    const dl = document.createElement("a");
    dl.href = fw.download;
    dl.textContent = `download ${DEV.stock} from M-VAVE`;
    const doc = document.createElement("a");
    doc.href = fw.repo;
    doc.textContent = "how it runs";
    const have = packages.has(fw.id) ? " Your copy is kept in this browser; open another .fwsc to replace it." :
      " Then click \u201cOpen .fwsc\u2026\u201d (or drop the file on the synth); this browser keeps it for next time.";
    parts = [b, " · M-VAVE's own firmware. This page does not host it and cannot fetch it: ", dl, "." + have +
      (DEVICE === "fm1" ? " It starts with a small patch bank of the emulator's own; \u201cDX7 bank\u2026\u201d loads a 32-voice .syx · " :
        " (A MIDI controller: its notes go to USB / Bluetooth MIDI, so no sound here; slower than real time in the browser so far.) · "), doc];
  } else {
    const a = document.createElement("a");
    a.href = fw.repo;
    a.textContent = "source";
    parts = [b, ` by ${fw.author} · ${fw.license} · `, a, ` · ${fw.about}`];
  }
  if (loadedUrl) {
    const p = document.createElement("a");
    p.href = loadedUrl;
    p.textContent = loadedUrl.replace(/\/$/, "").split("/").pop();   // April OS: its installer page
    parts.push(" Running ", p, ".");
  }
  about.replaceChildren(...parts);
  if (fw.id === "local") return;           // (not something a link can bring back)
  const q = new URLSearchParams(location.search);
  q.set("fw", fw.id);
  q.set("mhz", mhz);
  history.replaceState(null, "", `?${q}`);
  document.getElementById("fullpage").href = `./?fw=${fw.id}`;
}
showFirmware();

fwSel.addEventListener("change", () => {
  fw = byId.get(fwSel.value) || localFw;
  loadedUrl = null;
  if (!EMBED) store(DEV.key + "fw", fw.id);
  showFirmware();
  fwSel.blur();                 // back to playing from the keyboard
  if (node) boot();
});
mhzSel.addEventListener("change", () => {
  mhz = Number(mhzSel.value);
  store("fm1.mhz", mhz);
  showFirmware();
  mhzSel.blur();
  if (node) boot();
});

// a package from this computer: the file picker or a drop on the panel. With M-VAVE's entry
// chosen, or a file named like theirs, it is that entry's copy and is kept for next time.
let localFw = null;
async function openLocal(file) {
  if (!file) return;
  const mv = catalog.firmwares.find((f) => f.byo);
  if (mv && (fw.byo || DEV.stockFile.test(file.name))) {
    const bytes = new Uint8Array(await file.arrayBuffer());
    packages.set(mv.id, { bytes, url: null });
    idb("put", mv.id, bytes);
    fw = mv;
    fwSel.value = mv.id;
    if (!EMBED) store(DEV.key + "fw", mv.id);
    loadedUrl = null;
    showFirmware();
    if (node) boot();
    else status.textContent = `${file.name} ready: click Start`;
    return;
  }
  localFw = { id: "local", name: file.name.replace(/\.fwsc$/i, ""), version: "", bytes: new Uint8Array(await file.arrayBuffer()) };
  let opt = [...fwSel.options].find((o) => o.value === "local");
  if (!opt) fwSel.add(opt = new Option("", "local"), 0);
  opt.text = `${localFw.name} (your file)`;
  fwSel.value = "local";
  fw = localFw;
  loadedUrl = null;
  showFirmware();
  if (node) boot();
  else status.textContent = `${file.name} ready: click Start`;
}
openInput.addEventListener("change", () => { openLocal(openInput.files[0]); openInput.value = ""; });
bankInput.addEventListener("change", async () => {
  const file = bankInput.files[0];
  bankInput.value = "";
  if (!file) return;
  bank = new Uint8Array(await file.arrayBuffer());
  bankName.textContent = file.name;
  if (node) boot();
});
const stage = document.querySelector(".stage");
stage.addEventListener("dragover", (e) => { e.preventDefault(); });
stage.addEventListener("drop", (e) => {
  e.preventDefault();
  const file = [...e.dataTransfer.files].find((f) => /\.(fwsc|syx)$/i.test(f.name));
  if (!file) return;
  if (/\.syx$/i.test(file.name)) {
    file.arrayBuffer().then((b) => { bank = new Uint8Array(b); bankName.textContent = file.name; if (node) boot(); });
  } else {
    openLocal(file);
  }
});

// ---------------------------------------------------------------- drawing
function draw() {
  const mem = () => ui.memory.buffer;
  if (frame) {
    if (frame.lcd) {
      new Uint16Array(mem(), ui.panel_lcd(), 240 * 240).set(frame.lcd);
      frame.lcd = null;
    }
    new Float32Array(mem(), ui.panel_leds(), 41).set(frame.leds);
    if (frame.pads) for (let p = 0; p < 16; p++) ui.panel_set(5, p, frame.pads[p]);
  }
  for (let i = 0; i < 64; i++) ui.panel_set(0, i, down[i]);
  for (let i = 0; i < 8; i++) ui.panel_set(1, i, encClicks[i]);
  ui.panel_set(2, 0, pot);
  for (let i = 0; i < 9; i++) {
    ui.panel_set(3, i, turning[i]);
    if (turning[i]) turning[i]--;
  }
  if (board === 1) for (let i = 0; i < 23; i++) ui.panel_set(4, i, analog[i]);
  const p = ui.panel_draw();
  const img = new ImageData(new Uint8ClampedArray(mem(), p, W * H * 4), W, H);
  ctx.putImageData(img, 0, 0);
  requestAnimationFrame(draw);
}
requestAnimationFrame(draw);

// ---------------------------------------------------------------- input to the machine
const send = (m) => node && node.port.postMessage(m);
function setKey(id, on) {
  if (down[id] === (on ? 1 : 0)) return;
  down[id] = on ? 1 : 0;
  send({ type: "key", id, down: on });
}
function turn(i, clicks) {
  encClicks[i] += clicks;
  turning[i] = 10;
  send({ type: "enc", enc: ui.panel_enc_id(i), clicks });
}
function setPot(v) {
  pot = Math.max(0, Math.min(1023, Math.round(v)));
  turning[7] = 10;                           // (the FM-1's knob 7: its pot; the SMK-37's K8)
  send({ type: "pot", value: pot });
}
// SMK-37: an analog input (a pad's force, a fader, a wheel)
function setAnalog(i, v) {
  v = Math.max(0, Math.min(1023, Math.round(v)));
  if (analog[i] === v) return;
  analog[i] = v;
  send({ type: "analog", idx: i, value: v });
}
// SMK-37: the velocity of the next keys, as the time between a key's two contacts: 0 (softest)
// .. 1 (hardest) -> 80 ms .. 2 ms (the stock curve: full below ~4 ms, the least above ~100 ms)
function setVelocity(f) {
  send({ type: "vel", us: Math.round(Math.exp(Math.log(80000) + (Math.log(2000) - Math.log(80000)) * Math.max(0, Math.min(1, f)))) });
}
const NKEY = 37, BTN0 = 37;

// pointer: keys and buttons held while pressed (drag across keys: glissando), knobs dragged
// up / down or scrolled, MASTER as a pot. Every pointer on its own, so on a touch screen
// several fingers play a chord, or hold a key while one turns a knob.
const at = (e) => {
  const r = canvas.getBoundingClientRect();
  return [(e.clientX - r.left) * (W / r.width), (e.clientY - r.top) * (H / r.height)];
};
const holds = new Uint8Array(64);          // pointers holding each key / button
function press(id) { if (holds[id]++ === 0) setKey(id, true); }
function unpress(id) { if (holds[id] && --holds[id] === 0) setKey(id, false); }
const pointers = new Map();                // pointerId -> { grab, lastY, acc, held }
canvas.addEventListener("pointerdown", (e) => {
  const [x, y] = at(e);
  const p = { grab: ui.panel_hit(x, y), lastY: y, acc: 0, held: -1 };
  pointers.set(e.pointerId, p);
  canvas.setPointerCapture(e.pointerId);
  if (p.grab >= 100 && p.grab < 200) {
    p.held = p.grab - 100;
    // SMK-37: a key hit further down sounds harder
    if (board === 1 && p.held < NKEY) setVelocity(ui.panel_hit_y());
    press(p.held);
  } else if (board === 1 && p.grab >= 200) slide(p, y);
});
// SMK-37: a pad pressed (harder the lower it is hit), a fader or a wheel dragged
function slide(p, y) {
  if (p.grab < 300) setAnalog(p.grab - 200, 700);
  else if (p.grab < 400) setAnalog(16 + p.grab - 300, ui.panel_slide(p.grab - 300, y));
  else setAnalog(p.grab === 400 ? 20 : 21, ui.panel_slide(p.grab === 400 ? 4 : 5, y));
}
canvas.addEventListener("pointermove", (e) => {
  const p = pointers.get(e.pointerId);
  if (!p || p.grab < 0) return;
  const [x, y] = at(e);
  const keys = board === 1 ? [0, NKEY] : [14, 41];
  if (p.grab >= 100 && p.grab < 200 && p.held >= keys[0] && p.held < keys[1]) {
    const h = ui.panel_hit(x, y);              // glissando over the note keys
    if (h >= 100 + keys[0] && h < 100 + keys[1] && h - 100 !== p.held) {
      unpress(p.held);
      p.held = h - 100;
      if (board === 1) setVelocity(ui.panel_hit_y());
      press(p.held);
    }
  } else if (board === 1 && p.grab >= 300) {
    slide(p, y);
  } else if (p.grab >= 0 && p.grab < (board === 1 ? 8 : 7)) {
    p.acc += p.lastY - y;
    while (p.acc >= 12) { p.acc -= 12; turn(p.grab, 1); }
    while (p.acc <= -12) { p.acc += 12; turn(p.grab, -1); }
  } else if (board === 0 && p.grab === 7) {
    setPot(pot + (p.lastY - y) * 4);
  }
  p.lastY = y;
});
const release = (e) => {
  const p = pointers.get(e.pointerId);
  if (!p) return;
  if (p.held >= 0) unpress(p.held);
  if (board === 1 && p.grab >= 200 && p.grab < 216) setAnalog(p.grab - 200, 0);   // a pad let go
  if (board === 1 && p.grab === 400) setAnalog(20, PITCH_REST);                   // the pitch wheel springs back
  pointers.delete(e.pointerId);
};
canvas.addEventListener("pointerup", release);
canvas.addEventListener("pointercancel", release);
canvas.addEventListener("lostpointercapture", release);
let wheelAcc = 0;
canvas.addEventListener("wheel", (e) => {
  const [x, y] = at(e);
  const h = ui.panel_hit(x, y);
  if (board === 1 && h >= 300 && h < 304) {          // SMK-37: a fader scrolled
    e.preventDefault();
    setAnalog(16 + h - 300, analog[16 + h - 300] - e.deltaY * 2);
    return;
  }
  if (h < 0 || h > 7) return;
  e.preventDefault();
  if (board === 0 && h === 7) { setPot(pot - e.deltaY * 0.5); return; }
  wheelAcc -= e.deltaY;
  while (wheelAcc >= 40) { wheelAcc -= 40; turn(h, 1); }
  while (wheelAcc <= -40) { wheelAcc += 40; turn(h, -1); }
}, { passive: false });

// computer keyboard: A W S E D F T G Y H U J K O L P ; ' = C4..F5, Z X OCT- OCT+, Space PLAY,
// R REC, 1..0 FX SCL ENV LFO EDIT GLO HOME SAVE ARP SEQ, arrows / , . encoders. SMK-37: the same
// notes (keys 12..29), Z X OCTAVE- / +, Space SEQ PLAY, R SEQ REC, 1..8 the top pads, arrows K1 / K2,
// , . K3
const PIANO = "awsedftgyhujkolp;'";
const BUTTONS = [
  { z: 0, x: 1, " ": 12, r: 13, 1: 2, 2: 3, 3: 4, 4: 5, 5: 6, 6: 7, 7: 8, 8: 9, 9: 10, 0: 11 },
  { z: BTN0 + 4, x: BTN0 + 5, " ": BTN0 + 16, r: BTN0 + 17 },
];
const ENC_KEYS = { ArrowLeft: [0, -1], ArrowRight: [0, 1], ArrowDown: [1, -1], ArrowUp: [1, 1], ",": [2, -1], ".": [2, 1] };
function keyId(k) {
  const i = PIANO.indexOf(k.toLowerCase());
  if (i >= 0) return (board === 1 ? 12 : 21) + i;
  const b = BUTTONS[board === 1 ? 1 : 0][k.toLowerCase()];
  return b === undefined ? -1 : b;
}
const padKey = (k) => (board === 1 && /^[1-8]$/.test(k) ? Number(k) - 1 : -1);
addEventListener("keydown", (e) => {
  if (e.metaKey || e.ctrlKey || e.target instanceof HTMLSelectElement) return;
  if (ENC_KEYS[e.key]) { e.preventDefault(); turn(...ENC_KEYS[e.key]); return; }
  const pad = padKey(e.key);
  if (pad >= 0) { e.preventDefault(); if (!e.repeat) setAnalog(pad, 700); return; }
  const id = keyId(e.key);
  if (id < 0 || e.repeat) return;
  e.preventDefault();
  if (board === 1 && id < NKEY) setVelocity(0.75);
  setKey(id, true);
});
addEventListener("keyup", (e) => {
  const pad = padKey(e.key);
  if (pad >= 0) { setAnalog(pad, 0); return; }
  const id = keyId(e.key);
  if (id >= 0) setKey(id, false);
});

// ---------------------------------------------------------------- MIDI in (Web MIDI)
// The chosen input(s) play the synth through its MIDI IN jack (UART1 at 31250 baud in the
// emulator). Access is asked for only once an input is picked (Chrome asks the visitor), and on
// Start when one was picked on an earlier visit. Channel: channel messages rewritten to one
// channel (0 = as sent).
const midiSel = document.getElementById("midi");
const midiChSel = document.getElementById("midich");
const midiAct = document.getElementById("midiact");
let midi = null, midiAsked = null, midiActOff = 0;
let midiCh = Number(stored("fm1.midich") || 0);
for (let c = 1; c <= 16; c++) midiChSel.add(new Option(String(c), c));
midiChSel.value = midiCh;
const midiWant = () => stored("fm1.midi") || "";
if (!navigator.requestMIDIAccess) {
  midiSel.disabled = midiChSel.disabled = true;
  document.getElementById("midiin").title = "this browser has no Web MIDI (Chrome, Edge and Firefox do)";
}
function midiMessage(e) {
  const d = e.data;
  if (!d.length || !node) return;
  const bytes = Array.from(d);
  if (midiCh && bytes[0] >= 0x80 && bytes[0] < 0xf0) bytes[0] = (bytes[0] & 0xf0) | (midiCh - 1);
  if (board === 1) {
    // the SMK-37 has no MIDI in jack: notes C3..C6 play its keys (the velocity as the time between
    // the contacts), the pitch bend and the mod wheel its wheels
    const st = bytes[0] & 0xf0, k = bytes[1] - 48;
    if ((st === 0x90 || st === 0x80) && k >= 0 && k < NKEY) {
      const on = st === 0x90 && bytes[2] > 0;
      if (on) setVelocity((bytes[2] - 1) / 126);
      setKey(k, on);
    } else if (st === 0xe0) {
      const bend = ((bytes[2] << 7) | bytes[1]) - 8192;
      setAnalog(20, PITCH_REST + (bend >= 0 ? bend / 8191 * (640 - PITCH_REST) : bend / 8192 * (PITCH_REST - 445)));
    } else if (st === 0xb0 && bytes[1] === 1) {
      setAnalog(21, MOD_REST - bytes[2] / 127 * (MOD_REST - 474));
    }
  } else {
    send({ type: "midi", bytes });
  }
  if (bytes[0] < 0xf8) {
    midiAct.classList.add("on");
    clearTimeout(midiActOff);
    midiActOff = setTimeout(() => midiAct.classList.remove("on"), 120);
  }
}
// listen to the wanted inputs only (and, with the remote on, to every input for an FM-1 running Try
// Hard); the list follows devices coming and going
function midiConnect() {
  if (!midi) return;
  const want = midiSel.value;
  const names = [...midi.inputs.values()].map((i) => i.name);
  for (const o of [...midiSel.options]) if (o.value !== "" && o.value !== "*" && !names.includes(o.value)) o.remove();
  for (const n of names) if (![...midiSel.options].some((o) => o.value === n)) midiSel.add(new Option(n, n));
  if ([...midiSel.options].some((o) => o.value === want)) midiSel.value = want;
  for (const input of midi.inputs.values()) {
    const played = want === "*" || want === input.name;
    input.onmidimessage = !played && !remoteOn ? null : (e) => {
      if (remoteOn && remote.take(input, e.data)) return;
      if (played) midiMessage(e);
    };
  }
  remote.connect();
}
async function midiOpen() {
  if (midi || !navigator.requestMIDIAccess) return midiConnect();
  midiAsked ||= navigator.requestMIDIAccess().then((m) => {
    midi = m;
    midi.onstatechange = midiConnect;
  }, () => {
    midiAsked = null;
    midiSel.value = "";
    status.textContent = "MIDI: access was not given";
  });
  await midiAsked;
  midiConnect();
}
{
  const want = midiWant();
  if (want && want !== "*") midiSel.add(new Option(want, want));
  midiSel.value = want;
}
midiSel.addEventListener("change", () => {
  store("fm1.midi", midiSel.value);
  if (midiSel.value) midiOpen(); else midiConnect();
});
midiChSel.addEventListener("change", () => {
  midiCh = Number(midiChSel.value);
  store("fm1.midich", midiCh);
});

// ---------------------------------------------------------------- a real FM-1 as the panel (Try Hard)
// An FM-1 running Try Hard (try-hard/) sends its keys, buttons, encoders and MASTER over USB-MIDI on
// channel 16 and says hello once a second; this page plays them on the emulated panel, says hello
// back and sends the emulated LEDs to it. Protocol: try-hard/PROTOCOL.md. Its messages never reach
// the emulated MIDI IN.
// (the SMK-37's page has none of these: stand-ins)
const remoteSel = document.getElementById("remote") || document.createElement("select");
const remoteAct = document.getElementById("remoteact") || document.createElement("span");
const remoteStat = document.getElementById("remotestat") || document.createElement("span");
let remoteOn = false, remotePaused = false;
const remote = {
  CH: 15, HELLO: 127, T: 84,
  links: new Map(),                  // input id -> { input, output, seen, msb }
  ledSent: new Int8Array(41).fill(-1),
  actOff: 0,
  // a message from an input: true when it was the remote's (then it goes no further)
  take(input, d) {
    if (d.length < 3 || (d[0] & 0x0f) !== this.CH || d[0] < 0x80 || d[0] >= 0xf0) return false;
    let link = this.links.get(input.id);
    const st = d[0] & 0xf0;
    if (!link) {
      if (st !== 0xb0 || d[1] !== this.HELLO || d[2] !== this.T) return false;
      link = { input, output: null, seen: 0, msb: 0 };
      this.links.set(input.id, link);
      this.connect();
      this.ledSent.fill(-1);                       // (all LEDs again for the new one)
    }
    link.seen = performance.now();
    if (board !== 0) return true;                  // (the FM-1's panel only)
    if (st === 0x90 || st === 0x80) {
      if (d[1] < 41) setKey(d[1], st === 0x90 && d[2] > 0);
    } else if (st === 0xb0) {
      const cc = d[1];
      if (cc >= 16 && cc < 23) {
        const i = encPanel[cc - 16];
        if (i >= 0) turn(i, d[2] - 64);
      } else if (cc === 7) {
        link.msb = d[2];
        setPot(d[2] << 3);
      } else if (cc === 39) {
        setPot((link.msb << 3) | (d[2] >> 4));
      } else if (cc === this.HELLO) {
        this.status();
        return true;
      }
    }
    remoteAct.classList.add("on");
    clearTimeout(this.actOff);
    this.actOff = setTimeout(() => remoteAct.classList.remove("on"), 120);
    return true;
  },
  // each linked input's output: the port of the same name
  connect() {
    if (!midi) return;
    for (const [id, link] of this.links) {
      if (![...midi.inputs.values()].some((i) => i.id === id && i.state !== "disconnected")) { this.links.delete(id); continue; }
      link.output = [...midi.outputs.values()].find((o) => o.name === link.input.name && o.state !== "disconnected") || null;
    }
    this.status();
  },
  send(bytes) {
    if (remotePaused) return;
    for (const link of this.links.values()) try { link.output?.send(bytes); } catch { /* gone: connect() drops it */ }
  },
  // once a second: hello, all LEDs again (one lost message heals), and who went quiet
  tick() {
    if (!remoteOn) return;
    const now = performance.now();
    for (const [id, link] of this.links) if (now - link.seen > 3500) this.links.delete(id);
    this.send([0xb0 | this.CH, this.HELLO, this.T]);
    this.ledSent.fill(-1);
    this.leds();
    this.status();
  },
  // the emulated LEDs to the FM-1: only what changed
  leds() {
    if (!remoteOn || !this.links.size || !frame || board !== 0) return;
    for (let i = 0; i < 41; i++) {
      const on = frame.leds[i] > 0.04 ? 1 : 0;      // (the share of time it is driven: ~0.08 is full, as the panel draws it)
      if (on === this.ledSent[i]) continue;
      this.ledSent[i] = on;
      this.send([0x90 | this.CH, i, on ? 127 : 0]);
    }
  },
  status() {
    const names = [...this.links.values()].map((l) => l.input.name);
    remoteStat.textContent = !remoteOn ? "" : remotePaused ? "installing…" :
      names.length ? `linked: ${names.join(", ")}` : "waiting for an FM-1 running Try Hard";
  },
};
// the matrix encoder (what Try Hard sends: SELECT, ALGO, K1..K4, PRESETS) -> the panel's knob
const encPanel = new Int8Array(7).fill(-1);
for (let i = 0; i < 7; i++) encPanel[ui.panel_enc_id(i)] = i;
setInterval(() => remote.tick(), 1000);
async function remoteSet(on, ask = true) {
  remoteOn = on;
  store("fm1.remote", on ? "1" : "");
  remoteSel.value = on ? "on" : "";
  if (!on) {
    remote.send([0xb0 | remote.CH, 123, 0]);       // its LEDs off
    remote.links.clear();
    remote.status();
    midiConnect();
    return;
  }
  remote.status();
  if (!ask) return;                                // (a choice from an earlier visit: access is asked on Start)
  await midiOpen();
  remote.tick();
}
remoteSel.addEventListener("change", () => { remoteSel.blur(); remoteSet(remoteSel.value === "on"); });
if (!navigator.requestMIDIAccess || DEVICE !== "fm1") remoteSel.disabled = true;
else if (stored("fm1.remote")) remoteSet(true, false);
// the installer (try-hard/, in a frame below) writes over the same USB port: keep quiet meanwhile
addEventListener("message", (e) => {
  if (e.origin !== location.origin || e.data?.type !== "try-hard-install") return;
  remotePaused = !!e.data.busy;
  remote.status();
});

// ---------------------------------------------------------------- is the device keeping up?
// Two measures, per second: the load (the share of the audio thread's time the emulation takes,
// timed in the worklet; over ~100% and the sound must break up) and the drift of the audio
// clock against the wall clock (getOutputTimestamp: the output falls behind when quanta come
// late, also with a load below 100% when the buffer is too small for this device's hiccups).
// Some seconds of either, once the JIT has warmed up, bring up a warning that offers the fix.
const warnBox = document.getElementById("perfwarn");
const perf = {
  reset() {
    this.busy = 0; this.span = 0; this.worst = 0;
    this.secs = 0; this.bad = 0; this.drops = 0; this.loads = []; this.drift = 0;
    this.stamp = null; this.mips = 0; this.dismissed = false;
    warnBox.hidden = true;
  },
  add(m) {
    this.busy += m.busy;
    this.span += (m.quanta * m.frames) / 44.1;     // ms of audio those quanta stand for
    if (m.worst > this.worst) this.worst = m.worst;
  },
  load() { return this.loads.length ? this.loads[this.loads.length - 1] : 0; },
  loadText() { return this.loads.length ? `${Math.round(100 * this.load())}%` : "…"; },
  // once a second: close the window, judge it
  second(mips, m) {
    this.mips = mips;
    if (this.span > 0) this.loads.push(this.busy / this.span);
    if (this.loads.length > 30) this.loads.shift();
    this.busy = 0; this.span = 0;
    const ts = ac.getOutputTimestamp?.();
    let drift = 0;
    if (ts && ts.contextTime > 0) {
      if (this.stamp) {
        const wall = (ts.performanceTime - this.stamp.performanceTime) / 1000;
        if (wall > 0.5) drift = 1 - (ts.contextTime - this.stamp.contextTime) / wall;
      }
      this.stamp = ts;
    }
    this.drift = drift;
    if (++this.secs <= 3 || m.halted) return;      // (the first seconds: booting, compiling)
    const over = this.load() > 0.95, late = drift > 0.02;
    if (late) this.drops++;
    this.bad = over || late ? this.bad + 1 : 0;
    if (this.bad >= 3 && warnBox.hidden && !this.dismissed) this.warn(over);
  },
  warn(over) {
    const load = this.load();
    const lower = CLOCKS.filter((c) => c < mhz && c <= mhz * 0.8 / Math.max(load, 0.01));
    const pick = lower.length ? lower[lower.length - 1] : CLOCKS.find((c) => c < mhz);
    const text = document.getElementById("perfwarn-text");
    const fix = document.getElementById("perfwarn-fix");
    fix.hidden = false;
    if (over && pick) {
      text.textContent = `This device can't run ${fw.name} at ${mhz} MHz in real time (it needs ${Math.round(100 * load)}% ` +
        `of the audio thread), so the sound breaks up. Most firmwares play fine at a lower clock.`;
      fix.textContent = `Use ${pick} MHz`;
      fix.onclick = () => { setClock(pick); };
    } else if (over) {
      text.textContent = `Even at ${mhz} MHz this device can't run ${fw.name} in real time (it needs ` +
        `${Math.round(100 * load)}% of the audio thread), so the sound breaks up. A faster device or the desktop version will play it.`;
      fix.hidden = true;
    } else if (latency !== "playback") {
      text.textContent = `The sound drops out although the emulator has time to spare (load ${Math.round(100 * load)}%): ` +
        `this device's audio buffer is too small for it. A larger one adds a little delay to the keys.`;
      fix.textContent = "Use a larger audio buffer";
      fix.onclick = () => { setLatency("playback"); };
    } else {
      text.textContent = `The sound drops out on this device, even with the larger audio buffer (load ${Math.round(100 * load)}%).`;
      fix.hidden = true;
    }
    warnBox.hidden = false;
  },
  // for a bug report: what the device is and how it kept up
  report() {
    const r = (v, d = 0) => (Number.isFinite(v) ? v.toFixed(d) : "?");
    const loads = this.loads.map((l) => Math.round(100 * l));
    return [
      `FM-1 emulator report (${location.origin}${location.pathname})`,
      `firmware: ${fw.id} ${fw.version || ""} · clock ${mhz} MHz · JIT ${params.get("jit") === "0" ? "off" : "on"}`,
      `browser: ${navigator.userAgent}`,
      `cores: ${navigator.hardwareConcurrency ?? "?"} · memory: ${navigator.deviceMemory ?? "?"} GB`,
      `audio: ${ac ? `${ac.sampleRate} Hz, buffer "${latency}", base latency ${r(1000 * (ac.baseLatency ?? NaN), 1)} ms, ` +
        `output latency ${r(1000 * (ac.outputLatency ?? NaN), 1)} ms` : "not started"}`,
      `load (last ${loads.length} s): ${loads.join(" ")}% · worst quantum since boot ${r(this.worst, 1)} ms of ${r(128 / 44.1, 1)}`,
      `audio clock drift: ${r(100 * this.drift, 1)}% · seconds with drops: ${this.drops} · ${r(this.mips)} M instructions/s`,
      `short audio quanta: ${frame?.short ?? "?"}`,
    ].join("\n");
  },
};
document.getElementById("perfwarn-copy").addEventListener("click", async (e) => {
  const b = e.currentTarget;
  try {
    await navigator.clipboard.writeText(perf.report());
    b.textContent = "Copied";
  } catch {
    prompt("Copy this report:", perf.report());     // (no clipboard access: let them copy it)
  }
  setTimeout(() => { b.textContent = "Copy a report"; }, 2000);
});
document.getElementById("perfwarn-close").addEventListener("click", () => {
  warnBox.hidden = true;
  perf.dismissed = true;
});
function setClock(c) {
  mhz = c;
  if (![...mhzSel.options].some((o) => Number(o.value) === c)) mhzSel.add(new Option(`${c} MHz`, c));
  mhzSel.value = c;
  store("fm1.mhz", c);
  showFirmware();
  boot();
}
async function setLatency(l) {
  latency = l;
  store("fm1.latency", l);
  // a larger buffer needs a new audio context: rebuild the graph, boot again
  node.port.onmessage = null;
  node.disconnect();
  await ac.close();
  await startAudio();
  await boot();
  await ac.resume();
}

// ---------------------------------------------------------------- audio: start on a click
let ac = null, last = null, bootSeq = 0, booting = 0;

// (re)boot the machine in the audio thread with the selected firmware and clock
async function boot() {
  const seq = ++bootSeq;
  const f = fw;
  if (f.id !== "local" && !f.byo) status.textContent = `downloading ${f.name} from ${new URL(f.package || f.installer).host}…`;
  let got;
  try {
    got = await packageOf(f);
  } catch (err) {
    if (seq !== bootSeq) return;
    status.textContent = err instanceof NeedFile ? `${f.name}: open your FM-1.fwsc to boot it` :
      `${f.name}: could not load it (${err.message})`;
    return;
  }
  showFirmware();                                  // (says whether a kept copy is in use)
  if (seq !== bootSeq) return;                     // another choice came in meanwhile
  loadedUrl = got.url;
  showFirmware();
  down.fill(0);                                    // nothing held or turned across a reboot
  holds.fill(0);
  pointers.clear();
  encClicks.fill(0);
  frame = null;
  last = null;
  booting = seq;                                   // (frames of the old machine still on their way)
  perf.reset();
  node.port.postMessage({ type: "boot", seq, wasm: wasm.slice(0), package: got.bytes.slice().buffer,
    syx: bank ? bank.slice().buffer : null, mhz, jit: params.get("jit") !== "0" });
  status.textContent = `booting ${f.name}…`;
}

// the audio graph: a context with the chosen buffer, the emulator's worklet node in it
async function startAudio() {
  ac = new AudioContext({ sampleRate: 44100, latencyHint: latency });
  await ac.audioWorklet.addModule("worklet.js");
  node = new AudioWorkletNode(ac, "fm1", { numberOfInputs: 0, outputChannelCount: [2] });
  node.connect(ac.destination);
  node.port.onmessage = onWorklet;
  perf.reset();
}

// what the audio thread sends: the screen and LEDs with its timing, boot and error news
function onWorklet(e) {
  const m = e.data;
  if (m.type === "error") {
    status.textContent = `${fw.name}: ${m.message}`;
    return;
  }
  if (m.type === "booted") {
    if (m.seq === bootSeq) {
      booting = 0;
      setBoard(m.board);                     // (the package's board: an SMK-37 one opened here too)
      if (m.board === 1) for (let i = 16; i < 22; i++) send({ type: "analog", idx: i, value: analog[i] });
    }
    return;
  }
  if (m.type !== "frame" || booting) return;
  // Preserve an update not yet drawn when a status-only message arrives.
  if (!m.lcd && frame?.lcd) m.lcd = frame.lcd;
  frame = m;
  remote.leds();
  perf.add(m);
  const now = performance.now();
  if (!last || now - last.t > 1000) {
    const mips = last ? (m.minsns - last.minsns) / ((now - last.t) / 1000) : 0;
    perf.second(mips, m);
    status.textContent = m.halted ? `${fw.name}: the emulated CPU halted` :
      `${fw.name} · ${mhz} MHz · ${mips.toFixed(0)} M instructions/s · load ${perf.loadText()} · ` +
      `${(m.ms / 1000).toFixed(1)} s`;
    // (hover: the JIT's work in the audio thread, and the audio's health)
    status.title = [m.jit ? `JIT: ${m.jit.regions} regions compiled, ${m.jit.failed} failed${m.jit.error ? ` (${m.jit.error})` : ""}` : "",
      `load: the share of the audio thread's time the emulator takes (over 100%: the sound breaks up)`,
      `${m.short} short audio quanta (the firmware's audio fell behind its own clock)`].filter(Boolean).join("\n");
    last = { t: now, minsns: m.minsns };
  }
}

startBtn.addEventListener("click", async () => {
  startBtn.disabled = true;
  status.textContent = "starting audio…";
  await startAudio();
  await boot();
  if (midiSel.value || remoteOn) midiOpen();      // (an input or the remote picked on an earlier visit)
  await ac.resume();
  startBtn.hidden = true;
  // a phone on its side: the synth fills the screen, so bring all of it into view
  if (matchMedia("(orientation: landscape) and (max-height: 560px)").matches) canvas.scrollIntoView({ block: "center" });
});
