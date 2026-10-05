//! Measurements: error rates against theory, robustness sweeps, and the
//! end-to-end picture-quality runs that the report is built from.

use crate::channel::Channel;
use crate::conv;
use crate::fountain::{Scheme, T};
use crate::image::{psnr, ssim, Image};
use crate::link::{frames_for, receive_recording, Transmission, TxConfig};
use crate::modem::{modem, Constellation, FrameSpec, Modem, RxOptions, FS};
use crate::util::{parallel_map, q_func, Rng};

#[derive(Clone, Debug, Default)]
pub struct BerPoint {
    /// Signal-to-noise ratio per carrier, Es/N0, in dB.
    pub esn0_db: f64,
    pub uncoded_theory: f64,
    pub uncoded_modem: f64,
    /// Union bound (QPSK only; 0 where not applicable).
    pub coded_bound: f64,
    /// Same mapping, soft bits and decoder, but with perfect timing and channel knowledge.
    pub coded_ideal: f64,
    pub coded_modem: f64,
    /// Fraction of packets not delivered by the full modem.
    pub packet_loss: f64,
    pub frames_lost: usize,
    pub frames: usize,
}

pub fn uncoded_theory(cons: Constellation, esn0_db: f64) -> f64 {
    let g = 10f64.powf(esn0_db / 10.0);
    match cons {
        Constellation::Qpsk => q_func(g.sqrt()),
        Constellation::Qam16 => {
            let a = (g / 5.0).sqrt();
            0.75 * q_func(a) + 0.5 * q_func(3.0 * a) - 0.25 * q_func(5.0 * a)
        }
    }
}

fn random_packets(n: usize, rng: &mut Rng) -> Vec<[u8; T]> {
    (0..n)
        .map(|_| {
            let mut p = [0u8; T];
            for v in p.iter_mut() {
                *v = rng.next_u64() as u8;
            }
            p
        })
        .collect()
}

/// Coded bit error rate with perfect synchronisation and channel knowledge.
fn ideal_coded_ber(cons: Constellation, esn0_db: f64, blocks: usize, seed: u64) -> f64 {
    let mut rng = Rng::new(seed);
    let g = 10f64.powf(esn0_db / 10.0);
    let sigma = (0.5 / g).sqrt();
    let n = cons.info_bits_per_block();
    let (mut errs, mut total) = (0usize, 0usize);
    for _ in 0..blocks {
        let info: Vec<u8> = (0..n).map(|_| rng.bit()).collect();
        let coded = conv::encode(&info);
        let mut llr = Vec::with_capacity(coded.len());
        for b in coded.chunks_exact(cons.bits()) {
            let mut z = cons.map(b);
            z.re += sigma * rng.gauss();
            z.im += sigma * rng.gauss();
            cons.llr(z, g, &mut llr);
        }
        let dec = conv::viterbi(&llr, n);
        errs += dec.iter().zip(&info).filter(|(a, b)| a != b).count();
        total += n;
    }
    errs as f64 / total as f64
}

