//! Embedded (progressive) colour image codec.
//!
//! RGB -> YCbCr -> 9/7 wavelet -> bit-planes, most significant first.
//! Within a bit-plane each subband is described by a quadtree that says
//! where coefficients first become significant, then already-significant
//! coefficients get one more bit of precision. All decisions go through
//! an adaptive binary range coder.
//!
//! The byte stream is *embedded*: any prefix of it decodes to a valid
//! picture, and a longer prefix never decodes to a worse description of
//! the coefficients. The fountain layer relies on exactly this.

use crate::image::Image;
use crate::rangecoder::{Decoder, Encoder, Prob};
use crate::wavelet::{forward_2d, inverse_2d, levels_for, subbands, Orient, Subband};

pub const HEADER_LEN: usize = 12;
const MAGIC: [u8; 2] = *b"CX";
const VERSION: u8 = 1;
/// Quantiser step for the last bit-plane. Planes this fine are only
/// reached for small pictures with a generous byte budget.
const STEP: f32 = 0.5;
pub const MAX_DIM: usize = 4096;

struct Stop;
type R<T> = Result<T, Stop>;

trait BitIo {
    const ENC: bool;
    /// Encoder: write `bit` and return it. Decoder: ignore `bit`, return the decoded one.
    fn code(&mut self, p: &mut Prob, bit: u8) -> R<u8>;
}

struct EncIo {
    enc: Encoder,
    stop_at: usize,
}
impl BitIo for EncIo {
    const ENC: bool = true;
    #[inline]
    fn code(&mut self, p: &mut Prob, bit: u8) -> R<u8> {
        if self.enc.out.len() >= self.stop_at {
            return Err(Stop);
        }
        self.enc.encode(p, bit);
        Ok(bit)
    }
}

struct DecIo<'a> {
    dec: Decoder<'a>,
}
impl BitIo for DecIo<'_> {
    const ENC: bool = false;
    #[inline]
    fn code(&mut self, p: &mut Prob, _bit: u8) -> R<u8> {
        if self.dec.exhausted {
            return Err(Stop);
        }
        Ok(self.dec.decode(p))
    }
}

/// Coefficient state of one colour component, in the wavelet layout.
struct Comp {
    mag: Vec<u32>,
    neg: Vec<u8>,
    /// 0 = not yet significant, otherwise (plane where it became significant) + 1.
    sigplane: Vec<u8>,
    /// Lowest bit-plane known so far (decoder).
    low: Vec<u8>,
}

struct Band {
    sb: Subband,
    comp: usize,
    /// Quadtree level sizes; level 0 is the coefficients themselves.
    dims: Vec<(usize, usize)>,
    /// Per level >= 1: has this node been found significant.
    nodesig: Vec<Vec<u8>>,
    /// Encoder only, per level >= 1: bit length of the largest magnitude below the node.
    bitlen: Vec<Vec<u8>>,
}

struct Coder<C: BitIo> {
    io: C,
    w: usize,
    comps: Vec<Comp>,
    bands: Vec<Band>,
    ctx_leaf: Vec<Prob>,
    ctx_node: Vec<Prob>,
    ctx_sign: Vec<Prob>,
    ctx_ref: Vec<Prob>,
}

fn orient_class(o: Orient) -> usize {
    match o {
        Orient::LL => 0,
        Orient::HL | Orient::LH => 1,
        Orient::HH => 2,
    }
}

