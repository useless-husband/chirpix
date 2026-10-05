//! The whole link: picture -> codec -> fountain packets -> frames of
//! audio, and a recording -> packets -> pictures over time.

use crate::codec;
use crate::fountain::{Decoder, Encoder, Plan, Scheme, T};
use crate::image::Image;
use crate::modem::{modem, receive, Constellation, FrameSpec, RxOptions, RxReport, FRAME_LEN, FS};
use crate::util::crc32;

/// Share of the packets a listener collects in the design time that the
/// windowed scheme spends on new data; the rest is the price of making
/// the early layers decodable early (see docs/DESIGN.md).
pub const WINDOWED_FILL: f64 = 0.39;

#[derive(Clone, Copy, Debug)]
pub struct TxConfig {
    pub cons: Constellation,
    pub scheme: Scheme,
    /// Listening time the transmission is sized for.
    pub design_seconds: f64,
    /// Override the number of source packets.
    pub k: Option<usize>,
}

impl TxConfig {
    pub fn new(cons: Constellation, scheme: Scheme) -> TxConfig {
        TxConfig { cons, scheme, design_seconds: 75.0, k: None }
    }

    /// Whole frames a listener gets in the design time, assuming the
    /// frame they started in the middle of is lost.
    pub fn design_packets(&self) -> usize {
        let frames = (self.design_seconds * FS as f64 / FRAME_LEN as f64).floor() as usize;
        frames.saturating_sub(1).max(1) * self.cons.packets_per_frame()
    }

    pub fn source_packets(&self) -> usize {
        let n = self.design_packets();
        self.k.unwrap_or(match self.scheme {
            Scheme::Windowed => ((n as f64 * WINDOWED_FILL) as usize).max(1),
            Scheme::Flat => n.saturating_sub(6).max(1),
            Scheme::Carousel => n,
        })
    }
}

pub struct Transmission {
    pub cfg: TxConfig,
    /// The codec stream being sent.
    pub stream: Vec<u8>,
    pub encoder: Encoder,
    pub session: u8,
}

impl Transmission {
    pub fn new(img: &Image, cfg: &TxConfig) -> Transmission {
        let budget = cfg.source_packets().min(crate::fountain::MAX_K) * T;
        let stream = codec::encode(img, budget);
        let encoder = Encoder::new(cfg.scheme, &stream);
        let session = (crc32(&stream) & 0xFF) as u8;
        Transmission { cfg: *cfg, stream, encoder, session }
    }

    pub fn k(&self) -> usize {
        self.encoder.plan.k
    }

    pub fn frame(&self, counter: u32) -> Vec<f32> {
        let spec = FrameSpec {
            cons: self.cfg.cons,
            scheme: self.cfg.scheme as u8,
            session: self.session,
            counter: counter & 0xFF_FFFF,
            k: self.k() as u16,
        };
        let packets: Vec<[u8; T]> =
            (0..self.cfg.cons.packets_per_frame()).map(|i| self.encoder.packet(spec.packet_id(i))).collect();
        modem().modulate_frame(&spec, &packets)
    }

    /// Audio for frames `first..first + count`.
    pub fn audio(&self, first: u32, count: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(count * FRAME_LEN);
        for i in 0..count {
            out.extend(self.frame(first + i as u32));
        }
        out
    }
}

pub fn frames_for(seconds: f64) -> usize {
    (seconds * FS as f64 / FRAME_LEN as f64).ceil() as usize
}

/// Everything recovered from one recording.
pub struct Reception {
    pub report: RxReport,
    /// Estimated recorder clock offset in ppm.
    pub ppm: f64,
    pub spec: Option<FrameSpec>,
    /// (time in seconds from the start of the recording, packet id, payload), time-ordered.
    pub packets: Vec<(f64, u32, [u8; T])>,
    pub duration: f64,
}

pub fn receive_recording(samples: &[f32], rate: u32, opts: &RxOptions) -> Reception {
    let (report, ppm) = receive(samples, rate, opts);
    // If two transmissions are present, follow the one with more frames.
    let mut counts: std::collections::HashMap<(u8, u8, u16, u8), usize> = Default::default();
    for f in &report.frames {
        *counts.entry((f.spec.session, f.spec.scheme, f.spec.k, f.spec.cons as u8)).or_default() += 1;
    }
    let key = counts.iter().max_by_key(|(k, n)| (**n, **k)).map(|(k, _)| *k);
    let spec = key.and_then(|k| report.frames.iter().map(|f| f.spec).find(|s| (s.session, s.scheme, s.k, s.cons as u8) == k));
    let mut packets = Vec::new();
    if let Some(s) = spec {
        let ppf = s.cons.packets_per_frame() as u32;
        let counters: std::collections::HashSet<u32> = report
            .frames
            .iter()
            .filter(|f| (f.spec.session, f.spec.scheme, f.spec.k, f.spec.cons) == (s.session, s.scheme, s.k, s.cons))
            .map(|f| f.spec.counter)
            .collect();
        for p in &report.packets {
            if counters.contains(&(p.id / ppf)) {
                packets.push((p.end_sample as f64 / FS as f64, p.id, p.payload));
            }
        }
    }
    packets.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    Reception { report, ppm, spec, packets, duration: samples.len() as f64 / rate as f64 }
}