/// One point of the error-rate curve for the complete modem in white noise.
pub fn ber_point(m: &Modem, cons: Constellation, esn0_db: f64, frames: usize, seed: u64, opts: &RxOptions) -> BerPoint {
    let mut rng = Rng::new(seed);
    let mut audio = vec![0f32; 3000];
    let mut sent = Vec::new();
    for i in 0..frames {
        let spec = FrameSpec {
            cons,
            scheme: 0,
            session: 1,
            counter: i as u32,
            k: 100,
        };
        let pk = random_packets(cons.packets_per_frame(), &mut rng);
        audio.extend(m.modulate_frame(&spec, &pk));
        sent.push(m.frame_bits(&spec, &pk));
    }
    audio.extend(vec![0f32; 3000]);
    let g = 10f64.powf(esn0_db / 10.0);
    let sigma = (m.p.carrier_amp().powi(2) * m.p.nfft as f64 / (4.0 * g)).sqrt();
    for v in audio.iter_mut() {
        *v += (sigma * rng.gauss()) as f32;
    }
    let rep = m.demodulate(&audio, &RxOptions { keep_debug: true, ..*opts });
    let (mut raw_err, mut raw_tot, mut cod_err, mut cod_tot) = (0f64, 0f64, 0f64, 0f64);
    let mut delivered = 0usize;
    let mut seen = vec![false; frames];
    for f in &rep.frames {
        let c = f.spec.counter as usize;
        if c >= frames || seen[c] || f.blocks.len() != sent[c].len() {
            continue;
        }
        seen[c] = true;
        delivered += f.packet_ok.iter().filter(|&&b| b).count();
        for (blk, (info, coded)) in f.blocks.iter().zip(&sent[c]) {
            raw_err += blk.coded_hard.iter().zip(coded).filter(|(a, b)| a != b).count() as f64;
            raw_tot += coded.len() as f64;
            cod_err += blk.info.iter().zip(info).filter(|(a, b)| a != b).count() as f64;
            cod_tot += info.len() as f64;
        }
    }
    let lost = seen.iter().filter(|&&s| !s).count();
    // A frame that was never found counts as coin-flip bits.
    let per_frame_coded = (cons.coded_bits_per_block() * crate::modem::BLOCKS) as f64;
    let per_frame_info = (cons.info_bits_per_block() * crate::modem::BLOCKS) as f64;
    raw_err += 0.5 * lost as f64 * per_frame_coded;
    raw_tot += lost as f64 * per_frame_coded;
    cod_err += 0.5 * lost as f64 * per_frame_info;
    cod_tot += lost as f64 * per_frame_info;
    BerPoint {
        esn0_db,
        uncoded_theory: uncoded_theory(cons, esn0_db),
        uncoded_modem: raw_err / raw_tot.max(1.0),
        coded_bound: if cons == Constellation::Qpsk {
            conv::union_bound_ber(esn0_db).min(0.5)
        } else {
            0.0
        },
        coded_ideal: ideal_coded_ber(cons, esn0_db, frames * crate::modem::BLOCKS, seed ^ 0x1D),
        coded_modem: cod_err / cod_tot.max(1.0),
        packet_loss: 1.0 - delivered as f64 / (frames * cons.packets_per_frame()) as f64,
        frames_lost: lost,
        frames,
    }
}

pub fn ber_curve(cons: Constellation, esn0_dbs: &[f64], frames: usize, threads: usize) -> Vec<BerPoint> {
    parallel_map(esn0_dbs.to_vec(), threads, |e| {
        ber_point(modem(), cons, e, frames, 0xBE5 + (e * 10.0) as u64, &RxOptions::default())
    })
}

/// Es/N0 at which a curve crosses `target`, by log-linear interpolation.
pub fn crossing(points: &[(f64, f64)], target: f64) -> Option<f64> {
    for w in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (w[0], w[1]);
        if y0 >= target && y1 < target && y0 > 0.0 {
            let y1 = y1.max(target / 1e3);
            return Some(x0 + (x1 - x0) * (y0.ln() - target.ln()) / (y0.ln() - y1.ln()));
        }
    }
    None
}

/// Fraction of packets delivered when `frames` frames go through `ch`.
pub fn delivery(m: &Modem, cons: Constellation, ch: &Channel, frames: usize, opts: &RxOptions) -> (f64, f64) {
    let mut rng = Rng::new(ch.seed ^ 0xDE11);
    let mut audio = vec![0f32; 2000];
    for i in 0..frames {
        let spec = FrameSpec {
            cons,
            scheme: 0,
            session: 2,
            counter: i as u32,
            k: 100,
        };
        audio.extend(m.modulate_frame(&spec, &random_packets(cons.packets_per_frame(), &mut rng)));
    }
    // Keep the noise reference level that of the frames, not of padding.
    let rx = ch.apply(&audio);
    let (rep, _) = m.receive(&rx, FS, opts);
    let snr = if rep.frames.is_empty() {
        f64::NAN
    } else {
        rep.frames.iter().map(|f| f.snr_db).sum::<f64>() / rep.frames.len() as f64
    };
    (rep.packets.len() as f64 / (frames * cons.packets_per_frame()) as f64, snr)
}

