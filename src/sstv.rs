//! Analogue SSTV baseline: Robot 36 Colour, encoder and decoder.
//!
//! The way radio amateurs have sent pictures over audio channels for
//! decades, put through the same simulated room as the digital schemes.
//!
//! Timing and header follow J. L. Barber, "Proposal for SSTV Mode
//! Specifications" (Dayton SSTV forum, 2000), which documents the Robot
//! 1200C modes. One picture is 320x240 and every line takes 150 ms:
//!
//! ```text
//! | sync 1200 Hz 9 ms | porch 1500 Hz 3 ms | Y, 320 px, 88 ms | separator 4.5 ms | porch 1900 Hz 1.5 ms | chroma, 320 px, 44 ms |
//! ```
//!
//! Even lines (0, 2, ...) carry R-Y after a 1500 Hz separator, odd lines
//! B-Y after a 2300 Hz one, and both lines of a pair are shown with the
//! pair's two chroma lines. A value v in 0..255 is sent as 1500 + 800 v / 255 Hz.
//! The picture is preceded by the VIS header (910 ms): 1900 Hz 300 ms,
//! 1200 Hz 10 ms, 1900 Hz 300 ms, start bit 1200 Hz 30 ms, seven data bits
//! LSB first and an even-parity bit (1100 Hz = 1, 1300 Hz = 0, 30 ms each),
//! stop bit 1200 Hz 30 ms. Robot 36 is code 8. Header and picture: 36.91 s.
//!
//! The same timing is used by pySSTV 0.5.9 and by SSTVEncoder2; the two
//! differ in how they turn RGB into Y, R-Y and B-Y (see [`Convention`]).

use crate::dsp::{filter_same, lowpass, resample};
use crate::fft::Cpx;
use crate::image::Image;
use crate::modem::FS;
use std::f64::consts::TAU;

pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 240;
pub const VIS_CODE: u8 = 8;

const SYNC_MS: f64 = 9.0;
const SYNC_PORCH_MS: f64 = 3.0;
const Y_MS: f64 = 88.0;
const SEP_MS: f64 = 4.5;
const PORCH_MS: f64 = 1.5;
const C_MS: f64 = 44.0;
pub const LINE_MS: f64 = SYNC_MS + SYNC_PORCH_MS + Y_MS + SEP_MS + PORCH_MS + C_MS;
const LEADER_MS: f64 = 300.0;
const BREAK_MS: f64 = 10.0;
const BIT_MS: f64 = 30.0;
pub const VIS_MS: f64 = 2.0 * LEADER_MS + BREAK_MS + 10.0 * BIT_MS;
/// One header and one picture.
pub const FRAME_SECONDS: f64 = (VIS_MS + HEIGHT as f64 * LINE_MS) / 1000.0;
/// Where the parts of a line start, in ms after the start of its sync pulse.
const Y_AT: f64 = SYNC_MS + SYNC_PORCH_MS;
const SEP_AT: f64 = Y_AT + Y_MS;
const C_AT: f64 = SEP_AT + SEP_MS + PORCH_MS;

const F_BIT1: f64 = 1100.0;
const F_SYNC: f64 = 1200.0;
const F_BIT0: f64 = 1300.0;
const F_BLACK: f64 = 1500.0;
const F_LEADER: f64 = 1900.0;
const F_WHITE: f64 = 2300.0;

/// Peak level of the generated audio: the same RMS (0.25 of full scale) as
/// the OFDM signal. A constant-envelope signal could be played louder for
/// the same peak level; the comparison here is at equal average power.
pub const AMP: f32 = 0.353_553_4;

/// How RGB becomes Y, B-Y and R-Y. Encoders in use disagree; a receiver
/// has to assume one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Convention {
    /// The Robot colour equations given by Barber (BT.601 with studio
    /// swing: Y 16-235, chroma 16-240 around 128), chroma averaged over
    /// each pair of lines. SSTVEncoder2 does the same. The default.
    Spec,
    /// What pySSTV 0.5.9 sends: Pillow's full-range YCbCr (JPEG), rounded
    /// to integers, each line sending its own chroma.
    Pysstv,
}

impl Convention {
    pub fn parse(s: &str) -> Option<Convention> {
        match s {
            "spec" => Some(Convention::Spec),
            "pysstv" => Some(Convention::Pysstv),
            _ => None,
        }
    }

    /// Offset and matrix: `[Y, B-Y, R-Y] = off + m * [R, G, B]`.
    fn forward(self) -> ([f64; 3], [[f64; 3]; 3]) {
        match self {
            Convention::Spec => {
                let k = 0.003906;
                (
                    [16.0, 128.0, 128.0],
                    [
                        [65.738 * k, 129.057 * k, 25.064 * k],
                        [-37.945 * k, -74.494 * k, 112.439 * k],
                        [112.439 * k, -94.154 * k, -18.285 * k],
                    ],
                )
            }
            Convention::Pysstv => (
                [0.0, 128.0, 128.0],
                [[0.299, 0.587, 0.114], [-0.168736, -0.331264, 0.5], [0.5, -0.418688, -0.081312]],
            ),
        }
    }

    /// `[Y, B-Y, R-Y]` on the 0-255 scale of the frequency mapping.
    pub fn to_ycc(self, rgb: [f64; 3]) -> [f64; 3] {
        let (off, m) = self.forward();
        let v = [0, 1, 2].map(|i| off[i] + m[i][0] * rgb[0] + m[i][1] * rgb[1] + m[i][2] * rgb[2]);
        match self {
            Convention::Spec => v,
            Convention::Pysstv => v.map(|x| x.round().clamp(0.0, 255.0)),
        }
    }

    pub fn to_rgb(self, ycc: [f64; 3]) -> [f64; 3] {
        let (off, m) = self.forward();
        let inv = invert3(m);
        let d = [ycc[0] - off[0], ycc[1] - off[1], ycc[2] - off[2]];
        [0, 1, 2].map(|i| inv[i][0] * d[0] + inv[i][1] * d[1] + inv[i][2] * d[2])
    }
}

