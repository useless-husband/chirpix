# Design notes

What the parts are, which problems turned out to be hard, how each was settled, and what was tried or
considered and left out. Numbers quoted here come from `make report` (four Kodak photographs, simulated
channels) unless stated otherwise.

## The pipeline

```
picture ── codec ──► embedded byte stream ── fountain ──► endless packet sequence ── modem ──► audio
 (PNG)   wavelet,      any prefix decodes     windows,      packet n is a fixed       OFDM
         bit-planes                           XOR mixes     function of n             frames

audio ── modem ──► packets that passed CRC ── fountain ──► longest known prefix ── codec ──► picture
        (any start,    with sequence numbers    Gaussian       of the stream
         any gaps)                              elimination
```

Three layers, each with one property the next relies on:

| Layer | File | Property |
|---|---|---|
| Image codec | `codec.rs`, `wavelet.rs`, `rangecoder.rs` | Any prefix of the byte stream decodes; a longer prefix is never worse |
| Fountain code | `fountain.rs` | Any large enough set of packets recovers the stream, earliest part first |
| Modem | `modem.rs`, `conv.rs` | Every frame stands alone; a packet either arrives intact or not at all |

`link.rs` joins them, `channel.rs` is the simulated room, `experiments.rs` and `report.rs` measure.
`sstv.rs`, outside this pipeline, is the analogue baseline they are compared with.

## Modem

### Signal

48 kHz, real-valued OFDM. Carriers occupy 1031-7031 Hz. The band is a judgement, not a measurement: small
loudspeakers radiate little below about 1 kHz and room noise is strongest there; above 7-8 kHz some phone
recorders sample at 16 kHz or low-pass for speech. No loudspeaker or microphone response was measured for
this project. The simulator's band-limit sweeps show what is lost if the hardware passes less (see README).

One frame is 82 944 samples (1.728 s):

```
| chirp 1024 | CP 2048 + training 8192 | CP + header 8192 | 6 x (CP + data symbol 8192) |
```

- **Chirp**: linear sweep over the band, 21 ms. The receiver correlates the recording with an analytic copy
  of it (FFT correlation in blocks) and normalises by the local energy, so the detection statistic is
  between 0 and 1 regardless of recording level.
- **Training symbol**: all 1024 carriers known, with Newman (quadratic) phases so the time signal has a low
  peak factor.
- **Header symbol**: 8 header bytes + CRC-16, convolutionally coded to 128 QPSK points, repeated eight
  times across the 1024 carriers with a cyclic shift per repetition so the copies of one point are spread
  over the band. The repetitions are soft-combined. It says: constellation, packet scheme, session, frame
  counter, number of source packets.
- **Data symbols**: 896 data carriers + 128 pilots (every eighth carrier). One symbol is one code block:
  890 information bits (QPSK) or 1786 (16-QAM), rate-1/2 K=7 convolutional code, six tail bits, a random
  interleaver over the block. A QPSK block carries one 107-byte packet + CRC-32; a 16-QAM block carries two.

The CRC-32 covers the payload *and* the packet's sequence number, session and stream size, none of which is
transmitted in the block. A block decoded against the wrong header therefore fails its CRC instead of
handing the fountain decoder a packet with the wrong identity (which would poison every later XOR).

### Hard problem 1: echo, and why the symbols are 171 ms long

The first version used 21 ms symbols with a 5 ms cyclic prefix. It decoded perfectly in white noise and
then hit a ceiling in the simulated room: the SNR the receiver measured stopped at about 7 dB however loud
the signal was, and at a direct-to-reverberant ratio (DRR) of 0 dB nothing got through.

The reason is arithmetic. Sound that arrives later than the cyclic prefix belongs to the wrong symbol and is
interference. A room with RT60 = 0.45 s has an energy decay constant of 33 ms; a 5 ms prefix catches about
an eighth of the echo and the rest is noise that scales with the signal. With echo energy `E` relative to
the direct sound, the ceiling is roughly

```
SIR ≈ (1 + E − I) / I,   I = E · e^(−CP/τ) · g(τ / symbol)
```

where `g` falls as the symbol gets longer (an echo that overlaps only a small part of the next symbol does
little harm). Longer symbols with proportionally longer prefixes raise the ceiling.

The modem was rewritten around a `Params` struct and the choice made by measurement (`chirpix report`,
"Why the symbols are this long"; RT60 0.45 s, noise 25 dB down; QPSK / 16-QAM packets delivered):

