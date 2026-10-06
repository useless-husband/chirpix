//! RGB image container, quality metrics (PSNR, SSIM) and the procedural
//! test images used when no photographs are available (tests and CI).

use crate::util::Rng;

#[derive(Clone, PartialEq)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    /// Row-major RGB, 8 bits per sample.
    pub data: Vec<u8>,
}

impl Image {
    pub fn new(w: usize, h: usize) -> Self {
        Image {
            w,
            h,
            data: vec![0; w * h * 3],
        }
    }

    pub fn filled(w: usize, h: usize, rgb: [u8; 3]) -> Self {
        let mut img = Image::new(w, h);
        for p in img.data.chunks_exact_mut(3) {
            p.copy_from_slice(&rgb);
        }
        img
    }

    pub fn px(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.w + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    pub fn set(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        let i = (y * self.w + x) * 3;
        self.data[i..i + 3].copy_from_slice(&rgb);
    }

    /// Shrink by an integer factor with box averaging.
    pub fn downscale(&self, f: usize) -> Image {
        if f <= 1 {
            return self.clone();
        }
        let (w, h) = ((self.w / f).max(1), (self.h / f).max(1));
        let mut out = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0u32; 3];
                let mut n = 0;
                for yy in y * f..((y + 1) * f).min(self.h) {
                    for xx in x * f..((x + 1) * f).min(self.w) {
                        let p = self.px(xx, yy);
                        for c in 0..3 {
                            acc[c] += p[c] as u32;
                        }
                        n += 1;
                    }
                }
                out.set(x, y, [(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8]);
            }
        }
        out
    }

    /// Enlarge by an integer factor with pixel replication.
    pub fn upscale(&self, f: usize) -> Image {
        let mut out = Image::new(self.w * f, self.h * f);
        for y in 0..out.h {
            for x in 0..out.w {
                out.set(x, y, self.px(x / f, y / f));
            }
        }
        out
    }

    /// Resize to any size: area averaging when shrinking an axis, linear
    /// interpolation between pixel centres when enlarging it.
    pub fn resize(&self, w: usize, h: usize) -> Image {
        if (w, h) == (self.w, self.h) {
            return self.clone();
        }
        let (wx, wy) = (resize_weights(self.w, w), resize_weights(self.h, h));
        // Rows first, into floats; then columns.
        let mut tmp = vec![0f32; w * self.h * 3];
        for y in 0..self.h {
            for (x, taps) in wx.iter().enumerate() {
                for c in 0..3 {
                    tmp[(y * w + x) * 3 + c] = taps.iter().map(|&(i, g)| g * self.data[(y * self.w + i) * 3 + c] as f32).sum();
                }
            }
        }
        let mut out = Image::new(w, h);
        for (y, taps) in wy.iter().enumerate() {
            for x in 0..w {
                for c in 0..3 {
                    let v: f32 = taps.iter().map(|&(i, g)| g * tmp[(i * w + x) * 3 + c]).sum();
                    out.data[(y * w + x) * 3 + c] = v.round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        out
    }

    /// BT.601 luma in [0, 255].
    pub fn luma(&self) -> Vec<f32> {
        self.data
            .chunks_exact(3)
            .map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32)
            .collect()
    }
}

/// Input samples and weights for each output sample when `n_in` samples
/// become `n_out`. Each row of weights sums to one.
fn resize_weights(n_in: usize, n_out: usize) -> Vec<Vec<(usize, f32)>> {
    let scale = n_in as f64 / n_out as f64;
    (0..n_out)
        .map(|o| {
            let mut taps = Vec::new();
            if scale > 1.0 {
                // Output sample o covers input [o * scale, (o + 1) * scale).
                let (a, b) = (o as f64 * scale, (o + 1) as f64 * scale);
                for i in a.floor() as usize..(b.ceil() as usize).min(n_in) {
                    let cover = (b.min(i as f64 + 1.0) - a.max(i as f64)).max(0.0);
                    if cover > 0.0 {
                        taps.push((i, (cover / scale) as f32));
                    }
                }
            } else {
                let c = ((o as f64 + 0.5) * scale - 0.5).clamp(0.0, (n_in - 1) as f64);
                let i = c.floor() as usize;
                let f = c - i as f64;
                taps.push((i, (1.0 - f) as f32));
                if f > 0.0 {
                    taps.push((i + 1, f as f32));
                }
            }
            taps
        })
        .collect()
}

/// Peak signal-to-noise ratio in dB over all RGB samples. Capped at 99 dB
/// for identical images.
pub fn psnr(a: &Image, b: &Image) -> f64 {
    assert_eq!((a.w, a.h), (b.w, b.h), "PSNR needs equal sizes");
    let mut se = 0u64;
    for (x, y) in a.data.iter().zip(&b.data) {
        let d = *x as i64 - *y as i64;
        se += (d * d) as u64;
    }
    if se == 0 {
        return 99.0;
    }
    let mse = se as f64 / a.data.len() as f64;
    (10.0 * (255.0 * 255.0 / mse).log10()).min(99.0)
}

/// Mean structural similarity (Wang et al. 2004) on luma: 11x11 Gaussian
/// window with sigma 1.5, K1 = 0.01, K2 = 0.03, evaluated where the window
/// fits entirely inside the image.
pub fn ssim(a: &Image, b: &Image) -> f64 {
    assert_eq!((a.w, a.h), (b.w, b.h), "SSIM needs equal sizes");
    let (w, h) = (a.w, a.h);
    if w < 11 || h < 11 {
        return if a == b { 1.0 } else { 0.0 };
    }
    let mut g = [0f32; 11];
    let mut sum = 0.0;
    for (i, v) in g.iter_mut().enumerate() {
        let d = i as f32 - 5.0;
        *v = (-d * d / (2.0 * 1.5 * 1.5)).exp();
        sum += *v;
    }
    for v in g.iter_mut() {
        *v /= sum;
    }
    let (ow, oh) = (w - 10, h - 10);
    let blur = |src: &[f32]| -> Vec<f32> {
        let mut tmp = vec![0f32; ow * h];
        for y in 0..h {
            for x in 0..ow {
                let mut acc = 0.0;
                for (k, gv) in g.iter().enumerate() {
                    acc += gv * src[y * w + x + k];
                }
                tmp[y * ow + x] = acc;
            }
        }
        let mut out = vec![0f32; ow * oh];
        for y in 0..oh {
            for x in 0..ow {
                let mut acc = 0.0;
                for (k, gv) in g.iter().enumerate() {
                    acc += gv * tmp[(y + k) * ow + x];
                }
                out[y * ow + x] = acc;
            }
        }
        out
    };
    let (la, lb) = (a.luma(), b.luma());
    let mul = |p: &[f32], q: &[f32]| -> Vec<f32> { p.iter().zip(q).map(|(x, y)| x * y).collect() };
    let (ma, mb) = (blur(&la), blur(&lb));
    let (saa, sbb, sab) = (blur(&mul(&la, &la)), blur(&mul(&lb, &lb)), blur(&mul(&la, &lb)));
    let (c1, c2) = ((0.01f64 * 255.0).powi(2), (0.03f64 * 255.0).powi(2));
    let mut total = 0f64;
    for i in 0..ow * oh {
        let (mx, my) = (ma[i] as f64, mb[i] as f64);
        let vx = saa[i] as f64 - mx * mx;
        let vy = sbb[i] as f64 - my * my;
        let cxy = sab[i] as f64 - mx * my;
        total += ((2.0 * mx * my + c1) * (2.0 * cxy + c2)) / ((mx * mx + my * my + c1) * (vx + vy + c2));
    }
    total / (ow * oh) as f64
}

/// Procedural test pictures, fixed for a given name and size:
/// `scene` (sky, hills, water, a textured field), `chart` (zone plate, bars,
/// colour patches: hard edges and fine detail), `clouds` (smooth colour).
pub fn synthetic(name: &str, w: usize, h: usize) -> Image {
    let mut img = Image::new(w, h);
    let mut rng = Rng::new(0xC41F);
    let clamp = |v: f64| v.clamp(0.0, 255.0) as u8;
    match name {
        "chart" => {
            for y in 0..h {
                for x in 0..w {
                    let (u, v) = (x as f64 / w as f64, y as f64 / h as f64);
                    let rgb = if v < 0.5 && u < 0.5 {
                        // zone plate: spatial frequency rises with radius
                        let (dx, dy) = (x as f64 - w as f64 * 0.25, y as f64 - h as f64 * 0.25);
                        let r2 = dx * dx + dy * dy;
                        let g = 127.5 + 127.5 * (r2 * std::f64::consts::PI / (w as f64 * 0.6)).cos();
                        [g, g, g]
                    } else if v < 0.5 {
                        // vertical bars, period shrinking to the right
                        let period = 2.0 + 30.0 * (1.0 - u) * (1.0 - u) * 4.0;
                        let on = ((x as f64 / period) as usize) % 2 == 0;
                        if on {
                            [240.0, 240.0, 240.0]
                        } else {
                            [20.0, 20.0, 30.0]
                        }
                    } else if u < 0.6 {
                        // colour patches with hard edges
                        let (i, j) = ((u / 0.6 * 6.0) as usize, ((v - 0.5) / 0.5 * 3.0) as usize);
                        let k = i + j * 6;
                        [(k * 53 % 256) as f64, (k * 101 % 256) as f64, (255 - k * 29 % 256) as f64]
                    } else {
                        // text-like strokes on white
                        let (cx, cy) = (x / 3, y / 5);
                        let ink = (cx * 7 + cy * 13) % 5 < 2 && (y % 5) < 3 && cy % 2 == 0;
                        if ink {
                            [15.0, 15.0, 15.0]
                        } else {
                            [250.0, 250.0, 245.0]
                        }
                    };
                    img.set(x, y, [clamp(rgb[0]), clamp(rgb[1]), clamp(rgb[2])]);
                }
            }
        }
        "clouds" => {
            let waves: Vec<[f64; 5]> = (0..18)
                .map(|i| {
                    let f = 1.0 + (i / 3) as f64 * 1.7;
                    let ang = rng.f64() * std::f64::consts::TAU;
                    [
                        f * ang.cos(),
                        f * ang.sin(),
                        rng.f64() * std::f64::consts::TAU,
                        1.0 / f,
                        (i % 3) as f64,
                    ]
                })
                .collect();
            for y in 0..h {
                for x in 0..w {
                    let (u, v) = (x as f64 / w.max(h) as f64, y as f64 / w.max(h) as f64);
                    let mut c = [128.0f64; 3];
                    for wv in &waves {
                        let s = (std::f64::consts::TAU * (wv[0] * u + wv[1] * v) + wv[2]).sin() * wv[3] * 70.0;
                        c[wv[4] as usize] += s;
                        c[(wv[4] as usize + 1) % 3] += s * 0.4;
                    }
                    img.set(x, y, [clamp(c[0]), clamp(c[1]), clamp(c[2])]);
                }
            }
        }
        _ => {
            let noise: Vec<f64> = (0..w * h).map(|_| rng.gauss()).collect();
            for y in 0..h {
                for x in 0..w {
                    let (u, v) = (x as f64 / w as f64, y as f64 / h as f64);
                    let ridge = 0.45 + 0.12 * (u * 7.0).sin() + 0.06 * (u * 23.0 + 1.0).sin();
                    let ridge2 = 0.58 + 0.07 * (u * 11.0 + 2.0).sin();
                    let (sx, sy) = (u - 0.72, (v - 0.2) * h as f64 / w as f64);
                    let sun = sx * sx + sy * sy < 0.004;
                    let mut c = if sun {
                        [255.0, 240.0, 170.0]
                    } else if v < ridge {
                        [90.0 + 120.0 * v, 140.0 + 100.0 * v, 230.0 - 40.0 * v]
                    } else if v < ridge2 {
                        let t = 6.0 * noise[y * w + x];
                        [70.0 + t, 95.0 + t + 40.0 * (v - ridge), 80.0 + t]
                    } else if v < 0.8 {
                        let ripple = 18.0 * ((v * 140.0) + 3.0 * (u * 9.0).sin()).sin();
                        [40.0 + ripple, 90.0 + ripple, 150.0 + ripple + 60.0 * (v - ridge2)]
                    } else {
                        let t = 22.0 * noise[y * w + x];
                        let check = ((x / 8) + (y / 8)) % 2 == 0 && u > 0.6;
                        if check {
                            [200.0, 60.0, 50.0]
                        } else {
                            [120.0 + t, 150.0 + t, 60.0 + t * 0.5]
                        }
                    };
                    if !sun && v < ridge {
                        let glow = 60.0 * (-(sx * sx + sy * sy) * 60.0).exp();
                        c[0] += glow;
                        c[1] += glow * 0.8;
                    }
                    img.set(x, y, [clamp(c[0]), clamp(c[1]), clamp(c[2])]);
                }
            }
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psnr_of_known_error() {
        let a = Image::filled(16, 16, [100, 100, 100]);
        let b = Image::filled(16, 16, [110, 100, 100]);
        // MSE = 100/3
        let want = 10.0 * (255.0f64 * 255.0 / (100.0 / 3.0)).log10();
        assert!((psnr(&a, &b) - want).abs() < 1e-9);
        assert_eq!(psnr(&a, &a), 99.0);
    }

    #[test]
    fn ssim_properties() {
        let a = synthetic("scene", 96, 64);
        assert!((ssim(&a, &a) - 1.0).abs() < 1e-6);
        // A constant brightness shift keeps structure: SSIM stays high and
        // matches the closed form for constant images.
        let c1 = Image::filled(32, 32, [100, 100, 100]);
        let c2 = Image::filled(32, 32, [120, 120, 120]);
        let k = (0.01f64 * 255.0).powi(2);
        let want = (2.0 * 100.0 * 120.0 + k) / (100.0 * 100.0 + 120.0 * 120.0 + k);
        assert!((ssim(&c1, &c2) - want).abs() < 1e-4);
        // More noise must give lower SSIM.
        let mut rng = Rng::new(3);
        let mut prev = 1.0;
        for amp in [4.0, 12.0, 40.0] {
            let mut n = a.clone();
            for v in n.data.iter_mut() {
                *v = (*v as f64 + amp * rng.gauss()).clamp(0.0, 255.0) as u8;
            }
            let s = ssim(&a, &n);
            assert!(s < prev, "amp {amp}: {s} !< {prev}");
            prev = s;
        }
    }

    #[test]
    fn scaling_helpers() {
        let a = synthetic("clouds", 64, 48);
        let d = a.downscale(2);
        assert_eq!((d.w, d.h), (32, 24));
        let u = d.upscale(2);
        assert_eq!((u.w, u.h), (64, 48));
        assert!(psnr(&a, &u) > 30.0);
    }

    #[test]
    fn resize_keeps_flat_areas_and_smooth_pictures() {
        let g = Image::filled(768, 512, [10, 128, 250]);
        assert!(g.resize(320, 240).data.chunks_exact(3).all(|p| p == [10, 128, 250]));
        assert!(g.resize(320, 240).resize(768, 512) == g);
        // Non-integer factors both ways, as SSTV needs (768x512 -> 320x240 -> back).
        let a = synthetic("clouds", 768, 512);
        let s = a.resize(320, 240);
        assert_eq!((s.w, s.h), (320, 240));
        assert!(psnr(&a, &s.resize(768, 512)) > 35.0);
        // Area averaging: a one-pixel checkerboard shrinks to mid-grey.
        let mut c = Image::new(64, 64);
        for y in 0..64 {
            for x in 0..64 {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                c.set(x, y, [v, v, v]);
            }
        }
        assert!(c.resize(16, 16).data.iter().all(|&v| v == 128 || v == 127));
    }
}