fn invert3(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let c = |r: usize, k: usize| {
        let (r1, r2, k1, k2) = ((r + 1) % 3, (r + 2) % 3, (k + 1) % 3, (k + 2) % 3);
        m[r1][k1] * m[r2][k2] - m[r1][k2] * m[r2][k1]
    };
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    // inverse = transpose of the cofactors / det
    [0, 1, 2].map(|i| [0, 1, 2].map(|j| c(j, i) / det))
}

/// Shrink or stretch a picture to 320x240, as an SSTV sender has to.
pub fn fit(img: &Image) -> Image {
    img.resize(WIDTH, HEIGHT)
}

fn freq_of(v: f64) -> f64 {
    F_BLACK + (F_WHITE - F_BLACK) * v.clamp(0.0, 255.0) / 255.0
}

/// The VIS header for `code`, as (Hz, ms) segments.
pub fn header_tones(code: u8) -> Vec<(f64, f64)> {
    let mut t = vec![(F_LEADER, LEADER_MS), (F_SYNC, BREAK_MS), (F_LEADER, LEADER_MS), (F_SYNC, BIT_MS)];
    let mut ones = 0;
    for i in 0..7 {
        let bit = (code >> i) & 1;
        ones += bit;
        t.push((if bit == 1 { F_BIT1 } else { F_BIT0 }, BIT_MS));
    }
    t.push((if ones % 2 == 1 { F_BIT1 } else { F_BIT0 }, BIT_MS));
    t.push((F_SYNC, BIT_MS));
    t
}

/// Header and picture as (Hz, ms) segments. `pic` must be 320x240.
pub fn frame_tones(pic: &Image, conv: Convention) -> Vec<(f64, f64)> {
    assert_eq!((pic.w, pic.h), (WIDTH, HEIGHT), "fit the picture to 320x240 first");
    let ycc: Vec<[f64; 3]> = pic
        .data
        .chunks_exact(3)
        .map(|p| conv.to_ycc([p[0] as f64, p[1] as f64, p[2] as f64]))
        .collect();
    let mut t = header_tones(VIS_CODE);
    for line in 0..HEIGHT {
        let odd = line % 2 == 1;
        t.push((F_SYNC, SYNC_MS));
        t.push((F_BLACK, SYNC_PORCH_MS));
        for x in 0..WIDTH {
            t.push((freq_of(ycc[line * WIDTH + x][0]), Y_MS / WIDTH as f64));
        }
        t.push((if odd { F_WHITE } else { F_BLACK }, SEP_MS));
        t.push((F_LEADER, PORCH_MS));
        let ch = if odd { 1 } else { 2 };
        for x in 0..WIDTH {
            let v = match conv {
                Convention::Spec => {
                    let pair = line & !1;
                    0.5 * (ycc[pair * WIDTH + x][ch] + ycc[(pair + 1) * WIDTH + x][ch])
                }
                Convention::Pysstv => ycc[line * WIDTH + x][ch],
            };
            t.push((freq_of(v), C_MS / WIDTH as f64));
        }
    }
    t
}

/// Phase-continuous synthesis. Segment boundaries fall on the first sample
/// at or after each segment's nominal start, accumulated without rounding.
pub fn synthesize(tones: &[(f64, f64)], rate: u32, amp: f32) -> Vec<f32> {
    let spms = rate as f64 / 1000.0;
    let total: f64 = tones.iter().map(|t| t.1).sum();
    let mut out = Vec::with_capacity((total * spms) as usize + 1);
    let (mut t_ms, mut phase) = (0.0f64, 0.0f64);
    for &(f, ms) in tones {
        t_ms += ms;
        let end = (t_ms * spms + 1e-6).floor() as usize;
        let w = TAU * f / rate as f64;
        while out.len() < end {
            out.push(amp * phase.sin() as f32);
            phase += w;
        }
        phase %= TAU;
    }
    out
}

/// A beacon sending `pic` (320x240) over and over, header first: the audio
/// from `from` seconds after its first header, `seconds` long, at 48 kHz.
pub fn beacon_audio(pic: &Image, conv: Convention, from: f64, seconds: f64) -> Vec<f32> {
    let one = frame_tones(pic, conv);
    let c0 = (from / FRAME_SECONDS).floor().max(0.0) as usize;
    let c1 = ((from + seconds) / FRAME_SECONDS).ceil() as usize;
    let mut tones = Vec::with_capacity(one.len() * (c1 - c0));
    for _ in c0..c1.max(c0 + 1) {
        tones.extend_from_slice(&one);
    }
    let audio = synthesize(&tones, FS, AMP);
    let a = (((from - c0 as f64 * FRAME_SECONDS) * FS as f64).round() as usize).min(audio.len());
    let b = (a + (seconds * FS as f64).round() as usize).min(audio.len());
    audio[a..b].to_vec()
}

// ---------------------------------------------------------------- receiver

/// Which lines a receiver shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Placement {
    /// As SSTV programs do by default: start at a VIS header and count
    /// lines forward from it. Lines heard before the first header are lost.
    FromVis,
    /// Also keep lines heard before a header and place them, once the
    /// header arrives, by counting back from it (the lines just before a
    /// header are the end of the previous picture, which a beacon repeats).
    Buffered,
}

#[derive(Clone, Debug)]
pub struct Header {
    /// Seconds into the recording at which line 0's sync pulse starts.
    pub line0: f64,
    pub code: u8,
    pub parity_ok: bool,
}

#[derive(Clone)]
pub struct Line {
    /// Seconds into the recording at which the line's sync pulse starts.
    pub time: f64,
    pub row: usize,
    /// When the receiver could know the row: the end of the header it was
    /// counted from.
    pub known_at: f64,
    /// Counted forward from a header heard before the line.
    pub forward: bool,
    /// Carries B-Y (odd line) rather than R-Y.
    pub odd: bool,
    pub y: Vec<f32>,
    pub c: Vec<f32>,
}

