---
load_when: "using Studio, changing firmware, controller setup, backups or beginner troubleshooting"
last_verified: 2026-11-16
source_refs:
  - web/studio/app.js
  - web/studio/library.js
  - web/main.js
  - web/midi-input.js
  - context/status.md
  - crates/fm1-live/src/main.rs
  - crates/fm1-soc/src/controller.rs
  - crates/fm1-host/src/lib.rs
  - crates/fm1-plugin/src/editor.rs
  - web/hardware/**
---
# FM-1 Studio user guide

FM-1 Studio runs original FM-1 firmware on your computer. **Firmware** is the program that turns the FM-1 into an instrument; changing it can change the sounds, controls and sequencer completely. You do not need a physical FM-1 to use the emulator.

## Choose a version

All three versions run the same original firmware on your computer. The hardware FM-1 is optional for playing them.

| | Standalone Mac app | DAW plugins | Browser Studio |
|---|---|---|---|
| Best for | Playing an instrument without a DAW | Using an FM-1 instrument on a DAW track | Browsing and backing up a local collection |
| Where it runs | Its own Mac window | Inside a compatible DAW: VST3, AU or CLAP | A browser tab |
| Firmware | Open `.fwsc` or supported `.area` | Open the same files in the plugin | Import the same files into the collection |
| Keeping your work | Save in the firmware; automatic working flash and last-firmware recall are available; use explicit `--flash` files for independent setups | Save in the firmware, then save the DAW project; shared `.fm1save` import/export is available | Save in the firmware, then export a saved setup; back up the collection too |
| Where it is stored | Files on your Mac | DAW project state and exported files | This browser profile, address and port; exported files are separate backups |
| MIDI keyboard | Connect to the Mac; choose the performance input | Route MIDI through the DAW track | Choose a Web MIDI input and allow access |
| Offline use | Yes, with local firmware | Yes, with local firmware and an installed compatible host | After caching Studio and importing firmware; keep exported backups |
| USB hardware library | F6 / Fn-F6 opens the shared companion in Chrome or Edge | **Hardware Library ↗** opens that same companion | **Hardware Library ↗** opens the companion page |

The browser's collection screen is not built into the native instrument windows. Files can be moved between versions, but their libraries do not synchronize automatically. `.fm1save` preserves original firmware bytes and saved flash; none of the three guarantees recovery of every unsaved edit or the exact playback position. Actual Ableton/Logic acceptance remains pending even though the plugin bundles and engine tests pass. CLAP needs a host that supports CLAP; Logic uses AU, not VST3.

## Use your computer as the hardware FM-1's library

**Yes, for firmware with a suitable USB editor protocol.** The computer keeps project files and the FM-1 holds the project you are using now. You can swap projects over a USB data cable. This does not add active sample memory, make the FM-1 a USB drive, or stream a bigger sample library into it.

Our first shared Hardware Library supports **SLOOP 3.3.7 projects only**. It is implemented, with simulated transfer tests, but **has not been verified on a physical FM-1**. It does not currently back up samples, user sounds, FM6 banks, firmware or a whole device. A project that uses custom sounds or samples still needs those resources on the hardware. Keep them together in your computer backup folder; use the [official SLOOP 3 editor](https://isod89.github.io/sloop3-fm1/editor.html) for those resources and disconnect our companion first.

### Open the same companion from any version

- Standalone: restart the rebuilt Mac app, then press **F6** (or **Fn-F6**) in its instrument window. You can also launch the companion alone with `target/release/fm1-live --hardware-library`.
- Plugin: click **Hardware Library ↗** in its window. This opens a browser; USB transfer does not run in the DAW audio engine.
- Browser Studio: click **Hardware Library ↗**. With the local server running, the direct page is [Hardware Library](http://127.0.0.1:8642/hardware/).

On Mac, the native buttons open Chrome, or Edge if Chrome is unavailable. They create a self-contained companion file in `~/Library/Application Support/FM-1 Studio/`. No server is needed for that file. The browser version uses the same code in Studio's offline cache. If a browser blocks USB MIDI from a local file, open the localhost page instead. Neither launch route automatically connects or sends a project.

### Save a project from hardware

1. Run SLOOP 3.3.7 normally on the FM-1. Do not enter bootloader mode.
2. Connect a USB **data** cable. Stop playback and recording. Close other hardware editor tabs and applications.
3. In the companion, click **Choose USB MIDI ports** and allow MIDI/SysEx access. Choose the FM-1 input and output, then **Connect and check firmware**.
4. Name the project and click **Save project to computer**. Keep the downloaded `.fm1project` file in your backup folder. It includes the project as it is now, the firmware version and a checksum.

You can keep many such files on the computer. Saving another one does not replace the project on the FM-1. Hardware project files are separate from emulator `.fm1save` files and cannot be imported as emulator setups.

### Restore a project to hardware

1. Choose a `.fm1project` file. Its firmware version must match the connected FM-1.
2. Click **Prepare restore and save safety backup**. This downloads the hardware's current project without changing it.
3. Keep that safety file. Read the destination and replacement description, tick the confirmation, then click **Replace hardware project**.
4. Leave USB connected until read-back verification finishes. The project is replaced; samples and sound banks are not transferred.

If the project changed since preparation, prepare again for a fresh safety backup. If a cable disconnects or a request times out, the companion stops and requires reconnection; it does not blindly repeat a write. Keep your safety backup and inspect the device before another restore. A project safety backup is not a full-device recovery image.

Only one companion should control a device. Tabs from the same origin share a session lock; different browser profiles, browser origins and third-party editors cannot share that lock. Disconnect before changing editor. Loading a DAW project or emulator save never writes to hardware.

### Can every firmware use this without source changes?

No universal editor protocol exists for all FM-1 firmware. **No source changes are needed when the installed firmware already supplies the commands we need.** Each family still needs a small matching adapter and verification: SLOOP 2 and Felucca are not automatically compatible with SLOOP 3. Firmware without suitable read/write commands cannot gain this workflow just by connecting a cable. Never send SLOOP commands to stock firmware based on a port name.

The simplest expansion is one tested adapter at a time, using the same companion screen and transport. Firmware installation and raw device recovery stay separate. For developer details and acceptance requirements, see [Hardware Library](hardware-library.md).

## Start here

1. Start the local Studio server (instructions below).
2. Open Studio in a supported browser.
3. Click **Import firmware** and choose an original `.fwsc` or supported `.area` file.
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

## Get the correct firmware file for a project

Keep downloaded firmware in **`reference/firmwares/`**, the single local firmware directory used by the downloader, native catalog and test fixtures. Keep original versioned filenames; related files such as SLOOP 3’s application area and installer can share a versioned subfolder. This directory is ignored by git: do not commit firmware binaries, user saves or device backups. Store project saves and hardware backups separately.

1. Open the firmware author's official repository or download page. On GitHub, open **Releases**, choose the intended version, and expand **Assets**. Check prereleases explicitly: a beta may be newer than the release reached by a “latest” link.
2. For **FM-1 Studio**, download the original **`.fwsc` firmware update package**, or a supported plain **`.area` application** such as SLOOP 3. Keep its original filename and version. Use the author's installer/download page if the release assets do not include it.
3. Import that `.fwsc` with **Import firmware** (or choose it in the native app). For an existing saved project, keep the exact firmware version/package it used: setup recall checks the package hash, not just its name.
4. Keep a local copy of the package alongside your project backups. A source-code ZIP/tarball, editor HTML, sample bank or working-flash `.bin` is not a substitute for firmware.

**You do not need to compile a published firmware package.** Compile only when building from source or making your own changes, following that firmware project's build instructions and toolchain requirements. Compiling Studio is a separate task. Renaming a `.bin`, `.area` or ZIP to `.fwsc` does not convert it into a package.

### SLOOP 3: a different firmware format

Checked on **November 15, 2026 (Pacific time)**: the newest published SLOOP 3 release is [**3.3.7 public beta**](https://github.com/isod89/sloop3-fm1/releases/tag/v3.3.7-beta), in its own repository, separate from stable [SLOOP 2.8](https://github.com/isod89/sloop-fm1). The release includes older versions too; select files named **3.3.7**.

Downloaded originals are in the project's ignored `reference/firmwares/sloop3-3.3.7-beta/` folder:

| File | Purpose |
|---|---|
| `sloop3-3.3.7.area` | Precompiled plain application area for developers; not a `.fwsc` package |
| `installer-sloop3-3.3.7.html` | Self-contained physical-device installer with firmware embedded |

**Neither file needs compilation. The standalone Mac app now opens the original `.area` directly.** Restart the rebuilt `target/apps/FM-1 Studio.app`, press **Cmd+O**, and select `reference/firmwares/sloop3-3.3.7-beta/sloop3-3.3.7.area`. Allow several seconds for startup. The loader validates both record headers and the application checksum, installs a copy into emulated flash and executes the original entry stub. The original download stays unchanged.

**Standalone, browser Studio and the rebuilt plugins now accept `.fwsc` and `.area`.** Import the same original `.area` in browser Studio or open it in the plugin. Studio collection downloads retain the `.area` extension, and shared `.fm1save` exports retain the exact firmware bytes and hash. Close old browser Studio tabs and reopen after a build so the updated offline cache activates. This release publishes no `.fwsc` asset. Do not rename the file or substitute an older SLOOP 2 package.

For an explicit standalone launch with persistent saves:

```sh
target/release/fm1-live --area reference/firmwares/sloop3-3.3.7-beta/sloop3-3.3.7.area \
  --flash target/sloop3-working-flash.bin
```

Save within SLOOP, then quit normally. Back up that working flash separately. Verified: a 12-second native run displayed the five-track panel and produced 529,411 stereo audio frames with peak 5,081 in 16-bit PCM after scripted PLAY/note input. Full feature, MIDI-controller and hardware compatibility are not established.

For physical hardware, follow the [official SLOOP 3 instructions](https://github.com/isod89/sloop3-fm1#install): the HTML installer opens in Chrome/Edge and uses **m-bootloader MIDI MODE**, with a full device backup first. Upstream marks this as beta and asks users to expect bugs and keep backups. No physical hardware installation or verification was performed.

The upstream [build instructions](https://github.com/isod89/sloop3-fm1#the-firmware) are only needed for source builds: they require the pi32v2 LLVM toolchain and `fw/build.sh`, producing `build/fw/fw.area` and an embedded HTML installer.

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

Open `target/apps/FM-1 Studio.app` in the project folder and choose a `.fwsc` package or supported plain `.area` application in its picker. This currently uses the original native device interface, not the browser library.

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
3. Select an original **.fwsc** package or a plain **.area** application in the file picker. Cancel keeps the current firmware.
4. The old run closes normally and the selected firmware starts in its place. Audio/MIDI connections are reopened.

Changing firmware restarts the instrument and loses unsaved RAM edits. **Native launches now create a working-memory file automatically; explicit `--flash` selects your own.** For reliable native saving, launch from Terminal with the explicit `--flash` path shown above, save inside the firmware, then quit normally. Keep a different working-flash file for each firmware/setup. F4 preserves host options and selects the destination firmware’s automatic working-memory file; an explicit `--flash` path remains useful for independent setups.

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
| `.fwsc` / `.area` | Original firmware package / supported plain application area | Import to create an instrument |
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

Studio reselects the last item for a bookmarked instance; click Start again. Unsaved edits and playback position are not yet restored automatically.

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

Hardware flashing, full-device backup/restore and general live companion editing remain planned. The project-only SLOOP 3 Hardware Library described above is implemented with hardware verification pending. Emulator imports do not perform those operations. Physical-device installation and recovery remain separate from the project librarian and require their own implementation and hardware validation.

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

In the native app, **Shift-click** a key or button (Home, for example) to hold it down, then press another key while it's held. Shift-click it again, or click it, to release it. Drag encoders vertically; hold **Shift** for four-times finer mouse travel. Knob pointers now move between detents, with smaller visual rotation per step. Firmware parameters still move by their real encoder detents; the display never invents extra parameter precision. Mouse, keyboard and MIDI actions are sent immediately; visual easing does not delay them.

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

## Mac menu bar

With the standalone instrument focused, use the menus at the top of the Mac screen:

| Menu | Actions |
|---|---|
| File | Open Firmware… (Cmd+O); Close Window (Cmd+W) |
| Audio | Toggle WAV Recording (Cmd+Shift+R; F3 also works) |
| Instrument | Reset Firmware State… (confirmation required) |
| Hardware | Open Hardware Library (Cmd+Shift+L; F6 also works) |
| Help | Open the bundled user guide in TextEdit |

Firmware Save, playback and editing commands still belong to the running firmware's panel. Closing follows normal shutdown and finishes recording; save inside the firmware first. Native menu shortcuts take precedence over musical keyboard shortcuts. The menu bar is separate from the right-hand macOS status icons; the app does not add a background status item.

## Reopen a session and reset an instrument

The Mac standalone now reopens its last successfully loaded firmware on Finder launch. It remembers launch preferences (MIDI input/channel, clock, theme and other supplied options) and uses an automatic working-memory file per exact firmware hash unless you supplied `--flash`. Save your edits inside the firmware and quit normally. Firmware downloads must remain at their original location; if missing, choose a file again. Keep only one standalone window using a given working-memory file at a time.

Browser Studio remembers the selected item for the bookmarked instance URL and restores its saved working memory. Click Start again to enable sound. A new instance URL is deliberately independent. Plugins reopen the state saved in their DAW project; they must never replace it with the standalone's last instrument. Native and browser storage do not synchronize automatically.

**This is saved-state recall, not a complete live-session snapshot.** Unsaved RAM edits, held notes and exact playback position are not recovered. The running firmware may also require its Save command before edits reach persistent memory.

Choose **Instrument → Reset Firmware State…** in the standalone, **Reset Firmware State…** in the plugin toolbar, or the same action below the browser device panel. Reset restarts the selected emulator with its initial saved memory. Host preferences remain intact; physical hardware is never changed. Firmware may generate a new default project at boot, so defaults need not mean an empty screen.

Standalone asks for confirmation and retains the previous working-memory file beside it, named `*.before-reset-<timestamp>.bin`, after a normal shutdown. Files live in `~/Library/Application Support/FM-1 Studio/sessions/`; `last.json` records reopening options. Plugin/browser reset asks for confirmation: export a `.fm1save` before resetting to keep your work. Reset only affects the active plugin/working setup. Save the DAW project again if you want its reset state retained.

Firmware-owned Save and Undo remain on the firmware panel; host reset restarts the instrument rather than invoking firmware Undo.