| Symbol + prefix | QPSK rate | DRR +10 dB | +5 dB | 0 dB | −5 dB | −10 dB |
|---|---|---|---|---|---|---|
| 21 + 5 ms | 465 B/s | 100% / 100% | 100% / 0% | 0% / 0% | 0% / 0% | 0% / 0% |
| 43 + 11 ms | 456 B/s | 100% / 100% | 100% / 0% | 10% / 0% | 0% / 0% | 0% / 0% |
| 85 + 21 ms | 424 B/s | 100% / 100% | 100% / 98% | 99% / 0% | 2% / 0% | 0% / 0% |
| **171 + 43 ms** | 372 B/s | 100% / 100% | 100% / 100% | 100% / 96% | 100% / 8% | 85% / 0% |

At a metre or more from a small loudspeaker in an ordinary room the DRR is around 0 dB or below, so the
longest option was taken, at a cost of 20% in rate against the shortest. What it costs elsewhere:

- Carrier spacing is 5.9 Hz. A clock offset of 200 ppm moves a 7 kHz carrier by 1.4 Hz, a quarter of the
  spacing, which smears carriers into each other. Hence the two-pass receiver below.
- Anything that changes the channel within 0.2 s (a hand-held phone, people walking past) also smears
  carriers. The simulator does not model movement; this is the main untested risk for real use.
- A frame is 1.7 s, so a listener loses 0.9 s on average before the first whole frame.

### Hard problem 2: two clocks

Player and recorder run on different crystals. Nothing is mixed to a carrier, so an offset of ε stretches
time: a symbol that starts `t` samples after the training symbol arrives `ε·t` samples late, which the FFT
sees as a phase ramp across carriers of `2π·k·ε·t/N`.

1. For each data symbol, the pilots give the delay that best explains the ramp (a one-dimensional search:
   maximise `Re Σ r_k e^{j2πkd/N}`; coarse grid, fine grid, parabolic fit). The first symbol is searched
   over ±600 ppm, later ones around the previous answer.
2. A constant offset makes delay a straight line through zero at the training symbol, so the per-symbol
   delays are fitted with one slope. The pilots do not depend on the header, so this happens before the
   header is decoded and the header benefits from it.
3. The slope de-rotates every symbol. That fixes the ramp but not the smearing between carriers.
4. `Modem::receive` takes the median slope over all frames and, if it is more than 10 ppm, resamples the
   whole recording by that factor and decodes again. The second pass has no smearing left. The better of
   the two passes is kept.

An earlier version extrapolated the offset from the first two symbols to the rest of the frame. At low SNR
that estimate was noisy enough (±0.2 samples, multiplied by the distance) to lose one frame in 300 at an
SNR where every packet should arrive; fitting over the whole frame removed it.

Measured: 100% of packets from −400 to +400 ppm in both constellations; unreliable at ±1000 and beyond,
outside the search range.

### Hard problem 3: false starts

Random data occasionally correlates with the chirp above the detection threshold (about once per frame).
Two bugs came from this:

- The first peak-picker skipped a stretch of the correlation after each candidate; a false candidate just
  before a real chirp could hide it. Candidates are now local maxima within half a chirp length.
- With the long cyclic prefix and the heavily repeated header, a false candidate a few milliseconds before
  a real chirp decoded the *header* correctly and then blocked the real frame. Candidates are now tried
  strongest first, and an accepted frame rules out any candidate that would overlap it.

Tests: the clean loopback over every numerology reproduces the second bug deterministically; for the first,
`chirp_like_sounds_just_before_a_frame_do_not_hide_it` plants false chirps in front of a frame, and the
white-noise test fails if any of its frames is missed. In the report's error-rate runs (170 frames per
point) no frame is missed at any point where the coded error rate is zero, which is every point from 5 dB
(QPSK) and 12 dB (16-QAM) up.

### Hard problem 4: one training symbol is a noisy ruler

A channel estimate from a single symbol is as noisy as the data it will equalise, which costs 3 dB. Smoothing
across carriers is not safe here: with 43 ms of echo allowed, the channel legitimately changes every few
carriers. Instead the receiver collects more known symbols as it goes:

- once the header passes its CRC it is re-encoded and used as a second training symbol;
- every code block that passes its CRC is re-encoded likewise, the channel and per-carrier noise are
  re-estimated from all of them, and the blocks that failed are tried again.