impl<C: BitIo> Coder<C> {
    fn new(io: C, w: usize, h: usize, levels: usize, mags: Option<Vec<Vec<u32>>>, negs: Option<Vec<Vec<u8>>>) -> Self {
        let n = w * h;
        let mut comps = Vec::new();
        let mut mags = mags.map(|m| m.into_iter());
        let mut negs = negs.map(|m| m.into_iter());
        for _ in 0..3 {
            comps.push(Comp {
                mag: mags.as_mut().and_then(|m| m.next()).unwrap_or_else(|| vec![0; n]),
                neg: negs.as_mut().and_then(|m| m.next()).unwrap_or_else(|| vec![0; n]),
                sigplane: vec![0; n],
                low: vec![0; n],
            });
        }
        let mut bands = Vec::new();
        for sb in subbands(w, h, levels) {
            for comp in 0..3 {
                let mut dims = vec![(sb.w, sb.h)];
                while sb.w > 0 && sb.h > 0 && *dims.last().unwrap() != (1, 1) {
                    let (dw, dh) = *dims.last().unwrap();
                    dims.push((dw.div_ceil(2), dh.div_ceil(2)));
                }
                let nodesig = dims.iter().map(|&(a, b)| vec![0u8; a * b]).collect();
                let mut bitlen: Vec<Vec<u8>> = Vec::new();
                if C::ENC && sb.w > 0 && sb.h > 0 {
                    let m = &comps[comp].mag;
                    bitlen.push(
                        (0..sb.w * sb.h)
                            .map(|i| (32 - m[(sb.y0 + i / sb.w) * w + sb.x0 + i % sb.w].leading_zeros()) as u8)
                            .collect(),
                    );
                    for j in 1..dims.len() {
                        let (pw, ph) = dims[j - 1];
                        let (cw, ch) = dims[j];
                        let prev = &bitlen[j - 1];
                        let mut cur = vec![0u8; cw * ch];
                        for y in 0..ph {
                            for x in 0..pw {
                                let d = &mut cur[(y / 2) * cw + x / 2];
                                *d = (*d).max(prev[y * pw + x]);
                            }
                        }
                        bitlen.push(cur);
                    }
                }
                bands.push(Band { sb, comp, dims, nodesig, bitlen });
            }
        }
        Coder {
            io,
            w,
            comps,
            bands,
            ctx_leaf: vec![Prob::default(); 3 * 12],
            ctx_node: vec![Prob::default(); 3 * 6 * 3],
            ctx_sign: vec![Prob::default(); 4 * 9],
            ctx_ref: vec![Prob::default(); 2],
        }
    }

    /// Significance context of a coefficient from its eight neighbours
    /// inside the same subband, plus the sign context.
    #[inline]
    fn leaf_contexts(&self, b: usize, x: usize, y: usize) -> (usize, usize) {
        let band = &self.bands[b];
        let sb = &band.sb;
        let c = &self.comps[band.comp];
        let at = |dx: isize, dy: isize| -> i32 {
            let (nx, ny) = (x as isize + dx, y as isize + dy);
            if nx < 0 || ny < 0 || nx >= sb.w as isize || ny >= sb.h as isize {
                return 0;
            }
            let i = (sb.y0 + ny as usize) * self.w + sb.x0 + nx as usize;
            if c.sigplane[i] == 0 {
                0
            } else if c.neg[i] != 0 {
                -1
            } else {
                1
            }
        };
        let (l, r, u, d) = (at(-1, 0), at(1, 0), at(0, -1), at(0, 1));
        let hv = (l.abs() + r.abs() + u.abs() + d.abs()) as usize;
        let dg = (at(-1, -1).abs() + at(1, -1).abs() + at(-1, 1).abs() + at(1, 1).abs()) as usize;
        let leaf = orient_class(sb.orient) * 12 + hv.min(3) * 3 + dg.min(2);
        let hs = (l + r).clamp(-1, 1) + 1;
        let vs = (u + d).clamp(-1, 1) + 1;
        let sign = (sb.orient as usize) * 9 + (hs * 3 + vs) as usize;
        (leaf, sign)
    }

