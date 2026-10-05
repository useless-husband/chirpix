//! Radix-2 complex FFT with precomputed tables.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cpx {
    pub re: f64,
    pub im: f64,
}

impl Cpx {
    pub const ZERO: Cpx = Cpx { re: 0.0, im: 0.0 };
    pub fn new(re: f64, im: f64) -> Self {
        Cpx { re, im }
    }
    /// e^{j theta}
    pub fn expj(theta: f64) -> Self {
        Cpx {
            re: theta.cos(),
            im: theta.sin(),
        }
    }
    pub fn conj(self) -> Self {
        Cpx { re: self.re, im: -self.im }
    }
    pub fn norm2(self) -> f64 {
        self.re * self.re + self.im * self.im
    }
    pub fn abs(self) -> f64 {
        self.norm2().sqrt()
    }
    pub fn arg(self) -> f64 {
        self.im.atan2(self.re)
    }
    pub fn scale(self, s: f64) -> Self {
        Cpx {
            re: self.re * s,
            im: self.im * s,
        }
    }
}

impl std::ops::Add for Cpx {
    type Output = Cpx;
    fn add(self, o: Cpx) -> Cpx {
        Cpx {
            re: self.re + o.re,
            im: self.im + o.im,
        }
    }
}
impl std::ops::Sub for Cpx {
    type Output = Cpx;
    fn sub(self, o: Cpx) -> Cpx {
        Cpx {
            re: self.re - o.re,
            im: self.im - o.im,
        }
    }
}
impl std::ops::Mul for Cpx {
    type Output = Cpx;
    fn mul(self, o: Cpx) -> Cpx {
        Cpx {
            re: self.re * o.re - self.im * o.im,
            im: self.re * o.im + self.im * o.re,
        }
    }
}
impl std::ops::AddAssign for Cpx {
    fn add_assign(&mut self, o: Cpx) {
        self.re += o.re;
        self.im += o.im;
    }
}

/// FFT plan for one power-of-two size.
pub struct Fft {
    n: usize,
    rev: Vec<u32>,
    tw: Vec<Cpx>, // e^{-2 pi j k / n}, k < n/2
}