The noise variance per carrier, which scales the soft bits, comes from the pilot residuals, so it includes
echo and estimation error as well as background noise.

Result in white noise (`chirpix report`, "Error rate against theory"): coded bit error rate 1e-4 at Es/N0
4.1 dB for QPSK and 10.0 dB for 16-QAM, against 3.4 dB and 8.5 dB for the same decoder with perfect timing
and channel knowledge: an implementation loss of 0.7 dB and 1.5 dB. The decoder itself matches the textbook
union bound for this code (unit test `ber_in_gaussian_noise_respects_the_union_bound`).

### Choosing the constellation

There is no return channel. The sender uses QPSK unless a previous test recording, decoded by the same
program, reported at least 12 dB per carrier, in which case 16-QAM doubles the rate. In white noise 16-QAM
delivers 99% of packets from a reported 9-10 dB, so 12 dB leaves a margin. The rule is conservative: on the
"fair" test channel (reported 10 dB) it picks QPSK although 16-QAM would also have worked.

## Fountain code

### What it has to do

The listener may start at any time and loses unknown frames. So the sender cannot rely on any particular
packet arriving; it sends an endless sequence in which packet `n` is the XOR of a set of source packets
determined by `n` alone.

### Decoding: Gaussian elimination, not peeling

LT and Raptor codes use sparse combinations so that a linear-time "peeling" decoder works for hundreds of
thousands of source packets, and pay for it with a few percent of overhead that only becomes small at that
scale. Here K is about 85 to 200. At that size the decoder can afford full Gaussian elimination over GF(2)
(kept incrementally in reduced row-echelon form, one row per pivot), and then dense random combinations are
the better code: K source packets are recovered from K plus about 1.6 received packets on average, whatever
K is (unit test `flat_fountain_needs_about_two_extra_packets`).

A source packet counts as known when its row has a single 1. The usable stream is the run of known packets
from the start.

### Unequal protection, and what it costs

Expanding windows (Sejdinovic, Vukobratovic, Doufexi, Senk, Piechocki, 2009): window `i` is the first
`k_i` source packets; each coded packet belongs to one window, on a fixed 50-packet schedule spread evenly.
Six windows, with shares 34%, 16%, 14%, 10%, 10%, 16% of the packets and sizes 4.5%, 10%, 22%, 36%, 56%,
100% of K. Where a window adds eight packets or fewer to the one before, those packets are sent uncoded in
turn instead of as random mixes: the 1.6 extra packets would be a third of such a layer.

There is a hard limit on what any scheme of this kind can do. If the first `d_1` packets must be decodable
from *any* `t_1` seconds of listening, a fraction `d_1 / (R·t_1)` of every second must be about them
(`R` = packets per second). The same holds for each further layer, and the fractions cannot add to more
than one:

```
d_1/t_1 + d_2/t_2 + ... ≤ R
```

A plain file transfer puts everything in the last term and delivers `R·T` packets at time `T` and nothing
before. Spending a third of the airtime on a layer that completes in 5 s buys a picture at 5 s and returns
only 5/75 of that airtime's worth at 75 s. With the shares above, K = 0.34 × (packets heard in 75 s): the
windowed stream is about a third the size of the plain file. On the "fair" channel that is 26.4 dB against
31.1 dB at 75 s, the price of 19.1 dB at 5 s instead of nothing.

The shares are a choice, not an optimum. More weight on the last window moves the 75 s number up and every
earlier number down.

### Schedule check

`windows_decode_in_order_and_on_schedule`: for K = 85 with no loss and 40 random start points, the median
packet count at which each window completed was 11, 29, 77, 131, 176, 237, against 12, 48, 96, 150, 204,
252 packets heard after 5, 15, 30, 45, 60, 75 s. Random combinations occasionally need several packets
more than the minimum, so a window is late in up to a fifth of runs.

## Image codec

- RGB → YCbCr (BT.601), 9/7 wavelet by lifting, up to five levels, symmetric extension for any size. Subbands
  are scaled so the transform is close to orthonormal (unit test: a unit coefficient in any subband
  synthesises to energy 0.7-1.45), which lets all subbands share one bit-plane order.
- Bit-planes from the most significant down. Per plane and subband, a quadtree says where coefficients first
  become significant (a node is coded only until it becomes significant; the last child of a newly
  significant node is implied if its siblings were not), then already-significant coefficients get one
  more bit.
