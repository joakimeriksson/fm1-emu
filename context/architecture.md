---
load_when: "architecture, runtime boundaries, storage or bridge changes"
last_verified: 2026-11-15
source_refs:
  - web/studio/**
  - web/main.js
  - web/persistence.js
  - tools/package-studio.mjs
  - crates/fm1-plugin/src/**
---
# Architecture

## One execution core, separate host surfaces

Keep the Rust CPU/SoC and original firmware execution path authoritative. Standalone uses fm1-live; plugin uses the existing native worker; browser uses fm1-web through its AudioWorklet. Studio adds no synthesis implementation.

The browser Studio hosts the upstream-compatible device page in a same-origin iframe. Its narrow postMessage bridge accepts local imports and export requests only; check both origin and source window in each direction. It never starts audio automatically or performs hardware operations.

Studio mode (`studio=1`) disables automatic Hub discovery and catalog downloads. The default clock is explicitly 240 MHz. The device still starts only after a user gesture. The upstream page remains available independently for comparison.

## Data ownership

The Studio IndexedDB database stores immutable imported firmware/setup records, keyed by their content hash. Firmware identity is separately the original package hash. Import validation calls the existing unpacker, retaining unchanged bytes. Collection imports validate all entries before one write transaction. UI labels never render user filenames as HTML.

The device page owns its running flash and upstream save database. Studio exports its current persistent setup through the bridge; files can be imported back into the collection. Full CPU/RAM snapshotting is not implemented. Multiple saved setups of one package still address upstream’s shared working flash; this needs separate instance identities before stronger isolation claims.

There is no implicit native/browser storage synchronization. All hardware backup and recovery routes remain future work. Unknown formats cannot be converted into a firmware package simply by renaming a file.

## Offline boundary

The build produces an asset-hashed service worker and local static runtime. Install caches the complete required asset set; a failed install deletes its incomplete cache. Updates wait for the normal worker lifecycle, rather than forcefully activating during playback. Runtime requests are served from one installed build, including upstream cache-busting query variants.

Only the build’s same-origin asset allowlist is cached. Firmware comes from the explicit library/discovery flow, not arbitrary service-worker interception. The service worker scope covers the Studio and device page. Library bytes remain in IndexedDB; caches contain application assets. Browser eviction/clear-data behavior still requires exported backups.

## Future boundary

Firmware-specific adapters should target the emulated USB interface and physical MIDI transport with the same protocol contract. Privileged recovery stays in a standalone coordinator/helper, never the audio callback. Full machine snapshots, silent-firmware progress, general MIDI/SysEx and host transport need dedicated feasibility work before expanding the editor.
