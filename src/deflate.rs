//! DEFLATE (RFC 1951) and the zlib wrapper (RFC 1950), both directions.
//!
//! The decoder handles stored, fixed and dynamic blocks. The encoder does
//! hash-chain LZ77 and writes dynamic-Huffman blocks, which is what makes
//! the PNG files in the report a reasonable size.

use crate::util::adler32;

// ---------------------------------------------------------------- inflate

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bitbuf: u32,
    bitcnt: u32,
}

impl<'a> BitReader<'a> {
    fn bits(&mut self, need: u32) -> Result<u32, String> {
        while self.bitcnt < need {
            let b = *self.data.get(self.pos).ok_or("deflate stream ends early")?;
            self.pos += 1;
            self.bitbuf |= (b as u32) << self.bitcnt;
            self.bitcnt += 8;
        }
        let v = self.bitbuf & ((1u32 << need) - 1);
        self.bitbuf >>= need;
        self.bitcnt -= need;
        Ok(v)
    }
}

struct Huff {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huff {
    /// Canonical code from a list of code lengths (0 = unused symbol).
    fn new(lengths: &[u8]) -> Result<Huff, String> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[l as usize] += 1;
        }
        count[0] = 0;
        // Reject over-subscribed sets; incomplete sets are allowed (a block
        // with a single distance code is legal).
        let mut left = 1i32;
        for c in &count[1..] {
            left = (left << 1) - *c as i32;
            if left < 0 {
                return Err("over-subscribed Huffman code".into());
            }
        }
        let mut offs = [0u16; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + count[l];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbol[offs[l as usize] as usize] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huff { count, symbol })
    }

    fn decode(&self, br: &mut BitReader) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= br.bits(1)? as i32;
            let count = self.count[len] as i32;
            if code - count < first {
                return Ok(self.symbol[(index + (code - first)) as usize]);
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("invalid Huffman code".into())
    }
}

const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193,
    12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

fn fixed_lengths() -> (Vec<u8>, Vec<u8>) {
    let mut l = vec![8u8; 288];
    l[144..256].fill(9);
    l[256..280].fill(7);
    (l, vec![5u8; 30])
}

