//! The acoustic OFDM modem: packets in, audio samples out, and back.
//!
//! One frame (0.976 s at 48 kHz):
//!
//! ```text
//! | chirp | CP | training | training | header x2 | 32 data symbols (4 code blocks x 8) |
//!   1024   256    1024       1024      2 x 1280            32 x 1280        samples
//! ```
//!
//! Every frame is self-contained: its own preamble for detection and
//! timing, its own channel estimate, its own header. A receiver can start
//! anywhere and loses at most the frame it started in the middle of.

use crate::conv;
use crate::fft::{Cpx, Fft};
use crate::fountain::T;
use crate::util::{crc16, crc32, Rng};

pub const FS: u32 = 48_000;
pub const NFFT: usize = 1024;
pub const CP: usize = 256;
pub const SYM: usize = NFFT + CP;
/// First used FFT bin (1031 Hz); carriers are bins 22..=149 (to 6984 Hz).
pub const FIRST_BIN: usize = 22;
pub const NCAR: usize = 128;
pub const PILOT_STEP: usize = 8;
pub const NPILOT: usize = NCAR / PILOT_STEP;
pub const NDATA: usize = NCAR - NPILOT;
pub const CHIRP_LEN: usize = 1024;
pub const TRAIN_LEN: usize = CP + 2 * NFFT;
pub const HDR_SYMS: usize = 2;
pub const BLOCK_SYMS: usize = 8;
pub const BLOCKS: usize = 4;
pub const DATA_SYMS: usize = BLOCK_SYMS * BLOCKS;
pub const FRAME_LEN: usize = CHIRP_LEN + TRAIN_LEN + (HDR_SYMS + DATA_SYMS) * SYM;
/// The FFT window starts this many samples before the nominal symbol
/// start, so a slightly late timing estimate or a weak early arrival
/// still falls inside the cyclic prefix.
pub const BACKOFF: usize = 48;
/// Amplitude of one carrier in the time-domain signal.
pub const CARRIER_AMP: f64 = 0.25 * 0.125; // RMS 0.25 with 128 carriers: 0.25 * sqrt(2/128)
pub const CLIP: f32 = 0.85;
const HDR_BYTES: usize = 8;
const HDR_INFO_BITS: usize = NCAR - conv::TAIL; // 122
const PKT_BYTES: usize = T + 4;
const DETECT_THRESHOLD: f64 = 0.15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constellation {
    Qpsk = 0,
    Qam16 = 1,
}

impl Constellation {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Constellation::Qpsk),
            1 => Some(Constellation::Qam16),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Constellation::Qpsk => "QPSK",
            Constellation::Qam16 => "16-QAM",
        }
    }
    pub fn bits(self) -> usize {
        match self {
            Constellation::Qpsk => 2,
            Constellation::Qam16 => 4,
        }
    }
    pub fn packets_per_block(self) -> usize {
        self.bits() / 2
    }
    pub fn packets_per_frame(self) -> usize {
        self.packets_per_block() * BLOCKS
    }
    pub fn coded_bits_per_block(self) -> usize {
        BLOCK_SYMS * NDATA * self.bits()
    }
    pub fn info_bits_per_block(self) -> usize {
        self.coded_bits_per_block() / 2 - conv::TAIL
    }
    /// Payload bytes per second of transmission.
    pub fn byte_rate(self) -> f64 {
        (self.packets_per_frame() * T) as f64 * FS as f64 / FRAME_LEN as f64
    }

    /// Gray-mapped point with unit average energy.
    pub fn map(self, b: &[u8]) -> Cpx {
        match self {
            Constellation::Qpsk => {
                let s = std::f64::consts::FRAC_1_SQRT_2;
                Cpx::new(s * (1.0 - 2.0 * b[0] as f64), s * (1.0 - 2.0 * b[1] as f64))
            }
            Constellation::Qam16 => {
                let lvl = |sign: u8, inner: u8| -> f64 {
                    let m = if inner == 1 { 1.0 } else { 3.0 };
                    (if sign == 1 { -m } else { m }) / 10f64.sqrt()
                };
                Cpx::new(lvl(b[0], b[1]), lvl(b[2], b[3]))
            }
        }
    }

    /// Soft bits (max-log LLR, positive = bit 0) for an equalised point
    /// `z` whose noise variance is 1/`w`.
    pub fn llr(self, z: Cpx, w: f64, out: &mut Vec<f32>) {
        match self {
            Constellation::Qpsk => {
                let k = 2.0 * std::f64::consts::SQRT_2 * w;
                out.push((k * z.re) as f32);
                out.push((k * z.im) as f32);
            }
            Constellation::Qam16 => {
                let inv = w / 5.0;
                for v in [z.re, z.im] {
                    let x = v * 10f64.sqrt();
                    let l0 = if x.abs() <= 2.0 { 2.0 * x } else { 4.0 * (x - x.signum()) };
                    out.push((l0 * inv) as f32);
                    out.push((2.0 * (x.abs() - 2.0) * inv) as f32);
                }
            }
        }
    }
}

/// What the frame header carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSpec {
    pub cons: Constellation,
    /// Packet scheduling scheme (see `fountain::Scheme`).
    pub scheme: u8,
    /// Identifies one transmission so packets of two pictures never mix.
    pub session: u8,
    pub counter: u32,
    /// Number of source packets.
    pub k: u16,
}

