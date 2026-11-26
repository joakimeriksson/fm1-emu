# The FM-1's 3D model (web/fm1.glb)

The web page's 3D view (web/panel3d.js) draws `web/fm1.glb`. `build.py` makes it: the panel is an
original drawing made with Pillow (no product photo, no firmware artwork), on simple extruded shapes.
Rebuild: `pip install numpy trimesh pillow`, then `python build.py` with DejaVuSans.ttf installed or
beside it (matplotlib ships one: mpl-data/fonts/ttf), and copy `fm1.glb` to `web/`.

- v7: the FM-1 and FM1-EMU marks on the face, the upper keys' legends (OP1..OP6, PIT, GLO, MONO,
  POLY) as printed on the hardware, a lower profile, whole key caps as LEDs, a 40%-wide LCD.
  Metres, +Z the front, +Y up. Sizes are approximate, not measured; the LCD is larger than the real
  one on purpose, for the firmwares' small text.
- Nodes panel3d.js reads: `knob_{master,select,presets,algorithm,knob1..knob4}`,
  `button_{oct_down,oct_up,fx,sel,env,lfo,edit,glo,home,save,arp,seq,play_stop,rec}`,
  `key_lower_00..15` (F3..G5's white keys, left to right) and `key_upper_00..10` (the black keys),
  each key with a `_led` child (its own emissive material), and `lcd` (a square, UVs 0..1, for the
  240 x 240 screen).