/// Decompress a raw DEFLATE stream. `limit` bounds the output size so a
/// hostile file cannot exhaust memory.
pub fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let mut br = BitReader { data, pos: 0, bitbuf: 0, bitcnt: 0 };
    let mut out: Vec<u8> = Vec::new();
    loop {
        let last = br.bits(1)?;
        match br.bits(2)? {
            0 => {
                br.bitbuf = 0;
                br.bitcnt = 0;
                if br.pos + 4 > data.len() {
                    return Err("stored block header truncated".into());
                }
                let len = u16::from_le_bytes([data[br.pos], data[br.pos + 1]]) as usize;
                let nlen = u16::from_le_bytes([data[br.pos + 2], data[br.pos + 3]]) as usize;
                if len != (!nlen & 0xFFFF) {
                    return Err("stored block length check failed".into());
                }
                br.pos += 4;
                if br.pos + len > data.len() || out.len() + len > limit {
                    return Err("stored block truncated or too large".into());
                }
                out.extend_from_slice(&data[br.pos..br.pos + len]);
                br.pos += len;
            }
            t @ (1 | 2) => {
                let (ll, dl) = if t == 1 {
                    fixed_lengths()
                } else {
                    let nlen = br.bits(5)? as usize + 257;
                    let ndist = br.bits(5)? as usize + 1;
                    let ncode = br.bits(4)? as usize + 4;
                    if nlen > 286 || ndist > 30 {
                        return Err("bad dynamic block counts".into());
                    }
                    let mut cl = [0u8; 19];
                    for &o in CL_ORDER.iter().take(ncode) {
                        cl[o] = br.bits(3)? as u8;
                    }
                    let clh = Huff::new(&cl)?;
                    let mut lens = vec![0u8; nlen + ndist];
                    let mut i = 0;
                    while i < nlen + ndist {
                        let sym = clh.decode(&mut br)?;
                        let (val, rep) = match sym {
                            0..=15 => (sym as u8, 1),
                            16 => {
                                if i == 0 {
                                    return Err("repeat with no previous length".into());
                                }
                                (lens[i - 1], 3 + br.bits(2)? as usize)
                            }
                            17 => (0, 3 + br.bits(3)? as usize),
                            _ => (0, 11 + br.bits(7)? as usize),
                        };
                        if i + rep > nlen + ndist {
                            return Err("code length repeat overruns".into());
                        }
                        lens[i..i + rep].fill(val);
                        i += rep;
                    }
                    if lens[256] == 0 {
                        return Err("no end-of-block code".into());
                    }
                    let d = lens.split_off(nlen);
                    (lens, d)
                };
                let lh = Huff::new(&ll)?;
                let dh = Huff::new(&dl)?;
                loop {
                    let sym = lh.decode(&mut br)? as usize;
                    if sym < 256 {
                        if out.len() >= limit {
                            return Err("output larger than expected".into());
                        }
                        out.push(sym as u8);
                    } else if sym == 256 {
                        break;
                    } else {
                        let s = sym - 257;
                        if s >= 29 {
                            return Err("invalid length symbol".into());
                        }
                        let len = LEN_BASE[s] as usize + br.bits(LEN_EXTRA[s] as u32)? as usize;
                        let ds = dh.decode(&mut br)? as usize;
                        if ds >= 30 {
                            return Err("invalid distance symbol".into());
                        }
                        let dist = DIST_BASE[ds] as usize + br.bits(DIST_EXTRA[ds] as u32)? as usize;
                        if dist > out.len() {
                            return Err("distance reaches before start of output".into());
                        }
                        if out.len() + len > limit {
                            return Err("output larger than expected".into());
                        }
                        let start = out.len() - dist;
                        for i in 0..len {
                            let b = out[start + i];
                            out.push(b);
                        }
                    }
                }
            }
            _ => return Err("reserved block type".into()),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

pub fn zlib_decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    if data.len() < 6 {
        return Err("zlib stream too short".into());
    }
    if data[0] & 0x0F != 8 || (((data[0] as u16) << 8) | data[1] as u16) % 31 != 0 || data[1] & 0x20 != 0 {
        return Err("bad zlib header".into());
    }
    let out = inflate(&data[2..], limit)?;
    // The Adler-32 trailer is the last four bytes of a well-formed stream.
    let t = &data[data.len() - 4..];
    if u32::from_be_bytes([t[0], t[1], t[2], t[3]]) != adler32(&out) {
        return Err("zlib checksum mismatch".into());
    }
    Ok(out)
}

// ---------------------------------------------------------------- deflate

struct BitWriter {
    out: Vec<u8>,
    bitbuf: u64,
    bitcnt: u32,
}

impl BitWriter {
    fn put(&mut self, value: u32, n: u32) {
        self.bitbuf |= (value as u64) << self.bitcnt;
        self.bitcnt += n;
        while self.bitcnt >= 8 {
            self.out.push(self.bitbuf as u8);
            self.bitbuf >>= 8;
            self.bitcnt -= 8;
        }
    }
    /// Huffman codes go most-significant bit first.
    fn put_code(&mut self, code: u16, len: u8) {
        let rev = (code as u32).reverse_bits() >> (32 - len as u32);
        self.put(rev, len as u32);
    }
    fn finish(mut self) -> Vec<u8> {
        if self.bitcnt > 0 {
            self.out.push(self.bitbuf as u8);
        }
        self.out
    }
}

/// Huffman code lengths for `freq`, none longer than `limit` bits.
fn code_lengths(freq: &[u32], limit: u8) -> Vec<u8> {
    let n = freq.len();
    let mut f: Vec<u32> = freq.to_vec();
    loop {
        let used: Vec<usize> = (0..n).filter(|&i| f[i] > 0).collect();
        let mut len = vec![0u8; n];
        if used.len() <= 1 {
            // A single symbol still needs a one-bit code.
            for &i in &used {
                len[i] = 1;
            }
            return len;
        }
        // Standard Huffman construction on a sorted-merge basis.
        let mut nodes: Vec<(u64, usize, usize)> = Vec::new(); // weight, left, right
        let mut heap: std::collections::BinaryHeap<std::cmp::Reverse<(u64, usize)>> = Default::default();
        for &i in &used {
            nodes.push((f[i] as u64, usize::MAX, i));
            heap.push(std::cmp::Reverse((f[i] as u64, nodes.len() - 1)));
        }
        while heap.len() > 1 {
            let a = heap.pop().unwrap().0;
            let b = heap.pop().unwrap().0;
            nodes.push((a.0 + b.0, a.1, b.1));
            heap.push(std::cmp::Reverse((a.0 + b.0, nodes.len() - 1)));
        }
        let mut stack = vec![(nodes.len() - 1, 0u8)];
        let mut too_long = false;
        while let Some((id, depth)) = stack.pop() {
            let (_, l, r) = nodes[id];
            if l == usize::MAX {
                len[r] = depth;
                too_long |= depth > limit;
            } else {
                stack.push((l, depth + 1));
                stack.push((r, depth + 1));
            }
        }
        if !too_long {
            return len;
        }
        // Flatten the distribution and try again; converges quickly.
        for v in f.iter_mut() {
            if *v > 0 {
                *v = (*v).div_ceil(2);
            }
        }
    }
}

fn canonical_codes(len: &[u8]) -> Vec<u16> {
    let mut count = [0u16; 16];
    for &l in len {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u16; 16];
    let mut code = 0u16;
    for b in 1..16 {
        code = (code + count[b - 1]) << 1;
        next[b] = code;
    }
    len.iter()
        .map(|&l| {
            if l == 0 {
                0
            } else {
                let c = next[l as usize];
                next[l as usize] += 1;
                c
            }
        })
        .collect()
}

#[derive(Clone, Copy)]
enum Token {
    Lit(u8),
    Match(u16, u16), // length, distance
}

fn len_symbol(len: u16) -> usize {
    (0..29).rev().find(|&i| LEN_BASE[i] <= len).unwrap()
}
fn dist_symbol(dist: u16) -> usize {
    (0..30).rev().find(|&i| DIST_BASE[i] <= dist).unwrap()
}

fn lz77(data: &[u8]) -> Vec<Token> {
    const HASH_BITS: usize = 15;
    const WINDOW: usize = 32768;
    const MAX_CHAIN: usize = 24;
    let n = data.len();
    let mut tokens = Vec::with_capacity(n / 2 + 16);
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; n];
    let hash = |i: usize| -> usize {
        let v = (data[i] as u32) | (data[i + 1] as u32) << 8 | (data[i + 2] as u32) << 16;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS as u32)) as usize
    };
    let mut i = 0;
    while i < n {
        let (mut best_len, mut best_dist) = (0usize, 0usize);
        if i + 3 <= n {
            let h = hash(i);
            let mut cand = head[h];
            let max_len = (n - i).min(258);
            let mut chain = 0;
            while cand != usize::MAX && i - cand <= WINDOW && chain < MAX_CHAIN {
                let mut l = 0;
                while l < max_len && data[cand + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - cand;
                    if l == max_len {
                        break;
                    }
                }
                cand = prev[cand];
                chain += 1;
            }
        }
        if best_len >= 3 {
            tokens.push(Token::Match(best_len as u16, best_dist as u16));
            for k in i..i + best_len {
                if k + 3 <= n {
                    let h = hash(k);
                    prev[k] = head[h];
                    head[h] = k;
                }
            }
            i += best_len;
        } else {
            tokens.push(Token::Lit(data[i]));
            if i + 3 <= n {
                let h = hash(i);
                prev[i] = head[h];
                head[h] = i;
            }
            i += 1;
        }
    }
    tokens
}

