#!/bin/sh
# Cross-check the Robot 36 encoder and decoder against two independent
# implementations, installed into a private virtual environment under out/:
# pySSTV 0.5.9 (encoder) and sstv 0.2.0 (decoder). Needs python3 with venv
# and access to PyPI. chirpix itself does not use either.
set -eu
cd "$(dirname "$0")/.."
VENV=out/sstv-venv
[ -x "$VENV/bin/python" ] || python3 -m venv "$VENV"
"$VENV/bin/pip" install -q pysstv==0.5.9 sstv==0.2.0 pillow==12.3.0
cargo build --release -q
"$VENV/bin/python" scripts/sstv_crosscheck.py ./target/release/chirpix out/sstv-crosscheck
CHIRPIX_SSTV_FIXTURES=out/sstv-crosscheck cargo test --release -q --test sstv_crosscheck -- --ignored --nocapture --test-threads 1