#[derive(Clone, Debug)]
pub struct SweepPoint {
    pub label: String,
    pub x: f64,
    pub qpsk: f64,
    pub qam16: f64,
    /// SNR the receiver reported (QPSK run).
    pub rx_snr_db: f64,
}

#[derive(Clone, Debug)]
pub struct Sweep {
    pub title: String,
    pub x_label: String,
    pub note: String,
    pub points: Vec<SweepPoint>,
}

pub fn sweep(title: &str, x_label: &str, note: &str, cases: Vec<(f64, String, Channel)>, frames: usize, threads: usize) -> Sweep {
    let points = parallel_map(cases, threads, |(x, label, ch)| {
        let (q, snr) = delivery(modem(), Constellation::Qpsk, &ch, frames, &RxOptions::default());
        let (h, _) = delivery(modem(), Constellation::Qam16, &ch, frames, &RxOptions::default());
        SweepPoint {
            label,
            x,
            qpsk: q,
            qam16: h,
            rx_snr_db: snr,
        }
    });
    Sweep {
        title: title.into(),
        x_label: x_label.into(),
        note: note.into(),
        points,
    }
}

/// The standard robustness sweeps. Each varies one impairment around a
/// base channel and reports the share of packets delivered.
pub fn robustness_sweeps(frames: usize, threads: usize) -> Vec<Sweep> {
    let base = |snr: f64| Channel {
        rt60: 0.3,
        drr_db: 8.0,
        ..Channel::awgn(snr, 100)
    };
    let mut out = Vec::new();
    out.push(sweep(
        "Noise, no reverberation",
        "in-band SNR (dB)",
        "White noise only.",
        [0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 20.0]
            .iter()
            .map(|&s| (s, format!("{s:.0}"), Channel::awgn(s, 101)))
            .collect(),
        frames,
        threads,
    ));
    out.push(sweep(
        "Noise in a reverberant room",
        "in-band SNR (dB)",
        "RT60 0.45 s, direct-to-reverberant ratio 5 dB.",
        [2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 20.0, 25.0, 30.0]
            .iter()
            .map(|&s| {
                (
                    s,
                    format!("{s:.0}"),
                    Channel {
                        rt60: 0.45,
                        drr_db: 5.0,
                        ..Channel::awgn(s, 102)
                    },
                )
            })
            .collect(),
        frames,
        threads,
    ));
    out.push(sweep(
        "Recorder clock offset",
        "offset (ppm)",
        "SNR 16 dB, RT60 0.3 s, DRR 8 dB.",
        [-1000.0, -400.0, -200.0, -100.0, -50.0, 0.0, 50.0, 100.0, 200.0, 400.0, 1000.0]
            .iter()
            .map(|&p| (p, format!("{p:+.0}"), Channel { ppm: p, ..base(16.0) }))
            .collect(),
        frames,
        threads,
    ));
    out.push(sweep(
        "Direct-to-reverberant ratio",
        "DRR (dB)",
        "SNR 25 dB, RT60 0.5 s. Lower DRR means more of the sound arrives as echo.",
        [15.0, 10.0, 6.0, 3.0, 0.0, -3.0, -6.0]
            .iter()
            .map(|&d| {
                (
                    d,
                    format!("{d:.0}"),
                    Channel {
                        rt60: 0.5,
                        drr_db: d,
                        ..Channel::awgn(25.0, 103)
                    },
                )
            })
            .collect(),
        frames,
        threads,
    ));
    out.push(sweep(
        "Reverberation time",
        "RT60 (s)",
        "SNR 25 dB, DRR 3 dB.",
        [0.1, 0.2, 0.3, 0.5, 0.8, 1.2]
            .iter()
            .map(|&r| {
                (
                    r,
                    format!("{r:.1}"),
                    Channel {
                        rt60: r,
                        drr_db: 3.0,
                        ..Channel::awgn(25.0, 104)
                    },
                )
            })
            .collect(),
        frames,
        threads,
    ));
    out.push(sweep(
        "Upper band limit",
        "low-pass corner (kHz)",
        "SNR 16 dB, light reverberation; the modem uses 1.03-6.98 kHz. Carriers above the corner are lost.",
        [8.0, 7.0, 6.5, 6.0, 5.5, 5.0, 4.0]
            .iter()
            .map(|&k| {
                (
                    k,
                    format!("{k:.1}"),
                    Channel {
                        band: Some((300.0, k * 1000.0)),
                        ..base(16.0)
                    },
                )
            })
            .collect(),
        frames,
        threads,
    ));
    out.push(sweep(
        "Lower band limit",
        "high-pass corner (kHz)",
        "SNR 16 dB, light reverberation.",
        [0.5, 1.0, 1.5, 2.0, 2.5, 3.0]
            .iter()
            .map(|&k| {
                (
                    k,
                    format!("{k:.1}"),
                    Channel {
                        band: Some((k * 1000.0, 10_000.0)),
                        ..base(16.0)
                    },
                )
            })
            .collect(),
        frames,
        threads,
    ));
    out.push(sweep(
        "Clipping",
        "clip level (x RMS)",
        "SNR 25 dB, light reverberation. The signal's own peaks reach about 3.4 x RMS.",
        [3.0, 2.0, 1.5, 1.0, 0.7, 0.5, 0.3]
            .iter()
            .map(|&c| {
                (
                    c,
                    format!("{c:.1}"),
                    Channel {
                        clip: Some(c),
                        ..base(25.0)
                    },
                )
            })
            .collect(),
        frames,
        threads,
    ));
    out.push(sweep(
        "Impulsive noise",
        "clicks per second",
        "SNR 20 dB, light reverberation; each click is a 3 ms burst peaking 20 dB above the signal RMS.",
        [0.0, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0]
            .iter()
            .map(|&r| {
                (
                    r,
                    format!("{r:.1}"),
                    Channel {
                        impulses_per_s: r,
                        ..base(20.0)
                    },
                )
            })
            .collect(),
        frames,
        threads,
    ));
    out.push(sweep(
        "Recorder dropouts",
        "dropouts per minute",
        "SNR 20 dB, light reverberation; each dropout deletes 50 ms of audio.",
        [0.0, 6.0, 15.0, 30.0, 60.0, 120.0]
            .iter()
            .map(|&r| {
                (
                    r,
                    format!("{r:.0}"),
                    Channel {
                        dropouts_per_min: r,
                        ..base(20.0)
                    },
                )
            })
            .collect(),
        frames,
        threads,
    ));
    out
}