fn write_block(bw: &mut BitWriter, tokens: &[Token], last: bool) {
    let mut lf = [0u32; 286];
    let mut df = [0u32; 30];
    lf[256] = 1;
    for t in tokens {
        match *t {
            Token::Lit(b) => lf[b as usize] += 1,
            Token::Match(l, d) => {
                lf[257 + len_symbol(l)] += 1;
                df[dist_symbol(d)] += 1;
            }
        }
    }
    let ll = code_lengths(&lf, 15);
    let mut dl = code_lengths(&df, 15);
    if dl.iter().all(|&l| l == 0) {
        dl[0] = 1; // at least one distance code must be described
    }
    let lc = canonical_codes(&ll);
    let dc = canonical_codes(&dl);
    let nlen = (257..=286).rev().find(|&n| ll[n - 1] != 0).unwrap_or(257);
    let ndist = (1..=30).rev().find(|&n| dl[n - 1] != 0).unwrap_or(1);
    // Run-length encode the two length tables as one sequence.
    let all: Vec<u8> = ll[..nlen].iter().chain(dl[..ndist].iter()).copied().collect();
    let mut rle: Vec<(u8, u8)> = Vec::new(); // (symbol, extra value)
    let mut i = 0;
    while i < all.len() {
        let v = all[i];
        let mut run = 1;
        while i + run < all.len() && all[i + run] == v {
            run += 1;
        }
        let mut left = run;
        if v == 0 {
            while left >= 11 {
                let r = left.min(138);
                rle.push((18, (r - 11) as u8));
                left -= r;
            }
            if left >= 3 {
                rle.push((17, (left - 3) as u8));
                left = 0;
            }
        } else {
            rle.push((v, 0));
            left -= 1;
            while left >= 3 {
                let r = left.min(6);
                rle.push((16, (r - 3) as u8));
                left -= r;
            }
        }
        for _ in 0..left {
            rle.push((v, 0));
        }
        i += run;
    }
    let mut cf = [0u32; 19];
    for &(s, _) in &rle {
        cf[s as usize] += 1;
    }
    let cl = code_lengths(&cf, 7);
    let cc = canonical_codes(&cl);
    let ncode = (4..=19).rev().find(|&n| cl[CL_ORDER[n - 1]] != 0).unwrap_or(4);
    bw.put(last as u32, 1);
    bw.put(2, 2);
    bw.put((nlen - 257) as u32, 5);
    bw.put((ndist - 1) as u32, 5);
    bw.put((ncode - 4) as u32, 4);
    for &o in CL_ORDER.iter().take(ncode) {
        bw.put(cl[o] as u32, 3);
    }
    for &(s, extra) in &rle {
        bw.put_code(cc[s as usize], cl[s as usize]);
        match s {
            16 => bw.put(extra as u32, 2),
            17 => bw.put(extra as u32, 3),
            18 => bw.put(extra as u32, 7),
            _ => {}
        }
    }
    for t in tokens {
        match *t {
            Token::Lit(b) => bw.put_code(lc[b as usize], ll[b as usize]),
            Token::Match(l, d) => {
                let s = len_symbol(l);
                bw.put_code(lc[257 + s], ll[257 + s]);
                bw.put((l - LEN_BASE[s]) as u32, LEN_EXTRA[s] as u32);
                let ds = dist_symbol(d);
                bw.put_code(dc[ds], dl[ds]);
                bw.put((d - DIST_BASE[ds]) as u32, DIST_EXTRA[ds] as u32);
            }
        }
    }
    bw.put_code(lc[256], ll[256]);
}

