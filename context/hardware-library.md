---
load_when: "USB hardware librarian, project transfers, firmware adapters or host companion launch"
last_verified: 2026-11-16
source_refs:
  - web/hardware/**
  - crates/fm1-host/**
  - crates/fm1-live/src/main.rs
  - crates/fm1-plugin/src/editor.rs
  - tools/package-studio.mjs
---
# Hardware Library

## Scope and boundaries

A file-based SLOOP 3.3.7 project librarian, separate from emulation. One shared web UI and Web MIDI transport serve all three hosts. `fm1-host` embeds the exact HTML/JS sources in native builds and opens a stable local companion file in Chrome/Edge. Studio serves those sources with its offline asset set. No native USB service, background daemon, account, automatic synchronization, new synthesis engine or generalized adapter framework is needed for this increment.

`web/hardware/protocol.js` implements the inspected protocol contract independently; no upstream editor source is bundled or relicensed. `transport.js` owns one outstanding request, command matching, timeout poisoning and disconnect handling. `page.js` owns explicit connect, file download, preparation, destination review, safety backup acknowledgement and restore. Native hosts only launch the companion from a deliberate user gesture; they do not send SysEx. Browser Web Locks serialize companion connections within one origin. This does not serialize different origins/profiles or third-party apps; users must disconnect other editors. A future native coordinator is needed only if native transports are introduced.

The destination is always physical hardware. Emulator/DAW recall never invokes the companion or writes hardware. File I/O and launcher work run on a host job thread. The same embedded page continues independently of a plugin window closing. The companion itself must remain open during a transfer.

## Supported operations

SLOOP protocol 2, five tracks, firmware version exactly `3.3.7`. Unknown versions refuse connection; widening this fence requires layout/protocol evidence and tests. Port names are presentation only. INFO verifies protocol/version; this is protocol identification, not cryptographic device authentication. The user explicitly chooses the input/output pair.

Read current project with READ kind 2, slot 0, 384-byte chunks. Check kind, slot, offset, stable length/checksum, size (16..52992) and final CRC32. Validate GBPJ signature and supported project serialization version (1..15). A `.fm1project` JSON envelope records family, firmware version, display name, original byte values and CRC32. Import rejects malformed, oversized or corrupted records. This checksum detects accidental damage, not malicious modification; the firmware also validates the project before committing it.

Restore reviews one project replacement only. First read and download the current project as a safety file. Require explicit acknowledgement of retaining it, then recheck firmware, stopped state and unchanged current project. WBEGIN kind 0/slot 0, WDATA sequential chunks and WEND with CRC32 use the firmware's own validated commit. Validate acknowledgements; read saved project (kind 0) back and compare all bytes. BUSY replies permit bounded retries (40 × 250 ms); timeout, disconnect, refused/invalid acknowledgement and checksum mismatch do not permit automatic write retry. A partial failure must be reported as unverified, not successful or rolled back.

No sample, user-sound, FM6, firmware, bootloader, calibration or raw-flash commands are exposed. Project backups do not imply a complete musical setup or device image. Link to the matching official editor for broader operations rather than copying its UI or adding speculative features. Do not generalize SLOOP 3 assumptions to SLOOP 2/Felucca/stock. Additional firmware requires a documented existing protocol; modifying original firmware or substituting a source-port is not this product's solution.

## Protocol evidence

Inspected upstream SLOOP 3 sources for the release in use:

- [Release 3.3.7 beta](https://github.com/isod89/sloop3-fm1/releases/tag/v3.3.7-beta)
- [Wire contract](https://github.com/isod89/sloop3-fm1/blob/v3.3.7-beta/app/edproto.h): F0 7D 53 33, command, LSB-first 8-to-7 payload, F7; same-command replies, command 0 errors.
- [Editor requests](https://github.com/isod89/sloop3-fm1/blob/v3.3.7-beta/web/src/core.js): READ responses include total and CRC32; write acknowledgements and BUSY handling.
- [Device handlers](https://github.com/isod89/sloop3-fm1/blob/v3.3.7-beta/fw/main.c): INFO, READ, WBEGIN/WDATA/WEND and commit validation.
- [Project format](https://github.com/isod89/sloop3-fm1/blob/v3.3.7-beta/app/project.c) and [storage limits](https://github.com/isod89/sloop3-fm1/blob/v3.3.7-beta/app/store.h).

## Verification and extension checklist

Run `npm run test:hardware`, `cargo test --release --locked -p fm1-host` and `npm run check:firmware`. Protocol tests use a simulated device: byte packing/CRC, project roundtrip, chunked transfer, read-back, malformed/corrupt/unknown-version responses, playback refusal, disconnect, timeout/no retry and error-code handling. Native embedding is checked for self-contained sources. These are not physical-device evidence.

Rebuild web, standalone and plugins (`NO_INSTALL=1`) after changing shared companion sources. Check the served page and the embedded file in Chrome/Edge. Physical acceptance still requires an FM-1: exact installed version, port pairing, current-project backup, replacement/read-back, audible project recall with required external resources, busy response, disconnect recovery and a second client attempt. Record firmware version and what was actually tested. Never mark these passed from a simulated device.
