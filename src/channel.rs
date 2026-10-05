//! Simulated acoustic channel: what happens between the WAV file being
//! played and the WAV file a recorder writes.
//!
//! loudspeaker/microphone band-limiting -> room reverberation -> clock
//! offset between player and recorder -> background noise -> clicks ->
//! recorder dropouts -> clipping.

use crate::dsp::{bandpass, filter_same, resample};
use crate::fft::convolve;
use crate::modem::FS;
use crate::util::Rng;

#[derive(Clone, Debug)]
pub struct Channel {
    pub name: String,
    /// Signal-to-noise ratio in dB, counting only the noise that falls in
    /// the modem's band (bins 22..150, 6 kHz wide). `None`: no noise.
    pub snr_db: Option<f64>,
    /// Reverberation time in seconds (0 = none) and direct-to-reverberant
    /// energy ratio in dB.
    pub rt60: f64,
    pub drr_db: f64,
    /// Recorder clock offset in parts per million (positive = recorder fast).
    pub ppm: f64,
    /// Pass band in Hz of the loudspeaker/microphone pair.
    pub band: Option<(f64, f64)>,
    /// Clip the recording at this multiple of its RMS level.
    pub clip: Option<f64>,
    /// Clicks per second, and their peak level in dB above the signal RMS.
    pub impulses_per_s: f64,
    pub impulse_db: f64,
    /// Recorder dropouts per minute; each removes `dropout_ms` of audio.
    pub dropouts_per_min: f64,
    pub dropout_ms: f64,
    pub seed: u64,
}

impl Channel {
    pub fn clean() -> Channel {
        Channel {
            name: "ideal".into(),
            snr_db: None,
            rt60: 0.0,
            drr_db: 0.0,
            ppm: 0.0,
            band: None,
            clip: None,
            impulses_per_s: 0.0,
            impulse_db: 20.0,
            dropouts_per_min: 0.0,
            dropout_ms: 50.0,
            seed: 1,
        }
    }
    pub fn awgn(snr_db: f64, seed: u64) -> Channel {
        Channel { name: format!("AWGN {snr_db} dB"), snr_db: Some(snr_db), seed, ..Channel::clean() }
    }
    /// Quiet room, devices close together.
    pub fn good() -> Channel {
        Channel {
            name: "good".into(),
            snr_db: Some(25.0),
            rt60: 0.3,
            drr_db: 10.0,
            ppm: 20.0,
            band: Some((200.0, 12_000.0)),
            seed: 11,
            ..Channel::clean()
        }
    }
    /// Ordinary room, a metre or two apart, some background noise.
    pub fn fair() -> Channel {
        Channel {
            name: "fair".into(),
            snr_db: Some(14.0),
            rt60: 0.45,
            drr_db: 5.0,
            ppm: -70.0,
            band: Some((400.0, 9_000.0)),
            impulses_per_s: 0.5,
            seed: 12,
            ..Channel::clean()
        }
    }
    /// Noisy, echoing room with clicks and a recorder that drops audio.
    pub fn poor() -> Channel {
        Channel {
            name: "poor".into(),
            snr_db: Some(8.0),
            rt60: 0.6,
            drr_db: 2.0,
            ppm: 130.0,
            band: Some((600.0, 7_500.0)),
            impulses_per_s: 2.0,
            dropouts_per_min: 6.0,
            seed: 13,
            ..Channel::clean()
        }
    }

    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        match self.snr_db {
            Some(s) => parts.push(format!("SNR {s:.0} dB")),
            None => parts.push("no noise".into()),
        }
        if self.rt60 > 0.0 {
            parts.push(format!("RT60 {:.2} s, DRR {:.0} dB", self.rt60, self.drr_db));
        }
        if self.ppm != 0.0 {
            parts.push(format!("clock {:+.0} ppm", self.ppm));
        }
        if let Some((a, b)) = self.band {
            parts.push(format!("band {:.1}-{:.1} kHz", a / 1000.0, b / 1000.0));
        }
        if let Some(c) = self.clip {
            parts.push(format!("clipping at {c:.1} x RMS"));
        }
        if self.impulses_per_s > 0.0 {
            parts.push(format!("{:.1} clicks/s at +{:.0} dB", self.impulses_per_s, self.impulse_db));
        }
        if self.dropouts_per_min > 0.0 {
            parts.push(format!("{:.0} dropouts/min of {:.0} ms", self.dropouts_per_min, self.dropout_ms));
        }
        parts.join(", ")
    }

    pub fn apply(&self, x: &[f32]) -> Vec<f32> {
        let mut rng = Rng::new(self.seed);
        let fs = FS as f64;
        let mut y = x.to_vec();
        if let Some((lo, hi)) = self.band {
            y = filter_same(&y, &bandpass(lo / fs, hi / fs, 511));
        }
        if self.rt60 > 0.0 {
            let h = room_impulse(self.rt60, self.drr_db, self.seed ^ 0x5EED);
            let full = convolve(&y, &h);
            y = full[..y.len()].to_vec();
        }
        if self.ppm != 0.0 {
            y = resample(&y, 1.0 / (1.0 + self.ppm * 1e-6));
        }
        let power = y.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / y.len().max(1) as f64;
        let rms = power.sqrt();
        if let Some(snr) = self.snr_db {
            // White noise over the whole 24 kHz; only NCAR/(NFFT/2) of it is in band.
            let inband = 0.25; // 6 kHz of the 24 kHz the noise covers
            let sigma = (power / 10f64.powf(snr / 10.0) / inband).sqrt();
            for v in y.iter_mut() {
                *v += (sigma * rng.gauss()) as f32;
            }
        }
        if self.impulses_per_s > 0.0 {
            let peak = rms * 10f64.powf(self.impulse_db / 20.0);
            let mut t = 0.0;
            loop {
                t += -(1.0 - rng.f64()).ln() / self.impulses_per_s;
                let start = (t * fs) as usize;
                if start >= y.len() {
                    break;
                }
                for i in 0..144.min(y.len() - start) {
                    y[start + i] += (peak * (-(i as f64) / 30.0).exp() * rng.gauss()) as f32;
                }
            }
        }
        if self.dropouts_per_min > 0.0 {
            let len = (self.dropout_ms * 1e-3 * fs) as usize;
            let mut cuts = Vec::new();
            let mut t = 0.0;
            loop {
                t += -(1.0 - rng.f64()).ln() * 60.0 / self.dropouts_per_min;
                let start = (t * fs) as usize;
                if start + len >= y.len() {
                    break;
                }
                cuts.push(start);
            }
            for &start in cuts.iter().rev() {
                y.drain(start..start + len);
            }
        }
        if let Some(c) = self.clip {
            let lim = (c * rms) as f32;
            for v in y.iter_mut() {
                *v = v.clamp(-lim, lim);
            }
        }
        y
    }
}