/// Compress to a raw DEFLATE stream.
pub fn deflate(data: &[u8]) -> Vec<u8> {
    let tokens = lz77(data);
    let mut bw = BitWriter { out: Vec::with_capacity(data.len() / 2 + 64), bitbuf: 0, bitcnt: 0 };
    if tokens.is_empty() {
        write_block(&mut bw, &[], true);
    }
    let blocks: Vec<&[Token]> = tokens.chunks(1 << 15).collect();
    for (i, b) in blocks.iter().enumerate() {
        write_block(&mut bw, b, i + 1 == blocks.len());
    }
    bw.finish()
}

pub fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x9C];
    out.extend_from_slice(&deflate(data));
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;

    #[test]
    fn inflates_streams_made_by_zlib() {
        // python3: zlib.compress(b"hello hello hello hello\n", 9).hex()
        let z = [
            0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0xb9, 0x00, 0x70, 0xbe, 0x08, 0xbb,
        ];
        assert_eq!(zlib_decompress(&z, 1000).unwrap(), b"hello hello hello hello\n");
        // A stored block: 01, len=3, nlen, "abc"
        let stored = [0x01, 0x03, 0x00, 0xfc, 0xff, b'a', b'b', b'c'];
        assert_eq!(inflate(&stored, 10).unwrap(), b"abc");
    }

    #[test]
    fn round_trips_varied_data() {
        let mut rng = Rng::new(11);
        let mut cases: Vec<Vec<u8>> = vec![vec![], vec![7], vec![0; 100_000], b"abcabcabcabcabcabc".to_vec()];
        cases.push((0..70_000).map(|_| rng.next_u64() as u8).collect()); // incompressible
        cases.push((0..120_000).map(|i| ((i / 7) % 23) as u8 ^ (rng.below(50) == 0) as u8).collect());
        cases.push((0..50_000).map(|_| (rng.gauss() * 3.0) as i8 as u8).collect()); // PNG-like residuals
        for (i, c) in cases.iter().enumerate() {
            let z = zlib_compress(c);
            let back = zlib_decompress(&z, c.len() + 1).unwrap_or_else(|e| panic!("case {i}: {e}"));
            assert_eq!(&back, c, "case {i}");
        }
        // The compressor must actually compress compressible input.
        assert!(zlib_compress(&vec![0u8; 100_000]).len() < 600);
        assert!(zlib_compress(&cases[6]).len() < cases[6].len() * 6 / 10);
    }

    #[test]
    fn corrupted_streams_return_errors_not_panics() {
        let mut rng = Rng::new(12);
        let src: Vec<u8> = (0..5000).map(|i| (i % 50) as u8).collect();
        let good = zlib_compress(&src);
        for _ in 0..3000 {
            let mut b = good.clone();
            for _ in 0..1 + rng.below(3) {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
            if rng.below(4) == 0 {
                b.truncate(rng.below(b.len()));
            }
            let _ = zlib_decompress(&b, 100_000);
        }
    }

    #[test]
    fn length_limited_codes_respect_the_limit() {
        // Fibonacci-like frequencies force a deep tree.
        let mut f = vec![1u32, 1];
        for i in 2..40 {
            let v = f[i - 1] + f[i - 2];
            f.push(v);
        }
        let l = code_lengths(&f, 15);
        assert!(l.iter().all(|&x| x >= 1 && x <= 15));
        let kraft: f64 = l.iter().map(|&x| 0.5f64.powi(x as i32)).sum();
        assert!(kraft <= 1.0 + 1e-12);
    }
}
