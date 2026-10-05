//! Minimal WAV (RIFF) reader and writer.
//!
//! Reads PCM 8/16/24/32-bit integer and 32-bit float, including the
//! WAVE_FORMAT_EXTENSIBLE header phone recorders often write. Writes
//! 16-bit mono PCM.

use std::io::{self, Read, Write};

pub struct Wav {
    pub rate: u32,
    pub channels: u16,
    /// Channel 0 only, scaled to [-1, 1).
    pub samples: Vec<f32>,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

pub fn parse(bytes: &[u8]) -> io::Result<Wav> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad(
            "not a RIFF/WAVE file (convert it first, e.g. `afconvert -f WAVE -d LEI16 in.m4a out.wav`)",
        ));
    }
    let u16le = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let u32le = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // tag, channels, rate, bits
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32le(pos + 4) as usize;
        let body = pos + 8;
        if id == b"fmt " {
            if size < 16 || body + 16 > bytes.len() {
                return Err(bad("truncated fmt chunk"));
            }
            let mut tag = u16le(body);
            if tag == 0xFFFE && size >= 26 && body + 26 <= bytes.len() {
                tag = u16le(body + 24); // first two bytes of the sub-format GUID
            }
            fmt = Some((tag, u16le(body + 2), u32le(body + 4), u16le(body + 14)));
        } else if id == b"data" {
            let (tag, channels, rate, bits) = fmt.ok_or_else(|| bad("data chunk before fmt chunk"))?;
            if channels == 0 || rate == 0 {
                return Err(bad("zero channels or sample rate"));
            }
            // Some recorders write size 0 or 0xFFFFFFFF when streaming.
            let end = if size == 0 || body + size > bytes.len() {
                bytes.len()
            } else {
                body + size
            };
            let data = &bytes[body..end];
            let bps = (bits as usize).div_ceil(8);
            let frame = bps * channels as usize;
            if frame == 0 {
                return Err(bad("zero-sized sample frame"));
            }
            let mut samples = Vec::with_capacity(data.len() / frame);
            for f in data.chunks_exact(frame) {
                let s = &f[..bps];
                let v = match (tag, bits) {
                    (1, 8) => (s[0] as f32 - 128.0) / 128.0,
                    (1, 16) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
                    (1, 24) => (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0,
                    (1, 32) => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
                    (3, 32) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                    _ => return Err(bad("unsupported WAV sample format (need PCM 8/16/24/32-bit or 32-bit float)")),
                };
                samples.push(if v.is_finite() { v } else { 0.0 });
            }
            return Ok(Wav { rate, channels, samples });
        }
        pos = body.saturating_add(size).saturating_add(size & 1);
    }
    Err(bad("no data chunk found"))
}

pub fn read(path: &std::path::Path) -> io::Result<Wav> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.read_to_end(&mut bytes)?;
    parse(&bytes)
}

/// Encode mono samples as 16-bit PCM. Values outside [-1, 1] are clipped.
pub fn encode(samples: &[f32], rate: u32) -> Vec<u8> {
    let n = samples.len() * 2;
    let mut out = Vec::with_capacity(44 + n);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + n as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(n as u32).to_le_bytes());
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

pub fn write(path: &std::path::Path, samples: &[f32], rate: u32) -> io::Result<()> {
    std::fs::File::create(path)?.write_all(&encode(samples, rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_16_bit() {
        let x: Vec<f32> = (0..1000).map(|i| ((i as f32) * 0.05).sin() * 0.8).collect();
        let w = parse(&encode(&x, 48000)).unwrap();
        assert_eq!(w.rate, 48000);
        assert_eq!(w.channels, 1);
        assert_eq!(w.samples.len(), x.len());
        for (a, b) in x.iter().zip(&w.samples) {
            assert!((a - b).abs() < 1e-4);
        }
    }

    fn header(tag: u16, ch: u16, rate: u32, bits: u16, data: &[u8], extensible: bool) -> Vec<u8> {
        let mut o = Vec::new();
        o.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        // An unknown chunk with odd size must be skipped with its pad byte.
        o.extend_from_slice(b"LIST\x03\0\0\0abc\0");
        o.extend_from_slice(b"fmt ");
        let ba = ch * bits.div_ceil(8);
        let mut f = Vec::new();
        f.extend_from_slice(&(if extensible { 0xFFFEu16 } else { tag }).to_le_bytes());
        f.extend_from_slice(&ch.to_le_bytes());
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&(rate * ba as u32).to_le_bytes());
        f.extend_from_slice(&ba.to_le_bytes());
        f.extend_from_slice(&bits.to_le_bytes());
        if extensible {
            f.extend_from_slice(&22u16.to_le_bytes());
            f.extend_from_slice(&bits.to_le_bytes());
            f.extend_from_slice(&0u32.to_le_bytes());
            f.extend_from_slice(&tag.to_le_bytes());
            f.extend_from_slice(&[0u8; 14]);
        }
        o.extend_from_slice(&(f.len() as u32).to_le_bytes());
        o.extend_from_slice(&f);
        o.extend_from_slice(b"data");
        o.extend_from_slice(&(data.len() as u32).to_le_bytes());
        o.extend_from_slice(data);
        o
    }

    #[test]
    fn reads_other_sample_formats_and_takes_first_channel() {
        // 24-bit stereo: left = 0.5, right = -0.5
        let mut d = Vec::new();
        for _ in 0..4 {
            d.extend_from_slice(&[0x00, 0x00, 0x40, 0x00, 0x00, 0xC0]);
        }
        let w = parse(&header(1, 2, 44100, 24, &d, false)).unwrap();
        assert_eq!((w.rate, w.channels, w.samples.len()), (44100, 2, 4));
        assert!((w.samples[0] - 0.5).abs() < 1e-6);
        // float32 mono via WAVE_FORMAT_EXTENSIBLE
        let d: Vec<u8> = [0.25f32, -1.0].iter().flat_map(|v| v.to_le_bytes()).collect();
        let w = parse(&header(3, 1, 48000, 32, &d, true)).unwrap();
        assert_eq!(w.samples, vec![0.25, -1.0]);
        // 8-bit unsigned
        let w = parse(&header(1, 1, 8000, 8, &[128, 255, 0], false)).unwrap();
        assert_eq!(w.samples[0], 0.0);
        assert!(w.samples[1] > 0.99 && w.samples[2] == -1.0);
    }

    #[test]
    fn rejects_garbage_without_panicking() {
        assert!(parse(b"").is_err());
        assert!(parse(b"RIFF\0\0\0\0WAVEdata\x04\0\0\0abcd").is_err());
        assert!(parse(&[0u8; 100]).is_err());
        let mut rng = crate::util::Rng::new(5);
        let good = encode(&[0.1, 0.2, 0.3], 48000);
        for _ in 0..2000 {
            let mut b = good.clone();
            let i = rng.below(b.len());
            b[i] = rng.next_u64() as u8;
            b.truncate(rng.below(b.len() + 1));
            let _ = parse(&b);
        }
    }
}
