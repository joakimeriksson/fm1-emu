---
load_when: "architecture, runtime boundaries, storage or bridge changes"
last_verified: 2026-11-15
source_refs:
  - web/studio/**
  - web/main.js
  - web/persistence.js
  - tools/package-studio.mjs
  - crates/fm1-plugin/src/**
  - web/render-worker.js
  - web/render-runtime.js
  - web/worklet.js
  - crates/fm1-soc/src/controller.rs
---
# Architecture

## One execution core, separate host surfaces

Keep the Rust CPU/SoC and original firmware execution path authoritative. Standalone uses fm1-live; plugin uses the existing native worker; browser executes fm1-web in a dedicated render worker and sends audio through a small AudioWorklet queue. Studio adds no synthesis implementation.

The browser Studio hosts the upstream-compatible device page in a same-origin iframe. Its narrow postMessage bridge accepts local imports and export requests only; check both origin and source window in each direction. It never starts audio automatically or performs hardware operations.

Studio mode (`studio=1`) disables automatic Hub discovery and catalog downloads. The default clock is explicitly 240 MHz. The device still starts only after a user gesture. The upstream page remains available independently for comparison.

The browser renderer (`render-worker.js` + `render-runtime.js`) owns the machine, JIT and snapshots. A direct MessageChannel connects it to `worklet.js`, which consumes a bounded stereo queue (four quanta initially, growing by two on an underrun to at most 16). LCD/flash messages travel directly from the worker to the page; only audio and small control messages reach the realtime thread. Boot IDs reject stale audio; pause acknowledgements flush queued output. No SharedArrayBuffer or cross-origin isolation is required. This adds about 11.6 ms of queue headroom at 44.1 kHz while moving compilation off audio deadlines.

Factory MiniLab 3 profiles translate recognized CCs into physical panel actions before firmware MIDI. Only the standard performance port is auto-mapped. Mapped CCs are consumed once; other messages pass through. Rust and browser policies have matching regression cases. DAW plugin MIDI retains its existing raw behavior.

## Data ownership

The Studio IndexedDB database stores immutable imported firmware/setup records, keyed by their content hash. Firmware identity is separately the original package hash. Import validation calls the existing unpacker, retaining unchanged bytes. Collection imports validate all entries before one write transaction. UI labels never render user filenames as HTML.

The device page owns its running flash and upstream save database. Studio exports its current persistent setup through the bridge; files can be imported back into the collection. Full CPU/RAM snapshotting is not implemented. Working storage is now scoped by the Studio instance UUID in its URL plus the immutable library item ID. A new base-URL visit creates a separate instance; retaining an instance URL permits persistent recall. Deliberately opening the same instance URL twice shares the same working scope and retains upstream revision conflict detection. Switching waits for a flush acknowledgement; timeout keeps the current device. Reopening a setup resumes its scoped working copy rather than reapplying its original imported baseline.

There is no implicit native/browser storage synchronization. All hardware backup and recovery routes remain future work. Unknown formats cannot be converted into a firmware package simply by renaming a file.

## Offline boundary

The build produces an asset-hashed service worker and local static runtime. Install caches the complete required asset set; a failed install deletes its incomplete cache. Updates wait for the normal worker lifecycle, rather than forcefully activating during playback. Runtime requests are served from one installed build, including upstream cache-busting query variants.

Only the build’s same-origin asset allowlist is cached. Firmware comes from the explicit library/discovery flow, not arbitrary service-worker interception. The service worker scope covers the Studio and device page. Library bytes remain in IndexedDB; caches contain application assets. Browser eviction/clear-data behavior still requires exported backups.

## Future boundary

Firmware-specific adapters should target the emulated USB interface and physical MIDI transport with the same protocol contract. Privileged recovery stays in a standalone coordinator/helper, never the audio callback. Full machine snapshots, silent-firmware progress, general MIDI/SysEx and host transport need dedicated feasibility work before expanding the editor.

MIDI input routing uses exact port IDs; migration from a name is allowed only when unambiguous. Disconnect and route/channel changes send sustain-off, all-notes-off and all-sound-off for each channel. This is performance MIDI, not a hardware-transfer or SysEx implementation.
