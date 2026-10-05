//! CDF 9/7 wavelet transform by lifting, for any image size.
//!
//! The subbands are scaled so the transform is close to orthonormal: an
//! error of a given size in any coefficient costs about the same squared
//! error in the picture. That is what lets the codec send bit-planes of
//! all subbands in one global order.

const ALPHA: f32 = -1.586_134_3;
const BETA: f32 = -0.052_980_118;
const GAMMA: f32 = 0.882_911_1;
const DELTA: f32 = 0.443_506_85;
/// sqrt(2) / K with K = 1.230174105: low-pass DC gain becomes sqrt(2).
const ZETA: f32 = 1.149_604_4;

#[inline]
fn lift(x: &mut [f32], parity: usize, coef: f32) {
    let n = x.len();
    let mut i = parity;
    while i < n {
        // Whole-sample symmetric extension at both ends.
        let left = if i > 0 { x[i - 1] } else { x[i + 1] };
        let right = if i + 1 < n { x[i + 1] } else { x[i - 1] };
        x[i] += coef * (left + right);
        i += 2;
    }
}

/// One analysis step: `x` becomes [low half | high half].
pub fn forward_1d(x: &mut [f32], tmp: &mut Vec<f32>) {
    let n = x.len();
    if n < 2 {
        return;
    }
    lift(x, 1, ALPHA);
    lift(x, 0, BETA);
    lift(x, 1, GAMMA);
    lift(x, 0, DELTA);
    tmp.clear();
    tmp.extend_from_slice(x);
    let nl = n.div_ceil(2);
    for i in 0..n {
        if i % 2 == 0 {
            x[i / 2] = tmp[i] * ZETA;
        } else {
            x[nl + i / 2] = tmp[i] / ZETA;
        }
    }
}

pub fn inverse_1d(x: &mut [f32], tmp: &mut Vec<f32>) {
    let n = x.len();
    if n < 2 {
        return;
    }
    tmp.clear();
    tmp.extend_from_slice(x);
    let nl = n.div_ceil(2);
    for i in 0..n {
        x[i] = if i % 2 == 0 { tmp[i / 2] / ZETA } else { tmp[nl + i / 2] * ZETA };
    }
    lift(x, 0, -DELTA);
    lift(x, 1, -GAMMA);
    lift(x, 0, -BETA);
    lift(x, 1, -ALPHA);
}

fn pass_2d(data: &mut [f32], stride: usize, w: usize, h: usize, forward: bool) {
    let mut tmp = Vec::new();
    let mut col = vec![0f32; h];
    let rows = |data: &mut [f32], tmp: &mut Vec<f32>| {
        for y in 0..h {
            let row = &mut data[y * stride..y * stride + w];
            if forward {
                forward_1d(row, tmp);
            } else {
                inverse_1d(row, tmp);
            }
        }
    };
    if forward {
        rows(data, &mut tmp);
    }
    for x in 0..w {
        for y in 0..h {
            col[y] = data[y * stride + x];
        }
        if forward {
            forward_1d(&mut col, &mut tmp);
        } else {
            inverse_1d(&mut col, &mut tmp);
        }
        for y in 0..h {
            data[y * stride + x] = col[y];
        }
    }
    if !forward {
        rows(data, &mut tmp);
    }
}

/// Sizes of the low-pass region after each level, starting with the image.
fn level_sizes(w: usize, h: usize, levels: usize) -> Vec<(usize, usize)> {
    let mut v = vec![(w, h)];
    for _ in 0..levels {
        let (cw, ch) = *v.last().unwrap();
        v.push((cw.div_ceil(2), ch.div_ceil(2)));
    }
    v
}

/// In-place multi-level transform (Mallat layout: coarsest LL top-left).
pub fn forward_2d(data: &mut [f32], w: usize, h: usize, levels: usize) {
    for &(cw, ch) in level_sizes(w, h, levels).iter().take(levels) {
        pass_2d(data, w, cw, ch, true);
    }
}

pub fn inverse_2d(data: &mut [f32], w: usize, h: usize, levels: usize) {
    for &(cw, ch) in level_sizes(w, h, levels).iter().take(levels).rev() {
        pass_2d(data, w, cw, ch, false);
    }
}

