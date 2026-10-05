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
- **Analogue SSTV baseline.** Not built. The comparison baselines are digital and share the modem.
- **A streaming receiver**, an ultrasonic band, adaptive bit loading per carrier: future work.