#[derive(Clone, Debug)]
pub struct NumerologyRow {
    pub params: crate::modem::Params,
    pub byte_rate: f64,
    /// (DRR dB, QPSK delivery, 16-QAM delivery, receiver SNR) at RT60 0.45 s, noise SNR 25 dB.
    pub by_drr: Vec<(f64, f64, f64, f64)>,
    /// (ppm, QPSK delivery, 16-QAM delivery) with the resampling pass disabled, light reverberation.
    pub by_ppm: Vec<(f64, f64, f64)>,
}

/// The design-space sweep behind the choice of symbol length: the same
/// modem at four numerologies, against echo and against clock offset.
pub fn numerology_sweep(frames: usize, threads: usize) -> Vec<NumerologyRow> {
    use crate::modem::Params;
    let candidates = vec![
        Params::new(1024, 256),
        Params::new(2048, 512),
        Params::new(4096, 1024),
        Params::new(8192, 2048),
    ];
    parallel_map(candidates, threads, |p| {
        let m = Modem::new(p);
        let by_drr = [10.0, 5.0, 0.0, -5.0, -10.0]
            .iter()
            .map(|&d| {
                let ch = Channel {
                    rt60: 0.45,
                    drr_db: d,
                    ..Channel::awgn(25.0, 200)
                };
                let (q, snr) = delivery(&m, Constellation::Qpsk, &ch, frames, &RxOptions::default());
                let (h, _) = delivery(&m, Constellation::Qam16, &ch, frames, &RxOptions::default());
                (d, q, h, snr)
            })
            .collect();
        let by_ppm = [0.0, 50.0, 100.0, 200.0, 400.0]
            .iter()
            .map(|&ppm| {
                let ch = Channel {
                    rt60: 0.3,
                    drr_db: 8.0,
                    ppm,
                    ..Channel::awgn(20.0, 201)
                };
                let opts = RxOptions {
                    no_resample: true,
                    ..Default::default()
                };
                let (q, _) = delivery(&m, Constellation::Qpsk, &ch, frames, &opts);
                let (h, _) = delivery(&m, Constellation::Qam16, &ch, frames, &opts);
                (ppm, q, h)
            })
            .collect();
        NumerologyRow {
            params: p,
            byte_rate: p.byte_rate(Constellation::Qpsk),
            by_drr,
            by_ppm,
        }
    })
}

