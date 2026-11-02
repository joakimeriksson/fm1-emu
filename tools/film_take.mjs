// A browser take for the film: the page served locally (web/build.sh DIR; cd DIR; python3 -m http.server 8642)
// in a Chrome of its own, recorded through DevTools' screencast (no screen capture, so other windows may
// cover it):
//   open -na "Google Chrome" --args --user-data-dir=/tmp/fm1-take --no-first-run --remote-debugging-port=9222 \
//     --autoplay-policy=no-user-gesture-required --disable-backgrounding-occluded-windows --window-size=1280,940 about:blank
//   node tools/film_take.mjs 9222 "http://localhost:8642/?fw=felucca&mhz=100&script=take.txt" 30 work/film/take
// writes the frames (jpeg, 2560x1690) with their timestamps and the status line's readings to frames.json. The
// audio is fm1-live's for the same script (FM1_BENCH_FRAMES), aligned on the LCD: see the browser scene's
// _source in tools/film-0.4.0.json.
// record the tab's screencast: node rec.mjs <port> <url> <seconds> <outdir>
const [port, url, secs, outdir] = process.argv.slice(2);
const { writeFileSync, mkdirSync } = await import("node:fs");
mkdirSync(outdir, { recursive: true });
const list = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
const ws = new WebSocket(page.webSocketDebuggerUrl);
let id = 0; const pending = new Map(); const frames = []; const status = [];
const send = (method, params = {}) => new Promise((ok, no) => { const i = ++id; pending.set(i, { ok, no, m: method }); ws.send(JSON.stringify({ id: i, method, params })); });
ws.onmessage = (m) => {
  const d = JSON.parse(m.data);
  if (d.id && pending.has(d.id)) { const p = pending.get(d.id); pending.delete(d.id); d.error ? p.no(new Error(p.m + ": " + d.error.message)) : p.ok(d.result); return; }
  if (d.method === "Page.screencastFrame") {
    const n = frames.length; frames.push({ n, t: d.params.metadata.timestamp, w: d.params.metadata.deviceWidth, h: d.params.metadata.deviceHeight });
    writeFileSync(`${outdir}/${String(n).padStart(6, "0")}.jpg`, Buffer.from(d.params.data, "base64"));
    send("Page.screencastFrameAck", { sessionId: d.params.sessionId });
  }
};
await new Promise((ok) => (ws.onopen = ok));
await send("Page.enable");
await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 845, deviceScaleFactor: 2, mobile: false });
await send("Page.navigate", { url: new URL("/take-script.txt", url).href });  // same origin first: no renderer swap under the screencast
await new Promise((ok) => setTimeout(ok, 1500));
await send("Page.startScreencast", { format: "jpeg", quality: 95, maxWidth: 2560, maxHeight: 1690, everyNthFrame: 1 });
const t0 = Date.now();
await send("Page.navigate", { url });
const ev = async (expr) => (await send("Runtime.evaluate", { expression: expr, returnByValue: true })).result.value;
const end = t0 + Number(secs) * 1000; let last = "";
while (Date.now() < end) {
  const s = await ev(`[performance.timeOrigin + performance.now(), ([...document.querySelectorAll("*")].find(e => /MHz · .*x real time · load/.test(e.textContent) && e.children.length === 0) || {textContent: ""}).textContent]`).catch(() => null);
  if (s && s[1] !== last) { status.push({ t: s[0] / 1000, s: s[1] }); last = s[1]; }
  await new Promise((ok) => setTimeout(ok, 250));
}
await send("Page.stopScreencast");
await new Promise((ok) => setTimeout(ok, 500));
writeFileSync(`${outdir}/frames.json`, JSON.stringify({ frames, status }));
console.log(`frames ${frames.length}, ${frames[0]?.w}x${frames[0]?.h}, ${(frames.at(-1).t - frames[0].t).toFixed(2)} s, status lines ${status.length}`);
console.log(status.slice(0, 3).map((x) => x.s).join(" | "));
console.log(status.slice(-2).map((x) => x.s).join(" | "));
ws.close();
