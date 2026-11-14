#!/usr/bin/env bash
# A firmware reaches the internet through the emulated network, headless: it boots with a link and the real
# network behind it (--net internet), the key script asks it for a page, and at the end the panel is a PNG to look at.
# The check is the subnet's trace (FM1_NET_TRACE): a DNS answer for each host the firmware asks for, a NAT connection
# up, and bytes back from a host. Needs the real network.
#
# The default is fm1-base's WiFi example built for the emulator's mailbox (fm1-base: tools/build.py examples/wifi
# --wifi-emu): it joins "Home" by itself and PLAY fetches http://example.com/, the status line and the bytes on
# its screen.
#   FW=work/fm1-base/examples/wifi/build/wifi-emu.fwsc tools/check_wifi.sh [ms=20000]
#
#   LINK="--wifi jieli"          the fm1-live link: "--wifi oss" (modd's driver), "--usb ethernet" (a CDC-ECM adapter)
#   KEYS="12000:PLAY,12200:PLAY^"   FM1_BENCH_KEYS: what to press, and when (ms)
#   HOSTS="example.com"         the names the firmware must resolve
#   OUT=dir                     where the PNGs and the log go (default .scratch/screenshots/<date>)
#   BIN=target/release/fm1-live
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
BIN="${BIN:-target/release/fm1-live}"
FW="${FW:?FW=path/to/firmware.fwsc (a firmware with a network)}"
ms="${1:-20000}"
LINK="${LINK:---wifi jieli}"
KEYS="${KEYS:-12000:PLAY,12200:PLAY^}"
HOSTS="${HOSTS:-example.com}"
day="$(date -u +%F)"
stamp="$(date -u +%Y-%m-%dT%H%M%SZ)"
OUT="${OUT:-.scratch/screenshots/$day}"
mkdir -p "$OUT"
log="$OUT/$stamp-wifi.log"
# shellcheck disable=SC2086
echo "$BIN --fwsc $FW $LINK --net internet --shot $ms   (FM1_BENCH_KEYS=$KEYS)"
# shellcheck disable=SC2086
FM1_NET_TRACE=1 FM1_BENCH_KEYS="$KEYS" "$BIN" --fwsc "$FW" $LINK --net internet --shot "$ms" >"$log" 2>&1
rc=$?
failed=0
check() {
  if grep -q -- "$2" "$log"; then printf "  ok    %s\n" "$1"; else printf "  FAIL  %s\n" "$1"; failed=1; fi
}
echo "exit $rc; log $log"
[ "$rc" -eq 0 ] || failed=1
check "the shot ran to the end" "^work/panel.ppm at"
for h in $HOSTS; do check "dns answered: $h" "net: dns \"$h\" ->"; done
check "nat: a connection to a host came up" "^nat: .* connected"
check "nat: bytes back from a host" "bytes from the host"
if [ -s work/panel.ppm ]; then
  # the panel, and the 240x240 LCD alone (drawn at 2x at LCD_X/LCD_Y of fm1-live's panel.rs)
  if command -v magick >/dev/null; then
    magick work/panel.ppm "$OUT/$stamp-wifi-panel.png" &&
      magick work/panel.ppm -crop 480x480+370+42 +repage "$OUT/$stamp-wifi-lcd.png" &&
      echo "  png   $OUT/$stamp-wifi-panel.png, -lcd.png"
  else
    ffmpeg -loglevel error -y -i work/panel.ppm "$OUT/$stamp-wifi-panel.png" && echo "  png   $OUT/$stamp-wifi-panel.png"
  fi
fi
echo "the subnet and the NAT (first 20 lines):"
grep -E '^(net|nat): ' "$log" | head -20
exit "$failed"