#[derive(Clone)]
pub struct Snapshot {
    pub time: f64,
    pub packets: usize,
    pub rank: usize,
    /// Usable bytes of the image stream.
    pub prefix_bytes: usize,
    pub complete: bool,
    pub image: Option<Image>,
}

impl Reception {
    /// The picture as it stands after each of `times` seconds of the
    /// recording. With `atomic`, nothing is shown until the whole file has
    /// arrived (how an ordinary file transfer behaves).
    pub fn snapshots(&self, times: &[f64], atomic: bool) -> Vec<Snapshot> {
        let Some(spec) = self.spec else {
            return times
                .iter()
                .map(|&time| Snapshot { time, packets: 0, rank: 0, prefix_bytes: 0, complete: false, image: None })
                .collect();
        };
        let scheme = Scheme::from_u8(spec.scheme).unwrap_or(Scheme::Windowed);
        let k = (spec.k as usize).min(crate::fountain::MAX_K);
        let mut dec = Decoder::new(Plan::new(scheme, k));
        let mut next = 0;
        let mut out: Vec<Snapshot> = Vec::with_capacity(times.len());
        let mut last: Option<(usize, Option<Image>)> = None;
        for &time in times {
            while next < self.packets.len() && self.packets[next].0 <= time {
                dec.add(self.packets[next].1, &self.packets[next].2);
                next += 1;
            }
            let prefix = dec.prefix_packets();
            let complete = dec.complete();
            let usable = if atomic && !complete { 0 } else { prefix };
            let image = match &last {
                Some((p, img)) if *p == usable => img.clone(),
                _ => {
                    if usable == 0 {
                        None
                    } else {
                        codec::decode(&dec.prefix_bytes())
                    }
                }
            };
            last = Some((usable, image.clone()));
            out.push(Snapshot { time, packets: next, rank: dec.rank(), prefix_bytes: usable * T, complete, image });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::{psnr, synthetic};

    #[test]
    fn sizes_follow_the_design_time() {
        let cfg = TxConfig::new(Constellation::Qpsk, Scheme::Windowed);
        assert_eq!(cfg.design_packets(), 300);
        assert_eq!(cfg.source_packets(), 117);
        let cfg = TxConfig::new(Constellation::Qam16, Scheme::Carousel);
        assert_eq!(cfg.source_packets(), 600);
    }

    #[test]
    fn picture_survives_the_whole_chain_and_sharpens_with_time() {
        let img = synthetic("scene", 128, 96);
        let mut cfg = TxConfig::new(Constellation::Qpsk, Scheme::Windowed);
        cfg.design_seconds = 20.0;
        let tx = Transmission::new(&img, &cfg);
        // Start mid-transmission and mid-frame.
        let audio = tx.audio(1000, frames_for(22.0));
        let rec = receive_recording(&audio[20_000..], FS, &RxOptions::default());
        assert_eq!(rec.spec.unwrap().k as usize, tx.k());
        let snaps = rec.snapshots(&[2.0, 6.0, 12.0, 21.0], false);
        let q: Vec<f64> = snaps.iter().map(|s| s.image.as_ref().map(|i| psnr(&img, i)).unwrap_or(0.0)).collect();
        assert!(q[0] > 10.0, "a first picture within 2 s: {q:?}");
        assert!(q.windows(2).all(|p| p[1] >= p[0]), "quality must not fall: {q:?}");
        assert!(snaps[3].complete, "complete after the design time");
        assert!(snaps[3].image.as_ref().unwrap() == &codec::decode(&tx.stream).unwrap());
    }

    #[test]
    fn atomic_transfer_shows_nothing_until_complete() {
        let img = synthetic("clouds", 96, 96);
        let mut cfg = TxConfig::new(Constellation::Qam16, Scheme::Carousel);
        cfg.design_seconds = 8.0;
        let tx = Transmission::new(&img, &cfg);
        let audio = tx.audio(3, frames_for(9.0));
        let rec = receive_recording(&audio, FS, &RxOptions::default());
        let snaps = rec.snapshots(&[3.0, 8.9], true);
        assert!(snaps[0].image.is_none() && snaps[0].packets > 0);
        assert!(snaps[1].complete && snaps[1].image.is_some());
    }
}