impl FrameSpec {
    fn header_bytes(&self) -> [u8; HDR_BYTES + 2] {
        let mut b = [0u8; HDR_BYTES + 2];
        b[0] = 0x50 | ((self.cons as u8) << 2) | (self.scheme & 3);
        b[1] = self.session;
        b[2..5].copy_from_slice(&self.counter.to_be_bytes()[1..]);
        b[5..7].copy_from_slice(&self.k.to_be_bytes());
        let c = crc16(&b[..HDR_BYTES]);
        b[HDR_BYTES..].copy_from_slice(&c.to_be_bytes());
        b
    }

    fn parse(b: &[u8]) -> Option<FrameSpec> {
        if crc16(&b[..HDR_BYTES]) != u16::from_be_bytes([b[HDR_BYTES], b[HDR_BYTES + 1]]) || b[0] >> 4 != 5 || b[7] != 0 {
            return None;
        }
        let k = u16::from_be_bytes([b[5], b[6]]);
        if k == 0 {
            return None;
        }
        Some(FrameSpec {
            cons: Constellation::from_u8((b[0] >> 2) & 3)?,
            scheme: b[0] & 3,
            session: b[1],
            counter: u32::from_be_bytes([0, b[2], b[3], b[4]]),
            k,
        })
    }

    pub fn packet_id(&self, index: usize) -> u32 {
        self.counter * self.cons.packets_per_frame() as u32 + index as u32
    }

    /// CRC over the payload and everything that identifies the packet, so
    /// a packet attributed to the wrong sequence number never passes.
    fn packet_crc(&self, id: u32, payload: &[u8]) -> u32 {
        let mut v = payload.to_vec();
        v.extend_from_slice(&id.to_be_bytes());
        v.extend_from_slice(&[self.session, self.scheme, (self.k >> 8) as u8, self.k as u8]);
        crc32(&v)
    }
}

fn bytes_to_bits(bytes: &[u8], out: &mut Vec<u8>) {
    for &b in bytes {
        for i in (0..8).rev() {
            out.push((b >> i) & 1);
        }
    }
}

fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
    bits.chunks_exact(8).map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b)).collect()
}

fn permutation(n: usize) -> Vec<usize> {
    let mut rng = Rng::new(0x1A7E + n as u64);
    let mut p: Vec<usize> = (0..n).collect();
    for i in (1..n).rev() {
        p.swap(i, rng.below(i + 1));
    }
    p
}

fn is_pilot(c: usize) -> bool {
    c % PILOT_STEP == PILOT_STEP / 2
}

/// Precomputed tables shared by transmitter and receiver.
pub struct Modem {
    fft: Fft,
    pub chirp: Vec<f32>,
    chirp_spec_conj: Vec<Cpx>,
    chirp_energy: f64,
    train_car: Vec<Cpx>,
    perm_hdr: Vec<usize>,
    perm_qpsk: Vec<usize>,
    perm_qam: Vec<usize>,
    /// Pilot signs, [symbol][pilot].
    pilot: Vec<[f64; NPILOT]>,
    hdr_sign: Vec<f64>,
    scramble: Vec<u8>,
}

const DETECT_FFT: usize = 1 << 15;

pub fn modem() -> &'static Modem {
    static M: std::sync::OnceLock<Modem> = std::sync::OnceLock::new();
    M.get_or_init(Modem::new)
}

impl Modem {
    fn new() -> Modem {
        let fft = Fft::new(NFFT);
        let (f0, f1) = (FIRST_BIN as f64 * FS as f64 / NFFT as f64, (FIRST_BIN + NCAR) as f64 * FS as f64 / NFFT as f64);
        let mut chirp = Vec::with_capacity(CHIRP_LEN);
        let big = Fft::new(DETECT_FFT);
        let mut spec = vec![Cpx::ZERO; DETECT_FFT];
        let mut energy = 0.0;
        for (n, s) in spec.iter_mut().enumerate().take(CHIRP_LEN) {
            let t = n as f64 / FS as f64;
            let dur = CHIRP_LEN as f64 / FS as f64;
            let phase = std::f64::consts::TAU * (f0 * t + 0.5 * (f1 - f0) * t * t / dur);
            let edge = 64.0;
            let m = (n as f64 + 0.5).min(CHIRP_LEN as f64 - n as f64 - 0.5);
            let win = if m < edge { 0.5 - 0.5 * (std::f64::consts::PI * m / edge).cos() } else { 1.0 };
            chirp.push((0.25 * std::f64::consts::SQRT_2 * win * phase.sin()) as f32);
            // Analytic template: the correlation magnitude is then the envelope.
            *s = Cpx::new(win * phase.sin(), -win * phase.cos());
            energy += win * win / 2.0;
        }
        big.forward(&mut spec);
        let chirp_spec_conj = spec.iter().map(|v| v.conj()).collect();
        // Newman phases: a flat-spectrum training symbol with low peak factor.
        let train_car = (0..NCAR).map(|c| Cpx::expj(std::f64::consts::PI * (c * c) as f64 / NCAR as f64)).collect();
        let mut rng = Rng::new(0x9117);
        let pilot = (0..DATA_SYMS)
            .map(|_| {
                let mut p = [1.0; NPILOT];
                for v in p.iter_mut() {
                    *v = if rng.bit() == 1 { -1.0 } else { 1.0 };
                }
                p
            })
            .collect();
        let hdr_sign = (0..NCAR).map(|_| if rng.bit() == 1 { -1.0 } else { 1.0 }).collect();
        let scramble = (0..BLOCKS * 2 * PKT_BYTES).map(|_| rng.next_u64() as u8).collect();
        Modem {
            fft,
            chirp,
            chirp_spec_conj,
            chirp_energy: energy,
            train_car,
            perm_hdr: permutation(2 * NCAR),
            perm_qpsk: permutation(Constellation::Qpsk.coded_bits_per_block()),
            perm_qam: permutation(Constellation::Qam16.coded_bits_per_block()),
            pilot,
            hdr_sign,
            scramble,
        }
    }

