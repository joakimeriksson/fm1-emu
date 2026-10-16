# Try Hard's MIDI

Plain USB-MIDI, cable 0, all on **channel 16** (status bytes `x9F`, `x8F`, `xBF`). Other channels are not used, so
a page or a DAW can tell the panel apart from music.

Ids are the emulator's panel matrix ids, the same as Dinghy's: **0..13 the buttons, 14..40 the 27 keys** (14 = F3 ..
40 = G5).

| id | button | id | button | id | button |
| --- | --- | --- | --- | --- | --- |
| 0 | OCT− | 5 | LFO | 10 | ARP |
| 1 | OCT+ | 6 | EDIT | 11 | SEQ |
| 2 | FX | 7 | GLO | 12 | PLAY |
| 3 | SEL | 8 | HOME | 13 | REC |
| 4 | ENV | 9 | SAVE | | |

## FM-1 → page

| message | meaning |
| --- | --- |
| `9F id 127` | key or button `id` pressed |
| `8F id 0` | key or button `id` released |
| `BF 16+e v` | encoder `e` turned `v − 64` detents (+ clockwise): e = 0 SELECT, 1 ALGORITHM, 2..5 KNOB1..4, 6 PRESETS |
| `BF 7 msb`, `BF 39 lsb` | MASTER, 0..1023 = `msb << 3 \| lsb >> 4` (14-bit CC pair, MSB first) |
| `BF 127 84` | hello: sent once a second (84 is `T`) |

## Page → FM-1

| message | meaning |
| --- | --- |
| `9F id v` | LED `id` on (`v > 0`) or off (`v = 0`) |
| `8F id 0` | LED `id` off |
| `BF 123 0` | all LEDs off |
| `BF 127 84` | hello: the page is there. Sent once a second; the FM-1 shows *linked* and sends MASTER again |

The FM-1 counts the page gone after 3 s without a hello (its LEDs then chase while it waits). The page counts an
FM-1 gone after 3.5 s, and sends every LED again with each hello, so a lost message heals within a second.