    /// Significance pass for one quadtree node at bit-plane `p`.
    /// Returns whether the node is significant afterwards.
    fn sig_node(&mut self, b: usize, j: usize, x: usize, y: usize, p: u32, implied: bool) -> R<bool> {
        if j == 0 {
            let (i, comp) = {
                let band = &self.bands[b];
                ((band.sb.y0 + y) * self.w + band.sb.x0 + x, band.comp)
            };
            if self.comps[comp].sigplane[i] != 0 {
                return Ok(true);
            }
            let (lc, sc) = self.leaf_contexts(b, x, y);
            let truth = if C::ENC { ((self.comps[comp].mag[i] >> p) & 1) as u8 } else { 0 };
            let bit = if implied { 1 } else { self.io.code(&mut self.ctx_leaf[lc], truth)? };
            if bit == 0 {
                return Ok(false);
            }
            let neg = self.io.code(&mut self.ctx_sign[sc], self.comps[comp].neg[i])?;
            let c = &mut self.comps[comp];
            c.neg[i] = neg;
            c.sigplane[i] = p as u8 + 1;
            if !C::ENC {
                c.mag[i] = 1 << p;
                c.low[i] = p as u8;
            }
            return Ok(true);
        }
        let (cw, _) = self.bands[b].dims[j];
        let n = y * cw + x;
        let mut newly = false;
        if self.bands[b].nodesig[j][n] == 0 {
            let bit = if implied {
                1
            } else {
                let band = &self.bands[b];
                let left = x > 0 && band.nodesig[j][n - 1] != 0;
                let up = y > 0 && band.nodesig[j][n - cw] != 0;
                let ctx = (orient_class(band.sb.orient) * 6 + j.min(6) - 1) * 3 + left as usize + up as usize;
                let truth = if C::ENC { (band.bitlen[j][n] as u32 > p) as u8 } else { 0 };
                self.io.code(&mut self.ctx_node[ctx], truth)?
            };
            if bit == 0 {
                return Ok(false);
            }
            self.bands[b].nodesig[j][n] = 1;
            newly = true;
        }
        let (pw, ph) = self.bands[b].dims[j - 1];
        let mut kids = [(0usize, 0usize); 4];
        let mut nk = 0;
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let (kx, ky) = (2 * x + dx, 2 * y + dy);
            if kx < pw && ky < ph {
                kids[nk] = (kx, ky);
                nk += 1;
            }
        }
        let mut any = false;
        for (k, &(kx, ky)) in kids[..nk].iter().enumerate() {
            // A node that just became significant has at least one
            // significant child; if the others were not, the last one is.
            let imp = newly && !any && k + 1 == nk;
            any |= self.sig_node(b, j - 1, kx, ky, p, imp)?;
        }
        Ok(true)
    }

    /// One more magnitude bit for coefficients found in earlier planes.
    fn refine_band(&mut self, b: usize, p: u32) -> R<()> {
        let (sb, comp) = (self.bands[b].sb, self.bands[b].comp);
        for y in 0..sb.h {
            for x in 0..sb.w {
                let i = (sb.y0 + y) * self.w + sb.x0 + x;
                let sp = self.comps[comp].sigplane[i] as u32;
                if sp > p + 1 {
                    let truth = ((self.comps[comp].mag[i] >> p) & 1) as u8;
                    let bit = self.io.code(&mut self.ctx_ref[(sp > p + 2) as usize], truth)?;
                    if !C::ENC {
                        let c = &mut self.comps[comp];
                        c.mag[i] |= (bit as u32) << p;
                        c.low[i] = p as u8;
                    }
                }
            }
        }
        Ok(())
    }

    fn run(&mut self, top_plane: u32) -> R<()> {
        for p in (0..=top_plane).rev() {
            for b in 0..self.bands.len() {
                let depth = self.bands[b].dims.len() - 1;
                if self.bands[b].sb.w == 0 || self.bands[b].sb.h == 0 {
                    continue;
                }
                self.sig_node(b, depth, 0, 0, p, false)?;
            }
            for b in 0..self.bands.len() {
                self.refine_band(b, p)?;
            }
        }
        Ok(())
    }
}

pub struct Info {
    pub w: usize,
    pub h: usize,
    /// Length of the complete stream, header included.
    pub total_len: usize,
}