    fn perm(&self, cons: Constellation) -> &[usize] {
        match cons {
            Constellation::Qpsk => &self.perm_qpsk,
            Constellation::Qam16 => &self.perm_qam,
        }
    }

    /// One OFDM symbol body (no cyclic prefix) from 128 carrier values.
    fn synth(&self, car: &[Cpx]) -> Vec<f32> {
        let mut x = vec![Cpx::ZERO; NFFT];
        for (c, &v) in car.iter().enumerate() {
            x[FIRST_BIN + c] = v;
            x[NFFT - FIRST_BIN - c] = v.conj();
        }
        self.fft.inverse(&mut x);
        let g = CARRIER_AMP * NFFT as f64 / 2.0;
        x.iter().map(|v| (v.re * g) as f32).collect()
    }

    fn push_symbol(&self, out: &mut Vec<f32>, car: &[Cpx]) {
        let body = self.synth(car);
        out.extend_from_slice(&body[NFFT - CP..]);
        out.extend_from_slice(&body);
    }

    fn header_carriers(&self, spec: &FrameSpec) -> Vec<Cpx> {
        let mut bits = Vec::with_capacity(HDR_INFO_BITS);
        bytes_to_bits(&spec.header_bytes(), &mut bits);
        bits.resize(HDR_INFO_BITS, 0);
        let coded = conv::encode(&bits);
        let mut tx = vec![0u8; coded.len()];
        for (i, &b) in coded.iter().enumerate() {
            tx[self.perm_hdr[i]] = b;
        }
        tx.chunks_exact(2).map(|b| Constellation::Qpsk.map(b)).collect()
    }

    /// Information bits of code block `b`: its packets, scrambled, each
    /// followed by a CRC-32, zero-padded to the block size.
    fn block_info_bits(&self, spec: &FrameSpec, b: usize, packets: &[[u8; T]]) -> Vec<u8> {
        let ppb = spec.cons.packets_per_block();
        let mut bytes = Vec::with_capacity(ppb * PKT_BYTES);
        for j in 0..ppb {
            let idx = b * ppb + j;
            let crc = spec.packet_crc(spec.packet_id(idx), &packets[idx]);
            bytes.extend_from_slice(&packets[idx]);
            bytes.extend_from_slice(&crc.to_be_bytes());
        }
        for (i, v) in bytes.iter_mut().enumerate() {
            *v ^= self.scramble[b * 2 * PKT_BYTES + i];
        }
        let mut bits = Vec::with_capacity(spec.cons.info_bits_per_block());
        bytes_to_bits(&bytes, &mut bits);
        bits.resize(spec.cons.info_bits_per_block(), 0);
        bits
    }

    /// Coded, interleaved bits of a block in transmission order.
    pub fn block_coded_bits(&self, cons: Constellation, info: &[u8]) -> Vec<u8> {
        let coded = conv::encode(info);
        let perm = self.perm(cons);
        let mut tx = vec![0u8; coded.len()];
        for (i, &b) in coded.iter().enumerate() {
            tx[perm[i]] = b;
        }
        tx
    }

    /// Data-carrier values for a block: BLOCK_SYMS x NDATA points.
    fn block_points(&self, cons: Constellation, info: &[u8]) -> Vec<Cpx> {
        self.block_coded_bits(cons, info).chunks_exact(cons.bits()).map(|b| cons.map(b)).collect()
    }

    /// The transmitted information and coded bits of every block of a
    /// frame, for error-rate measurements.
    pub fn frame_bits(&self, spec: &FrameSpec, packets: &[[u8; T]]) -> Vec<(Vec<u8>, Vec<u8>)> {
        (0..BLOCKS)
            .map(|b| {
                let info = self.block_info_bits(spec, b, packets);
                let coded = self.block_coded_bits(spec.cons, &info);
                (info, coded)
            })
            .collect()
    }

    /// Build the audio for one frame.
    pub fn modulate_frame(&self, spec: &FrameSpec, packets: &[[u8; T]]) -> Vec<f32> {
        assert_eq!(packets.len(), spec.cons.packets_per_frame());
        let mut out = Vec::with_capacity(FRAME_LEN);
        out.extend_from_slice(&self.chirp);
        let train = self.synth(&self.train_car);
        out.extend_from_slice(&train[NFFT - CP..]);
        out.extend_from_slice(&train);
        out.extend_from_slice(&train);
        let hdr = self.header_carriers(spec);
        self.push_symbol(&mut out, &hdr);
        let hdr2: Vec<Cpx> = hdr.iter().zip(&self.hdr_sign).map(|(v, s)| v.scale(*s)).collect();
        self.push_symbol(&mut out, &hdr2);
        for b in 0..BLOCKS {
            let info = self.block_info_bits(spec, b, packets);
            let pts = self.block_points(spec.cons, &info);
            for s in 0..BLOCK_SYMS {
                let car = self.data_symbol_carriers(b * BLOCK_SYMS + s, &pts[s * NDATA..(s + 1) * NDATA]);
                self.push_symbol(&mut out, &car);
            }
        }
        debug_assert_eq!(out.len(), FRAME_LEN);
        for v in out.iter_mut() {
            *v = v.clamp(-CLIP, CLIP);
        }
        out
    }

