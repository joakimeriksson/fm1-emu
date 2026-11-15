---
load_when: "using Studio, changing firmware, controller setup, backups or beginner troubleshooting"
last_verified: 2026-11-15
source_refs:
  - web/studio/app.js
  - web/studio/library.js
  - web/main.js
  - web/midi-input.js
  - context/status.md
  - crates/fm1-live/src/main.rs
  - crates/fm1-soc/src/controller.rs
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

### Change firmware in the standalone window

1. Click the emulator window so it has keyboard focus.
2. Press **Cmd+O** on Mac, or **Ctrl+O**. **F4** also works; use **Fn-F4** if F4 invokes a system feature. Release the keys to open the picker.
3. Select an original **.fwsc** package in the file picker. Cancel keeps the current firmware.
4. The old run closes normally and the selected firmware starts in its place. Audio/MIDI connections are reopened.

Changing firmware restarts the instrument and loses unsaved RAM edits. **The current native picker launch does not automatically create a persistent working-flash file.** For reliable native saving, launch from Terminal with the explicit `--flash` path shown above, save inside the firmware, then quit normally. Keep a different working-flash file for each firmware/setup. F4 preserves MIDI/channel/clock options but deliberately does not carry a flash file into another firmware, so use the explicit Terminal command when the destination needs persistent flash too.

### MiniLab 3 on macOS

For musical keys, choose the input named **Minilab3 MIDI**, rather than MCU/HUI, ALV or DIN THRU. Start with channel 1 for Felucca's first part. A focused computer-keyboard panel is not required for USB MIDI input.

The updated native app detects newly connected inputs and reconnections about twice per second. The old running executable must be restarted once to use this fix. When launching explicitly, add `--midi "Minilab3 MIDI"`; this prevents the DAW-control ports from being mixed into performance input. If firmware is generative, consult its manual before expecting ordinary piano-key behavior.

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

## Audacity 4 and the macOS verification warning

Audacity 4 is an audio editor with VST effect support; our FM-1 bundle is an instrument. Its standard VST instrument workflow is not supported in Audacity. Use the standalone emulator to make audio, then import a WAV into Audacity, or use an instrument host such as Ableton for MIDI performance.

A warning that Apple cannot verify the plugin is a separate macOS verification issue. A local ad-hoc signature can be structurally valid without a Developer ID/notarization. A downloaded bundle may also carry quarantine attributes. Do not mistake this warning for an antivirus finding, or assume a valid signature proves a downloaded plugin trustworthy.

On 2026-11-15 the installed system-wide FM-1 bundle had a valid ad-hoc signature and quarantine attributes. Audacity recorded it as Error with errorCode -3; this code's meaning was not established. The installed wrapper and embedded CLAP hashes differed from this project's local build. We did not remove quarantine, change Gatekeeper or replace that installation. Direct Audacity instrument use remains unsupported by the published host documentation, not a completed acceptance test.

