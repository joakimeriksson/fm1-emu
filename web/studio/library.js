import { appFromPackage, isApplicationArea } from "../fwsc.js";
import { packageHash, importSave, exportSave } from "../persistence.js";

export const MAX_FILE = 16 << 20;
export async function prepareImport(name, bytes, resolvePackage = async () => null) {
  if (!(bytes instanceof Uint8Array) || !bytes.length || bytes.length > MAX_FILE)
    throw new Error("Choose a non-empty firmware or save file smaller than 16 MB.");
  if (/\.fm1save$/i.test(name)) {
    const record = await importSave(new TextDecoder().decode(bytes), resolvePackage);
    appFromPackage(record.package);
    return { id: await packageHash(bytes), kind: "save", name: record.name, hash: record.hash,
      bytes, package: record.package, importedAt: new Date().toISOString() };
  }
  if (!/\.(fwsc|area)$/i.test(name)) throw new Error("Choose an original .fwsc or .area firmware or a .fm1save setup.");
  if (bytes.length > 8 << 20) throw new Error("Firmware packages must be smaller than 8 MB.");
  appFromPackage(bytes); // Validate the package; retain the original unchanged bytes.
  const hash = await packageHash(bytes);
  return { id: hash, kind: "firmware", name: name.replace(/\.(fwsc|area)$/i, ""), hash,
    bytes: bytes.slice(), package: bytes.slice(), importedAt: new Date().toISOString() };
}
export function filterItems(items, query) {
  const q = query.trim().toLocaleLowerCase();
  return items.filter(item => (item.name + " " + item.hash).toLocaleLowerCase().includes(q))
    .sort((a,b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
}
export function downloadItem(item) {
  return { bytes: item.bytes, name: item.name.replace(/[^a-z0-9_ .-]/gi, "_") +
    (item.kind === "save" ? ".fm1save" : (isApplicationArea(item.bytes) ? ".area" : ".fwsc")) };
}
export function encodeCollection(items) {
  if (items.length > 256 || items.reduce((n,item) => n + item.bytes.length,0) > 24 << 20)
    throw new Error("Collection exceeds this archive format’s 256-item / 24 MB limit. Export items individually.");
  return JSON.stringify({ format: "fm1-studio-library", version: 1,
    items: items.map(item => ({ filename: downloadItem(item).name,
      data: Array.from(item.bytes) })) });
}
export async function decodeCollection(text, resolvePackage) {
  if (text.length > 128 << 20) throw new Error("Collection backup is too large.");
  const data = JSON.parse(text);
  if (data?.format !== "fm1-studio-library" || data.version !== 1 ||
      !Array.isArray(data.items) || data.items.length > 256) throw new Error("Unsupported collection backup.");
  let total = 0;
  const valid = data.items.map(item => {
    if (typeof item?.filename !== "string" || item.filename.length > 256 ||
        !Array.isArray(item.data) || (total += item.data.length) > 24 << 20 ||
        item.data.some(byte => !Number.isInteger(byte) || byte < 0 || byte > 255))
      throw new Error("Invalid collection contents.");
    return { name: item.filename, bytes: new Uint8Array(item.data) };
  });
  // Resolve firmware entries first, independent of archive ordering.
  const packages = new Map();
  const results = [];
  for (const item of valid.filter(item => /\.(fwsc|area)$/i.test(item.name))) {
    const record = await prepareImport(item.name, item.bytes);
    packages.set(record.hash, record.package); results.push(record);
  }
  for (const item of valid.filter(item => !/\.(fwsc|area)$/i.test(item.name)))
    results.push(await prepareImport(item.name, item.bytes,
      async hash => packages.get(hash) || await resolvePackage?.(hash)));
  return results;
}
export class Library {
  async open() {
    if (!this.db) this.db = new Promise((resolve,reject) => {
      const req = indexedDB.open("fm1-studio-library", 1);
      req.onupgradeneeded = () => req.result.createObjectStore("items", {keyPath:"id"});
      req.onerror = () => reject(req.error);
      req.onblocked = () => reject(new Error("Close other Studio tabs to open the library."));
      req.onsuccess = () => {
        req.result.onversionchange = () => { req.result.close(); this.db = null; };
        resolve(req.result);
      };
    }).catch(error => { this.db = null; throw error; });
    return this.db;
  }
  async all() {
    const db = await this.open();
    return new Promise((resolve,reject) => {
      const tx = db.transaction("items","readonly"), request = tx.objectStore("items").getAll();
      tx.oncomplete = () => resolve(request.result);
      tx.onabort = tx.onerror = () => reject(tx.error || new Error("Cannot read library."));
    });
  }
  async putMany(items) {
    const db = await this.open();
    return new Promise((resolve,reject) => {
      const tx = db.transaction("items","readwrite"), store = tx.objectStore("items");
      for (const item of items) {
        const request = store.getKey(item.id);
        request.onsuccess = () => { if (request.result === undefined) store.add(item); };
      }
      tx.oncomplete = resolve;
      tx.onabort = tx.onerror = () => reject(tx.error || new Error("Library not saved. Export a backup and check browser storage."));
    });
  }
}