- Everything is coded with an adaptive binary range coder (the LZMA construction) with contexts from the
  eight neighbours' significance and signs. Probabilities are two exponential averages (fast and slow)
  with 16-bit precision, because most decisions in the top planes are "still zero" with probability well
  above 0.99.

### Hard problem 5: making every prefix decode

The decoder must stop exactly where its bytes stop being real. The range decoder holds a 32-bit window into
the stream; a bit decided while that window contains only real bytes equals what the encoder wrote. When the
decoder needs a byte that is not there it raises a flag and the codec unwinds without using another bit
(`every_prefix_decodes_to_a_prefix`, `every_single_byte_prefix_decodes`).

The subtle half is the encoder. A range coder ends by flushing a few bytes, and a decoder reading those
would decode symbols the encoder never wrote. The encoder therefore codes 16 bytes past its budget and
truncates: the output is then a genuine prefix of a longer stream and the decoder's rule covers it
(`a_prefix_equals_a_smaller_budget` checks that cutting a long stream equals asking for a short one).

Size: 24 kB for kodim23 (768x512) gives 38.2 dB RGB PSNR; encode about 30 ms, decode about 12 ms.

Colour is not subsampled and chroma is not down-weighted. The headline metric is PSNR over RGB, for which
the three components matter about equally; a perceptual weighting would raise SSIM on luma and lower PSNR.

## File formats

- `wav.rs`: PCM 8/16/24/32-bit and float, WAVE_FORMAT_EXTENSIBLE, first channel only.
- `deflate.rs`: inflate (stored, fixed, dynamic) and a deflate with hash-chain LZ77 and dynamic Huffman
  blocks (length-limited by flattening frequencies until the tree fits). `png.rs`: all colour types at 8/16
  bits, no interlace; writes RGB with per-row filter choice.
- Tested against streams produced by zlib, and fuzzed with bit flips and truncation.

## Simulated channel

`channel.rs`, in order: band-pass FIR; convolution with a synthetic room response (unit direct path, then
from 1.5 ms exponentially decaying Gaussian noise with the stated RT60 and direct-to-reverberant ratio:
Polack's model; a unit test recovers the RT60 by Schroeder integration); resampling for clock offset; white
noise; clicks (3 ms bursts); deleted stretches of audio; clipping.

SNR is signal power over the noise power inside the modem's 6 kHz band. For a flat channel it equals Es/N0
per carrier.

Not modelled: loudspeaker non-linearity, automatic gain control and noise suppression in phone recorders,
lossy audio compression, movement, coloured noise. Any of these can dominate in practice.

## Analogue baseline: SSTV Robot 36

The obvious question about a digital picture link over sound is whether it beats the analogue way radio
amateurs have done it for decades. `sstv.rs` is that baseline: a Robot 36 encoder and decoder, run through the
same simulated channels, with the same seeds and start times, by the same report. The numbers in this section
were measured on a cloud Linux VM with four vCPUs (`make report`, `make sstv-check`).

### Why Robot 36

Robot 36 and Martin M1 are both among the most used modes and both are supported by every mainstream SSTV
program and by the two independent implementations used below. Robot 36 takes 36.91 s per picture with its
header; Martin M1 takes 256 lines of 446.446 ms (pySSTV's constants), 114.3 s, longer than the whole 75 s this
project is measured over, so at 75 s a Martin M1 listener has never seen a complete picture. Robot 36 fits
twice into the window and gives a natural comparison point, 36.9 s, that is added to every table. Its colour
(luma plus alternating R-Y and B-Y at half the vertical rate) is also the closest to how the chirpix codec
handles colour. The price is resolution: 320x240 with chroma shared by line pairs.

### Signal, and where it comes from

Timing and header are those of J. L. Barber, "Proposal for SSTV Mode Specifications" (Dayton SSTV forum,
2000), which documents the Robot 1200C modes. The hosts that serve that paper were not reachable from the VM
(its network policy blocked them), so every number was checked against two implementations whose sources
were: pySSTV 0.5.9 (`pysstv/color.py`, `pysstv/sstv.py`) and SSTVEncoder2 (`Modes/Robot36.java`,
`Modes/Mode.java`, `ImageFormats/YuvConverter.java`, which has the studio-swing colour equations used here).
They agree on the timing below.

```
header (910 ms): 1900 Hz 300 ms | 1200 Hz 10 ms | 1900 Hz 300 ms | start bit 1200 Hz 30 ms |
                 7 bits LSB first + even parity, 1100 Hz = 1, 1300 Hz = 0, 30 ms each | stop bit 1200 Hz 30 ms
                 Robot 36 is code 8
line (150 ms):   sync 1200 Hz 9 ms | porch 1500 Hz 3 ms | Y, 320 px, 88 ms |
                 separator 4.5 ms: 1500 Hz on even lines, 2300 Hz on odd | porch 1900 Hz 1.5 ms |
                 chroma, 320 px, 44 ms: R-Y on even lines, B-Y on odd
value v (0-255) is sent as 1500 + 800 v / 255 Hz; 240 lines; header + picture = 36.91 s
```

The two encoders disagree about colour. Barber's equations are BT.601 with studio swing (Y 16-235, chroma
16-240); pySSTV uses Pillow's full-range YCbCr (Y 0-255), truncated to integers, and sends each line's own
chroma where SSTVEncoder2 averages each pair of lines. A receiver has to assume one; the wrong one costs
contrast and colour. `sstv.rs` follows Barber by default (`--convention spec`) and can do what pySSTV does
(`--convention pysstv`). The independent decoder used below assumes full range.

### Checked against independent implementations

`make sstv-check` installs pySSTV 0.5.9, the `sstv` 0.2.0 package (Python bindings to a Rust SSTV codec) and
Pillow 12.3.0 into a virtual environment under `out/`, makes fixtures with them and runs
`tests/sstv_crosscheck.rs`. CI runs it on Linux. On two built-in test pictures at 320x240:

- **Encoder against pySSTV, sample by sample.** Given Pillow's own YCbCr values, our encoder's 1 771 680
  samples differ from pySSTV's by at most one 16-bit step (pySSTV truncates; 1.002 steps in the f32
  comparison). Same length, same segment boundaries, same phase. Our float version of Pillow's colour
  conversion is within one level of it (one level off in 3 765 and 4 564 of 76 800 pixels).
