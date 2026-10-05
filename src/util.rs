//! Small shared helpers: a seeded random generator, CRCs and base64.

/// SplitMix64. Every random choice in the project (test data, channel noise,
/// fountain code combinations) comes from this generator with an explicit
/// seed, so every result is reproducible.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Uniform integer in [0, n).
    pub fn below(&mut self, n: usize) -> usize {
        (((self.next_u64() >> 32) * n as u64) >> 32) as usize
    }
    pub fn bit(&mut self) -> u8 {
        (self.next_u64() >> 63) as u8
    }
    /// Standard normal (Box-Muller).
    pub fn gauss(&mut self) -> f64 {
        let u1 = 1.0 - self.f64();
        let u2 = self.f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

fn crc32_table() -> &'static [u32; 256] {
    static T: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    })
}

/// CRC-32 (IEEE 802.3, the one PNG and zlib use), continuing from `crc`.
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let t = crc32_table();
    let mut c = !crc;
    for &b in data {
        c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

/// CRC-16/CCITT-FALSE (polynomial 0x1021, initial value 0xFFFF).
pub fn crc16(data: &[u8]) -> u16 {
    let mut c: u16 = 0xFFFF;
    for &b in data {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x1021 } else { c << 1 };
        }
    }
    c
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5000) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

pub fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(A[(n >> 18) as usize & 63] as char);
        s.push(A[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    s
}

/// Gaussian tail probability Q(x) = P(N(0,1) > x), used for the textbook
/// error-rate curves. Abramowitz-Stegun 7.1.26 erfc, relative error < 2e-7
/// over the range the curves use.
pub fn q_func(x: f64) -> f64 {
    0.5 * erfc(x / std::f64::consts::SQRT_2)
}

pub fn erfc(x: f64) -> f64 {
    // Numerical Recipes erfc (Chebyshev fit), fractional error < 1.2e-7.
    let z = x.abs();
    let t = 1.0 / (1.0 + 0.5 * z);
    let poly = -z * z - 1.26551223
        + t * (1.00002368
            + t * (0.37409196
                + t * (0.09678418
                    + t * (-0.18628806 + t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277))))))));
    let ans = t * poly.exp();
    if x >= 0.0 {
        ans
    } else {
        2.0 - ans
    }
}

/// Run `jobs` closures on at most `threads` worker threads, results in order.
pub fn parallel_map<T: Send, R: Send>(items: Vec<T>, threads: usize, f: impl Fn(T) -> R + Sync) -> Vec<R> {
    let n = items.len();
    let queue = std::sync::Mutex::new(items.into_iter().enumerate().collect::<Vec<_>>());
    let out = std::sync::Mutex::new((0..n).map(|_| None).collect::<Vec<Option<R>>>());
    std::thread::scope(|s| {
        for _ in 0..threads.clamp(1, 4).min(n.max(1)) {
            s.spawn(|| loop {
                let job = queue.lock().unwrap().pop();
                match job {
                    Some((i, item)) => {
                        let r = f(item);
                        out.lock().unwrap()[i] = Some(r);
                    }
                    None => break,
                }
            });
        }
    });
    out.into_inner().unwrap().into_iter().map(|r| r.unwrap()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_known_vectors() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc16(b"123456789"), 0x29B1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn q_function_matches_table() {
        // Values from standard normal tables.
        for (x, q) in [
            (0.0, 0.5),
            (1.0, 0.158_655_25),
            (2.0, 0.022_750_13),
            (3.0, 1.349_898e-3),
            (4.0, 3.167_124e-5),
        ] {
            assert!((q_func(x) / q - 1.0).abs() < 1e-5, "Q({x})");
        }
    }

    #[test]
    fn rng_is_deterministic_and_roughly_normal() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        assert_eq!(a.next_u64(), b.next_u64());
        let n = 200_000;
        let (mut s, mut s2) = (0.0, 0.0);
        for _ in 0..n {
            let g = a.gauss();
            s += g;
            s2 += g * g;
        }
        assert!((s / n as f64).abs() < 0.01);
        assert!((s2 / n as f64 - 1.0).abs() < 0.02);
    }
}
