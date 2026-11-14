#!/usr/bin/env bash
# A video of a headless run: fm1-live --shot --record - streams a PPM frame every 16 ms into
# ffmpeg (62.5 fps, resampled to 60 for players), then the audio it wrote to work/audio.raw is
# muxed in without touching the picture.
#
#   tools/bench_video.sh                                 # default: Felucca, 14.5 s, Happy Birthday
#   tools/bench_video.sh --fw ghoulbox --shot 8000       # any firmware, any duration
#   tools/bench_video.sh --lcd-only                      # the LCD alone instead of the panel
#   KEYS="800:C4,1200:C4^" tools/bench_video.sh         # custom input script
#   OUT=demo.mp4 tools/bench_video.sh                    # custom output path
#
# Requires: fm1-live (cargo build -p fm1-live --release), ffmpeg.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

BIN="${BIN:-target/release/fm1-live}"
OUT="${OUT:-work/video.mp4}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Default: Felucca playing Happy Birthday
if [ -z "${KEYS:-}" ]; then
  KEYS="800:PLAY,950:PLAY^"
  KEYS="$KEYS,1500:C4,1700:C4^,1750:C4,1950:C4^,2000:D4,2450:D4^,2500:C4,2950:C4^,3000:F4,3450:F4^,3500:E4,4400:E4^"
  KEYS="$KEYS,4500:C4,4700:C4^,4750:C4,4950:C4^,5000:D4,5450:D4^,5500:C4,5950:C4^,6000:G4,6450:G4^,6500:F4,7400:F4^"
  KEYS="$KEYS,7500:C4,7700:C4^,7750:C4,7950:C4^,8000:C5,8450:C5^,8500:A4,8950:A4^,9000:F4,9450:F4^,9500:E4,9950:E4^,10000:D4,10900:D4^"
  KEYS="$KEYS,11000:Bb4,11200:Bb4^,11250:Bb4,11450:Bb4^,11500:A4,11950:A4^,12000:F4,12450:F4^,12500:G4,12950:G4^,13000:F4,13900:F4^"
  DEFAULT_SHOT=14500
fi

# Pass remaining args through to fm1-live (e.g. --fw, --fwsc, --shot, --lcd-only)
EXTRA_ARGS=("$@")

# If no --shot in extra args and we have a default, add it
if ! printf '%s\n' "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}" | grep -q -- '--shot'; then
  EXTRA_ARGS+=(--shot "${DEFAULT_SHOT:-8000}")
fi

# If no --fw/--fwsc/--bin in extra args, default to felucca
if ! printf '%s\n' "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}" | grep -qE -- '--(fw|fwsc|bin)'; then
  EXTRA_ARGS+=(--fw felucca)
fi

# The frames straight into the encoder (a full-panel PPM is 3 MB; none lands on disk); the
# run's reports come on stderr. A halt exits non-zero, which stops the pipeline right here.
echo "recording..." >&2
FM1_BENCH_KEYS="$KEYS" "$BIN" "${EXTRA_ARGS[@]}" --record - \
  | ffmpeg -loglevel error -y -f image2pipe -c:v ppm -framerate 62.5 -i - \
           -vf fps=60 -c:v libx264 -pix_fmt yuv420p -crf 18 "$tmp/video.mp4"

# The audio: really 24 MHz / 544 = 44117.6 Hz, tagged so and resampled to 44100 for the AAC.
echo "muxing -> $OUT" >&2
mkdir -p "$(dirname "$OUT")"
ffmpeg -loglevel error -y -i "$tmp/video.mp4" -f s16le -ar 44118 -ac 2 -i work/audio.raw \
       -c:v copy -c:a aac -b:a 192k -ar 44100 "$OUT"
echo "$(du -h "$OUT" | cut -f1)  $OUT"
