#!/bin/sh
# Send a picture through the real macOS audio stack without making a
# sound: play into the virtual device "BlackHole 2ch" and record from it.
# Needs BlackHole (https://existential.audio/blackhole/), the Swift
# compiler, and microphone permission for the terminal. Exits 77 (skipped)
# if any of those is missing.
set -eu
cd "$(dirname "$0")/.."
DEV=${CHIRPIX_LOOP_DEVICE:-BlackHole 2ch}
BIN=./target/release/chirpix
OUT=out/loopback
command -v swift >/dev/null 2>&1 || { echo "skipped: no swift compiler"; exit 77; }
[ -x "$BIN" ] || { echo "run 'make build' first"; exit 1; }
mkdir -p "$OUT"
"$BIN" testimage scene -o "$OUT/scene.png" --size 384x256
"$BIN" encode "$OUT/scene.png" -o "$OUT/tx.wav" --mode "${1:-robust}" --design 30 --seconds 36
set +e
swift scripts/audio_loop.swift --play "$OUT/tx.wav" --record "$OUT/rx.wav" \
  --out-device "$DEV" --in-device "$DEV" --no-prompt
code=$?
set -e
if [ "$code" -eq 3 ] || [ "$code" -eq 4 ]; then
  echo "skipped: audio_loop exit $code (no permission or no \"$DEV\" device)"
  exit 77
fi
[ "$code" -eq 0 ] || exit "$code"
"$BIN" decode "$OUT/rx.wav" -o "$OUT/decoded" --every 6 --ref "$OUT/scene.png"
