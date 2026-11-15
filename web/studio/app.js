import { Library, prepareImport, filterItems, downloadItem, encodeCollection, decodeCollection } from "./library.js";
import { fetchPackage } from "../fwsc.js";
const $ = id => document.getElementById(id);
const library = new Library();
const instanceQuery = new URLSearchParams(location.search);
let instanceId = instanceQuery.get("instance");
if (!/^[a-f0-9-]{36}$/.test(instanceId || "")) {
  instanceId = crypto.randomUUID();
  instanceQuery.set("instance", instanceId);
  history.replaceState(null,"", "?" + instanceQuery);
}
let items = [], active = null, frameReady = false, pendingItem = null, noticeTimer;
const frame = $("device-frame");
function notice(message, error = false) {
  $("notice").textContent = message; $("notice").dataset.error = error; $("notice").hidden = false;
  clearTimeout(noticeTimer); noticeTimer = setTimeout(() => $("notice").hidden = true, error ? 16000 : 6000);
}
function showView(view) {
  for (const page of document.querySelectorAll("[data-page]")) page.hidden = page.dataset.page !== view;
  for (const button of document.querySelectorAll("[data-view]")) {
    if (button.dataset.view === view) button.setAttribute("aria-current","page");
    else button.removeAttribute("aria-current");
  }
}
for (const button of document.querySelectorAll("[data-view]")) button.addEventListener("click", () => showView(button.dataset.view));
const openPicker = () => $("file-input").click();
for (const id of ["import-side","import-main","import-library"]) $(id).addEventListener("click", openPicker);
const resolvePackage = async hash => items.find(item => item.hash === hash)?.package || null;
async function ingestFiles(files) {
  const added = [];
  // Validate everything first; store all entries in one transaction.
  const stagedPackages = new Map();
  const ordered = [...files].sort((a,b) => Number(!/\.fwsc$/i.test(a.name)) - Number(!/\.fwsc$/i.test(b.name)));
  for (const file of ordered) {
    if (file.size > (/\.fm1library$/i.test(file.name) ? 128 << 20 : 16 << 20)) throw new Error("File exceeds the import size limit.");
    const resolver = async hash => stagedPackages.get(hash) || await resolvePackage(hash);
    const records = /\.fm1library$/i.test(file.name)
      ? await decodeCollection(await file.text(), resolver)
      : [await prepareImport(file.name, new Uint8Array(await file.arrayBuffer()), resolver)];
    for (const record of records) { stagedPackages.set(record.hash, record.package); added.push(record); }
  }
  await library.putMany(added); await refresh();
  notice(added.length === 1 ? "Saved locally. Ready offline." : added.length + " files saved locally.");
  if (added[0]) loadItem(added[0]);
}
$("file-input").addEventListener("change", async e => {
  try { await ingestFiles(e.target.files); } catch (error) { notice(error.message, true); }
  finally { e.target.value = ""; }
});
function element(tag, cls, text) {
  const el = document.createElement(tag); if (cls) el.className = cls; if (text != null) el.textContent = text; return el;
}
function sendItem(item) {
  frame.contentWindow.postMessage({source:"fm1-studio",type:"load",kind:item.kind,name:item.name,
    bytes:item.bytes, package:item.package}, location.origin);
}
let loadQueue = Promise.resolve();
const flushWaiters = new Map();
function loadItem(item) {
  loadQueue = loadQueue.then(() => switchItem(item)).catch(error => notice(error.message,true));
  return loadQueue;
}
async function switchItem(item) {
  if (active?.id === item.id && frame.getAttribute("src")) { showView("device"); return; }
  if (frameReady) await new Promise((resolve,reject) => {
    const request = crypto.randomUUID();
    const timer = setTimeout(() => {flushWaiters.delete(request);reject(new Error("Could not save the current device. Export its setup before switching."));},10000);
    flushWaiters.set(request,{resolve,reject,timer});
    frame.contentWindow.postMessage({source:"fm1-studio",type:"flush",request},location.origin);
  });
  active = item; pendingItem = item; frameReady = false; showView("device"); render();
  $("empty-device").hidden = true; frame.hidden = false;
  $("active-name").textContent = item.name;
  $("identity-name").textContent = item.name;
  $("identity-description").textContent = (item.kind === "save" ? "Persistent setup" : "Original firmware package") +
    " · " + Math.round(item.package.length / 1024) + " KB · saved locally";
  $("identity-hash").textContent = item.hash;
  $("engine-status").textContent = "Preparing the device…";
  $("export-save").disabled = true;
  frame.src = "../index.html?embed=1&studio=1&mhz=240&instance=" + instanceId + ":" + item.id;
}
function saveFile(bytes, name) {
  const url = URL.createObjectURL(new Blob([bytes],{type:"application/octet-stream"}));
  const link = Object.assign(document.createElement("a"),{href:url,download:name});
  link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000);
}
function render() {
  const filtered = filterItems(items,$("search").value);
  $("library-count").textContent = String(items.length);
  $("sidebar-count").textContent = String(items.length).padStart(2,"0");
  $("firmware-list").replaceChildren(); $("library-grid").replaceChildren();
  for (const item of filtered) {
    const button = element("button","firmware-item");
    button.setAttribute("aria-pressed",String(active?.id === item.id));
    const details = element("span"), title = element("span","item-name",item.name);
    details.append(title,element("small",null,(item.kind === "save" ? "SETUP" : "FIRMWARE") + " / " + item.hash.slice(0,8)));
    button.append(element("span","item-icon",item.kind === "save" ? "S" : "F"), details);
    button.addEventListener("click",() => loadItem(item)); $("firmware-list").append(button);
    const card = element("article","library-card");
    card.append(element("span","eyebrow",item.kind === "save" ? "SAVED SETUP" : "ORIGINAL FIRMWARE"),
      element("h3",null,item.name),element("p",null,Math.round(item.package.length/1024)+" KB · Available offline"),
      element("code",null,item.hash));
    const actions = element("div","action-row");
    const play = element("button","button primary","Open instrument");
    play.addEventListener("click",()=>loadItem(item));
    const download = element("button","text-button","Export ↗");
    download.addEventListener("click",()=> { const f=downloadItem(item);saveFile(f.bytes,f.name); });
    actions.append(play,download);card.append(actions);$("library-grid").append(card);
  }
  if (!filtered.length) {
    $("firmware-list").append(element("p","sidebar-empty",items.length ? "No matches." : "An open collection. Start with your first firmware."));
    $("library-grid").append(element("p","library-empty",items.length ? "No matches. Try another name or hash." : "Your collection starts here. Import firmware or a saved setup; everything stays local."));
  }
  $("export-library").disabled = !items.length;
}
async function refresh() { items = await library.all(); render(); }
$("search").addEventListener("input",render);
$("export-library").addEventListener("click",() => {
  try {
    saveFile(encodeCollection(items),"FM-1 collection.fm1library");
    notice("Collection exported. Keep this backup separately from browser storage.");
  } catch(error) { notice(error.message,true); }
});
$("discover").addEventListener("click",async e => {
  const button=e.currentTarget;button.disabled=true;button.textContent="Downloading…";
  try {
    const catalog=await (await fetch("../firmwares.json")).json();
    const firmware=catalog.firmwares.find(f=>f.id==="felucca");
    if (!firmware) throw new Error("Felucca is unavailable in this catalog. Import a .fwsc instead.");
    const pkg=await fetchPackage(firmware);
    await ingestFiles([new File([pkg.bytes], "Felucca " + firmware.version + ".fwsc")]);
  } catch(error) { notice("Download failed: "+error.message,true); }
  finally {button.disabled=false;button.textContent="Get Felucca ↗";}
});
$("export-save").addEventListener("click",()=> {
  frame.contentWindow.postMessage({source:"fm1-studio",type:"export"}, location.origin);
});
window.addEventListener("message",event=> {
  if(event.origin!==location.origin || event.source!==frame.contentWindow || event.data?.source!=="fm1-device") return;
  const message=event.data;
  if(message.type==="flushed" || (message.type==="error" && message.request)) {
    const pending = flushWaiters.get(message.request);
    if(pending) {clearTimeout(pending.timer);flushWaiters.delete(message.request);
      if(message.type==="error") pending.reject(new Error(message.text)); else pending.resolve();}
  } else if(message.type==="ready") {
    frameReady=true; if(pendingItem) {sendItem(pendingItem);pendingItem=null;}
  } else if(message.type==="status") {
    $("engine-status").textContent=message.text;
    $("export-save").disabled=!message.running;
  } else if(message.type==="export") {
    saveFile(message.text,(message.name || "FM-1").replace(/[^a-z0-9_ .-]/gi,"_")+".fm1save");
    notice("Saved flash exported. Unsaved RAM edits need the firmware’s own Save command.");
  } else if(message.type==="error") notice(message.text,true);
});
async function offlineStatus() {
  const label=$("offline-state");
  if(!("serviceWorker" in navigator)) {label.lastChild.textContent="Offline cache unavailable";return;}
  try {
    await navigator.serviceWorker.register("../sw.js");
    await navigator.serviceWorker.ready;
    label.lastChild.textContent="Ready offline";
    if(!navigator.serviceWorker.controller) notice("Offline assets saved. Reload once before going offline.");
  } catch(error) {label.lastChild.textContent="Offline cache unavailable";notice(error.message,true);}
}
try {await refresh();} catch(error) {notice(error.message,true);$("engine-status").textContent="Library unavailable. Check browser storage permissions.";}
offlineStatus();
