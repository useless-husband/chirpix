//! PNG reader and writer built on this crate's own DEFLATE.
//!
//! Reads non-interlaced PNGs of every colour type at 8 or 16 bits per
//! sample (and 1/2/4-bit grey or palette); alpha is dropped. Writes 8-bit
//! RGB with per-row adaptive filtering.

use crate::deflate::{zlib_compress, zlib_decompress};
use crate::image::Image;
use crate::util::{crc32, crc32_update};

const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (a as i32, b as i32, c as i32);
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

pub fn decode(bytes: &[u8]) -> Result<Image, String> {
    if bytes.len() < 8 || bytes[..8] != SIG {
        return Err("not a PNG file".into());
    }
    let mut pos = 8;
    let mut ihdr: Option<(usize, usize, u8, u8)> = None;
    let mut palette: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    let mut ended = false;
    while pos + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]]) as usize;
        let kind = &bytes[pos + 4..pos + 8];
        if pos + 12 + len > bytes.len() {
            return Err("PNG chunk runs past end of file".into());
        }
        let body = &bytes[pos + 8..pos + 8 + len];
        let crc = u32::from_be_bytes([
            bytes[pos + 8 + len],
            bytes[pos + 9 + len],
            bytes[pos + 10 + len],
            bytes[pos + 11 + len],
        ]);
        if crc32_update(crc32(kind), body) != crc {
            return Err("PNG chunk CRC mismatch".into());
        }
        match kind {
            b"IHDR" => {
                if len != 13 {
                    return Err("bad IHDR".into());
                }
                let w = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
                let h = u32::from_be_bytes([body[4], body[5], body[6], body[7]]) as usize;
                if w == 0 || h == 0 || w > 16384 || h > 16384 {
                    return Err("PNG dimensions out of range (max 16384)".into());
                }
                if body[10] != 0 || body[11] != 0 {
                    return Err("unsupported PNG compression/filter method".into());
                }
                if body[12] != 0 {
                    return Err("interlaced PNG is not supported; re-save it without interlacing".into());
                }
                ihdr = Some((w, h, body[8], body[9]));
            }
            b"PLTE" => palette = body.to_vec(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => {
                ended = true;
                break;
            }
            _ => {}
        }
        pos += 12 + len;
    }
    let (w, h, depth, ctype) = ihdr.ok_or("PNG has no IHDR")?;
    if !ended {
        return Err("PNG has no IEND".into());
    }
    let channels = match ctype {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err("bad PNG colour type".into()),
    };
    let ok_depth = match ctype {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        _ => matches!(depth, 8 | 16),
    };
    if !ok_depth {
        return Err("bad PNG bit depth".into());
    }
    let bpp = (channels * depth as usize).div_ceil(8); // bytes per pixel for filtering
    let stride = (w * channels * depth as usize).div_ceil(8);
    if (stride + 1) * h > 400 << 20 {
        return Err("PNG is too large (more than 400 MB of pixel data)".into());
    }
    let raw = zlib_decompress(&idat, (stride + 1) * h)?;
    if raw.len() != (stride + 1) * h {
        return Err("PNG pixel data has the wrong size".into());
    }
    // Undo the row filters in place.
    let mut cur = vec![0u8; stride];
    let mut prev = vec![0u8; stride];
    let mut img = Image::new(w, h);
    for y in 0..h {
        let line = &raw[y * (stride + 1)..(y + 1) * (stride + 1)];
        let ft = line[0];
        for i in 0..stride {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            let x = line[1 + i];
            cur[i] = match ft {
                0 => x,
                1 => x.wrapping_add(a),
                2 => x.wrapping_add(b),
                3 => x.wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => x.wrapping_add(paeth(a, b, c)),
                _ => return Err("bad PNG filter type".into()),
            };
        }
        for x in 0..w {
            // Fetch sample `s` of pixel x as 8 bits.
            let sample = |s: usize| -> u8 {
                match depth {
                    8 => cur[x * channels + s],
                    16 => cur[(x * channels + s) * 2],
                    d => {
                        let bit = x * d as usize;
                        let v = (cur[bit / 8] >> (8 - d as usize - bit % 8)) & ((1u8 << d) - 1);
                        if ctype == 3 {
                            v
                        } else {
                            (v as u32 * 255 / ((1u32 << d) - 1)) as u8
                        }
                    }
                }
            };
            let rgb = match ctype {
                0 | 4 => {
                    let g = sample(0);
                    [g, g, g]
                }
                3 => {
                    let i = sample(0) as usize * 3;
                    if i + 2 >= palette.len() {
                        return Err("palette index out of range".into());
                    }
                    [palette[i], palette[i + 1], palette[i + 2]]
                }
                _ => [sample(0), sample(1), sample(2)],
            };
            img.data[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&rgb);
        }
        std::mem::swap(&mut cur, &mut prev);
    }
    Ok(img)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out.extend_from_slice(&crc32_update(crc32(kind), body).to_be_bytes());
}