/// Number of decomposition levels used for an image of this size: up to
/// five, keeping the coarsest band at least 8 samples on its short side.
pub fn levels_for(w: usize, h: usize) -> usize {
    let mut l = 0;
    let mut m = w.min(h);
    while l < 5 && m >= 16 {
        m = m.div_ceil(2);
        l += 1;
    }
    l
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Orient {
    LL,
    HL,
    LH,
    HH,
}

#[derive(Clone, Copy, Debug)]
pub struct Subband {
    /// 1 = finest; `levels` = coarsest.
    pub level: usize,
    pub orient: Orient,
    pub x0: usize,
    pub y0: usize,
    pub w: usize,
    pub h: usize,
}

/// Subband rectangles, coarsest first.
pub fn subbands(w: usize, h: usize, levels: usize) -> Vec<Subband> {
    let sizes = level_sizes(w, h, levels);
    let (lw, lh) = sizes[levels];
    let mut out = vec![Subband { level: levels, orient: Orient::LL, x0: 0, y0: 0, w: lw, h: lh }];
    for level in (1..=levels).rev() {
        let (pw, ph) = sizes[level - 1];
        let (lw, lh) = sizes[level];
        out.push(Subband { level, orient: Orient::HL, x0: lw, y0: 0, w: pw - lw, h: lh });
        out.push(Subband { level, orient: Orient::LH, x0: 0, y0: lh, w: lw, h: ph - lh });
        out.push(Subband { level, orient: Orient::HH, x0: lw, y0: lh, w: pw - lw, h: ph - lh });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;

    #[test]
    fn perfect_reconstruction_for_all_small_sizes() {
        let mut rng = Rng::new(41);
        let mut tmp = Vec::new();
        for n in 1..40 {
            let x: Vec<f32> = (0..n).map(|_| rng.gauss() as f32 * 50.0).collect();
            let mut y = x.clone();
            forward_1d(&mut y, &mut tmp);
            inverse_1d(&mut y, &mut tmp);
            for (a, b) in x.iter().zip(&y) {
                assert!((a - b).abs() < 1e-3, "n={n}");
            }
        }
        for (w, h) in [(1, 1), (2, 3), (17, 16), (33, 47), (64, 64), (100, 37)] {
            let levels = levels_for(w, h);
            let x: Vec<f32> = (0..w * h).map(|_| rng.gauss() as f32 * 50.0).collect();
            let mut y = x.clone();
            forward_2d(&mut y, w, h, levels);
            inverse_2d(&mut y, w, h, levels);
            let err = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(err < 2e-3, "{w}x{h}: {err}");
        }
    }

    #[test]
    fn constant_signal_goes_entirely_to_low_band_with_sqrt2_gain() {
        let mut x = vec![10f32; 32];
        forward_1d(&mut x, &mut Vec::new());
        for v in &x[..16] {
            assert!((v - 10.0 * std::f32::consts::SQRT_2).abs() < 1e-3);
        }
        for v in &x[16..] {
            assert!(v.abs() < 1e-4);
        }
    }

    #[test]
    fn transform_is_close_to_orthonormal() {
        // Energy of white noise must be preserved within a few percent,
        // and a unit error in any subband must come back as roughly unit energy.
        let mut rng = Rng::new(42);
        let (w, h, levels) = (128, 96, 4);
        let x: Vec<f32> = (0..w * h).map(|_| rng.gauss() as f32).collect();
        let mut y = x.clone();
        forward_2d(&mut y, w, h, levels);
        let e0: f64 = x.iter().map(|v| (*v as f64).powi(2)).sum();
        let e1: f64 = y.iter().map(|v| (*v as f64).powi(2)).sum();
        assert!((e1 / e0 - 1.0).abs() < 0.05, "energy ratio {}", e1 / e0);
        for sb in subbands(w, h, levels) {
            let mut z = vec![0f32; w * h];
            z[(sb.y0 + sb.h / 2) * w + sb.x0 + sb.w / 2] = 1.0;
            inverse_2d(&mut z, w, h, levels);
            let e: f64 = z.iter().map(|v| (*v as f64).powi(2)).sum();
            assert!(e > 0.7 && e < 1.45, "{sb:?}: synthesis energy {e}");
        }
    }

    #[test]
    fn subbands_tile_the_image_exactly() {
        for (w, h) in [(768, 512), (37, 100), (16, 16), (5, 3)] {
            let levels = levels_for(w, h);
            let mut cover = vec![0u8; w * h];
            for sb in subbands(w, h, levels) {
                for y in sb.y0..sb.y0 + sb.h {
                    for x in sb.x0..sb.x0 + sb.w {
                        cover[y * w + x] += 1;
                    }
                }
            }
            assert!(cover.iter().all(|&c| c == 1), "{w}x{h}");
        }
    }
}
