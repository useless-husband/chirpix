# Status: analogue SSTV baseline (branch `sstv-baseline`)

All runs were on a cloud Linux VM with four vCPUs.

## What changed

- `src/sstv.rs`: Robot 36 encoder (Barber 2000 timing) and decoder; `chirpix sstv-encode | sstv-decode`.
- Report: three SSTV rows per channel, a 36.9 s column, two SSTV picture strips, SSTV in the start-time
  chart, an SSTV section with a white-noise sweep.
- `make sstv-check` (pySSTV 0.5.9, sstv 0.2.0), also in CI; CI now runs on every pushed branch.
- README (en, zh-TW), DESIGN.md, 導讀, CHANGELOG.

## Commands and results

- `make test`: 84 passed. `make lint`: clean.
- `make sstv-check`: 4 passed; our encoder equals pySSTV within one 16-bit step on every sample.
- `make data report` (5 min 8 s): "fair" at 36.9 / 75 s, chirpix 22.8 / 26.4 dB, SSTV 16.9 / 16.9 dB;
  "poor" at 75 s, 24.8 against 14.2 dB; white noise at 2 dB, chirpix nothing, SSTV 15.7 dB. The chirpix
  numbers match the README's exactly.
- Actions: green at the head; runs 3 and 4 failed `cargo fmt --check` on intermediate commits.

## Failed or left out

- `make data` failed: the network policy blocks r0k.us. The files came from a GitHub mirror
  (MohamedBakrAli/Kodak-Lossless-True-Color-Image-Suite) with matching SHA-256.
- Barber's paper was unreachable too; the timing was checked against pySSTV and SSTVEncoder2 sources.
- SSTV loses in every simulated room; it wins only below the modem's threshold in white noise.
- Smoothing was tuned for PSNR on built-in pictures; on clean audio our decoder is 2.3 dB below the
  independent one on one picture, above it on the other.
- Not done: equal-peak-power comparison, other modes, regenerating `docs/report.png`, re-running the
  README's chirpix CLI session.