/// Parse the stream header; `None` until the first 12 bytes are present and valid.
pub fn info(stream: &[u8]) -> Option<Info> {
    if stream.len() < HEADER_LEN || stream[0..2] != MAGIC || stream[2] != VERSION {
        return None;
    }
    let w = u16::from_be_bytes([stream[4], stream[5]]) as usize;
    let h = u16::from_be_bytes([stream[6], stream[7]]) as usize;
    let total_len = (stream[9] as usize) << 16 | (stream[10] as usize) << 8 | stream[11] as usize;
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM || stream[3] as usize != levels_for(w, h) || stream[8] > 30 {
        return None;
    }
    if total_len < HEADER_LEN {
        return None;
    }
    Some(Info { w, h, total_len })
}

/// Encode `img` into at most `max_bytes` bytes (at least the 12-byte header).
pub fn encode(img: &Image, max_bytes: usize) -> Vec<u8> {
    let (w, h) = (img.w, img.h);
    assert!(w >= 1 && h >= 1 && w <= MAX_DIM && h <= MAX_DIM, "image size out of range");
    let max_bytes = max_bytes.clamp(HEADER_LEN, 0xFF_FFFF);
    let levels = levels_for(w, h);
    let n = w * h;
    let mut planes = vec![vec![0f32; n]; 3];
    for (i, px) in img.data.chunks_exact(3).enumerate() {
        let (r, g, b) = (px[0] as f32, px[1] as f32, px[2] as f32);
        planes[0][i] = 0.299 * r + 0.587 * g + 0.114 * b - 128.0;
        planes[1][i] = -0.168_736 * r - 0.331_264 * g + 0.5 * b;
        planes[2][i] = 0.5 * r - 0.418_688 * g - 0.081_312 * b;
    }
    let mut mags = Vec::new();
    let mut negs = Vec::new();
    let mut max_mag = 0u32;
    for pl in planes.iter_mut() {
        forward_2d(pl, w, h, levels);
        let mag: Vec<u32> = pl.iter().map(|v| (v.abs() / STEP) as u32).collect();
        max_mag = max_mag.max(mag.iter().copied().max().unwrap_or(0));
        negs.push(pl.iter().map(|v| (*v < 0.0) as u8).collect::<Vec<u8>>());
        mags.push(mag);
    }
    let top_plane = if max_mag == 0 { 0 } else { 31 - max_mag.leading_zeros() };
    let body_budget = max_bytes - HEADER_LEN;
    // Encode a little past the budget and cut: the result is then a true
    // prefix of a longer stream, so the decoder's "stop when the bytes run
    // out" rule never sees symbols the encoder did not write.
    let io = EncIo { enc: Encoder::new(), stop_at: body_budget + 16 };
    let mut coder = Coder::new(io, w, h, levels, Some(mags), Some(negs));
    let _ = coder.run(top_plane);
    let mut body = coder.io.enc.finish();
    body.truncate(body_budget);
    let total = HEADER_LEN + body.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&MAGIC);
    out.push(VERSION);
    out.push(levels as u8);
    out.extend_from_slice(&(w as u16).to_be_bytes());
    out.extend_from_slice(&(h as u16).to_be_bytes());
    out.push(top_plane as u8);
    out.extend_from_slice(&[(total >> 16) as u8, (total >> 8) as u8, total as u8]);
    out.extend_from_slice(&body);
    out
}

