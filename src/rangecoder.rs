//! Adaptive binary range coder (the carry-propagating byte-wise design
//! used by LZMA), with one property the image codec depends on: decoding a
//! *prefix* of the encoder's output yields a prefix of the encoded bits.
//!
//! The decoder keeps a 32-bit window into the byte stream. A bit decided
//! while that window holds only real bytes is exactly the bit the encoder
//! wrote. As soon as the decoder would have to read past the end of the
//! data it raises `exhausted`, and the caller must stop before using any
//! further bit.

/// Probability that the next bit is 0, as two exponential averages with
/// different speeds (fast one adapts, slow one is precise).
#[derive(Clone, Copy)]
pub struct Prob {
    fast: u16,
    slow: u16,
}

impl Default for Prob {
    fn default() -> Self {
        Prob { fast: 1 << 15, slow: 1 << 15 }
    }
}

impl Prob {
    #[inline]
    fn p0(&self) -> u32 {
        ((self.fast as u32 + self.slow as u32) >> 1).clamp(48, 65536 - 48)
    }
    #[inline]
    fn update(&mut self, bit: u8) {
        if bit == 0 {
            self.fast += ((65536 - self.fast as u32) >> 4) as u16;
            self.slow += ((65536 - self.slow as u32) >> 7) as u16;
        } else {
            self.fast -= self.fast >> 4;
            self.slow -= self.slow >> 7;
        }
    }
}

pub struct Encoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    pub out: Vec<u8>,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    pub fn new() -> Self {
        Encoder { low: 0, range: 0xFFFF_FFFF, cache: 0, cache_size: 1, out: Vec::new() }
    }

    #[inline]
    pub fn encode(&mut self, prob: &mut Prob, bit: u8) {
        let bound = (self.range >> 16) * prob.p0();
        if bit == 0 {
            self.range = bound;
        } else {
            self.low += bound as u64;
            self.range -= bound;
        }
        prob.update(bit);
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.shift_low();
        }
    }

    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.out.push(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        self.out
    }
}

pub struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    range: u32,
    code: u32,
    /// Set once the decoder has had to invent a byte; bits decoded after
    /// this point are meaningless.
    pub exhausted: bool,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = Decoder { data, pos: 0, range: 0xFFFF_FFFF, code: 0, exhausted: false };
        for _ in 0..5 {
            d.code = (d.code << 8) | d.next() as u32;
        }
        d
    }

    #[inline]
    fn next(&mut self) -> u8 {
        if self.pos < self.data.len() {
            self.pos += 1;
            self.data[self.pos - 1]
        } else {
            self.exhausted = true;
            0
        }
    }

    /// Decode one bit. Only valid if `exhausted` was false before the call.
    #[inline]
    pub fn decode(&mut self, prob: &mut Prob) -> u8 {
        let bound = (self.range >> 16) * prob.p0();
        let bit = if self.code < bound {
            self.range = bound;
            0
        } else {
            self.code -= bound;
            self.range -= bound;
            1
        };
        prob.update(bit);
        while self.range < (1 << 24) {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next() as u32;
        }
        bit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;

    fn source(rng: &mut Rng, n: usize) -> Vec<(usize, u8)> {
        // Several contexts with very different statistics, including one
        // that is almost always 0 (the common case in bit-plane coding).
        (0..n)
            .map(|_| {
                let ctx = rng.below(4);
                let p1 = [0.5, 0.1, 0.002, 0.97][ctx];
                (ctx, (rng.f64() < p1) as u8)
            })
            .collect()
    }

    #[test]
    fn round_trip_and_compression_near_entropy() {
        let mut rng = Rng::new(31);
        let syms = source(&mut rng, 400_000);
        let mut probs = [Prob::default(); 4];
        let mut enc = Encoder::new();
        for &(c, b) in &syms {
            enc.encode(&mut probs[c], b);
        }
        let bytes = enc.finish();
        let h = |p: f64| -(p * p.log2() + (1.0 - p) * (1.0 - p).log2());
        let entropy_bits = 100_000.0 * (h(0.5) + h(0.1) + h(0.002) + h(0.97));
        let ratio = bytes.len() as f64 * 8.0 / entropy_bits;
        assert!(ratio < 1.04, "coded size is {ratio:.3} x entropy");
        let mut probs = [Prob::default(); 4];
        let mut dec = Decoder::new(&bytes);
        for (i, &(c, b)) in syms.iter().enumerate() {
            assert!(!dec.exhausted, "ran dry at symbol {i}");
            assert_eq!(dec.decode(&mut probs[c]), b, "symbol {i}");
        }
    }

    #[test]
    fn every_prefix_decodes_to_a_prefix() {
        let mut rng = Rng::new(32);
        let syms = source(&mut rng, 3000);
        let mut probs = [Prob::default(); 4];
        let mut enc = Encoder::new();
        for &(c, b) in &syms {
            enc.encode(&mut probs[c], b);
        }
        let bytes = enc.finish();
        let mut last = 0;
        for cut in 0..=bytes.len() {
            let mut probs = [Prob::default(); 4];
            let mut dec = Decoder::new(&bytes[..cut]);
            let mut n = 0;
            while !dec.exhausted && n < syms.len() {
                let (c, b) = syms[n];
                assert_eq!(dec.decode(&mut probs[c]), b, "cut {cut} symbol {n}");
                n += 1;
            }
            assert!(n >= last, "more bytes must never decode fewer symbols");
            last = n;
        }
        assert_eq!(last, syms.len());
    }
}