pub struct Reception {
    /// Length of the recording in seconds.
    pub duration: f64,
    pub headers: Vec<Header>,
    /// Every line that could be placed, in time order.
    pub lines: Vec<Line>,
    /// Line sync pulses detected and consistent with their neighbours.
    pub pulses: usize,
    /// Median clock offset of the recording against the nominal line rate, ppm.
    pub ppm: f64,
    /// Median rms frequency error over one Y pixel inside the sync pulses.
    pub sync_noise_hz: f64,
    /// Pixel widths over which Y and chroma were measured.
    pub smoothing: (f64, f64),
}

/// Post-detection smoothing for a measured noise level: each Y pixel is
/// measured over `w` pixel widths and each chroma pixel over `4 w`, with
/// `w` from 4 (clean) to 16 (noise of 256 Hz and more). Chosen on the
/// built-in test pictures to maximise PSNR (not on the Kodak photographs).
pub fn auto_smoothing(sync_noise_hz: f64) -> (f64, f64) {
    let w = (sync_noise_hz / 16.0).clamp(4.0, 16.0);
    (w, 4.0 * w)
}

/// Half-width of the receiver's band around 1900 Hz: 300-3500 Hz.
const BASEBAND_HZ: f64 = 1600.0;
const SYNC_THRESHOLD: f64 = 0.3;

/// `e^(j w k)` for k = start, start + 1, ... by recurrence, re-anchored
/// every 4096 steps.
struct Osc {
    w: f64,
    k: usize,
    v: Cpx,
    step: Cpx,
}

impl Osc {
    fn new(w: f64, start: usize) -> Osc {
        Osc {
            w,
            k: start,
            v: Cpx::expj((w * start as f64) % TAU),
            step: Cpx::expj(w),
        }
    }
    fn next(&mut self) -> Cpx {
        let v = self.v;
        self.k += 1;
        self.v = if self.k % 4096 == 0 {
            Cpx::expj((self.w * self.k as f64) % TAU)
        } else {
            self.v * self.step
        };
        v
    }
}

struct Rx {
    z: Vec<Cpx>,
    spms: f64,
}

impl Rx {
    fn new(x: &[f32]) -> Rx {
        let fs = FS as f64;
        let w0 = TAU * F_LEADER / fs;
        let (mut re, mut im) = (vec![0f32; x.len()], vec![0f32; x.len()]);
        for (k, v) in x.iter().enumerate() {
            let ph = (w0 * k as f64) % TAU;
            re[k] = v * ph.cos() as f32;
            im[k] = -v * ph.sin() as f32;
        }
        let h = lowpass(BASEBAND_HZ / fs, 255);
        let (re, im) = (filter_same(&re, &h), filter_same(&im, &h));
        Rx {
            z: re.iter().zip(&im).map(|(a, b)| Cpx::new(*a as f64, *b as f64)).collect(),
            spms: fs / 1000.0,
        }
    }

    /// Amplitude-weighted mean frequency over samples [a, b).
    fn freq(&self, a: f64, b: f64) -> Option<f64> {
        let (a, b) = ((a.ceil().max(1.0)) as usize, (b.ceil() as usize).min(self.z.len()));
        if b <= a {
            return None;
        }
        let mut s = Cpx::default();
        for k in a..b {
            s += self.z[k] * self.z[k - 1].conj();
        }
        Some(F_LEADER + s.arg() * FS as f64 / TAU)
    }

    /// Share of the power in each window that is a steady 1200 Hz tone, for
    /// a window of one sync pulse starting at every sample.
    fn sync_ratio(&self) -> Vec<f32> {
        let z = &self.z;
        let l = (SYNC_MS * self.spms) as usize;
        let n = z.len();
        let mut out = vec![0f32; n];
        if n <= l {
            return out;
        }
        let w = TAU * (F_LEADER - F_SYNC) / FS as f64;
        let floor = 1e-9 * z.iter().map(|v| v.norm2()).sum::<f64>() / n as f64 * l as f64;
        let (mut head, mut tail) = (Osc::new(w, 0), Osc::new(w, 0));
        let (mut s, mut e) = (Cpx::default(), 0.0);
        for k in 0..l {
            s += z[k] * head.next();
            e += z[k].norm2();
        }
        for t in 0..=n - l {
            out[t] = (s.norm2() / (l as f64 * (e.max(0.0) + floor))) as f32;
            let old = z[t] * tail.next();
            if t + l < n {
                s = s + z[t + l] * head.next() - old;
                e += z[t + l].norm2() - z[t].norm2();
            }
        }
        out
    }

