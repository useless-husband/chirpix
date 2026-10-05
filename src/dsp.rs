//! Resampling and small filter helpers shared by the receiver and the
//! channel simulator.

use crate::fft::convolve;

const HALF: usize = 16; // interpolation kernel half-width in input samples
const PHASES: usize = 512;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..40 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

fn kaiser(pos: f64, half: f64, beta: f64) -> f64 {
    let r = pos / half;
    if r.abs() >= 1.0 {
        0.0
    } else {
        bessel_i0(beta * (1.0 - r * r).sqrt()) / bessel_i0(beta)
    }
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x)
    }
}

fn kernel_table() -> &'static Vec<f32> {
    static T: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        // table[p][k] = h(k - HALF + 1 - p/PHASES), k in 0..2*HALF
        let mut t = Vec::with_capacity((PHASES + 1) * 2 * HALF);
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            for k in 0..2 * HALF {
                let d = k as f64 - (HALF as f64 - 1.0) - frac;
                t.push((sinc(d) * kaiser(d, HALF as f64, 9.0)) as f32);
            }
        }
        t
    })
}

/// Windowed-sinc low-pass FIR with cutoff `fc` (cycles per sample).
pub fn lowpass(fc: f64, taps: usize) -> Vec<f32> {
    let m = (taps - 1) as f64 / 2.0;
    (0..taps).map(|i| (2.0 * fc * sinc(2.0 * fc * (i as f64 - m)) * kaiser(i as f64 - m, m + 1.0, 8.0)) as f32).collect()
}

/// Band-pass FIR between `f_lo` and `f_hi` (cycles per sample).
pub fn bandpass(f_lo: f64, f_hi: f64, taps: usize) -> Vec<f32> {
    let (a, b) = (lowpass(f_hi, taps), lowpass(f_lo, taps));
    a.iter().zip(&b).map(|(x, y)| x - y).collect()
}

/// Filter and keep the original length and alignment (removes the FIR delay).
pub fn filter_same(x: &[f32], h: &[f32]) -> Vec<f32> {
    let y = convolve(x, h);
    let d = (h.len() - 1) / 2;
    y[d..d + x.len()].to_vec()
}

/// Resample by band-limited interpolation: `out[m] = x(m * step)`, where
/// `step` is input samples per output sample. `step` slightly different
/// from 1 models (or undoes) a sample-clock offset; 44100/48000 converts
/// a 44.1 kHz recording to 48 kHz.
pub fn resample(x: &[f32], step: f64) -> Vec<f32> {
    assert!(step > 0.01 && step < 100.0);
    let filtered;
    let src: &[f32] = if step > 1.0005 {
        // Downsampling: remove what would alias first.
        filtered = filter_same(x, &lowpass(0.47 / step, 129));
        &filtered
    } else {
        x
    };
    let table = kernel_table();
    let n_out = ((src.len() as f64) / step).floor() as usize;
    let mut out = Vec::with_capacity(n_out);
    for m in 0..n_out {
        let t = m as f64 * step;
        let i0 = t.floor() as isize;
        let frac = t - i0 as f64;
        let pf = frac * PHASES as f64;
        let p = pf as usize;
        let a = (pf - p as f64) as f32;
        let k0 = &table[p * 2 * HALF..(p + 1) * 2 * HALF];
        let k1 = &table[(p + 1) * 2 * HALF..(p + 2) * 2 * HALF];
        let base = i0 - (HALF as isize - 1);
        let mut acc = 0f32;
        if base >= 0 && (base as usize + 2 * HALF) <= src.len() {
            let seg = &src[base as usize..base as usize + 2 * HALF];
            for k in 0..2 * HALF {
                acc += seg[k] * (k0[k] + a * (k1[k] - k0[k]));
            }
        } else {
            for k in 0..2 * HALF {
                let idx = base + k as isize;
                if idx >= 0 && (idx as usize) < src.len() {
                    acc += src[idx as usize] * (k0[k] + a * (k1[k] - k0[k]));
                }
            }
        }
        out.push(acc);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(f: f64, n: usize, rate: f64) -> Vec<f32> {
        (0..n).map(|i| (std::f64::consts::TAU * f * i as f64 / rate).sin() as f32).collect()
    }

    #[test]
    fn resampling_a_tone_gives_the_same_tone_on_the_new_grid() {
        for (f, step) in [(1000.0, 1.0002), (7000.0, 0.9998), (6500.0, 44100.0 / 48000.0), (3000.0, 2.0)] {
            let x = tone(f, 20_000, 48000.0);
            let y = resample(&x, step);
            let mut worst = 0f64;
            for (m, &v) in y.iter().enumerate().skip(200).take(y.len() - 400) {
                let want = (std::f64::consts::TAU * f * m as f64 * step / 48000.0).sin();
                worst = worst.max((v as f64 - want).abs());
            }
            assert!(worst < 2e-3, "f={f} step={step}: error {worst}");
        }
    }

    #[test]
    fn resample_round_trip_is_transparent() {
        let x: Vec<f32> = tone(1500.0, 30_000, 48000.0).iter().zip(tone(6900.0, 30_000, 48000.0)).map(|(a, b)| a + b).collect();
        let step = 1.0 + 150e-6;
        let back = resample(&resample(&x, step), 1.0 / step);
        let err = x.iter().zip(&back).skip(300).take(29_000).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(err < 3e-3, "round trip error {err}");
    }

    #[test]
    fn bandpass_passes_the_band_and_rejects_outside() {
        let h = bandpass(500.0 / 48000.0, 8000.0 / 48000.0, 257);
        let rms = |v: &[f32]| (v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
        for (f, pass) in [(100.0, false), (3000.0, true), (7000.0, true), (12000.0, false)] {
            let y = filter_same(&tone(f, 8000, 48000.0), &h);
            let g = rms(&y[1000..7000]) * std::f64::consts::SQRT_2;
            if pass {
                assert!((g - 1.0).abs() < 0.02, "{f} Hz gain {g}");
            } else {
                assert!(g < 0.01, "{f} Hz gain {g}");
            }
        }
    }
}