- **Our decoder on pySSTV audio** (scene / clouds): 30.8 / 35.2 dB with light smoothing (1 and 4 pixel
  widths), 25.7 / 38.3 dB with the smoothing the receiver picks itself, against 28.0 / 32.6 dB for the `sstv`
  package on the same audio. Assuming studio swing instead costs 3.6 / 7.4 dB.
- **The `sstv` package on our audio:** 28.3 / 32.2 dB for `--convention pysstv` (pySSTV's own audio: 28.0 /
  32.6). For our default studio-swing audio it shows 26.1 / 26.8 dB; reading its Y, B-Y, R-Y back out and
  through the studio-swing equations gives 28.1 / 31.7 dB.
- **Either encoder through the simulated room**, decoded by us: within 0.15 dB of each other on "good" and
  "fair".

The decoders were compared only on clean audio: neither independent implementation has a channel model.

### Receiver

FM discriminator on a complex baseband (1900 Hz centre, ±1600 Hz band): a pixel's frequency is the angle of
the sum of `z[k] conj(z[k-1])` over its time, which weights each sample by its power. Then:

1. **Header.** Tone powers at 1100, 1200, 1300 and 1900 Hz in 1 ms bins. A header needs both 300 ms leaders
   (24 of 29 10-ms windows mostly 1900 Hz) and some 1200 Hz in the start and stop bits; the VIS bits are
   decoded but a bad code is not fatal (the receiver is set to Robot 36).
2. **Line sync.** The share of power that is a steady 1200 Hz tone, over 9 ms windows at every sample; peaks
   with a neighbour one or two lines away are kept, chained into runs on one straight time line, and each run
   fitted (the slope is the clock offset: −68 ppm measured for −70, +133 for +130).
3. **Rows.** Counted from a header heard before the line (`--placement vis`, what SSTV programs do) or, for
   lines heard before any header, back from the next one (`--placement buffered`). Between runs, a line takes
   its time from the nearest run; a receiver that loses sync free-runs.
4. **Pixels**, averaged over a window that grows with the noise (below).

### Hard problems

**Echo.** Through the "fair" room the picture is barely recognisable, and smoothing only partly helps
(one picture of kodim23: 13.1 dB measuring each pixel on its own, 17.9 dB smoothed; with no channel 28.7 /
26.1 dB). An FM discriminator follows the sum of the direct sound and its echo; with the echo a third as
strong (DRR 5 dB) and at other pixel frequencies, every pixel is pulled towards its neighbours' recent past,
and the beat between them throws the frequency about. Nothing in SSTV can tell the two apart: there is no
training signal and no guard time. The "fair" room (14 dB of noise plus the echo) leaves SSTV where 4 dB of
noise alone would: 16.9 dB at 75 s, against 17.0 dB with white noise at 4 dB and no echo. This is the main
reason SSTV loses here, and it is a property of the simulated rooms, which are the same for both.