    /// VIS headers, by the tone powers of 1 ms bins.
    fn headers(&self) -> Vec<Header> {
        let spm = self.spms as usize;
        let m = self.z.len() / spm;
        let tones = [F_BIT1, F_SYNC, F_BIT0, F_LEADER];
        let mut ps = vec![vec![Cpx::default(); m + 1]; tones.len()];
        let mut pe = vec![0f64; m + 1];
        let mut osc: Vec<Osc> = tones.iter().map(|f| Osc::new(-TAU * (f - F_LEADER) / FS as f64, 0)).collect();
        for b in 0..m {
            let mut s = [Cpx::default(); 4];
            let mut e = 0.0;
            for k in b * spm..(b + 1) * spm {
                e += self.z[k].norm2();
                for i in 0..tones.len() {
                    s[i] += self.z[k] * osc[i].next();
                }
            }
            pe[b + 1] = pe[b] + e;
            for i in 0..tones.len() {
                ps[i][b + 1] = ps[i][b] + s[i];
            }
        }
        let ratio = |i: usize, a: usize, b: usize| -> f64 {
            let e = pe[b] - pe[a];
            if e <= 0.0 {
                return 0.0;
            }
            (ps[i][b] - ps[i][a]).norm2() / ((b - a) as f64 * spm as f64 * e)
        };
        let (bit1, sync, bit0, leader) = (0, 1, 2, 3);
        // tau: the first ms of the start bit.
        let mut found: Vec<(usize, f64)> = Vec::new();
        let lead = LEADER_MS as usize;
        for tau in lead..m.saturating_sub(300) {
            let start = ratio(sync, tau + 2, tau + 28);
            if start < 0.3 || ratio(sync, tau + 272, tau + 298) < 0.2 {
                continue;
            }
            let ok = (0..29)
                .filter(|i| ratio(leader, tau - 295 + 10 * i, tau - 285 + 10 * i) >= 0.4)
                .count();
            if ok < 24 {
                continue;
            }
            // Best alignment: the whole 30 ms bit inside the window.
            let fit = ratio(sync, tau, tau + 30);
            match found.last_mut() {
                Some((t, s)) if tau - *t < 40 => {
                    if fit > *s {
                        *t = tau;
                        *s = fit;
                    }
                }
                _ => found.push((tau, fit)),
            }
        }
        found
            .into_iter()
            .map(|(tau, _)| {
                let bits: Vec<u8> = (0..8)
                    .map(|i| {
                        let (a, b) = (tau + 30 * (i + 1) + 4, tau + 30 * (i + 2) - 4);
                        (ratio(bit1, a, b) > ratio(bit0, a, b)) as u8
                    })
                    .collect();
                let code = (0..7).fold(0u8, |c, i| c | (bits[i] << i));
                Header {
                    line0: (tau as f64 + 10.0 * BIT_MS) / 1000.0,
                    code,
                    parity_ok: bits.iter().map(|&b| b as u32).sum::<u32>() % 2 == 0,
                }
            })
            .collect()
    }

    /// Rms error in Hz of frequencies measured over one Y pixel inside the
    /// sync pulse of the line at `t`, which is a known 1200 Hz tone.
    fn sync_noise(&self, t: f64) -> f64 {
        let p = Y_MS / WIDTH as f64;
        let n = ((SYNC_MS - 3.0) / p) as usize;
        let e: f64 = (0..n)
            .map(|i| {
                let a = t + (2.0 + p * i as f64) * self.spms;
                self.freq(a, a + p * self.spms).map_or(0.0, |f| (f - F_SYNC).powi(2))
            })
            .sum();
        (e / n as f64).sqrt()
    }

    fn demod_line(&self, t: f64, widen: (f64, f64)) -> (Vec<f32>, Vec<f32>, Option<bool>) {
        let s = self.spms;
        // Pixel i of a scan starting at `at` with pixels `p` ms long, measured
        // over a window `widen` pixels wide centred on it.
        let value = |at: f64, p: f64, i: usize, widen: f64| {
            let mid = at + p * (i as f64 + 0.5);
            let half = 0.5 * p * widen;
            let (a, b) = ((mid - half).max(at), (mid + half).min(at + p * WIDTH as f64));
            self.freq(t + a * s, t + b * s)
                .map_or(128.0, |f| ((f - F_BLACK) * 255.0 / 800.0) as f32)
        };
        let y = (0..WIDTH).map(|i| value(Y_AT, Y_MS / WIDTH as f64, i, widen.0)).collect();
        let c = (0..WIDTH).map(|i| value(C_AT, C_MS / WIDTH as f64, i, widen.1)).collect();
        (y, c, self.separator(t))
    }

    /// The separator says whether the line at `t` is odd (2300 Hz) or even
    /// (1500 Hz); None when it is unclear.
    fn separator(&self, t: f64) -> Option<bool> {
        let s = self.spms;
        match self.freq(t + (SEP_AT + 0.5) * s, t + (SEP_AT + SEP_MS - 0.5) * s) {
            Some(f) if f > 2150.0 => Some(true),
            Some(f) if f < 1650.0 => Some(false),
            _ => None,
        }
    }
}

/// A run of sync pulses on one straight time line `t = a + b * idx`.
struct Seg {
    pts: Vec<(i64, f64)>,
}

impl Seg {
    fn fit(&self, period: f64) -> (f64, f64) {
        let n = self.pts.len() as f64;
        let span = self.pts.last().unwrap().0 - self.pts[0].0;
        if self.pts.len() < 3 || span < 4 {
            let (i, t) = self.pts[0];
            return (t - period * i as f64, period);
        }
        let mx = self.pts.iter().map(|p| p.0 as f64).sum::<f64>() / n;
        let my = self.pts.iter().map(|p| p.1).sum::<f64>() / n;
        let sxx: f64 = self.pts.iter().map(|p| (p.0 as f64 - mx).powi(2)).sum();
        let sxy: f64 = self.pts.iter().map(|p| (p.0 as f64 - mx) * (p.1 - my)).sum();
        let b = sxy / sxx;
        (my - b * mx, b)
    }
}

/// Decode a recording at any sample rate, smoothing as [`auto_smoothing`]
/// says for the noise measured on the sync pulses.
pub fn receive(samples: &[f32], rate: u32) -> Reception {
    receive_with(samples, rate, None)
}