    fn data_symbol_carriers(&self, sym: usize, data: &[Cpx]) -> Vec<Cpx> {
        let mut car = Vec::with_capacity(NCAR);
        let mut d = 0;
        for c in 0..NCAR {
            if is_pilot(c) {
                car.push(Cpx::new(self.pilot[sym][c / PILOT_STEP], 0.0));
            } else {
                car.push(data[d]);
                d += 1;
            }
        }
        car
    }

    /// FFT of NFFT samples starting at `start`, returning the 128 carriers.
    fn analyse(&self, x: &[f32], start: usize) -> Vec<Cpx> {
        let mut buf: Vec<Cpx> = x[start..start + NFFT].iter().map(|&v| Cpx::new(v as f64, 0.0)).collect();
        self.fft.forward(&mut buf);
        buf[FIRST_BIN..FIRST_BIN + NCAR].to_vec()
    }

    /// Normalised correlation of the recording with the chirp, as
    /// candidate frame starts (sample index of the chirp's first sample).
    fn detect(&self, x: &[f32]) -> Vec<(usize, f64)> {
        if x.len() < CHIRP_LEN {
            return Vec::new();
        }
        let big = Fft::new(DETECT_FFT);
        let hop = DETECT_FFT - CHIRP_LEN;
        let mut cum = Vec::with_capacity(x.len() + 1);
        cum.push(0f64);
        for &v in x {
            cum.push(cum.last().unwrap() + (v as f64) * (v as f64));
        }
        let total = x.len() - CHIRP_LEN + 1;
        let mut rho = vec![0f32; total];
        let mut buf = vec![Cpx::ZERO; DETECT_FFT];
        let mut base = 0;
        while base < total {
            for (i, b) in buf.iter_mut().enumerate() {
                *b = Cpx::new(x.get(base + i).copied().unwrap_or(0.0) as f64, 0.0);
            }
            big.forward(&mut buf);
            for (b, t) in buf.iter_mut().zip(&self.chirp_spec_conj) {
                *b = *b * *t;
            }
            big.inverse(&mut buf);
            for i in 0..hop.min(total - base) {
                let e = cum[base + i + CHIRP_LEN] - cum[base + i];
                rho[base + i] = if e > 1e-9 { (buf[i].abs() / (e * self.chirp_energy).sqrt()) as f32 } else { 0.0 };
            }
            base += hop;
        }
        // Candidates: points above the threshold that are the largest
        // within half a chirp length on either side.
        let mut cands = Vec::new();
        let half = CHIRP_LEN / 2;
        let mut i = 0;
        while i < total {
            if rho[i] as f64 > DETECT_THRESHOLD {
                let (lo, hi) = (i.saturating_sub(half), (i + half + 1).min(total));
                let best = rho[i];
                if rho[lo..i].iter().all(|&v| v < best) && rho[i + 1..hi].iter().all(|&v| v <= best) {
                    // Earliest arrival at least half as strong as the strongest.
                    let first = (i.saturating_sub(160)..i).find(|&k| rho[k] >= 0.5 * best).unwrap_or(i);
                    cands.push((first, best as f64));
                    i += half;
                    continue;
                }
            }
            i += 1;
        }
        cands
    }
}

