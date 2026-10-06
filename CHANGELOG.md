# Changelog

## Unreleased

Analogue SSTV baseline.

- `src/sstv.rs`: Robot 36 encoder (Barber's timing and studio-swing colour; `--convention pysstv` for
  Pillow's full-range YCbCr as pySSTV sends it) and decoder (FM discriminator, VIS detection from tone
  powers, line sync runs with a clock-offset fit, separator parity across dropouts, smoothing chosen from the
  noise on the sync pulses, placement from a header or buffered back from the next one).
- `chirpix sstv-encode | sstv-decode`.
- Report: three SSTV rows in every channel's table, a 36.9 s column (one Robot 36 picture) for every scheme,
  SSTV in the start-time chart and per-image table, two SSTV picture strips, and a section with the
  resolution limit, one picture per channel and a white-noise sweep against chirpix.
- `make sstv-check` (`scripts/sstv_crosscheck.sh`, `tests/sstv_crosscheck.rs`): pySSTV 0.5.9 and sstv 0.2.0
  in a virtual environment; encoder equal to pySSTV's sample for sample, decoders cross-checked. Run in CI.
- `Image::resize` (area averaging down, linear up).
- CI runs on every pushed branch.

## 0.1.0 (unreleased)

First version.

- OFDM acoustic modem at 48 kHz in 1.03-7.03 kHz: chirp preamble, one training symbol, repeated coded header,
  pilots on every eighth carrier, per-frame clock-offset fit, two-pass resampling, QPSK and 16-QAM, rate-1/2
  K=7 convolutional code with soft Viterbi decoding, CRC-32 per packet, decision-directed second pass.
- Symbol length chosen by an echo sweep: 171 ms symbols with a 43 ms cyclic prefix.
- Expanding-window fountain code over 107-byte packets with a Gaussian-elimination decoder; six windows sized
  to complete after about 1/15, 1/5, 2/5, 3/5, 4/5 and all of the design listening time.
- Progressive colour image codec: 9/7 wavelet, quadtree bit-planes, adaptive binary range coder; every prefix
  of the stream decodes.
- Own WAV, PNG and DEFLATE code.
- Channel simulator: band-limiting, synthetic room impulse response, clock offset, white noise, clicks,
  dropouts, clipping.
- `chirpix encode | decode | simulate | report | testimage | info`.
- Static HTML report: pictures over time, PSNR/SSIM against listening time and start time, error rate
  against theory, robustness sweeps, numerology sweep, spectrogram and constellation.
- `scripts/audio_loop.swift` and `scripts/loopback.sh`: play and record through named audio devices; verified
  through the BlackHole virtual device. Not verified through a real loudspeaker and microphone.