// ---------------------------------------------------------------- end to end

#[derive(Clone)]
pub struct QualityPoint {
    pub time: f64,
    pub bytes: usize,
    pub psnr: Option<f64>,
    pub ssim: Option<f64>,
}

#[derive(Clone)]
pub struct E2eRun {
    pub image: String,
    pub channel: String,
    pub scheme: String,
    pub cons: Constellation,
    pub source_bytes: usize,
    /// Share of transmitted packets that arrived.
    pub delivered: f64,
    pub rx_snr_db: f64,
    pub rx_ppm: f64,
    pub points: Vec<QualityPoint>,
    /// Pictures at the times asked for in `keep_images`.
    pub images: Vec<(f64, Option<Image>)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Variant {
    /// Progressive codec + expanding-window fountain (this project).
    Windowed,
    /// Progressive codec + ordinary fountain, shown only when complete.
    FlatAtomic,
    /// Plain file sent in order and repeated, shown only when complete.
    CarouselAtomic,
    /// Same carousel, half the file size (two passes in the design time).
    CarouselHalfAtomic,
    /// Progressive codec sent in order and repeated, no fountain code:
    /// shows whatever prefix has arrived.
    CarouselProgressive,
}

impl Variant {
    pub fn name(self) -> &'static str {
        match self {
            Variant::Windowed => "chirpix (windowed fountain)",
            Variant::FlatAtomic => "flat fountain, all-or-nothing",
            Variant::CarouselAtomic => "plain file, one pass",
            Variant::CarouselHalfAtomic => "plain file, half size, two passes",
            Variant::CarouselProgressive => "progressive, in order, no fountain",
        }
    }
    pub fn config(self, cons: Constellation, design_seconds: f64) -> (TxConfig, bool) {
        let mut cfg = TxConfig::new(cons, Scheme::Windowed);
        cfg.design_seconds = design_seconds;
        match self {
            Variant::Windowed => (cfg, false),
            Variant::FlatAtomic => (
                TxConfig {
                    scheme: Scheme::Flat,
                    ..cfg
                },
                true,
            ),
            Variant::CarouselAtomic => (
                TxConfig {
                    scheme: Scheme::Carousel,
                    ..cfg
                },
                true,
            ),
            Variant::CarouselHalfAtomic => {
                let c = TxConfig {
                    scheme: Scheme::Carousel,
                    ..cfg
                };
                (
                    TxConfig {
                        k: Some(c.design_packets() / 2),
                        ..c
                    },
                    true,
                )
            }
            Variant::CarouselProgressive => (
                TxConfig {
                    scheme: Scheme::Carousel,
                    ..cfg
                },
                false,
            ),
        }
    }
}