References: [Audacity plugin support](https://plugins.audacityteam.org/) and [Audacity 4 transition guide](https://www.audacityteam.org/manual/new-in-audacity-4/audacity-3-to-4-transition-guide/).

## macOS unblocking: project-specific files only

For a trusted local build, first try opening the standalone app, then System Settings → Privacy & Security → Open Anyway if offered. This creates an exception for that item. See https://support.apple.com/en-us/102445.

For trusted downloaded builds, removing com.apple.quarantine from the exact app/plugin bundle is an alternative. Recursive removal includes the embedded CLAP in our VST3/AU bundles. This does not sign/notarize the software or verify its safety. Quit the host before changing installed plugins, then reopen/rescan. Do not disable Gatekeeper globally or recursively unblock the entire project, Downloads, or plugin directory.

Relevant local artifacts: target/apps/FM-1 Studio.app; target/release/fm1-live; target/release/fm1-emu; target/plugin/FM-1.vst3; target/plugin/FM-1.component; target/plugin/FM-1.clap. Installed copies, if present, require their own item-specific treatment under the user's Library/Audio/Plug-Ins or /Library/Audio/Plug-Ins. System-wide copies may need administrator privileges. Firmware packages, saves and web source files do not need executable approval.

## Native HYP1 launch and sound comparison

From the project root:

```sh
mkdir -p "$HOME/Music/FM-1/Working"
./target/release/fm1-live \
  --fwsc reference/firmwares/hyp1-1.5.65.fwsc \
  --mhz 240 --midi all \
  --flash "$HOME/Music/FM-1/Working/hyp1-1.5.65.bin"
```

The raw web link with mhz=100 underclocks the emulated CPU relative to nominal 240 MHz. Try mhz=240, using the same exact firmware and piece/settings before comparing. The catalog now selects HYP1 1.5.65. An older cached browser build may still list 1.5.62; close its tabs and reopen, or import the exact package. The maker's web player compiles firmware source for its browser runtime; ours executes CPU instructions. Sound agreement between those paths needs testing, not assumption.

For a sound comparison, match the firmware version, piece, Noise Colour and effect settings first. Native ten-second runs of the same 1.5.65 package at 100/240 MHz both produced audio without 16-bit clipping; this does not rule out underruns, CPU/peripheral errors, different initial state or synthesis differences. Neither boot success nor zero clipping is proof of musical correctness. No matched-state recording comparison with the author's player has passed yet.

### HYP1 keeps sounding after releasing a key

HYP1 normally generates its own music; its ordinary key mode edits the scale, rather than acting like a conventional piano. Open Pad (ENV), Drone (LFO), or Acid (ARP), then hold SEL and press ENV to put that voice in BY HAND mode. The voice's keys then play it manually. Repeat the combination to return it to generative mode. Other generated voices continue; use PLAY to stop the arrangement. Envelope release and effects can leave a tail after note-off.

The reported held notes were caused by the MiniLab’s **Hold** button being on; turning it off resolved them. Diagnostic capture confirmed channel-1 note-on and note-off messages arriving at the Mac. This establishes controller transmission; it does not establish every HYP1 MIDI route's behavior. If a manual voice remains stuck, distinguish it from other generated voices before changing firmware or controller settings.

The native firmware picker fires once after releasing Cmd/Ctrl+O or F4, avoiding repeated dialogs from held/repeated key events. Restart the application after rebuilding to use these changes.


### HYP1 crackle versus continuous noise

The reproduced startup selected piece **06**, with Noise Colour **Black**. Black deliberately generates vinyl crackle. The author’s player can start with a different piece/colour, so two fresh launches need not sound alike. This was confirmed by the user: choosing **White** removed the unexpected crackle.

1. Turn **SELECT** to **Noise**.
2. Turn **KNOB2** (Colour) to **White** or **Pink** for a continuous texture.
3. Adjust **KNOB1** (Level), **KNOB3** (Tone) and **KNOB4** (Pump) to taste. Level zero mutes this layer.
4. To keep it, use HYP1’s own store operation, then preserve your working flash as described above.

This changes the piece, not the emulator’s sound engine. A render check of the original 1.5.65 binary produced identical native/browser samples across 655,360 frames; the native interpreter also matched the first 346,880 frames. This checks agreement between our engines, not equivalence to physical hardware or the author’s source-compiled player.

## MiniLab 3 control profile

Restart the rebuilt native app. In the browser, reopen the updated page, choose **Minilab3 MIDI** under MIDI in and leave **Controller → Auto · MiniLab 3** selected. Use the factory **Arturia** mode (Shift + Pad 3), not DAW mode. No controller settings are written by the app.

| MiniLab control | FM-1 action |
|---|---|
| Knobs 1–4 | KNOB1–KNOB4 on the current firmware page |
| Knob 5 | SELECT / page navigation |
| Knob 6 | PRESETS / piece selection |
| Knob 7 | ALGORITHM |
| Knob 8 or fader 4 | MASTER volume |
| Large encoder | SELECT; Shift + turn selects PRESETS |
| Large encoder click | HOME |
| Shift button | Hold SCL (called SEL in HYP1); release to let go |
| Keyboard, pitch/mod strips, Hold/sustain | Normal firmware MIDI input |
| Pads and faders 1–3 | Normal firmware MIDI input; their effect depends on the firmware |

The first message from each parameter/navigation knob anchors its position; subsequent movement turns the emulated encoder. This avoids jumping a parameter whose value the firmware has not exposed. MASTER is an absolute volume control. Factory parameter/navigation CC values stop at 0/127: turn back when an endpoint is reached. Customized CC assignments and relative modes on the eight small knobs are not auto-detected. The large encoder uses its factory relative format. The profile consumes mapped CCs so they cannot simultaneously change a second firmware MIDI parameter.

Pads remain musical notes rather than hidden Save/Record commands. Faders 1–3 retain firmware assignments; a configurable mapping/learn editor and plugin controller-profile selection are later work. The native/browser profile is not automatically applied to DAW-hosted MIDI. **Raw MIDI** in the browser, or native `--controller-raw`, bypasses panel mapping. Auto/all-input routing excludes MiniLab MCU/HUI, ALV and DIN Thru ports; explicitly selecting one still permits it.

Reference: [Arturia’s MIDI implementation chart](https://support.arturia.com/hc/en-us/articles/6189475866396-MiniLab-3-General-Questions). Factory knob/fader traffic was captured from the attached controller; physical testing of every mapped gesture after rebuilding remains separate.

## Responsive controls and audio

Drag encoders vertically; hold **Shift** for four-times finer mouse travel. Knob pointers now move between detents, with smaller visual rotation per step. Firmware parameters still move by their real encoder detents; the display never invents extra parameter precision. Mouse, keyboard and MIDI actions are sent immediately; visual easing does not delay them.

Native playback now starts with a **12 ms** queue rather than 50 ms, and the FM-1 panel polls at up to 120 Hz. This is a queue target, not a measured end-to-end latency promise. The queue grows if needed to prevent underruns. For an explicit device buffer, add `--buffer 128`; if unsupported the app falls back to the device default. For more headroom add `--latency 24`.

The browser executes firmware/JIT in a worker and feeds a bounded audio queue, starting at four quanta (about 11.6 ms at 44.1 kHz) and adding two quanta after an underrun, up to 16. LCD and save messages travel directly to the page without passing through the audio thread. The status tooltip separates boot/firmware short quanta from actual output underruns. If an old offline build appears, close all tabs for that origin and reopen; updates wait until active sessions finish. Browser storage belongs to its hostname and port: changing port opens a separate library.

## Try GR!MM

The local firmware folder contains **GR!MM 2.0.2**, verified against the author’s release digest. Open `reference/firmwares/grimm-2.0.2.fwsc` using Cmd/Ctrl+O in the native app, or **Open .fwsc…** in the browser (Cmd/Ctrl+O also opens that chooser). To add it to Studio’s library, use **Import firmware**.

For a persistent native setup, use a separate working file:

```sh
./target/release/fm1-live --fwsc reference/firmwares/grimm-2.0.2.fwsc \
  --flash "$HOME/Music/FM-1/Working/grimm-2.0.2.bin"
```

Source: [GR!MM v2.0.2](https://github.com/GRiMMBIENT/grimm/releases/tag/v2.0.2). The release fixes missing factory samples after switching firmware. Its emulated six-second boot/render check passed; hardware and full feature acceptance remain pending.
