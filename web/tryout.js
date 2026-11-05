// A try-out: the page started with ?flash=URL and/or ?script=URL, for another page of this site that
// wants its visitors to try something in the emulator (fm1-dock's catalog: a module installed and
// picked). The firmware (?fw=) boots from its package's own flash, with:
//   ?flash=URL   flash sectors laid over it: records of a little-endian u32 flash offset (4 KiB
//                aligned) and the sector's 4096 bytes (as the saves keep them: persistence.js's
//                packSectors), at most 256 of them;
//   ?script=URL  an input script on the emulated clock, in fm1-live's FM1_BENCH_KEYS syntax: `ms:mHEX`
//                MIDI bytes into the MIDI IN jack (the DIN jack, at its 31250 baud), `ms:id` / `ms:id^`
//                a key or button pressed / released, `ms:eN:clicks` an encoder, `ms:pot:N` the pot;
//                ms counts from the boot.
// Both are fetched from this page's own origin only (a path, or a URL on it): never from another
// site. A try-out is a session of its own: it neither loads nor saves this browser's saved flash.
// Nothing of it goes anywhere but into the emulated machine (no MIDI port of the computer).
import { FLASH_SIZE, SECTOR_SIZE, validateSectors } from './persistence.js';

export const MAX_SCRIPT = 1 << 20;
const RECORD = 4 + SECTOR_SIZE;

// the parameter's URL, if it is on this page's origin (http or https); else an error
export function sameOrigin(value, base) {
  let url;
  try { url = new URL(value, base); } catch { throw new Error(`not a URL: ${value}`); }
  const page = new URL(base);
  if (!/^https?:$/.test(url.protocol) || url.origin !== page.origin) throw new Error(`only files from ${page.origin} can be loaded`);
  return url;
}

// the try-out the page's parameters ask for, or null
export function tryOutOf(params, base) {
  const flash = params.get('flash'), script = params.get('script');
  if (!flash && !script) return null;
  return { flash: flash ? sameOrigin(flash, base) : null, script: script ? sameOrigin(script, base) : null };
}

// a ?flash= file into sectors (validated as a save's are)
export function parseSectors(bytes) {
  if (!(bytes instanceof Uint8Array) || !bytes.length || bytes.length % RECORD || bytes.length > 256 * RECORD) {
    throw new Error('not a flash sector file (records of a 4-byte offset and 4096 bytes)');
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength), sectors = [];
  for (let o = 0; o < bytes.length; o += RECORD) {
    sectors.push({ at: view.getUint32(o, true), data: bytes.slice(o + 4, o + RECORD) });
  }
  return validateSectors(sectors);
}

// the latest time in a script (ms), for when it has played
export function scriptEnd(text) {
  let end = 0;
  for (const k of text.split(',')) {
    const t = Number(k.split(':', 1)[0].trim());
    if (k.includes(':') && Number.isFinite(t) && t > end) end = t;
  }
  return end;
}

async function get(url, fetchFn) {
  // (no redirects: a same-origin URL must not lead elsewhere)
  const r = await fetchFn(url.href, { credentials: 'same-origin', redirect: 'error', cache: 'no-cache' });
  if (!r.ok) throw new Error(`${url.pathname}: HTTP ${r.status}`);
  return r;
}

// the try-out's files: { sectors, script, end }
export async function loadTryOut(t, fetchFn = fetch) {
  const [sectors, script] = await Promise.all([
    t.flash ? get(t.flash, fetchFn).then(async (r) => parseSectors(new Uint8Array(await r.arrayBuffer()))) : [],
    t.script ? get(t.script, fetchFn).then(async (r) => {
      const text = await r.text();
      if (text.length > MAX_SCRIPT) throw new Error(`${t.script.pathname}: larger than ${MAX_SCRIPT} bytes`);
      return text.trim();
    }) : '',
  ]);
  if (sectors.some((s) => s.at + SECTOR_SIZE > FLASH_SIZE)) throw new Error('a sector beyond the flash');
  return { sectors, script, end: scriptEnd(script) };
}