pub struct E2eJob {
    pub image_name: String,
    pub image: std::sync::Arc<Image>,
    pub channel: Channel,
    pub variant: Variant,
    pub cons: Constellation,
    pub design_seconds: f64,
    /// Seconds into the (endless) transmission at which the listener starts recording.
    pub start: f64,
    pub listen: f64,
    pub times: Vec<f64>,
    pub keep_images: Vec<f64>,
}

pub fn run_e2e(job: &E2eJob) -> E2eRun {
    let (cfg, atomic) = job.variant.config(job.cons, job.design_seconds);
    let tx = Transmission::new(&job.image, &cfg);
    // Transmit a little before the listener starts and after they stop,
    // then cut the recording to exactly [start, start + listen].
    let fl = modem().frame_len();
    let first_frame = (job.start * FS as f64 / fl as f64).floor() as usize;
    let nframes = frames_for(job.listen + 1.0) + 2;
    let audio = tx.audio(first_frame as u32, nframes);
    let received = job.channel.apply(&audio);
    let skip = ((job.start * FS as f64) as usize).saturating_sub(first_frame * fl);
    let end = (skip + (job.listen * FS as f64) as usize).min(received.len());
    let rec = receive_recording(&received[skip.min(end)..end], FS, &RxOptions::default());
    let snaps = rec.snapshots(&job.times, atomic);
    let sent_packets = (job.listen * FS as f64 / fl as f64) * cfg.cons.packets_per_frame() as f64;
    let mut points = Vec::new();
    let mut images = Vec::new();
    let mut cache: Option<(usize, f64, f64)> = None;
    for s in &snaps {
        let (p, q) = match (&s.image, &cache) {
            (None, _) => (None, None),
            (Some(_), Some((b, p, q))) if *b == s.prefix_bytes => (Some(*p), Some(*q)),
            (Some(img), _) => {
                let (p, q) = (psnr(&job.image, img), ssim(&job.image, img));
                cache = Some((s.prefix_bytes, p, q));
                (Some(p), Some(q))
            }
        };
        points.push(QualityPoint {
            time: s.time,
            bytes: s.prefix_bytes,
            psnr: p,
            ssim: q,
        });
        if job.keep_images.iter().any(|t| (t - s.time).abs() < 1e-6) {
            images.push((s.time, s.image.clone()));
        }
    }
    let frames = &rec.report.frames;
    E2eRun {
        image: job.image_name.clone(),
        channel: job.channel.name.clone(),
        scheme: job.variant.name().into(),
        cons: job.cons,
        source_bytes: tx.stream.len(),
        delivered: rec.packets.len() as f64 / sent_packets.max(1.0),
        rx_snr_db: if frames.is_empty() {
            f64::NAN
        } else {
            frames.iter().map(|f| f.snr_db).sum::<f64>() / frames.len() as f64
        },
        rx_ppm: rec.ppm,
        points,
        images,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theory_curves_match_known_values() {
        // QPSK: BER 1e-3 at Eb/N0 6.79 dB, i.e. Es/N0 9.80 dB.
        assert!((uncoded_theory(Constellation::Qpsk, 9.80) / 1e-3 - 1.0).abs() < 0.03);
        // 16-QAM: BER 1e-3 at Eb/N0 10.5 dB, i.e. Es/N0 16.5 dB.
        assert!((uncoded_theory(Constellation::Qam16, 16.55) / 1e-3 - 1.0).abs() < 0.08);
    }

    #[test]
    fn crossing_interpolates_in_the_log_domain() {
        let pts = [(0.0, 1e-1), (2.0, 1e-3), (4.0, 1e-5)];
        assert!((crossing(&pts, 1e-2).unwrap() - 1.0).abs() < 1e-9);
        assert!((crossing(&pts, 1e-4).unwrap() - 3.0).abs() < 1e-9);
        assert!(crossing(&pts, 1e-9).is_none());
    }
}