/// Find the delay (in samples) that best explains the phase ramp of
/// `r[k]` across carriers: maximise Re sum r_k e^{+j 2 pi bin_k d / N}.
fn estimate_delay(r: &[(usize, Cpx)], centre: f64, span: f64) -> f64 {
    let metric = |d: f64| -> f64 {
        r.iter().map(|&(bin, v)| (v * Cpx::expj(std::f64::consts::TAU * bin as f64 * d / NFFT as f64)).re).sum()
    };
    let step = 0.05;
    let n = (span / step).ceil() as i32;
    let (mut best, mut bd) = (f64::MIN, centre);
    for i in -n..=n {
        let d = centre + i as f64 * step;
        let m = metric(d);
        if m > best {
            best = m;
            bd = d;
        }
    }
    // Parabolic refinement around the best grid point.
    let (a, c) = (metric(bd - step), metric(bd + step));
    let denom = a - 2.0 * best + c;
    if denom < -1e-12 {
        bd += 0.5 * step * (a - c) / denom;
    }
    bd
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RxOptions {
    /// Keep per-block bit decisions and constellation points for analysis.
    pub keep_debug: bool,
    /// Skip the decision-directed second pass (for measuring what it buys).
    pub no_refine: bool,
}

#[derive(Clone, Debug)]
pub struct RxPacket {
    pub id: u32,
    pub payload: [u8; T],
    /// Sample index in the recording at which this packet's code block ended.
    pub end_sample: usize,
}

#[derive(Clone, Debug, Default)]
pub struct BlockDebug {
    /// Hard decisions on the coded bits, in transmission order.
    pub coded_hard: Vec<u8>,
    /// Decoded information bits (before the CRC check).
    pub info: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct RxFrame {
    /// Sample index of the chirp start.
    pub start: usize,
    pub corr: f64,
    pub spec: FrameSpec,
    /// Per packet of the frame: did its CRC pass.
    pub packet_ok: Vec<bool>,
    /// Mean per-carrier signal-to-noise ratio in dB, as seen by the decoder.
    pub snr_db: f64,
    /// Sample-clock offset of the recording relative to the transmitter.
    pub ppm: f64,
    pub snr_per_carrier: Vec<f32>,
    pub channel_mag: Vec<f32>,
    pub points: Vec<(f32, f32)>,
    pub blocks: Vec<BlockDebug>,
}

#[derive(Clone, Debug, Default)]
pub struct RxReport {
    pub frames: Vec<RxFrame>,
    pub packets: Vec<RxPacket>,
    /// Chirp detections that did not lead to a valid header.
    pub false_starts: usize,
}

struct FrameFront {
    y1: Vec<Cpx>,
    y2: Vec<Cpx>,
    base: usize,
}

impl Modem {
    /// FFTs of the two training symbols, if they look like a training pair.
    fn front(&self, x: &[f32], pos: usize) -> Option<FrameFront> {
        let nominal = pos + CHIRP_LEN + CP;
        if nominal < BACKOFF || nominal - BACKOFF + 2 * NFFT > x.len() {
            return None;
        }
        let base = nominal - BACKOFF;
        let y1 = self.analyse(x, base);
        let y2 = self.analyse(x, base + NFFT);
        // The two symbols are identical on air; they must agree here.
        let (mut cross, mut pow) = (Cpx::ZERO, 0.0);
        for c in 0..NCAR {
            cross += y2[c] * y1[c].conj();
            pow += 0.5 * (y1[c].norm2() + y2[c].norm2());
        }
        if pow <= 0.0 || cross.abs() / pow < 0.25 {
            return None;
        }
        Some(FrameFront { y1, y2, base })
    }

    /// Channel and noise estimate from the training pair, given the
    /// timing drift `d12` (samples) between its two symbols.
    fn train_estimate(&self, front: &FrameFront, d12: f64) -> (Vec<Cpx>, Vec<f64>) {
        let mut h = Vec::with_capacity(NCAR);
        let mut raw_noise = Vec::with_capacity(NCAR);
        for c in 0..NCAR {
            let y2c = front.y2[c] * Cpx::expj(std::f64::consts::TAU * (FIRST_BIN + c) as f64 * d12 / NFFT as f64);
            h.push((front.y1[c] + y2c).scale(0.5) * self.train_car[c].conj());
            raw_noise.push((front.y1[c] - y2c).norm2() / 2.0);
        }
        // One sample per carrier is a very noisy variance estimate; average neighbours.
        let noise = (0..NCAR)
            .map(|c| {
                let (lo, hi) = (c.saturating_sub(8), (c + 9).min(NCAR));
                (raw_noise[lo..hi].iter().sum::<f64>() / (hi - lo) as f64).max(1e-12)
            })
            .collect();
        (h, noise)
    }

    fn decode_block(&self, spec: &FrameSpec, b: usize, llr_tx: &[f32]) -> (Vec<u8>, Vec<Option<[u8; T]>>) {
        let cons = spec.cons;
        let perm = self.perm(cons);
        let mut llr = vec![0f32; llr_tx.len()];
        for (i, l) in llr.iter_mut().enumerate() {
            *l = llr_tx[perm[i]];
        }
        let info = conv::viterbi(&llr, cons.info_bits_per_block());
        let ppb = cons.packets_per_block();
        let mut bytes = bits_to_bytes(&info[..ppb * PKT_BYTES * 8]);
        for (i, v) in bytes.iter_mut().enumerate() {
            *v ^= self.scramble[b * 2 * PKT_BYTES + i];
        }
        let mut out = Vec::with_capacity(ppb);
        for j in 0..ppb {
            let p = &bytes[j * PKT_BYTES..(j + 1) * PKT_BYTES];
            let id = spec.packet_id(b * ppb + j);
            let crc = u32::from_be_bytes([p[T], p[T + 1], p[T + 2], p[T + 3]]);
            if spec.packet_crc(id, &p[..T]) == crc {
                let mut payload = [0u8; T];
                payload.copy_from_slice(&p[..T]);
                out.push(Some(payload));
            } else {
                out.push(None);
            }
        }
        (info, out)
    }

    /// Demodulate one frame whose chirp starts at `pos`.
    fn demod_frame(&self, x: &[f32], pos: usize, corr: f64, opts: &RxOptions) -> Option<(RxFrame, Vec<RxPacket>)> {
        let front = self.front(x, pos)?;
        let tau = std::f64::consts::TAU;
        let rot = |c: usize, d: f64| Cpx::expj(tau * (FIRST_BIN + c) as f64 * d / NFFT as f64);
        // Offset of a symbol's FFT window from the first training window.
        let off = |sym: usize| 2 * NFFT + CP + sym * SYM;
        let avail = (0..DATA_SYMS).take_while(|&s| front.base + off(HDR_SYMS + s) + NFFT <= x.len()).count();
        let nblocks = avail / BLOCK_SYMS;
        if nblocks == 0 {
            return None;
        }
        let nsym = nblocks * BLOCK_SYMS;
        let mut ys: Vec<Vec<Cpx>> = (0..nsym).map(|s| self.analyse(x, front.base + off(HDR_SYMS + s))).collect();
        // ---- timing drift. The pilots do not depend on the header, so
        // the clock offset is measured first, over the whole frame: a
        // constant offset makes the delay a straight line in time.
        let (h0, noise0) = self.train_estimate(&front, 0.0);
        let mid = NFFT as f64 / 2.0; // h0 is referenced to the middle of the training pair
        let (mut sxx, mut sxy) = (0.0, 0.0);
        let mut prev = 0.0;
        for (s, y) in ys.iter().enumerate() {
            let r: Vec<(usize, Cpx)> = (0..NPILOT)
                .map(|p| {
                    let c = p * PILOT_STEP + PILOT_STEP / 2;
                    (FIRST_BIN + c, (y[c] * h0[c].conj()).scale(self.pilot[s][p] / noise0[c]))
                })
                .collect();
            prev = estimate_delay(&r, prev, if s == 0 { 2.5 } else { 0.75 });
            let t = off(HDR_SYMS + s) as f64 - mid;
            sxx += t * t;
            sxy += t * prev;
        }
        let slope = sxy / sxx;
        let (mut h, mut noise) = self.train_estimate(&front, slope * NFFT as f64);
        for (s, y) in ys.iter_mut().enumerate() {
            let d = slope * off(HDR_SYMS + s) as f64;
            for (c, v) in y.iter_mut().enumerate() {
                *v = *v * rot(c, d);
            }
        }
        // ---- header: two copies, soft-combined
        let mut llr_tx = vec![0f32; 2 * NCAR];
        let mut yh = Vec::with_capacity(HDR_SYMS);
        for hs in 0..HDR_SYMS {
            let mut y = self.analyse(x, front.base + off(hs));
            let d = slope * off(hs) as f64;
            let mut l = Vec::with_capacity(2 * NCAR);
            for c in 0..NCAR {
                y[c] = y[c] * rot(c, d);
                let sign = if hs == 1 { self.hdr_sign[c] } else { 1.0 };
                let hp = h[c].norm2().max(1e-18);
                let z = (y[c] * h[c].conj()).scale(sign / hp);
                Constellation::Qpsk.llr(z, hp / noise[c], &mut l);
            }
            for (a, b) in llr_tx.iter_mut().zip(&l) {
                *a += b;
            }
            yh.push(y);
        }
        let mut llr = vec![0f32; 2 * NCAR];
        for (i, l) in llr.iter_mut().enumerate() {
            *l = llr_tx[self.perm_hdr[i]];
        }
        let hdr_bits = conv::viterbi(&llr, HDR_INFO_BITS);
        let spec = FrameSpec::parse(&bits_to_bytes(&hdr_bits[..(HDR_BYTES + 2) * 8]))?;
        let cons = spec.cons;
        // The header passed its CRC, so its two symbols are now known and
        // double the amount of training.
        let hdr_car = self.header_carriers(&spec);
        let h_train = h.clone();
        for c in 0..NCAR {
            let a = yh[0][c] * hdr_car[c].conj();
            let b = yh[1][c] * hdr_car[c].conj().scale(self.hdr_sign[c]);
            h[c] = (h_train[c].scale(2.0) + a + b).scale(0.25);
        }
        // Effective noise per carrier from the pilot residuals (this also
        // contains channel-estimate error and inter-symbol interference).
        {
            let pv: Vec<f64> = (0..NPILOT)
                .map(|p| {
                    let c = p * PILOT_STEP + PILOT_STEP / 2;
                    ys.iter().enumerate().map(|(s, y)| (y[c] - h[c].scale(self.pilot[s][p])).norm2()).sum::<f64>() / nsym as f64
                })
                .collect();
            for (c, n) in noise.iter_mut().enumerate() {
                let f = (c as f64 - (PILOT_STEP / 2) as f64) / PILOT_STEP as f64;
                let i = (f.floor().max(0.0) as usize).min(NPILOT - 1);
                let j = (i + 1).min(NPILOT - 1);
                let a = (f - i as f64).clamp(0.0, 1.0);
                *n = (pv[i] * (1.0 - a) + pv[j] * a).max(1e-12);
            }
        }
        let ppb = cons.packets_per_block();
        let demod_block = |b: usize, h: &[Cpx], noise: &[f64]| -> (Vec<f32>, Vec<Cpx>) {
            let mut llr_tx = Vec::with_capacity(cons.coded_bits_per_block());
            let mut pts = Vec::with_capacity(BLOCK_SYMS * NDATA);
            for y in &ys[b * BLOCK_SYMS..(b + 1) * BLOCK_SYMS] {
                for c in (0..NCAR).filter(|&c| !is_pilot(c)) {
                    let hp = h[c].norm2().max(1e-18);
                    let z = (y[c] * h[c].conj()).scale(1.0 / hp);
                    cons.llr(z, hp / noise[c], &mut llr_tx);
                    pts.push(z);
                }
            }
            (llr_tx, pts)
        };
        let mut results: Vec<(Vec<u8>, Vec<Option<[u8; T]>>)> = Vec::with_capacity(nblocks);
        let mut debug: Vec<BlockDebug> = Vec::new();
        let mut points = Vec::new();
        for b in 0..nblocks {
            let (llr_tx, pts) = demod_block(b, &h, &noise);
            if opts.keep_debug {
                debug.push(BlockDebug { coded_hard: llr_tx.iter().map(|&l| (l < 0.0) as u8).collect(), info: Vec::new() });
                if b == 0 {
                    points = pts.iter().map(|z| (z.re as f32, z.im as f32)).collect();
                }
            }
            results.push(self.decode_block(&spec, b, &llr_tx));
        }
        // ---- second pass: blocks that passed their CRC are now known
        // symbols; use them as extra training and retry the failed blocks.
        let good: Vec<bool> = results.iter().map(|r| r.1.iter().all(|p| p.is_some())).collect();
        if !opts.no_refine && good.iter().any(|&g| g) && good.iter().any(|&g| !g) {
            let mut num: Vec<Cpx> = h.iter().map(|v| v.scale(4.0)).collect();
            let mut den = vec![4.0f64; NCAR];
            let mut known: Vec<(usize, Vec<Cpx>)> = Vec::new();
            for b in 0..nblocks {
                let pts = if good[b] { Some(self.block_points(cons, &results[b].0)) } else { None };
                for s in 0..BLOCK_SYMS {
                    let sym = b * BLOCK_SYMS + s;
                    let mut car = vec![Cpx::ZERO; NCAR];
                    let mut d = 0;
                    for (c, v) in car.iter_mut().enumerate() {
                        if is_pilot(c) {
                            *v = Cpx::new(self.pilot[sym][c / PILOT_STEP], 0.0);
                        } else {
                            if let Some(p) = &pts {
                                *v = p[s * NDATA + d];
                            }
                            d += 1;
                        }
                    }
                    for c in 0..NCAR {
                        num[c] += ys[sym][c] * car[c].conj();
                        den[c] += car[c].norm2();
                    }
                    known.push((sym, car));
                }
            }
            for c in 0..NCAR {
                h[c] = num[c].scale(1.0 / den[c]);
            }
            let mut res = vec![0f64; NCAR];
            let mut cnt = vec![0f64; NCAR];
            for (sym, car) in &known {
                for c in 0..NCAR {
                    if car[c].norm2() > 0.0 {
                        res[c] += (ys[*sym][c] - h[c] * car[c]).norm2();
                        cnt[c] += 1.0;
                    }
                }
            }
            let per: Vec<f64> = (0..NCAR).map(|c| res[c] / cnt[c].max(1.0)).collect();
            for (c, n) in noise.iter_mut().enumerate() {
                let (lo, hi) = (c.saturating_sub(2), (c + 3).min(NCAR));
                *n = (per[lo..hi].iter().sum::<f64>() / (hi - lo) as f64).max(1e-12);
            }
            for b in (0..nblocks).filter(|&b| !good[b]) {
                let (llr_tx, _) = demod_block(b, &h, &noise);
                let retry = self.decode_block(&spec, b, &llr_tx);
                let better = retry.1.iter().filter(|p| p.is_some()).count() >= results[b].1.iter().filter(|p| p.is_some()).count();
                if better {
                    results[b] = retry;
                }
            }
        }
        let mut packets = Vec::new();
        let mut packet_ok = vec![false; cons.packets_per_frame()];
        for (b, (info, pk)) in results.into_iter().enumerate() {
            if opts.keep_debug {
                debug[b].info = info;
            }
            for (j, p) in pk.into_iter().enumerate() {
                if let Some(payload) = p {
                    packet_ok[b * ppb + j] = true;
                    let end_sample = (front.base + off(HDR_SYMS + (b + 1) * BLOCK_SYMS - 1) + NFFT).min(x.len());
                    packets.push(RxPacket { id: spec.packet_id(b * ppb + j), payload, end_sample });
                }
            }
        }
        let snr_per_carrier: Vec<f32> = (0..NCAR).map(|c| (10.0 * (h[c].norm2() / noise[c]).log10()) as f32).collect();
        let snr_lin = (0..NCAR).map(|c| h[c].norm2() / noise[c]).sum::<f64>() / NCAR as f64;
        let frame = RxFrame {
            start: pos,
            corr,
            spec,
            packet_ok,
            snr_db: 10.0 * snr_lin.log10(),
            ppm: slope * 1e6,
            snr_per_carrier,
            channel_mag: h.iter().map(|v| (v.abs() / (CARRIER_AMP * NFFT as f64 / 2.0)) as f32).collect(),
            points,
            blocks: debug,
        };
        Some((frame, packets))
    }

    /// Find and decode every frame in a 48 kHz recording.
    pub fn demodulate(&self, x: &[f32], opts: &RxOptions) -> RxReport {
        let mut report = RxReport::default();
        let mut busy_until = 0usize;
        for (pos, corr) in self.detect(x) {
            if pos < busy_until {
                continue;
            }
            match self.demod_frame(x, pos, corr, opts) {
                Some((frame, packets)) => {
                    // Nothing else can start before this frame's data ends.
                    busy_until = pos + FRAME_LEN - 2 * CHIRP_LEN;
                    report.frames.push(frame);
                    report.packets.extend(packets);
                }
                None => report.false_starts += 1,
            }
        }
        report
    }
}

/// Receive a recording at any sample rate: convert to 48 kHz, decode,
/// and if the frames show a sample-clock offset, resample to remove it
/// and decode again. Packet times refer to the original recording.
pub fn receive(samples: &[f32], rate: u32, opts: &RxOptions) -> (RxReport, f64) {
    let m = modem();
    let step0 = rate as f64 / FS as f64;
    let converted;
    let x: &[f32] = if rate != FS {
        converted = crate::dsp::resample(samples, step0);
        &converted
    } else {
        samples
    };
    let mut first = m.demodulate(x, opts);
    let mut ppms: Vec<f64> = first.frames.iter().map(|f| f.ppm).collect();
    if ppms.len() < 2 {
        return (first, ppms.first().copied().unwrap_or(0.0));
    }
    ppms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let ppm = ppms[ppms.len() / 2];
    if ppm.abs() < 15.0 {
        return (first, ppm);
    }
    let step = 1.0 + ppm * 1e-6;
    let fixed = crate::dsp::resample(x, step);
    let mut second = m.demodulate(&fixed, opts);
    if second.packets.len() >= first.packets.len() {
        for p in second.packets.iter_mut() {
            p.end_sample = (p.end_sample as f64 * step) as usize;
        }
        for f in second.frames.iter_mut() {
            f.start = (f.start as f64 * step) as usize;
            f.ppm += ppm;
        }
        (second, ppm)
    } else {
        first.false_starts += 0;
        (first, ppm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_packets(n: usize, seed: u64) -> Vec<[u8; T]> {
        let mut rng = Rng::new(seed);
        (0..n)
            .map(|_| {
                let mut p = [0u8; T];
                for v in p.iter_mut() {
                    *v = rng.next_u64() as u8;
                }
                p
            })
            .collect()
    }

    fn spec(cons: Constellation, counter: u32) -> FrameSpec {
        FrameSpec { cons, scheme: 0, session: 0xA7, counter, k: 124 }
    }

    #[test]
    fn frame_layout_and_levels() {
        assert_eq!(FRAME_LEN, 46_848);
        assert_eq!(Constellation::Qpsk.info_bits_per_block(), 890);
        assert_eq!(Constellation::Qam16.info_bits_per_block(), 1786);
        let m = modem();
        let f = m.modulate_frame(&spec(Constellation::Qpsk, 1), &test_packets(4, 1));
        assert_eq!(f.len(), FRAME_LEN);
        let data = &f[CHIRP_LEN + TRAIN_LEN..];
        let rms = (data.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / data.len() as f64).sqrt();
        assert!((rms - 0.25).abs() < 0.01, "data RMS {rms}");
        let clipped = f.iter().filter(|v| v.abs() >= CLIP).count();
        assert!(clipped < FRAME_LEN / 500, "{clipped} clipped samples");
    }

    #[test]
    fn constellations_have_unit_energy_and_consistent_soft_bits() {
        for cons in [Constellation::Qpsk, Constellation::Qam16] {
            let n = cons.bits();
            let mut energy = 0.0;
            for m in 0..(1u32 << n) {
                let bits: Vec<u8> = (0..n).map(|i| ((m >> i) & 1) as u8).collect();
                let z = cons.map(&bits);
                energy += z.norm2();
                let mut l = Vec::new();
                cons.llr(z, 10.0, &mut l);
                for (b, v) in bits.iter().zip(&l) {
                    assert_eq!(*b == 1, *v < 0.0, "{cons:?} point {m}");
                }
            }
            assert!((energy / (1u32 << n) as f64 - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn header_round_trip_and_rejects_corruption() {
        let s = FrameSpec { cons: Constellation::Qam16, scheme: 2, session: 9, counter: 0x12_3456, k: 4000 };
        let b = s.header_bytes();
        assert_eq!(FrameSpec::parse(&b), Some(s));
        for i in 0..b.len() * 8 {
            let mut c = b;
            c[i / 8] ^= 1 << (i % 8);
            assert_eq!(FrameSpec::parse(&c), None, "bit {i}");
        }
    }

    #[test]
    fn clean_loopback_recovers_every_packet() {
        let m = modem();
        for cons in [Constellation::Qpsk, Constellation::Qam16] {
            let mut audio = vec![0f32; 5000];
            let mut sent = Vec::new();
            for counter in 0..3 {
                let sp = spec(cons, counter + 100);
                let pk = test_packets(cons.packets_per_frame(), counter as u64);
                audio.extend(m.modulate_frame(&sp, &pk));
                sent.push((sp, pk));
            }
            audio.extend(vec![0f32; 3000]);
            let rep = m.demodulate(&audio, &RxOptions::default());
            assert_eq!(rep.frames.len(), 3, "{cons:?}");
            assert_eq!(rep.packets.len(), 3 * cons.packets_per_frame());
            for (f, (sp, pk)) in rep.frames.iter().zip(&sent) {
                assert_eq!(f.spec, *sp);
                assert!(f.snr_db > 25.0, "clean channel SNR reads {:.1} dB", f.snr_db);
                assert!(f.ppm.abs() < 3.0);
                for (i, p) in pk.iter().enumerate() {
                    let got = rep.packets.iter().find(|r| r.id == sp.packet_id(i)).unwrap();
                    assert_eq!(&got.payload, p);
                }
            }
        }
    }

    #[test]
    fn a_recording_that_starts_mid_frame_loses_only_that_frame() {
        let m = modem();
        let mut audio = Vec::new();
        for counter in 0..4 {
            audio.extend(m.modulate_frame(&spec(Constellation::Qpsk, counter), &test_packets(4, counter as u64)));
        }
        let rep = m.demodulate(&audio[FRAME_LEN / 2..audio.len() - 7000], &RxOptions::default());
        let counters: Vec<u32> = rep.frames.iter().map(|f| f.spec.counter).collect();
        assert_eq!(counters, vec![1, 2, 3]);
        // The last frame was cut short: its final block is gone, the rest survive.
        assert_eq!(rep.frames[2].packet_ok, vec![true, true, true, false]);
        assert_eq!(rep.packets.len(), 11);
    }

    #[test]
    fn silence_and_noise_produce_no_packets() {
        let m = modem();
        let mut rng = Rng::new(77);
        let noise: Vec<f32> = (0..200_000).map(|_| 0.2 * rng.gauss() as f32).collect();
        assert!(m.demodulate(&noise, &RxOptions::default()).packets.is_empty());
        assert!(m.demodulate(&vec![0f32; 100_000], &RxOptions::default()).packets.is_empty());
        assert!(m.demodulate(&[0.5f32; 10], &RxOptions::default()).packets.is_empty());
    }
}