/// Decode a stream or any prefix of one. Returns `None` only if the header
/// is missing or invalid; a header alone gives a flat grey picture.
pub fn decode(stream: &[u8]) -> Option<Image> {
    let inf = info(stream)?;
    let (w, h) = (inf.w, inf.h);
    let levels = stream[3] as usize;
    let top_plane = stream[8] as u32;
    let end = stream.len().min(inf.total_len);
    let io = DecIo { dec: Decoder::new(&stream[HEADER_LEN..end]) };
    let mut coder = Coder::new(io, w, h, levels, None, None);
    let _ = coder.run(top_plane);
    let n = w * h;
    let mut planes = Vec::new();
    for c in &coder.comps {
        let mut pl = vec![0f32; n];
        for i in 0..n {
            if c.sigplane[i] != 0 {
                // Reconstruct inside the uncertainty interval, a little
                // below its middle because small magnitudes are likelier.
                let unit = (1u32 << c.low[i]) as f32;
                let frac = if c.low[i] + 1 == c.sigplane[i] { 0.40 } else { 0.48 };
                let v = (c.mag[i] as f32 + frac * unit) * STEP;
                pl[i] = if c.neg[i] != 0 { -v } else { v };
            }
        }
        inverse_2d(&mut pl, w, h, levels);
        planes.push(pl);
    }
    let mut img = Image::new(w, h);
    for i in 0..n {
        let (y, cb, cr) = (planes[0][i] + 128.0, planes[1][i], planes[2][i]);
        let rgb = [y + 1.402 * cr, y - 0.344_136 * cb - 0.714_136 * cr, y + 1.772 * cb];
        for c in 0..3 {
            img.data[i * 3 + c] = rgb[c].round().clamp(0.0, 255.0) as u8;
        }
    }
    Some(img)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{psnr, synthetic};
    use crate::util::Rng;

    #[test]
    fn quality_rises_with_every_longer_prefix() {
        for name in ["scene", "chart", "clouds"] {
            let img = synthetic(name, 160, 120);
            let stream = encode(&img, 12_000);
            assert!(stream.len() <= 12_000);
            assert_eq!(info(&stream).unwrap().total_len, stream.len());
            let mut prev = 0.0;
            for cut in [12, 40, 100, 300, 1000, 3000, 8000, stream.len()] {
                let cut = cut.min(stream.len());
                let out = decode(&stream[..cut]).expect("prefix must decode");
                let q = psnr(&img, &out);
                assert!(q >= prev - 0.35, "{name}: PSNR fell from {prev:.2} to {q:.2} at {cut} bytes");
                prev = prev.max(q);
            }
            assert!(prev > 24.0, "{name}: only {prev:.1} dB at 12 kB");
        }
    }

    #[test]
    fn every_single_byte_prefix_decodes() {
        let img = synthetic("scene", 48, 40);
        let stream = encode(&img, 1500);
        for cut in 0..=stream.len() {
            let out = decode(&stream[..cut]);
            assert_eq!(out.is_some(), cut >= HEADER_LEN, "cut {cut}");
        }
    }

    #[test]
    fn a_prefix_equals_a_smaller_budget() {
        // Cutting a long stream at N bytes must give the same picture as
        // asking the encoder for N bytes: that is what "embedded" means.
        let img = synthetic("scene", 96, 72);
        let long = encode(&img, 6000);
        for n in [200, 1000, 2500] {
            let short = encode(&img, n);
            assert_eq!(&long[HEADER_LEN..n], &short[HEADER_LEN..n]);
            let a = decode(&long[..n]).unwrap();
            let b = decode(&short).unwrap();
            assert!(a == b, "budget {n}");
        }
    }

    #[test]
    fn large_budget_is_near_lossless_and_odd_sizes_work() {
        for (w, h) in [(1, 1), (3, 2), (17, 9), (31, 64), (50, 50)] {
            let img = synthetic("scene", w, h);
            let stream = encode(&img, 1 << 20);
            let out = decode(&stream).unwrap();
            assert!(psnr(&img, &out) > 44.0, "{w}x{h}: {:.1} dB", psnr(&img, &out));
        }
        let flat = Image::filled(40, 30, [128, 128, 128]);
        let out = decode(&encode(&flat, 4000)).unwrap();
        assert!(psnr(&flat, &out) > 45.0);
    }

    #[test]
    fn garbage_streams_never_panic() {
        let mut rng = Rng::new(51);
        let good = encode(&synthetic("chart", 40, 40), 800);
        for _ in 0..500 {
            let mut b = good.clone();
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(b.len());
                b[i] = rng.next_u64() as u8;
            }
            b.truncate(rng.below(b.len() + 1));
            let _ = decode(&b);
        }
    }
}