/// Synthetic room impulse response: a unit direct path followed, from
/// 1.5 ms on, by exponentially decaying Gaussian noise (Polack's model)
/// whose decay reaches -60 dB after `rt60` seconds and whose energy is
/// `drr_db` below the direct path. Normalised to unit total energy.
pub fn room_impulse(rt60: f64, drr_db: f64, seed: u64) -> Vec<f32> {
    let fs = FS as f64;
    let mut rng = Rng::new(seed);
    let n = ((rt60 * fs) as usize).clamp(64, (1.5 * fs) as usize);
    let onset = (0.0015 * fs) as usize;
    let mut h = vec![0f64; n];
    for (i, v) in h.iter_mut().enumerate().skip(onset) {
        let t = (i - onset) as f64 / fs;
        *v = rng.gauss() * (-6.907_755 * t / rt60).exp();
    }
    let tail: f64 = h.iter().map(|v| v * v).sum();
    let g = (10f64.powf(-drr_db / 10.0) / tail.max(1e-30)).sqrt();
    for v in h.iter_mut() {
        *v *= g;
    }
    h[0] = 1.0;
    let total: f64 = h.iter().map(|v| v * v).sum::<f64>().sqrt();
    h.iter().map(|v| (v / total) as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_impulse_has_the_stated_decay_and_ratio() {
        let (rt60, drr) = (0.5, 4.0);
        let h = room_impulse(rt60, drr, 3);
        let direct = (h[0] as f64).powi(2);
        let tail: f64 = h[1..].iter().map(|v| (*v as f64).powi(2)).sum();
        assert!((10.0 * (direct / tail).log10() - drr).abs() < 0.01);
        assert!(((direct + tail) - 1.0).abs() < 1e-5);
        // Schroeder backward integration: slope between -5 and -25 dB gives RT60.
        let mut edc = vec![0f64; h.len()];
        let mut acc = 0.0;
        for i in (1..h.len()).rev() {
            acc += (h[i] as f64).powi(2);
            edc[i] = acc;
        }
        let db = |i: usize| 10.0 * (edc[i] / edc[1]).log10();
        let t5 = (1..h.len()).find(|&i| db(i) < -5.0).unwrap();
        let t25 = (1..h.len()).find(|&i| db(i) < -25.0).unwrap();
        let measured = 3.0 * (t25 - t5) as f64 / FS as f64;
        assert!((measured / rt60 - 1.0).abs() < 0.12, "RT60 measured {measured:.3} s");
    }

    #[test]
    fn noise_level_matches_the_in_band_definition() {
        let mut rng = Rng::new(9);
        let x: Vec<f32> = (0..200_000).map(|_| 0.1 * rng.gauss() as f32).collect();
        let y = Channel::awgn(10.0, 4).apply(&x);
        let noise: f64 = x.iter().zip(&y).map(|(a, b)| ((b - a) as f64).powi(2)).sum::<f64>() / x.len() as f64;
        // In-band SNR 10 dB with a quarter of the noise in band: total noise = signal * 0.4.
        assert!((noise / (0.01 * 0.4) - 1.0).abs() < 0.03, "noise power ratio {}", noise / 0.004);
    }

    #[test]
    fn clock_offset_changes_length_and_dropouts_remove_audio() {
        let x = vec![0.1f32; 480_000];
        let mut ch = Channel::clean();
        ch.ppm = 200.0;
        let y = ch.apply(&x);
        assert!((y.len() as i64 - 480_096).abs() <= 1, "length {}", y.len());
        let mut ch = Channel::clean();
        ch.dropouts_per_min = 60.0;
        let y = ch.apply(&x);
        assert!(y.len() < x.len() && (x.len() - y.len()) % 2400 == 0);
    }
}