**Noise and smoothing.** The frequency error of a discriminator measured over a time T falls as T^-1.5, so
averaging over neighbouring pixels helps a lot, at the cost of sharpness. The receiver measures the rms
frequency error over one Y pixel inside the sync pulses (a known 1200 Hz tone): 0 Hz with no channel, about
116, 242 and 444 Hz on "good", "fair" and "poor". It then averages each Y pixel over `clamp(noise / 16 Hz,
4, 16)` pixel widths and each chroma pixel over four times as many. The rule was fitted to the built-in test
pictures for the best PSNR against the full-size original, and errs towards blur on clean audio (kodim23 with
no channel: 26.1 dB, against 28.7 dB unsmoothed). Smoothing this heavily (16 pixels is 4.4 ms) is generous
to SSTV under PSNR; a person might prefer the sharper, noisier picture.

**Counting lines across dropouts.** A dropout deletes 50 ms, a third of a line. The run of sync pulses after
it starts on a new time line, and the receiver counts the lines across the gap by its length. Two dropouts
in one gap (100 ms) make the count one short, and every later row lands one row off. The even/odd separator
tones exist for this: two runs that disagree about which lines are odd are one line apart from where the
count put them. With that correction, a dropout costs the line it hits and the line that shares its chroma
(unit test: no more than two line pairs per dropout, at 30 dropouts a minute).

**Finding the header in a room.** At DRR 0 dB a room's response has deep notches. One simulated "poor" room
put 1200 Hz 15 dB below 1900 Hz, so the start bit drowned in the echo of the 1900 Hz leader and no header was
ever found: no picture in 2 of 32 runs. Echo also made the header pattern fit at a second offset 45 ms later.
Now both leaders are required, the start and stop bits need only 15% of the power, and candidates within half
a second are one header. Over 48 recordings (four photographs, ideal to poor channels, three seeds each) all
192 headers are found and no false one.

### Results

The table in the README and the report's "Analogue baseline" section. In short: chirpix is better in all three
rooms at every time, by 5.9 dB at 36.9 s and 9.5 dB at 75 s on "fair". SSTV has no threshold: in white noise
below the modem's (between 4 and 2 dB) it still gives a picture, 15.7 dB at 2 dB and 14.2 dB at 0 dB, where
chirpix gives nothing. It has no start-up delay for a listener who is there when a picture starts, but a
partial picture from the top is worth little under PSNR (12.6 dB after 5 s, against 19.1 dB for chirpix's
whole coarse picture), and a listener who joins mid-picture waits for the next header unless the receiver
buffers.

Equal average power is the comparison here. SSTV's constant envelope (peak 1.4 x RMS against 3.4 x for the
OFDM signal) would let it be played about 7.6 dB louder for the same peak level, which matters if the
loudspeaker, not the room noise, is the limit. That case was not measured.

### Left out

- Other modes (Martin, Scottie, PD). PD modes send luma twice per chroma pair and are used for higher
  resolution; at these SNRs resolution is not what limits SSTV.
- Averaging repeated pictures of a beacon (3 dB per doubling, in principle). SSTV programs do not do it.
- Starting without a header, from the line sync alone (some programs can). Without a header the receiver
  does not know which row it is on.
- A noise blanker for the clicks: tried, +0.05 dB, removed.

## Considered and left out

- **LDPC instead of the convolutional code.** Roughly 2 dB better at these block sizes. The convolutional
  code was chosen because its decoder can be checked against a brute-force maximum-likelihood search and
  a published bound.
- **Smoothing the channel estimate across carriers** (or in the delay domain). Would recover most of the
  remaining implementation loss when echo is short; biased when it is long, which is the case the long
  prefix exists for.
- **Sequence number inside each packet instead of a frame header.** Would let a receiver use a frame whose
  header was lost; costs 4 bytes in 107 and the header is already far more robust than the payload.
- **Re-synchronising inside a frame after a dropout.** A deleted stretch of audio shifts everything after
  it; the rest of that frame is lost (the dropout sweep shows it: at 30 dropouts a minute only a third of
  packets arrive).
- **A streaming receiver**, an ultrasonic band, adaptive bit loading per carrier: future work.
