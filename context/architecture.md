---
load_when: "architecture, runtime boundaries, storage or bridge changes"
last_verified: 2026-11-22
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
  - crates/fm1-host/src/lib.rs
  - crates/fm1-host/src/audio.rs
  - crates/fm1-live/src/wav.rs
  - crates/fm1-live/src/main.rs
  - crates/fm1-soc/src/fwsc.rs
  - web/hardware/**
---
# Architecture

## One execution core, separate host surfaces

Keep the Rust CPU/SoC and original firmware execution path authoritative. Standalone uses fm1-live; plugin uses the existing native worker; browser executes fm1-web in a dedicated render worker and sends audio through a small AudioWorklet queue. Studio adds no synthesis implementation.

The browser Studio hosts the upstream-compatible device page in a same-origin iframe. Its narrow postMessage bridge accepts local imports and export requests only; check both origin and source window in each direction. It never starts audio automatically or performs hardware operations.

Studio mode (`studio=1`) disables automatic Hub discovery and catalog downloads. The default clock is explicitly 240 MHz. The device still starts only after a user gesture. The upstream page remains available independently for comparison.

The browser renderer (`render-worker.js` + `render-runtime.js`) owns the machine, JIT and snapshots. A direct MessageChannel connects it to `worklet.js`, which consumes a bounded stereo queue (four quanta initially, growing by two on an underrun to at most 16 and shrinking by one after each ten seconds of uninterrupted output). LCD/flash messages travel directly from the worker to the page; only audio and small control messages reach the realtime thread. Boot IDs reject stale audio; pause acknowledgements flush queued output. No SharedArrayBuffer or cross-origin isolation is required. This adds about 11.6 ms of queue headroom at 44.1 kHz while moving compilation off audio deadlines.

Factory MiniLab 3 profiles translate recognized CCs into physical panel actions before firmware MIDI. Only the standard performance port is auto-mapped. Mapped CCs are consumed once; other messages pass through. Rust and browser policies have matching regression cases. DAW plugin MIDI retains its existing raw behavior.

## Desktop audio delivery

Standalone and plugins use `fm1-host::audio::Playback` over preallocated SPSC stereo rings. Device/plugin callbacks take no queue or UI locks and allocate no playback buffers. Startup waits for prefill. A dropout counts once, fades to zero over 32 output samples, waits for refill and fades back in; steady playback retains fractional conversion phase. Linear rate conversion remains in use. This recovery softens a discontinuity; it cannot reconstruct missing samples.

Standalone starts with a 12 ms queue and requests a 128-frame macOS device buffer, falling back to the device default if unsupported. Its adaptive queue can still grow to 80 ms. New plugin instances use a 512-source-frame lead (about 12 ms) beyond the configured maximum host block; saved Lead choices and latency reporting are retained. Both producers use interactive QoS on macOS, not an Audio Workgroup deadline guarantee. Plugin starvation telemetry uses an atomic episode counter instead of a callback-side view lock.

WAV writes use a separate worker with a bounded queue of 32 audio batches. If storage cannot keep up, the take stops with an error; playback never waits for queue space. Finalization drains accepted batches. Queue copies still allocate on the producer, outside the device callback. Browser output uses a fixed 16-slot queue and the same 32-sample dropout fades; it retains MessageChannel transport.

Firmware execution, dual-core scheduling, DMA extraction, DIN MIDI timing and GUI input polling are unchanged. Audio Workgroups, band-limited conversion, timestamped standalone input, shared-memory browser transport and verified offline DAW rendering remain separate work.

## Data ownership

The Studio IndexedDB database stores immutable imported firmware/setup records, keyed by their content hash. Firmware identity is separately the original package hash. Import validation calls the existing unpacker, retaining unchanged bytes. Collection imports validate all entries before one write transaction. UI labels never render user filenames as HTML.

The device page owns its running flash and upstream save database. Studio exports its current persistent setup through the bridge; files can be imported back into the collection. Full CPU/RAM snapshotting is not implemented. Working storage is now scoped by the Studio instance UUID in its URL plus the immutable library item ID. A new base-URL visit creates a separate instance; retaining an instance URL permits persistent recall. Deliberately opening the same instance URL twice shares the same working scope and retains upstream revision conflict detection. Switching waits for a flush acknowledgement; timeout keeps the current device. Reopening a setup resumes its scoped working copy rather than reapplying its original imported baseline.

There is no implicit native/browser storage synchronization. The shared project-only Hardware Library is separate from emulation; hardware verification and full backup/recovery remain pending. See [hardware-library.md](hardware-library.md). Unknown formats cannot be converted into a firmware package simply by renaming a file.

## Offline boundary

The build produces an asset-hashed service worker and local static runtime. Install caches the complete required asset set; a failed install deletes its incomplete cache. Updates wait for the normal worker lifecycle, rather than forcefully activating during playback. Runtime requests are served from one installed build, including upstream cache-busting query variants.

Only the build’s same-origin asset allowlist is cached. Firmware comes from the explicit library/discovery flow, not arbitrary service-worker interception. The service worker scope covers the Studio and device page. Library bytes remain in IndexedDB; caches contain application assets. Browser eviction/clear-data behavior still requires exported backups.

## Future boundary

Firmware-specific adapters should target the emulated USB interface and physical MIDI transport with the same protocol contract. Privileged recovery stays in a standalone coordinator/helper, never the audio callback. Full machine snapshots, silent-firmware progress, general MIDI/SysEx and host transport need dedicated feasibility work before expanding the editor.

MIDI input routing uses exact port IDs; migration from a name is allowed only when unambiguous. Disconnect and route/channel changes send sustain-off, all-notes-off and all-sound-off for each channel. This is performance MIDI, not a hardware-transfer or SysEx implementation.

## Standalone plain application areas

`fm1_soc::fwsc::from_area` validates the m-bootloader `app_area_head`/`app.bin` records and payload CRC, retains original XIP bytes and installs an encrypted copy into blank emulated flash at 0x4000. The standalone loader supplies a boot stack for the original entry stub, then uses the existing SoC/SFC path. All three hosts call `firmware_from_bytes` for content detection and `boot_firmware` for the boot stack, board, flash and SFC handoff. Plugin and WASM restore through `Soc::restore_flash`; direct copies of raw saved flash into XIP bypass decryption and are forbidden. JS `appFromPackage` provides preflight validation only; Rust remains authoritative at execution. The existing WASM `fm1_boot_fwsc` ABI also accepts `.area` to preserve caller compatibility. Original bytes and their hash remain the save identity for either format. Browser collection exports infer the original extension from the bytes, including older records without format metadata.

## Shared hardware companion

Standalone F6 and the plugin toolbar use `fm1-host` to open the same embedded companion that Studio serves at `/hardware/`. It is a deliberately separate physical-device destination with one protocol adapter, a bounded Web MIDI session and explicit project-file operations. Device capabilities are version-gated; no emulator import, plugin state recall or firmware change calls hardware code. Browser and native-launch copies have separate origins, so their session locks do not coordinate across those origins. Close other editors before connecting. Details and limits live in [hardware-library.md](hardware-library.md).

Standalone GUI recall stores launch arguments and a content-hashed working-flash file under the host session directory. Headless diagnostics remain explicit. Finder reopening restores that exact instrument, while DAW project recall remains instance-owned and browser selection is keyed by instance URL. Reset uses each host's existing restart/load path with initial flash; no CPU/RAM snapshot or hardware operation is implied.
