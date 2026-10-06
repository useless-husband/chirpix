# Status: analogue SSTV baseline (branch `sstv-baseline`)

Everything below was run on a cloud Linux VM with four vCPUs.

## What changed

- `src/sstv.rs`: Robot 36 encoder (Barber 2000 timing) and decoder; `chirpix sstv-encode | sstv-decode`.
- Report: three SSTV rows per channel, a 36.9 s column, two SSTV picture strips, SSTV in the start-time
  chart, and an SSTV section (resolution limit, one picture per channel, white-noise sweep).
- `make sstv-check`: cross-check against pySSTV 0.5.9 and sstv 0.2.0, also in CI. CI now runs on every
  pushed branch.
- README (en, zh-TW), DESIGN.md, 導讀, CHANGELOG.

## Commands and results

- `make test`: 84 passed (the 4 cross-check tests are ignored there).
- `make lint`: clean.
- `make sstv-check`: 4 passed. Encoder equals pySSTV within one 16-bit step on all 1 771 680 samples.
- `make data report`: 5 min 8 s. On "fair" at 36.9 / 75 s: chirpix 22.8 / 26.4 dB, SSTV 16.9 / 16.9 dB.
  On "poor" at 75 s: 24.8 against 14.2 dB. In white noise at 2 dB, chirpix shows nothing and SSTV gives
  15.7 dB. The chirpix numbers match the README's exactly (also checked with a run before any change).
- GitHub Actions on the branch: green at the head. Runs 3 and 4 failed `cargo fmt --check` on two
  intermediate commits; the next commit fixed it.

## Failed or left out

- `make data` failed: the VM's network policy blocks r0k.us (403). The four files came from a GitHub
  mirror (MohamedBakrAli/Kodak-Lossless-True-Color-Image-Suite) and matched the script's SHA-256 values.
- Barber's paper was unreachable for the same reason; the timing was checked against the pySSTV and
  SSTVEncoder2 sources instead.
- SSTV loses in every simulated room. It wins only below the modem's threshold in white noise.
- Decoder smoothing was tuned for PSNR on the built-in pictures. On clean audio it is 2.3 dB below the
  independent decoder on one test picture (above it on the other).
- Not done: an equal-peak-power comparison, other SSTV modes. `docs/report.png` was not regenerated
  (no binaries), and the README's chirpix CLI session was not re-run.