/// `smoothing`: Y and chroma pixels are measured over these many pixel
/// widths (1 = each pixel on its own); None chooses by the noise.
pub fn receive_with(samples: &[f32], rate: u32, smoothing: Option<(f64, f64)>) -> Reception {
    let x: Vec<f32> = if rate == FS {
        samples.to_vec()
    } else {
        resample(samples, rate as f64 / FS as f64)
    };
    let rx = Rx::new(&x);
    let spms = rx.spms;
    let n = x.len() as f64;
    let p_nom = LINE_MS * spms;
    let tol = 1.5 * spms;
    let headers = rx.headers();

    // Sync pulse candidates: the best point of every run above threshold.
    let r = rx.sync_ratio();
    let mut cands: Vec<(f64, f32)> = Vec::new();
    let mut t = 0;
    while t < r.len() {
        if (r[t] as f64) < SYNC_THRESHOLD {
            t += 1;
            continue;
        }
        let mut best = t;
        while t < r.len() && (r[t] as f64) >= SYNC_THRESHOLD {
            if r[t] > r[best] {
                best = t;
            }
            t += 1;
        }
        match cands.last_mut() {
            Some(c) if (best as f64 - c.0) < 5.0 * spms => {
                if r[best] > c.1 {
                    *c = (best as f64, r[best]);
                }
            }
            _ => cands.push((best as f64, r[best])),
        }
    }
    // Inside a header (its 1200 Hz parts, and line 0's sync, which runs on
    // from the stop bit) a pulse says nothing about line timing.
    let spans: Vec<(f64, f64)> = headers
        .iter()
        .map(|h| (h.line0 * 1000.0 * spms - VIS_MS * spms - tol, h.line0 * 1000.0 * spms + 5.0 * spms))
        .collect();
    cands.retain(|c| !spans.iter().any(|s| c.0 > s.0 && c.0 < s.1));
    // Keep pulses with a neighbour one or two lines away.
    let times: Vec<f64> = cands.iter().map(|c| c.0).collect();
    let confirmed: Vec<f64> = times
        .iter()
        .copied()
        .filter(|&c| {
            [-2.0, -1.0, 1.0, 2.0].iter().any(|k| {
                let want = c + k * p_nom;
                let i = times.partition_point(|&v| v < want - tol);
                i < times.len() && (times[i] - want).abs() <= tol
            })
        })
        .collect();

    // Regions between headers: [start, end) in samples, with the header (if
    // any) that opens it and the one that closes it.
    let mut bounds: Vec<(f64, f64, Option<usize>, Option<usize>)> = Vec::new();
    let mut start = (0.0, None);
    for (i, h) in headers.iter().enumerate() {
        let l0 = h.line0 * 1000.0 * spms;
        bounds.push((start.0, l0 - VIS_MS * spms, start.1, Some(i)));
        start = (l0, Some(i));
    }
    bounds.push((start.0, n, start.1, None));

    // Clock: slopes of long runs of pulses, for runs too short to fit.
    let mut slopes = Vec::new();
    let mut lines: Vec<Line> = Vec::new();
    let mut plan: Vec<(f64, usize, bool, f64)> = Vec::new(); // (time, row, forward, known_at)
    for pass in 0..2 {
        let period = if pass == 0 || slopes.is_empty() {
            p_nom
        } else {
            let mut s = slopes.clone();
            s.sort_by(f64::total_cmp);
            s[s.len() / 2]
        };
        plan.clear();
        for &(ra, rb, open, close) in &bounds {
            if rb - ra < p_nom * 0.5 {
                continue;
            }
            let pulses: Vec<f64> = confirmed.iter().copied().filter(|&c| c >= ra - tol && c < rb).collect();
            let mut segs: Vec<Seg> = Vec::new();
            for &c in &pulses {
                if let Some(s) = segs.last_mut() {
                    let (li, lt) = *s.pts.last().unwrap();
                    let k = ((c - lt) / period).round() as i64;
                    if k < 1 {
                        continue;
                    }
                    let (a, b) = s.fit(period);
                    if (c - (a + b * (li + k) as f64)).abs() <= tol {
                        s.pts.push((li + k, c));
                    } else {
                        segs.push(Seg { pts: vec![(li + k, c)] });
                    }
                } else {
                    segs.push(Seg { pts: vec![(0, c)] });
                }
            }
            // A lone pulse off the grid is more likely wrong than a dropout.
            segs.retain(|s| s.pts.len() > 1);
            // Counting lines across a gap by time goes wrong when more than
            // half a line of audio was lost in it. The separators show it: two
            // runs that disagree about which lines are odd are one line apart
            // from where the count put them.
            let votes: Vec<i64> = segs
                .iter()
                .map(|s| {
                    s.pts
                        .iter()
                        .filter_map(|&(i, t)| rx.separator(t).map(|odd| if odd == (i.rem_euclid(2) == 1) { 1 } else { -1 }))
                        .sum()
                })
                .collect();
            let mut shift = 0i64;
            let mut shifts = vec![0i64; segs.len()];
            for j in 1..segs.len() {
                let flip = |v: i64, sh: i64| if sh % 2 == 0 { v } else { -v };
                let (prev, cur) = (flip(votes[j - 1], shifts[j - 1]), flip(votes[j], shift));
                if prev.abs() >= 2 && cur.abs() >= 2 && prev.signum() != cur.signum() {
                    let ((i0, t0), (i1, t1)) = (*segs[j - 1].pts.last().unwrap(), segs[j].pts[0]);
                    let frac = (t1 - t0) / period - (i1 - i0) as f64;
                    // Lost audio makes the gap look short; inserted audio, long.
                    shift += if frac >= -0.1 { 1 } else { -1 };
                }
                shifts[j] = shift;
            }
            for (s, sh) in segs.iter_mut().zip(&shifts) {
                for p in s.pts.iter_mut() {
                    p.0 += sh;
                }
            }
            if pass == 0 {
                for s in &segs {
                    if s.pts.len() >= 10 {
                        slopes.push(s.fit(period).1);
                    }
                }
                continue;
            }
            // Runs as (first idx, last idx, a, b): line idx starts at a + b * idx.
            let mut runs: Vec<(i64, i64, f64, f64)> = segs
                .iter()
                .map(|s| {
                    let (a, b) = s.fit(period);
                    (s.pts[0].0, s.pts.last().unwrap().0, a, b)
                })
                .collect();
            // Row offsets (row = idx + off) counted from the opening header and
            // back from the closing one.
            let (fwd, bwd) = if runs.is_empty() {
                // No usable pulses after a header: run free at the nominal
                // rate, as a receiver that has lost sync does.
                match open {
                    Some(_) => {
                        runs.push((0, 0, ra, period));
                        (Some(0), None)
                    }
                    None => continue,
                }
            } else {
                let (i0, t0) = segs[0].pts[0];
                let (i1, t1) = *segs.last().unwrap().pts.last().unwrap();
                (
                    open.map(|_| ((t0 - ra) / period).round() as i64 - i0),
                    close.map(|_| HEIGHT as i64 - 1 - ((rb - period - t1) / period).round() as i64 - i1),
                )
            };
            // A line between runs takes its time from the nearest run.
            let time_of = |idx: i64| -> f64 {
                let r = runs.iter().min_by_key(|r| (r.0 - idx).max(idx - r.1).max(0)).unwrap();
                r.2 + r.3 * idx as f64
            };
            for row in 0..HEIGHT as i64 {
                for (off, forward) in [(fwd, true), (bwd, false)] {
                    let Some(off) = off else { continue };
                    let idx = row - off;
                    if !forward && fwd.is_some_and(|f| (0..HEIGHT as i64).contains(&(idx + f))) {
                        continue; // already placed by counting forward
                    }
                    let t = time_of(idx);
                    if t < ra.max(0.0) - tol || t + p_nom > rb.min(n) + tol {
                        continue;
                    }
                    let known_at = if forward { ra } else { rb + VIS_MS * spms };
                    plan.push((t.max(0.0), row as usize, forward, known_at));
                }
            }
        }
    }
    plan.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut noise: Vec<f64> = plan.iter().step_by(plan.len() / 120 + 1).map(|p| rx.sync_noise(p.0)).collect();
    noise.sort_by(f64::total_cmp);
    let sync_noise_hz = noise.get(noise.len() / 2).copied().unwrap_or(0.0);
    let smoothing = smoothing.unwrap_or_else(|| auto_smoothing(sync_noise_hz));
    for (t, row, forward, known_at) in plan {
        let (y, c, odd) = rx.demod_line(t, smoothing);
        lines.push(Line {
            time: t / FS as f64,
            row,
            known_at: known_at / FS as f64,
            forward,
            odd: odd.unwrap_or(row % 2 == 1),
            y,
            c,
        });
    }
    let ppm = if slopes.is_empty() {
        0.0
    } else {
        let mut s = slopes.clone();
        s.sort_by(f64::total_cmp);
        (s[s.len() / 2] / p_nom - 1.0) * 1e6
    };
    Reception {
        duration: n / FS as f64,
        headers,
        lines,
        pulses: confirmed.len(),
        ppm,
        sync_noise_hz,
        smoothing,
    }
}