pub fn encode(img: &Image) -> Vec<u8> {
    let (w, h) = (img.w, img.h);
    let stride = w * 3;
    let mut raw = Vec::with_capacity((stride + 1) * h);
    let zero = vec![0u8; stride];
    let mut cand = vec![0u8; stride];
    let mut best = vec![0u8; stride];
    for y in 0..h {
        let cur = &img.data[y * stride..(y + 1) * stride];
        let prev = if y > 0 {
            &img.data[(y - 1) * stride..y * stride]
        } else {
            &zero[..]
        };
        let (mut best_ft, mut best_cost) = (0u8, u64::MAX);
        for ft in 0..5u8 {
            let mut cost = 0u64;
            for i in 0..stride {
                let a = if i >= 3 { cur[i - 3] } else { 0 };
                let b = prev[i];
                let c = if i >= 3 { prev[i - 3] } else { 0 };
                let v = match ft {
                    0 => cur[i],
                    1 => cur[i].wrapping_sub(a),
                    2 => cur[i].wrapping_sub(b),
                    3 => cur[i].wrapping_sub(((a as u16 + b as u16) / 2) as u8),
                    _ => cur[i].wrapping_sub(paeth(a, b, c)),
                };
                cand[i] = v;
                cost += (v as i8).unsigned_abs() as u64;
            }
            if cost < best_cost {
                best_cost = cost;
                best_ft = ft;
                std::mem::swap(&mut cand, &mut best);
            }
        }
        raw.push(best_ft);
        raw.extend_from_slice(&best);
    }
    let mut out = SIG.to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib_compress(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

pub fn read(path: &std::path::Path) -> Result<Image, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    decode(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn write(path: &std::path::Path, img: &Image) -> std::io::Result<()> {
    std::fs::write(path, encode(img))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::synthetic;
    use crate::util::Rng;

    #[test]
    fn round_trip_is_lossless() {
        for (i, img) in [synthetic("scene", 97, 61), synthetic("chart", 64, 64), synthetic("clouds", 33, 130)]
            .iter()
            .enumerate()
        {
            let png = encode(img);
            let back = decode(&png).unwrap();
            assert_eq!((back.w, back.h), (img.w, img.h), "image {i}");
            assert!(back.data == img.data, "image {i}");
        }
    }

    #[test]
    fn reads_a_png_written_by_another_encoder() {
        // 2x2 RGB PNG produced by Python's zlib/struct (rows: red, green / blue, white), filter 0.
        let png: [u8; 75] = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x02, 0x00,
            0x00, 0x00, 0x02, 0x08, 0x02, 0x00, 0x00, 0x00, 0xfd, 0xd4, 0x9a, 0x73, 0x00, 0x00, 0x00, 0x12, 0x49, 0x44, 0x41, 0x54, 0x78,
            0xda, 0x63, 0xf8, 0xcf, 0xc0, 0xc0, 0x00, 0xc2, 0x0c, 0xff, 0x81, 0x00, 0x00, 0x1f, 0xee, 0x05, 0xfb, 0xf1, 0xab, 0xba, 0x77,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let img = decode(&png).unwrap();
        assert_eq!((img.w, img.h), (2, 2));
        assert_eq!(img.data, vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
    }

    #[test]
    fn corrupted_files_return_errors_not_panics() {
        let good = encode(&synthetic("scene", 40, 30));
        let mut rng = Rng::new(21);
        for _ in 0..2000 {
            let mut b = good.clone();
            let i = rng.below(b.len());
            b[i] ^= 1 << rng.below(8);
            if rng.below(5) == 0 {
                b.truncate(rng.below(b.len()));
            }
            // Any single-bit corruption is caught by a chunk CRC or the signature check.
            assert!(decode(&b).is_err() || b == good);
        }
    }
}
