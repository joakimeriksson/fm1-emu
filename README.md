# FM-1 emulator

The **M-VAVE FM-1** is a pocket FM synthesizer, and a small community writes its own firmwares
for it: drum machines, grooveboxes, ambient generators, a chord harp. This project emulates the
FM-1 itself, its CPU, screen, keys, knobs and audio, so every one of those firmwares runs
unchanged in a browser, on a computer or as an instrument plugin in a DAW (AU, VST3). You can
try a firmware before you install it, write one without a synth on the desk, or just play.

**Play it: [joakimeriksson.github.io/fm1-emu](https://joakimeriksson.github.io/fm1-emu/)**:
pick a firmware, press Start, play with the computer keyboard, the mouse, a touch screen or
a MIDI keyboard.

[![The emulated FM-1's front panel running fifteen of the firmwares in turn](docs/media/firmwares.gif)](https://joakimeriksson.github.io/fm1-emu/)

With sound: nine of the firmwares in 50 seconds.

https://github.com/user-attachments/assets/6849316e-7239-497e-b617-be74317dc2b1

The page hosts no firmware: it downloads each one from its author's own site when you start
it, the same update package you would install on a real FM-1. M-VAVE's own firmware can't be
fetched that way, so download `FM-1.fwsc` from M-VAVE and open it on the page.

And the **[M-VAVE SMK-37](https://joakimeriksson.github.io/fm1-emu/smk37/)**: a 37-key keyboard with
the FM-1's chip inside, whose community ports run here too ([below](#the-smk-37)).

Also here: **[FM-1 SID](fm1-sid/)**, a firmware that turns the FM-1 into a Commodore 64 SID
synth, with a step sequencer and stereo chorus, delay and reverb, that also plays C64 tunes
([its page](https://joakimeriksson.github.io/fm1-emu/fm1-sid/) plays all of HVSC).

## What runs

47 community firmwares: every one on [the FM-1 Firmware Hub](https://thefirmwarehub.com/) that
runs here (its packages, fetched from the Hub; `tools/hub_catalog.mjs` follows its catalog), and a
few more from [fm1-editor.com/firmware](https://fm1-editor.com/firmware/). Also FM-1 SID, and
M-VAVE's stock firmware (V13 and V15) with both CPU cores running. Each name below opens the page
with that firmware picked; press Start. (`?fw=` takes the Hub's ids too; `&embed=1` shows only
the device and its controls, for a window on another site.)

<!-- firmwares: node tools/readme_firmwares.mjs -->
| Firmware (opens it on the page) | By | What it is |
|---|---|---|
| [M-VAVE stock firmware](https://joakimeriksson.github.io/fm1-emu/?fw=mvave) | [M-VAVE](https://github.com/joakimeriksson/fm1-emu/blob/master/docs/stock-firmware.md) | The firmware the FM-1 ships with: a DX7-style FM synth, both cores running. Bring your own `FM-1.fwsc`. |
| [Felucca 1.6.1](https://joakimeriksson.github.io/fm1-emu/?fw=felucca) | [hugelton](https://github.com/hugelton/Felucca) | The original open firmware: a four-operator FM synth with sequencer, arpeggiator and effects. |
| [Felucca [Salt] 0.9-salt14](https://joakimeriksson.github.io/fm1-emu/?fw=felucca-salt) | [ChanceTheMaker](https://github.com/ChanceTheMaker/Felucca) | A Felucca fork with its own sound and editor changes. |
| [AMB-1 1.2](https://joakimeriksson.github.io/fm1-emu/?fw=amb) | [charlesvestal](https://github.com/charlesvestal/fm1-amb) | A generative ambient machine inspired by Music for Airports. |
| [April OS BETA 1.1](https://joakimeriksson.github.io/fm1-emu/?fw=april) | [beashoko](https://github.com/beashoko/april-os) | A sampler inspired by the Casio SK-1: no synth engines, only samples, recorded through the installer page's microphone bridge. |
| [ChoralRoot 0.22-beta1](https://joakimeriksson.github.io/fm1-emu/?fw=choralroot) | [Quixotic7](https://github.com/Quixotic7/ChoralRootFM1) | A port of the Choral Root synth. |
| [fm1-chord 0.3](https://joakimeriksson.github.io/fm1-emu/?fw=chord) | [math0ne](https://github.com/math0ne/fm1-chord) | A chord machine after the HiChord, with extras: chords in a key under the white keys, slash chords and inversions, sounds, envelopes, effects and a looper. |
| [CoralFlower 0.2](https://joakimeriksson.github.io/fm1-emu/?fw=coralflower) | [FicusFicusov](https://github.com/FicusFicusov/CoralFlower) | Chords after the Orchid, with a UX made for the FM-1: one key turns every note into a chord with a bass, 27 scales, an arpeggiator, a looper, a plate reverb, and a pixel flower that grows as you play. |
| [CS-1 2026.10.15](https://joakimeriksson.github.io/fm1-emu/?fw=cs1) | [twochannels](https://github.com/twochannels/CS-1) | A Yamaha CS-80 in spirit: eight voices of two channels, high- and low-pass filters in series, ring modulator, the touch response and 22 tones from the owner's manual. |
| [CTL-1 0.9.2](https://joakimeriksson.github.io/fm1-emu/?fw=ctl) | [charlesvestal](https://github.com/charlesvestal/fm1-ctl) | A MIDI controller: the keys play in a scale with its chords on the black keys, four banks of knobs, an arpeggiator and Euclidean drums, with General MIDI sounds of its own. |
| [Dinghy 0.3](https://joakimeriksson.github.io/fm1-emu/?fw=dinghy) | [zvenson](https://github.com/zvenson/dinghy) | Your base for your own FM-1 firmware: everything under it done and documented (updates, USB rescue, USB-MIDI, USB audio, keys, screen, settings), a 4-voice sine on top. |
| [DUELL 0.2.13-beta](https://joakimeriksson.github.io/fm1-emu/?fw=duell) | [OSCILL/R](https://github.com/oscillr/DUELL) | A multimorphing bitimbral synthesizer: two complete parts, three ways to play them together, and a morph that glides between two versions of every sound. |
| [ETCH 1.38](https://joakimeriksson.github.io/fm1-emu/?fw=etch) | [SikkenWorks](https://github.com/SikkenWorks/ETCH) | Draw the sound: a waveform drawn and changed with the knobs, morphed into another one, and a third modulating the result. |
| [Fart Machine One 1.0.0](https://joakimeriksson.github.io/fm1-emu/?fw=fart) | [adamcharleswilsonmusic-code](https://github.com/adamcharleswilsonmusic-code/Fart-Machine-One) | A questionable musical instrument: eleven artisanal farts, reverb, delay and a sixteen-step sequencer. |
| [Felucca 8 Patterns 1.5.1_and_sequences](https://joakimeriksson.github.io/fm1-emu/?fw=felucca8) | [echoe](https://github.com/echoe/Felucca_8_patterns) | Felucca 1.5.1 with eight sequences a track to switch between and chain one after another; the user samples are left out to make room. |
| [FiMba-1 0.6.1](https://joakimeriksson.github.io/fm1-emu/?fw=fimba) | [jadamsowers](https://github.com/jadamsowers/fm1-fimba) | A physically modelled kalimba (thumb piano), with mbira patterns, tape and grains. |
| [FM-106 Beta4](https://joakimeriksson.github.io/fm1-emu/?fw=fm106) | [PatttF](https://github.com/PatttF/fm-106) | The Juno-106 emulation of KR-106 ported to the FM-1: six voices on both cores, the BBD stereo chorus, 128 factory presets and a 1980s Roland look. |
| [FM-1 Doom 0.5](https://joakimeriksson.github.io/fm1-emu/?fw=fm1doom) | [zvenson](https://github.com/zvenson/fm1doom) | Doom on the FM-1: one arena from FreeDM, played on the keys and a knob, on the FM-1's screen, with synthesised sound effects and no music. |
| [FoMni 0.4](https://joakimeriksson.github.io/fm1-emu/?fw=fomni) | [charlesvestal](https://github.com/charlesvestal/fm1-fomni) | A chord harp inspired by the Omnichord: hold a chord, strum the keys. |
| [FuMi-1 1.0.7](https://joakimeriksson.github.io/fm1-emu/?fw=fumi) | [cartesive](https://github.com/cartesive/fumi-1) | Inspired by the Suiko ST-50 shigin conductor: koto and shakuhachi-style tones. |
| [GHOULBOX 2.1](https://joakimeriksson.github.io/fm1-emu/?fw=ghoulbox) | [jasonpersinger](https://github.com/jasonpersinger/ghoulbox-fm1-dungeon-synth) | Dungeon synth (a Felucca fork): cathedral reverb and dark drones. |
| [Hortator 0.16.0](https://joakimeriksson.github.io/fm1-emu/?fw=hortator) | [DeadActive](https://github.com/DeadActive/hortator) | A drum-focused firmware based on Felucca. |
| [HYP-1 1.5.1](https://joakimeriksson.github.io/fm1-emu/?fw=hyp1) | [RedtailWorks](https://github.com/RustyPawz/fm1-HYP1) | Hypnotic techno and acid: a breathing pad, a deep drone, a generative 303 line and grain over drums you sequence. |
| [Jangada 1.0](https://joakimeriksson.github.io/fm1-emu/?fw=jangada) | [zednaked](https://github.com/zednaked/jangada) | Superwave analog voices and a modulation matrix. |
| [JIANT FM 0.7.2-e317df3](https://joakimeriksson.github.io/fm1-emu/?fw=jiant) | [juanjiant-bit](https://github.com/juanjiant-bit/JIANT) | A bio-synthetic groovebox: six tracks drawn as living beings, eight engines (FM6 with Dexed, analog, phase, voice), mutating macros and SLOOP's song mode. |
| [MAKU 0.1](https://joakimeriksson.github.io/fm1-emu/?fw=maku) | [okumurari](https://github.com/okumurari2/maku) | An ambience machine for DJ interludes: switch it on and a random world in key plays, four parts, buttons that are verbs, and the worlds you like breed new ones. |
| [Melodee 1.3.6](https://joakimeriksson.github.io/fm1-emu/?fw=melodee) | [keremimo](https://github.com/keremimo/melodee) | A melodic synth firmware (a Felucca fork) with dual-core audio. |
| [MYCELIUM 1.0-test](https://joakimeriksson.github.io/fm1-emu/?fw=mycelium) | [Mandalars](https://github.com/megagott/mycelium-fm1) | Forest, Psytrance, Goa and Hitech grooves, built on Felucca and SLOOP: original drums and icons and four factory songs. |
| [NOIZ DEMONS 666](https://joakimeriksson.github.io/fm1-emu/?fw=noizdemons) | [SPIKE the Percussionist](https://github.com/SPIKE-the-Percussionist/noiz_demons) | Felucca with randomization as a ritual: every new sound summons one of 30 demons of the Ars Goetia, its sigil takes the screen and its rank decides how wild the sound gets. |
| [OberVave 1.3.1](https://joakimeriksson.github.io/fm1-emu/?fw=obervave) | [zvenson](https://github.com/zvenson/obervave) | An eight-voice analog-style polysynth after the American polys of 1979-84: two oscillators a voice, sync, cross-mod, a morphing filter, an arpeggiator, a sequencer, chorus, delay and plate reverb. |
| [ORBIT 0.7.1](https://joakimeriksson.github.io/fm1-emu/?fw=orbit) | [dspaudio](https://github.com/dspaudio/orbit) | A four-track groovebox based on SLOOP: three synth parts and a drum machine, an event tape to rearrange them, OP-1-inspired screens. |
| [PIGEON 0.1-beta-candidate](https://joakimeriksson.github.io/fm1-emu/?fw=pigeon) | [The Shrimp Blimp](https://github.com/TheShrimpBlimp/pigeon) | An old-school hip-hop and house sampler after the SP-404 and the PO-33: two song samples and a 16-pad kit, up to 32 chops a song, a 64-step sequencer, patterns and a song arrangement. |
| [Polly 0.4.3](https://joakimeriksson.github.io/fm1-emu/?fw=polly) | [charlesvestal](https://github.com/charlesvestal/fm1-polly) | A pocket arranger after the Yamaha QY70: pick a style, play a chord and a band of eight follows, with General MIDI sounds. |
| [PurpleMonkey 0.11](https://joakimeriksson.github.io/fm1-emu/?fw=purplemonkey) | [Quixotic7](https://github.com/Quixotic7/PurpleMonkeyFM1) | A firmware for kids. |
| [FM1 Quest 0.21](https://joakimeriksson.github.io/fm1-emu/?fw=quest) | [hericdk](https://github.com/hericdk/fm1-quest) | A live RPG adventure inside the synth. |
| [Rainbow Mode 1.1.5-rb](https://joakimeriksson.github.io/fm1-emu/?fw=rainbow) | [joshbartlettband-web](https://github.com/joshbartlettband-web/felucca-rainbow) | A music toy for little hands (a Felucca fork). |
| [FM-1 SID 0.1](https://joakimeriksson.github.io/fm1-emu/?fw=sid) | [joakimeriksson](https://github.com/joakimeriksson/fm1-emu/tree/master/fm1-sid) | A SID chip on the FM-1, as a synth: the keys play 12 instruments, KNOB1-4 are cutoff, resonance, attack, release; FX opens chorus, delay and reverb. |
| [Skiff 1.1](https://joakimeriksson.github.io/fm1-emu/?fw=skiff) | [illuminus5](https://github.com/illuminus5/skiff) | A six-track groovebox built on Felucca 1.1.5: fourteen synth and drum engines, a 64-step sequencer with chords, ratchets and locks, songs and a performance layer. |
| [SLOOP 2.6](https://joakimeriksson.github.io/fm1-emu/?fw=sloop) | [isod89](https://github.com/isod89/sloop-fm1) | A live groovebox: four looping tracks. |
| [SLOOP ALG ALG05-TEST](https://joakimeriksson.github.io/fm1-emu/?fw=sloop-alg) | [shaw-core](https://github.com/shaw-core/Sloop_ALG02) | A SLOOP test build with extra FM algorithms. |
| [SLOOP DX7 Banks 2.5-b28-test](https://joakimeriksson.github.io/fm1-emu/?fw=sloop-banks) | [majnikool](https://github.com/majnikool/sloop-fm1) | SLOOP with up to twelve DX7 voice banks and presets grouped by kind. |
| [SLOOP Floyd FM 2.4](https://joakimeriksson.github.io/fm1-emu/?fw=sloop-floyd) | [renebohne](https://github.com/renebohne/sloop-fm1) | SLOOP with a four-operator FM engine after Floyd Steinberg's 4-OP idea: an envelope per operator and a live oscilloscope that draws the envelopes as you play. |
| [sloopDX 3.5](https://joakimeriksson.github.io/fm1-emu/?fw=sloopdx) | [zvenson](https://github.com/zvenson/dxsloop) | SLOOP's live workflow with a six-operator DX7 engine from Dexed: 32 algorithms, your own .syx banks, every voice edited on the FM-1, and FM drums. |
| [Tonitor 2026.10.15](https://joakimeriksson.github.io/fm1-emu/?fw=tonitor) | [Nuno Zimas](https://github.com/nzimas/tonitor) | Six tracks for live ambient and experimental music: three synth engines, patches generated by algorithm, free-length loops with up to ten overdubs each, a granulator, filters and a lush reverb. |
| [TRIBE 0.1-beta](https://joakimeriksson.github.io/fm1-emu/?fw=tribe) | [apiel](https://github.com/apiel/fm1-tribe) | A ten-track groovebox built on X0X where any engine plays on any track: kicks, 909 and 808 voices, a 303, sixteen clips a track and scatter effects. |
| [WaveLoop FM-1 1.0.5.2-midi23](https://joakimeriksson.github.io/fm1-emu/?fw=waveloop) | [ELI7VH](https://github.com/ELI7VH/Felucca) | A Felucca mod: standalone Arturia MiniLab 3 control, fine track mixing, a DJ filter, momentary effects and MIDI feedback. |
| [winampFM Synth 0.12](https://joakimeriksson.github.io/fm1-emu/?fw=winampfm) | [hericdk](https://github.com/hericdk/winampFM) | Winamp 2.x on the FM-1, built on SLOOP: your sounds as a playlist, four tracks recorded as loops, effects on sliders and a visualizer of lines. |
| [X0X 1.1.0](https://joakimeriksson.github.io/fm1-emu/?fw=x0x) | [charlesvestal](https://github.com/charlesvestal/fm1-x0x) | A groovebox: 909, 808, two 303s and a break player. |
| [zp12 2.4.2](https://joakimeriksson.github.io/fm1-emu/?fw=zp12) | [zvenson](https://github.com/zvenson/zp12) | A 12-bit sampling drum machine after the E-mu SP-1200: 32 pads at 26 kHz with no interpolation, filtered channels on the knobs, loops and songs. |
<!-- /firmwares -->

MIDI in works through the emulated MIDI IN jack, so each firmware reads MIDI with its own
driver: Web MIDI on the page (Chrome, Edge, Firefox), any MIDI input natively. M-VAVE's
firmware listens on channel 2 in the emulator (not yet checked on a real FM-1); Felucca plays
its parts on channels 1–4.

## The SMK-37

M-VAVE's SMK-37 (Pro / Elite, the MKE-P37 and the Donner Starrykey 37 Play share its hardware) is a
37-key keyboard with 16 RGB pads, 8 knobs, 4 faders, pitch and mod wheels and a screen, around the
FM-1's chip: same flash layout, audio and update path, only the panel is wired differently.
genkigenki's [fm1-to-smk37](https://github.com/genkigenki/fm1-to-smk37) kit maps that wiring and
ports FM-1 firmwares to it. The emulator models the SMK-37's panel from that map, and a package's
identity says which board it runs on.

**Play it: [joakimeriksson.github.io/fm1-emu/smk37](https://joakimeriksson.github.io/fm1-emu/smk37/)**.
The firmwares come from [genkigenki's install page](https://genkigenki.github.io/smk37/)
(`node tools/smk37_catalog.mjs` refreshes the list). M-VAVE's own SMK-37 firmware runs as the
MIDI controller it is (screen, buttons, pads, keys; it names the chord you hold), without sound (its
notes go to USB / Bluetooth MIDI) and slower than real time in the browser so far.

<!-- smk37-firmwares: node tools/readme_firmwares.mjs -->
| Firmware (opens it on the page) | By | What it is |
|---|---|---|
| [M-VAVE stock firmware 016](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=stock) | [M-VAVE](https://github.com/genkigenki/fm1-to-smk37) | The firmware the SMK-37 Pro ships with: a MIDI controller. Bring your own `SMK-37 Pro_016.fwsc`. |
| [Makkuro37 0.1](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=makkuro37) | [genkigenki](https://github.com/genkigenki/Makkuro37) | A microtonal drone synth: eight voices as eight soot creatures, 96 presets. |
| [Vector37 0.2.2](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=vector37) | [genkigenki](https://github.com/genkigenki/Vector37) | An orbiting overtone synth: a moon circles a sun along a path, and where it is decides how much of each of four sounds you hear. |
| [Snoop37 2.6.5](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=snoop37) | [isod89 (port: genkigenki)](https://github.com/genkigenki/sloop-fm1) | SLOOP, the four-track looper groovebox, adapted for the SMK-37. |
| [X0X 0.2](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=x0x37) | [Charles Vestal (port: genkigenki)](https://github.com/genkigenki/fm1-x0x) | A 909, an 808, two 303s with a line generator and a breakbeat player, all at once. |
| [FoMni 0.2](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=fomni37) | [Charles Vestal (port: genkigenki)](https://github.com/genkigenki/fm1-fomni) | An Omnichord-style chord harp: the 16 pads become the strum plate, the keybed holds the chords. |
| [MAKU 0.1](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=maku37) | [okumurari2 (port: genkigenki)](https://github.com/genkigenki/MAKU37) | An always-on ambience machine: power it on and a new, random, in-key world is already playing. |
| [Rainbow mode 0.2](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=rainbow37) | [joshbartlettband-web (port: genkigenki)](https://github.com/genkigenki/RAINBOW37) | A music toy for kids: colour-coded notes, 40 friends who sing along, no menus. |
| [GHOULBOX 2.1](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=ghoulbox) | [Jason Persinger (port: genkigenki)](https://github.com/genkigenki/Felucca/tree/ghoulbox-smk37) | Dungeon-synth firmware: dark ambient, medieval and fantasy sounds, with drones, a hurdy-gurdy crank and two sounds at once. |
| [ChoralRoot 0.21](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=choralroot) | [Quixotic7 (port: genkigenki)](https://github.com/genkigenki/ChoralRootFM1/tree/smk37) | A chord instrument in the manner of the Orchid: one hand plays roots, the other picks the chord. |
| [ORBIT 0.7.0](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=orbit) | [dspaudio (port: genkigenki)](https://github.com/genkigenki/orbit/tree/smk37) | A four-track groovebox with OP-1-inspired screens: three synth parts, a drum machine, an event tape and a mixer. |
| [AMB-1 1.1](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=amb) | [Charles Vestal (port: genkigenki)](https://github.com/genkigenki/fm1-amb/tree/smk37) | A generative ambient machine inspired by Music for Airports: loops of different lengths drift past each other and never repeat. |
| [Polly 0.4.1](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=polly) | [Charles Vestal (port: genkigenki)](https://github.com/genkigenki/fm1-polly/tree/smk37) | A pocket arranger inspired by the Yamaha QY70: a band of eight General MIDI instruments follows the chords you play. |
| [winampFM 0.1](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=winamp) | [Hericdk (port: genkigenki)](https://github.com/genkigenki/winampFM/tree/smk37) | A synth groovebox in the look of Winamp 2.x, built on SLOOP: player, playlist, effect sliders and a visualizer. |
| [OberVave 1.2.1](https://joakimeriksson.github.io/fm1-emu/smk37/?fw=obervave) | [zvenson](https://github.com/zvenson/obervave/blob/main/SMK37.md) | One 8-voice analog-style polysynth in the spirit of the American poly synths of 1979–84, with chorus, delay and a plate reverb. |
<!-- /smk37-firmwares -->

## On your computer

```sh
cargo run --release -p fm1-live -- --fw list           # the firmwares
node tools/fetch_firmwares.mjs                          # download them into reference/firmwares
cargo run --release -p fm1-live -- --fw felucca         # a window with the synth, audio out
cargo run --release -p fm1-live -- --fwsc FM-1.fwsc     # M-VAVE's own, from its package
```

`fm1-live` runs the real 240 MHz clock on Apple silicon Macs, with an AArch64 JIT; elsewhere
it interprets (`--mhz` lowers the clock if that can't keep up). It plays every MIDI input of the computer (`--midi NAME`, `--midi none`,
`--midi-ch N`) and loads a DX7 bank into M-VAVE's firmware with `--syx bank.syx`.
`fm1-emu` is the batch runner for tests and debugging (WAV and screen output, breakpoints,
memory dumps).
Both take `--flaky-core1 RATE [--seed N]`: the second core misreads RAM now and then, as on
some units, to test a firmware's fallback to one core ([docs/stock-firmware.md](docs/stock-firmware.md#a-flaky-core-1---flaky-core1-fm1_socflaky)).

The page builds with `web/build.sh` (Rust to WebAssembly; needs the `wasm32-unknown-unknown`
target), the whole site with `tools/build_site.sh`.

## In your DAW: AU and VST3

The emulator also comes as an **instrument plugin**, so a firmware plays in a DAW like any
other synth:

| Format | Loads in |
|---|---|
| **AU** | Logic Pro, GarageBand, MainStage, and most other Mac DAWs |
| **VST3** | Ableton Live, Cubase, FL Studio, Reaper, Bitwig, Studio One, Cakewalk… |
| **CLAP** | Bitwig, Reaper, FL Studio, Studio One… |

**What it does:**
- **The firmware:** **Load firmware…** in the plugin's window picks a `.fwsc` package: any
  community firmware (the table above links each author's) or M-VAVE's own. An SMK-37 package
  gets the SMK-37's panel.
- **Playing it:** MIDI from the DAW plays it, as through the FM-1's MIDI IN. A *MIDI channel*
  setting moves all notes to one channel; M-VAVE's firmware listens on 2. The panel is the
  page's, played with the mouse: keys, buttons, knobs, and on the SMK-37 the pads, faders and
  wheels.
- **Automation:** *Master* (the MASTER pot) is a parameter the DAW can automate.
- **The project keeps the firmware and its patches.** The package and every flash sector the
  firmware has written are saved with the song, so it reopens with the same sounds.
- **Timing:** the emulator runs a little ahead of the DAW and reports that delay as the plugin's
  latency, so the DAW compensates and notes land where they were placed. The status bar shows
  how much faster than real time the firmware could run on this computer; most run at 3–6x,
  the heaviest at about 1.5x.

**Download it** from [the releases](https://github.com/joakimeriksson/fm1-emu/releases/latest),
for Macs with Apple silicon (M1 or later). The zip has the three plugins and an `INSTALL.txt` that
says where each goes. Until a release is signed with an Apple Developer ID, macOS blocks it until
you allow it, with one Terminal command that `INSTALL.txt` gives.

**Build and install it yourself (macOS, Apple silicon).** You need Rust, CMake and Ninja
(`brew install cmake ninja`), then:

```sh
tools/build_plugin.sh
```

This builds the plugin and installs `FM-1.component`, `FM-1.vst3` and `FM-1.clap` in
`~/Library/Audio/Plug-Ins`. It then runs Apple's AU validator on them.

**In GarageBand:**
1. *Settings → Audio/MIDI*: tick *Enable Audio Units*, then quit and reopen GarageBand.
2. Make a *Software Instrument* track and open its Smart Controls (**B**).
3. Click the *Instrument* slot and choose *AU Instruments → fm1-emu → FM-1*.
4. Click the slot to open its window, then *Load firmware…*.
5. Play it with **⌘K** (Musical Typing) or a MIDI keyboard.

**In Logic:** a Software Instrument track's instrument slot, *AU Instruments → fm1-emu → FM-1*.

**In VST3 or CLAP hosts:** add *FM-1* (by fm1-emu) as an instrument on a track.

**How it's built:** it's one plugin written in Rust as a CLAP with
[nih-plug](https://github.com/robbert-vdh/nih-plug). [clap-wrapper](https://github.com/free-audio/clap-wrapper)
makes the AU and VST3 of it, and the plugin stays MIT. All its parts are MIT, ISC or Apache-2.0,
including Apple's AudioUnitSDK and Steinberg's VST3 SDK, which is MIT since 3.8.

**So far it has passed:**
- Apple's AU validator (`auval`);
- Steinberg's VST3 validator (47 of 47 tests);
- GarageBand, as an AU;
- Steinberg's VST3 test host.

Logic, Ableton and the Windows / Linux builds haven't been tried yet. More in
[crates/fm1-plugin](crates/fm1-plugin/README.md).

## How it works

The FM-1 is built on a JieLi AC791N: two cores of JieLi's own 32-bit pi32v2 CPU, with no
public instruction set manual. The instruction set here was recovered from the firmwares
themselves (`isa/`, `docs/isa-notes.md`) and checked against test programs built with JieLi's
toolchain. On top of the CPU sit models of the chip's timers, interrupts, flash, the
240×240 screen, the key matrix and encoders, the knobs, the audio output and the MIDI UART.
Hot code is compiled, to AArch64 natively and to WebAssembly in the browser.
How it was built and checked, step by step, is told in a technical report:
[Emulating a Synthesizer Before It Arrives](https://joakimeriksson.github.io/fm1-emu/fm1-emu-report.pdf)
(PDF, 8 pages).

- `crates/`: `fm1-isa` (decoder tables), `fm1-core` (CPU and JITs), `fm1-soc` (the chip and
  the board), `fm1-live` (the native synth), `fm1-emu` (batch runner), `fm1-web` (the wasm
  build), `fm1-gdbstub`
- `web/`: the page; `web/fwsc.js` unpacks the update packages
- `docs/`: the hardware notes (`recon.md`), the instruction set (`isa-notes.md`), M-VAVE's
  firmware (`stock-firmware.md`); `PLAN.md` is the work log
- `tools/`, `probes/`: ISA extraction, benchmarks, differential test programs
- `fm1-sid/`: the SID study and player firmware (its own README)

## Licences

The emulator is MIT (`LICENSE`). The firmwares are their authors' and are not part of this
repository; the page fetches them from the authors' sites. `fm1-sid/` has files under other
licences, among them its player firmware, GPL-3.0 since it is built on Felucca's code:
its README lists them. The plugin (`crates/fm1-plugin`) is MIT too; the libraries and SDKs it
builds with are MIT, ISC or Apache-2.0.

## Credits

Built on the community's reverse engineering of the FM-1:
[AL-255/FM-1-RE](https://github.com/AL-255/FM-1-RE),
[ip2k/lunar-modulator](https://github.com/ip2k/lunar-modulator),
kagaimiq's [jielie](https://kagaimiq.github.io/jielie), the hardware layer of
[hugelton/Felucca](https://github.com/hugelton/Felucca) and
[awesome-fm-1](https://github.com/cicloid/awesome-fm-1). Thanks to every firmware author for publishing their work.