impl Reception {
    fn usable(&self, at: f64, placement: Placement) -> impl Iterator<Item = &Line> {
        self.lines.iter().filter(move |l| {
            l.time + LINE_MS / 1000.0 <= at + 1e-9 && l.known_at <= at + 1e-9 && (l.forward || placement == Placement::Buffered)
        })
    }

    /// Lines on screen at `at` seconds (a row received twice counts once).
    pub fn rows_shown(&self, at: f64, placement: Placement) -> usize {
        let mut seen = [false; HEIGHT];
        for l in self.usable(at, placement) {
            seen[l.row] = true;
        }
        seen.iter().filter(|&&s| s).count()
    }

    /// The 320x240 picture on screen `at` seconds into the recording, or
    /// None before the first line. Later lines overwrite earlier ones; rows
    /// not yet received are mid-grey, as is missing chroma.
    pub fn picture(&self, at: f64, placement: Placement, conv: Convention) -> Option<Image> {
        let mut y: Vec<Option<&[f32]>> = vec![None; HEIGHT];
        let mut cb: Vec<Option<&[f32]>> = vec![None; HEIGHT / 2];
        let mut cr: Vec<Option<&[f32]>> = vec![None; HEIGHT / 2];
        let mut any = false;
        for l in self.usable(at, placement) {
            any = true;
            y[l.row] = Some(&l.y);
            if l.odd {
                cb[l.row / 2] = Some(&l.c);
            } else {
                cr[l.row / 2] = Some(&l.c);
            }
        }
        if !any {
            return None;
        }
        let neutral = conv.to_ycc([128.0; 3]);
        let mut img = Image::filled(WIDTH, HEIGHT, [128, 128, 128]);
        for row in 0..HEIGHT {
            let Some(yl) = y[row] else { continue };
            for x in 0..WIDTH {
                let ycc = [
                    yl[x] as f64,
                    cb[row / 2].map_or(neutral[1], |c| c[x] as f64),
                    cr[row / 2].map_or(neutral[2], |c| c[x] as f64),
                ];
                let rgb = conv.to_rgb(ycc);
                img.set(x, row, rgb.map(|v| v.round().clamp(0.0, 255.0) as u8));
            }
        }
        Some(img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::Channel;
    use crate::image::{psnr, synthetic};
    use crate::util::Rng;

    fn noise_picture(seed: u64) -> Image {
        let mut rng = Rng::new(seed);
        let mut img = Image::new(WIDTH, HEIGHT);
        for v in img.data.iter_mut() {
            *v = rng.below(256) as u8;
        }
        img
    }

    /// Strongest of the mode's tones in samples [a, b) of `x`, by correlation.
    fn tone_at(x: &[f32], a: usize, b: usize) -> f64 {
        let cands = [F_BIT1, F_SYNC, F_BIT0, F_BLACK, F_LEADER, F_WHITE];
        let power = |f: f64| {
            let w = TAU * f / FS as f64;
            let (mut c, mut s) = (0.0, 0.0);
            for k in a..b {
                c += x[k] as f64 * (w * k as f64).cos();
                s += x[k] as f64 * (w * k as f64).sin();
            }
            c * c + s * s
        };
        *cands.iter().max_by(|p, q| power(**p).total_cmp(&power(**q))).unwrap()
    }

    /// One picture with `pad` seconds of silence on each side.
    fn one_picture(pic: &Image, pad: f64) -> Vec<f32> {
        let z = vec![0f32; (pad * FS as f64) as usize];
        let mut a = z.clone();
        a.extend(synthesize(&frame_tones(pic, Convention::Spec), FS, AMP));
        a.extend(z);
        a
    }

    #[test]
    fn timing_matches_the_published_mode() {
        assert_eq!(LINE_MS, 150.0);
        assert_eq!(VIS_MS, 910.0);
        assert!((FRAME_SECONDS - 36.91).abs() < 1e-12);
        let h = header_tones(VIS_CODE);
        let f: Vec<f64> = h.iter().map(|t| t.0).collect();
        // Leader, break, leader, start bit; 8 = 0001000 LSB first; one 1, so the
        // even-parity bit is 1; stop bit.
        assert_eq!(
            f,
            [1900.0, 1200.0, 1900.0, 1200.0, 1300.0, 1300.0, 1300.0, 1100.0, 1300.0, 1300.0, 1300.0, 1100.0, 1200.0]
        );
        assert_eq!(h.iter().map(|t| t.1).sum::<f64>(), 910.0);
        let t = frame_tones(&noise_picture(1), Convention::Spec);
        assert_eq!(t.len(), h.len() + HEIGHT * (2 + WIDTH + 2 + WIDTH));
        let per_line = 4 + 2 * WIDTH;
        for line in [0usize, 1, 2, 119, 239] {
            let l = &t[h.len() + line * per_line..h.len() + (line + 1) * per_line];
            assert!((l.iter().map(|s| s.1).sum::<f64>() - 150.0).abs() < 1e-9);
            assert_eq!((l[0], l[1]), ((1200.0, 9.0), (1500.0, 3.0)));
            let sep = l[2 + WIDTH];
            assert_eq!(sep, (if line % 2 == 1 { 2300.0 } else { 1500.0 }, 4.5));
            assert_eq!(l[3 + WIDTH], (1900.0, 1.5));
            assert!((l[2].1 - 0.275).abs() < 1e-12 && (l[4 + WIDTH].1 - 0.1375).abs() < 1e-12);
            assert!(l[2..2 + WIDTH]
                .iter()
                .chain(&l[4 + WIDTH..])
                .all(|s| (1500.0..=2300.0).contains(&s.0)));
        }
        // Sample counts at 48 and 44.1 kHz: no drift from accumulated rounding.
        assert_eq!(synthesize(&t, 48_000, AMP).len(), 1_771_680);
        assert_eq!(synthesize(&t, 44_100, AMP).len(), 1_627_731);
    }

    #[test]
    fn the_audio_has_each_tone_where_the_spec_puts_it() {
        let x = synthesize(&frame_tones(&noise_picture(2), Convention::Spec), FS, AMP);
        let ms = |v: f64| (v * FS as f64 / 1000.0).round() as usize;
        // Header parts, skipping a millisecond at each edge.
        for (a, b, f) in [
            (1.0, 299.0, 1900.0),
            (311.0, 609.0, 1900.0),
            (611.0, 639.0, 1200.0),
            (731.0, 759.0, 1100.0),
            (881.0, 909.0, 1200.0),
        ] {
            assert_eq!(tone_at(&x, ms(a), ms(b)), f, "header at {a} ms");
        }
        for line in [0usize, 1, 2, 119, 238, 239] {
            let t0 = VIS_MS + LINE_MS * line as f64;
            assert_eq!(tone_at(&x, ms(t0 + 0.5), ms(t0 + 8.5)), 1200.0, "sync of line {line}");
            assert_eq!(tone_at(&x, ms(t0 + 9.3), ms(t0 + 11.7)), 1500.0, "porch of line {line}");
            let sep = tone_at(&x, ms(t0 + SEP_AT + 0.3), ms(t0 + SEP_AT + 4.2));
            assert_eq!(sep, if line % 2 == 1 { 2300.0 } else { 1500.0 }, "separator of line {line}");
        }
        // Constant envelope and continuous phase: no sample jumps by more than
        // one step of the highest frequency would allow.
        let max_step = (AMP as f64 * TAU * 2300.0 / FS as f64) as f32 * 1.01;
        assert!(x.windows(2).all(|w| (w[1] - w[0]).abs() <= max_step));
    }

    #[test]
    fn colour_equations_match_the_published_coefficients() {
        let close = |a: [f64; 3], b: [f64; 3], tol: f64| a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= tol);
        let s = Convention::Spec;
        // Studio swing: black 16, white 235, red (81.5, 90.2, 240).
        assert!(close(s.to_ycc([0.0; 3]), [16.0, 128.0, 128.0], 1e-9));
        assert!(close(s.to_ycc([255.0; 3]), [235.0, 128.0, 128.0], 0.02));
        assert!(close(s.to_ycc([255.0, 0.0, 0.0]), [81.48, 90.21, 239.99], 0.01));
        assert!(close(s.to_ycc([0.0, 0.0, 255.0]), [40.96, 239.99, 109.79], 0.01));
        // Pillow's full-range YCbCr, as pySSTV sends it.
        let p = Convention::Pysstv;
        assert!(close(p.to_ycc([255.0; 3]), [255.0, 128.0, 128.0], 0.0));
        assert!(close(p.to_ycc([255.0, 0.0, 0.0]), [76.0, 85.0, 255.0], 0.0));
        // Round trips.
        let mut rng = Rng::new(7);
        for _ in 0..1000 {
            let rgb = [0, 1, 2].map(|_| rng.below(256) as f64);
            assert!(close(s.to_rgb(s.to_ycc(rgb)), rgb, 1e-6));
            assert!(close(p.to_rgb(p.to_ycc(rgb)), rgb, 1.5));
        }
    }

    #[test]
    fn clean_loopback_recovers_the_picture_line_by_line() {
        let pic = fit(&synthetic("clouds", 768, 512));
        let rx = receive(&one_picture(&pic, 0.3), FS);
        assert_eq!(rx.headers.len(), 1);
        let h = &rx.headers[0];
        assert_eq!((h.code, h.parity_ok), (VIS_CODE, true));
        assert!((h.line0 - 1.21).abs() < 0.002, "line 0 at {}", h.line0);
        assert_eq!(rx.lines.len(), HEIGHT);
        assert!(rx
            .lines
            .iter()
            .enumerate()
            .all(|(i, l)| l.row == i && l.forward && l.odd == (i % 2 == 1)));
        assert!(rx.lines.iter().all(|l| (l.time - (1.21 + 0.15 * l.row as f64)).abs() < 1e-4));
        // No start-up delay: the first line is on screen 150 ms after the header.
        let first = 1.21 + 0.15;
        assert!(rx.picture(first - 0.01, Placement::FromVis, Convention::Spec).is_none());
        assert_eq!(rx.rows_shown(first + 0.001, Placement::FromVis), 1);
        assert_eq!(rx.rows_shown(1.21 + 0.15 * 120.0 + 0.001, Placement::FromVis), 120);
        let out = rx.picture(40.0, Placement::FromVis, Convention::Spec).unwrap();
        let q = psnr(&pic, &out);
        assert!(q > 30.0, "clean loopback {q:.2} dB");
    }

    #[test]
    fn a_late_listener_needs_the_next_header_unless_it_buffers() {
        let pic = fit(&synthetic("scene", 768, 512));
        // Start 20 s into the first picture, listen for 40 s.
        let rx = receive(&beacon_audio(&pic, Convention::Spec, 20.0, 40.0), FS);
        assert_eq!(rx.headers.len(), 1);
        let next = FRAME_SECONDS - 20.0; // the next header starts here
        let known = next + VIS_MS / 1000.0;
        for p in [Placement::FromVis, Placement::Buffered] {
            assert!(rx.picture(known - 0.01, p, Convention::Spec).is_none(), "{p:?}");
        }
        // Buffering places the lines heard so far as soon as the header is in.
        let early = rx.rows_shown(known + 0.001, Placement::Buffered);
        assert!((110..=114).contains(&early), "{early} rows counted back");
        assert_eq!(rx.rows_shown(known + 0.001, Placement::FromVis), 0);
        let end_vis = rx.rows_shown(40.0, Placement::FromVis);
        assert_eq!(end_vis, 147, "whole lines between the header (17.82 s) and 40 s");
        assert_eq!(rx.rows_shown(40.0, Placement::Buffered), HEIGHT);
        // Lines counted back from the header land on the right rows.
        let full = rx.picture(40.0, Placement::Buffered, Convention::Spec).unwrap();
        let q = psnr(&pic, &full);
        assert!(q > 24.0, "buffered picture {q:.2} dB");
    }

    #[test]
    fn decodes_44k1_recordings_with_a_clock_offset() {
        let pic = fit(&synthetic("clouds", 768, 512));
        let a = one_picture(&pic, 0.5);
        // Recorder at 44.1 kHz, its clock 130 ppm fast.
        let rec = resample(&a, 48_000.0 / 44_100.0 / (1.0 + 130e-6));
        let rx = receive(&rec, 44_100);
        assert!((rx.ppm - 130.0).abs() < 10.0, "clock {:.1} ppm", rx.ppm);
        assert_eq!(rx.rows_shown(rx.duration, Placement::FromVis), HEIGHT);
        let q = psnr(&pic, &rx.picture(rx.duration, Placement::FromVis, Convention::Spec).unwrap());
        assert!(q > 30.0, "{q:.2} dB");
    }

    #[test]
    fn noise_and_dropouts_degrade_the_picture_gradually() {
        let pic = fit(&synthetic("scene", 768, 512));
        let a = one_picture(&pic, 0.5);
        let grey = psnr(&pic, &Image::filled(WIDTH, HEIGHT, [128, 128, 128]));
        let mut last = f64::INFINITY;
        let mut noise = 0.0;
        for snr in [30.0, 20.0, 10.0, 5.0, 0.0] {
            let rx = receive(&Channel::awgn(snr, 40).apply(&a), FS);
            let q = psnr(&pic, &rx.picture(rx.duration, Placement::FromVis, Convention::Spec).unwrap());
            assert_eq!(rx.headers.len(), 1, "header at {snr} dB");
            assert_eq!(rx.rows_shown(rx.duration, Placement::FromVis), HEIGHT, "rows at {snr} dB");
            assert!(q < last + 0.3, "quality rose from {last:.2} to {q:.2} dB at {snr} dB");
            assert!(rx.sync_noise_hz > noise);
            assert!(q > grey + 2.0, "{q:.2} dB at {snr} dB SNR: no better than grey");
            last = q;
            noise = rx.sync_noise_hz;
        }
        // 50 ms of audio deleted every 2 s on average. The line a dropout hits
        // is lost, and with it the chroma its pair shares; sync is found again
        // at the next line, so no more than two pairs suffer per dropout.
        let ch = Channel {
            dropouts_per_min: 30.0,
            seed: 41,
            ..Channel::clean()
        };
        let y = ch.apply(&a);
        let cuts = (a.len() - y.len()) / 2400;
        let rx = receive(&y, FS);
        assert!(rx.rows_shown(rx.duration, Placement::FromVis) >= HEIGHT - 2);
        let clean = receive(&a, FS).picture(rx.duration, Placement::FromVis, Convention::Spec).unwrap();
        let out = rx.picture(rx.duration, Placement::FromVis, Convention::Spec).unwrap();
        let row_mse = |img: &Image, r: usize| -> f64 {
            (0..WIDTH)
                .map(|x| {
                    (0..3)
                        .map(|c| (pic.px(x, r)[c] as f64 - img.px(x, r)[c] as f64).powi(2))
                        .sum::<f64>()
                })
                .sum::<f64>()
                / (3 * WIDTH) as f64
        };
        let bad = (0..HEIGHT).filter(|&r| row_mse(&out, r) > row_mse(&clean, r) + 100.0).count();
        assert!(cuts >= 10 && bad <= 4 * cuts, "{bad} rows damaged by {cuts} dropouts");
    }

    #[test]
    fn smoothing_follows_the_measured_noise() {
        assert_eq!(auto_smoothing(0.0), (4.0, 16.0));
        assert_eq!(auto_smoothing(128.0), (8.0, 32.0));
        assert_eq!(auto_smoothing(1000.0), (16.0, 64.0));
    }
}