impl Fft {
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 2, "FFT size must be a power of two");
        let bits = n.trailing_zeros();
        let rev = (0..n as u32).map(|i| i.reverse_bits() >> (32 - bits)).collect();
        let tw = (0..n / 2)
            .map(|k| Cpx::expj(-2.0 * std::f64::consts::PI * k as f64 / n as f64))
            .collect();
        Fft { n, rev, tw }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// In-place forward transform: X[k] = sum x[n] e^{-2 pi j k n / N}.
    pub fn forward(&self, x: &mut [Cpx]) {
        self.run(x, false);
    }

    /// In-place inverse transform, including the 1/N factor.
    pub fn inverse(&self, x: &mut [Cpx]) {
        self.run(x, true);
        let s = 1.0 / self.n as f64;
        for v in x.iter_mut() {
            *v = v.scale(s);
        }
    }

    fn run(&self, x: &mut [Cpx], inverse: bool) {
        let n = self.n;
        assert_eq!(x.len(), n);
        for i in 0..n {
            let j = self.rev[i] as usize;
            if i < j {
                x.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= n {
            let half = len / 2;
            let step = n / len;
            for start in (0..n).step_by(len) {
                for k in 0..half {
                    let mut w = self.tw[k * step];
                    if inverse {
                        w = w.conj();
                    }
                    let a = x[start + k];
                    let b = x[start + k + half] * w;
                    x[start + k] = a + b;
                    x[start + k + half] = a - b;
                }
            }
            len <<= 1;
        }
    }
}

/// Direct O(N^2) DFT, the definition the FFT is tested against.
pub fn naive_dft(x: &[Cpx]) -> Vec<Cpx> {
    let n = x.len();
    (0..n)
        .map(|k| {
            let mut acc = Cpx::ZERO;
            for (i, &v) in x.iter().enumerate() {
                acc += v * Cpx::expj(-2.0 * std::f64::consts::PI * (k * i % n) as f64 / n as f64);
            }
            acc
        })
        .collect()
}

/// Linear convolution of two real sequences by FFT (output length a+b-1).
pub fn convolve(a: &[f32], b: &[f32]) -> Vec<f32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    // Overlap-add in blocks so memory stays bounded for minutes of audio.
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let m = short.len();
    let n = (2 * m).next_power_of_two().max(4096);
    let block = n - m + 1;
    let fft = Fft::new(n);
    let mut hs = vec![Cpx::ZERO; n];
    for (i, &v) in short.iter().enumerate() {
        hs[i].re = v as f64;
    }
    fft.forward(&mut hs);
    let mut out = vec![0f32; long.len() + m - 1];
    let mut buf = vec![Cpx::ZERO; n];
    for (bi, chunk) in long.chunks(block).enumerate() {
        buf.fill(Cpx::ZERO);
        for (i, &v) in chunk.iter().enumerate() {
            buf[i].re = v as f64;
        }
        fft.forward(&mut buf);
        for (v, h) in buf.iter_mut().zip(&hs) {
            *v = *v * *h;
        }
        fft.inverse(&mut buf);
        let base = bi * block;
        for i in 0..(chunk.len() + m - 1) {
            out[base + i] += buf[i].re as f32;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;

    #[test]
    fn fft_matches_naive_dft() {
        let mut rng = Rng::new(1);
        for n in [2usize, 4, 8, 64, 256, 1024] {
            let x: Vec<Cpx> = (0..n).map(|_| Cpx::new(rng.gauss(), rng.gauss())).collect();
            let want = naive_dft(&x);
            let mut got = x.clone();
            Fft::new(n).forward(&mut got);
            let err = got.iter().zip(&want).map(|(a, b)| (*a - *b).abs()).fold(0.0, f64::max);
            assert!(err < 1e-9 * n as f64, "n={n} err={err}");
        }
    }

    #[test]
    fn inverse_undoes_forward_and_parseval_holds() {
        let mut rng = Rng::new(2);
        let n = 4096;
        let x: Vec<Cpx> = (0..n).map(|_| Cpx::new(rng.gauss(), rng.gauss())).collect();
        let fft = Fft::new(n);
        let mut y = x.clone();
        fft.forward(&mut y);
        let et: f64 = x.iter().map(|v| v.norm2()).sum();
        let ef: f64 = y.iter().map(|v| v.norm2()).sum::<f64>() / n as f64;
        assert!((et / ef - 1.0).abs() < 1e-12);
        fft.inverse(&mut y);
        let err = x.iter().zip(&y).map(|(a, b)| (*a - *b).abs()).fold(0.0, f64::max);
        assert!(err < 1e-10);
    }

    #[test]
    fn single_tone_lands_in_one_bin() {
        let n = 1024;
        let k = 37;
        let mut x: Vec<Cpx> = (0..n)
            .map(|i| Cpx::expj(2.0 * std::f64::consts::PI * (k * i) as f64 / n as f64))
            .collect();
        Fft::new(n).forward(&mut x);
        for (i, v) in x.iter().enumerate() {
            let want = if i == k { n as f64 } else { 0.0 };
            assert!((v.abs() - want).abs() < 1e-8);
        }
    }

    #[test]
    fn fft_convolution_matches_direct() {
        let mut rng = Rng::new(3);
        let a: Vec<f32> = (0..9000).map(|_| rng.gauss() as f32).collect();
        let b: Vec<f32> = (0..300).map(|_| rng.gauss() as f32).collect();
        let got = convolve(&a, &b);
        assert_eq!(got.len(), a.len() + b.len() - 1);
        for &i in &[0usize, 1, 299, 300, 4500, 8999, 9298] {
            let mut want = 0f64;
            for (j, &bv) in b.iter().enumerate() {
                if i >= j && i - j < a.len() {
                    want += a[i - j] as f64 * bv as f64;
                }
            }
            assert!((got[i] as f64 - want).abs() < 1e-3, "i={i}");
        }
    }
}
