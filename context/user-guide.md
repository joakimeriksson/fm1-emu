---
load_when: "using Studio, changing firmware, controller setup, backups or beginner troubleshooting"
last_verified: 2026-11-15
source_refs:
  - web/studio/app.js
  - web/studio/library.js
  - web/main.js
  - web/midi-input.js
  - context/status.md
---
# FM-1 Studio user guide

FM-1 Studio runs original FM-1 firmware on your computer. **Firmware** is the program that turns the FM-1 into an instrument; changing it can change the sounds, controls and sequencer completely. You do not need a physical FM-1 to use the emulator.

## Start here

1. Start the local Studio server (instructions below).
2. Open Studio in a supported browser.
3. Click **Import firmware** and choose an original `.fwsc` file.
4. Click **Start** on the emulated device.
5. Play the panel or connect a MIDI keyboard.

Loading a package here changes the software instrument. It does **not** flash a connected physical FM-1.

## Open the browser Studio

If the preview server is already running, open its supplied link. The normal local address is [Studio](http://127.0.0.1:8642/studio/). The HYP1 verification workspace uses [port 8645](http://127.0.0.1:8645/studio/).

To start it yourself, open Terminal:

```sh
cd ~/GitHub/stevez0r/fm-1
npm run serve
```

Keep that Terminal running. If it says the address is already in use, try the browser link first; another server may already be running. Stop your server with Control-C.

If the built files are missing or you need the latest source changes, run `npm run build:web` first. This needs the development tools listed in the [README](../README.md); ordinary use of an already-built Studio does not.

## Change firmware safely

1. Use the running firmware’s own save command if you want to keep your edits.
2. Click **Export saved setup** and keep the downloaded file.
3. Click **Import firmware** and select the new `.fwsc`, or select an existing item in the collection.
4. Click **Start** in the new device view. Changing firmware restarts the emulated instrument.
5. Follow that firmware’s manual; familiar buttons may now do different things.

Studio waits for saved flash to finish writing before switching. It cannot yet preserve unsaved RAM edits. If switching reports a save error, stay on the current instrument and export before trying again.

Get packages from the firmware maker’s published download/installer page. Choose the firmware download, not a source-code ZIP, web simulator, or physical-device Install button. Updating Studio itself is a different operation from updating an instrument’s firmware.

New package bytes get a new hash and library item. The old version stays available; importing an update does not silently replace it.

## HYP1: the tested example

We tested HYP1 **1.5.65**, downloaded from its [author’s site](https://rustypawz.github.io/fm1-HYP1/install/). [Direct package](https://rustypawz.github.io/fm1-HYP1/firmware/hyp1-1.5.65.fwsc). Keep a local copy because the current version can change.

After Start, HYP1 generates music automatically. Click **ARP** for Acid; its knobs adjust level, cutoff, resonance and envelope modulation. **PLAY** stops or resumes playback. **PRESETS** selects a piece.

**SAVE normally controls breakdown/build/drop. To store a piece, first open Global with GLO, then press SAVE.** Export the Studio setup afterward to keep a separate backup. These are firmware-specific instructions; see the [HYP1 manual](https://rustypawz.github.io/fm1-HYP1/manual/) for playing voices by hand, drum sequencing and effects.

Verified here: original-package import, native/browser boot, moving display, non-zero native audio, Acid-page navigation and triggering breakdown. Physical FM-1 behavior, USB keyboard input with HYP1 and exhaustive feature compatibility were not tested.

## Play with mouse or computer keyboard

Click keys/buttons on the device. Drag a knob vertically or scroll over it. Start at a comfortable system volume.

Computer shortcuts apply inside the emulated device, so click its panel first. Common mappings include Space for PLAY, Z/X for octave and A through apostrophe for notes. The actual firmware decides what those controls mean. If a shortcut does nothing, use the visible control and check focus before assuming the firmware is broken.

Held-button combinations depend on the panel implementation; for complicated gestures, consult the firmware’s manual. The canvas controls still need accessibility improvements.

## Connect a USB or traditional MIDI keyboard

1. Connect a class-compliant USB MIDI keyboard to the **Mac**.
2. For a traditional DIN MIDI keyboard, connect its MIDI OUT to a macOS-compatible USB MIDI interface’s MIDI IN, then connect that interface to the Mac.
3. Start a firmware in Studio.
4. In the device’s controls below the panel, choose the keyboard under **MIDI in** and allow browser MIDI access.
5. Leave **Channel** at **as sent** initially. If necessary, set the channel required by the firmware.

On small screens, scroll inside the device view to reach MIDI controls. Chrome or Edge is the simplest starting browser. If MIDI access is unavailable, on-screen controls still work.

Some firmware responds to notes on particular channels; some is primarily a generative sequencer and needs a manual-play mode. A connected keyboard alone does not guarantee that every firmware plays ordinary piano notes.

Choose a specific input when possible; **all inputs** accepts every available controller. Studio distinguishes ports by ID and releases sustain/notes on disconnection or route changes. Actual physical-controller acceptance is still pending.

## Use the native Mac app

Open `target/apps/FM-1 Studio.app` in the project folder and choose a `.fwsc` package in its picker. This currently uses the original native device interface, not the browser library.

For explicit controller selection and persistent flash, use Terminal:

```sh
target/release/fm1-live --fwsc /path/to/instrument.fwsc \
  --flash /path/to/my-working-flash.bin --midi "Keyboard name"
```

Replace those paths and the keyboard name with yours. Choose a different flash filename for each independent setup. The native app writes that working flash at normal shutdown, so quit normally and retain separate backups.

The default native input accepts all MIDI inputs. Use `--midi none` to disable them. Browser and native libraries do not automatically synchronize.

## Use a DAW

The VST3 build is at `target/plugin/FM-1.vst3`. It has **not** been automatically installed. If installing it yourself, first quit the DAW and preserve any existing FM-1 plugin; development builds currently use upstream-compatible identifiers.

On macOS, the user VST3 folder is `~/Library/Audio/Plug-Ins/VST3/`. Copy the whole bundle there, reopen/rescan in Ableton, and load FM-1 on a MIDI/instrument track. Choose a firmware in its window. Select your keyboard as the track input and enable monitoring or record-arm.

Save the firmware’s edits, then save the DAW project. Current project recall retains firmware and saved flash, **not a guaranteed complete unsaved session**. Real Ableton acceptance, latency measurement and offline-bounce verification remain pending.

Logic needs an Audio Unit rather than VST3; an AU bundle is built but real Logic validation remains pending. Pro Tools AAX is not implemented. For Audacity, use recorded/rendered audio rather than expecting VST instrument hosting.

## Understand files and backups

| File | What it keeps | How to use it |
|---|---|---|
| `.fwsc` | Original firmware program | Import to create an instrument |
| `.fm1save` | Firmware identity and saved flash/settings/content | Import a saved setup; not a complete running-session snapshot |
| `.fm1library` | Studio’s imported collection | Back up/restore library items |
| Working flash `.bin` | Native instance’s persistent device memory | Keep with that native setup; not interchangeable with a browser setup file |
| WAV/audio file | Recorded sound | Import into a DAW or Audacity |

**A collection backup does not automatically capture the currently running instrument’s latest edits.** First save in the firmware, export its saved setup, then import that setup into the library if you want it included in a collection backup.

To back up the collection: open **Library → Back up collection**. To restore: use **Import files** and select the archive. Validation failures leave the existing collection intact. Initial archives support up to 256 items and 24 MiB of binary contents.

Some setup files omit firmware bytes. Import the exact matching `.fwsc` first, then the setup. A matching name is insufficient; Studio checks the package hash.

## Keep independent instruments

A new visit to Studio’s base URL creates an independent working instance. Its generated URL contains an instance ID; bookmark that URL to return to the same saved working state.

Each library item has its own working flash within that instance. Reopening an item resumes its saved working copy. Opening the **exact same instance URL** twice intentionally addresses the same state and can produce a save conflict. Open the base URL for another independent instance.

Current Studio starts with the collection view available but requires you to select the desired item and Start again. Unsaved edits and playback position are not yet restored automatically.

## Work offline

Load Studio while online, wait for **Ready offline**, then reload once before disconnecting. Import the packages you want to use. Verify reopening your chosen instrument before travelling.

Discovery/downloads need internet; already stored packages do not. Keep the same browser profile, origin and port: `localhost` and `127.0.0.1`, or two different ports, have separate libraries.

Browser storage can be removed by clearing website data or by storage pressure. Download backups outside the browser. Offline caching is not a backup.

After a Studio update, close all Studio tabs and reopen to let a waiting service-worker update activate. Avoid clearing site data just to refresh code; that also removes the local library.

## Troubleshooting

| Symptom | First things to try |
|---|---|
| No sound | Click Start; check system output/volume; check the firmware’s PLAY and level controls |
| Keyboard silent | Select the input, allow MIDI permission, check channel and firmware manual-play mode |
| Controls absent | Scroll within the device view; browser zoom may help |
| Audio crackles | Close demanding apps; inspect runtime load; compare with native playback |
| Firmware rejected | Choose the original `.fwsc`, not a ZIP/source file; download again from its maker |
| Save conflict | Keep the current tab; export its setup; close other tabs using that instance URL |
| Setup asks for firmware | Import its exact matching package before importing the setup |
| Library appears empty | Check browser profile, hostname and port; restore your exported collection if needed |
| Recent UI changes missing | Close all Studio tabs and reopen; cached updates wait for active sessions to finish |

Recording controls, sample/patch editors, MIDI-file playback, full volatile recall and physical-device transfers are not all implemented in Studio yet. Do not treat another firmware’s web simulator or manual as evidence that the same feature exists in our wrapper.

## Physical hardware

Hardware flashing, full-device backup/restore and live companion editing are planned but not verified or exposed as completed Studio workflows. Emulator imports do not perform those operations. Keep physical-device installation instructions separate until the companion has been implemented and tested.
