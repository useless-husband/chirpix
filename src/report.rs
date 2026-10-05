//! Runs the experiments and writes the self-contained HTML report.

use crate::channel::Channel;
use crate::chart::{esc, render, Chart, Series};
use crate::experiments::{self as ex, E2eJob, E2eRun, Variant};
use crate::fft::{Cpx, Fft};
use crate::image::{psnr, ssim, Image};
use crate::link::{Transmission, TxConfig};
use crate::modem::{modem, Constellation, Params, RxOptions, FS};
use crate::util::{base64, parallel_map};
use std::sync::Arc;

pub struct ReportConfig {
    pub quick: bool,
    pub images: Vec<(String, Image)>,
    pub image_note: String,
    pub threads: usize,
}

pub struct ReportOutput {
    pub html: String,
    /// Plain-text version of the headline tables.
    pub summary: String,
}

/// Receiver SNR above which the sender should use 16-QAM (see the
/// "choosing the constellation" section of the report).
pub const QAM16_MIN_SNR_DB: f64 = 12.0;

pub fn recommend(snr_db: f64) -> Constellation {
    if snr_db >= QAM16_MIN_SNR_DB {
        Constellation::Qam16
    } else {
        Constellation::Qpsk
    }
}

const CSS: &str = r#"
:root{--bg:#f9f9f7;--surface:#fcfcfb;--ink:#0b0b0b;--ink2:#52514e;--muted:#898781;--grid:#e1e0d9;--axis:#c3c2b7;
--s1:#2a78d6;--s2:#eb6834;--s3:#1baf7a;--s4:#eda100;--s5:#e87ba4}
@media (prefers-color-scheme:dark){:root{--bg:#0d0d0d;--surface:#1a1a19;--ink:#ffffff;--ink2:#c3c2b7;--muted:#898781;--grid:#2c2c2a;--axis:#383835;
--s1:#3987e5;--s2:#d95926;--s3:#199e70;--s4:#c98500;--s5:#d55181}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif}
main{max-width:1120px;margin:0;padding:28px 32px 80px}
h1{font-size:26px;margin:0 0 6px}h2{font-size:19px;margin:44px 0 8px;padding-top:14px;border-top:1px solid var(--grid)}
h3{font-size:15px;margin:22px 0 6px}
p,li{max-width:78ch;color:var(--ink2)}p.lead{color:var(--ink);font-size:16px}
code{font:13px ui-monospace,Menlo,monospace;background:var(--surface);padding:1px 4px;border:1px solid var(--grid);border-radius:3px}
table{border-collapse:collapse;margin:10px 0 18px;font-size:14px;background:var(--surface)}
th,td{border:1px solid var(--grid);padding:5px 10px;text-align:right;white-space:nowrap}
th{font-weight:600;color:var(--ink2)}td:first-child,th:first-child{text-align:left}
td.ours{font-weight:600}
td small{color:var(--muted)}
.row{display:flex;flex-wrap:wrap;gap:18px;align-items:flex-start}
figure{margin:0}
figure.chart{background:var(--surface);border:1px solid var(--grid);padding:10px 12px;width:548px;max-width:100%}
figure.chart.small{width:360px}
figcaption{font-weight:600;font-size:14px;margin-bottom:4px;color:var(--ink)}
.legend{display:flex;flex-wrap:wrap;gap:4px 16px;font-size:12.5px;color:var(--ink2);margin:2px 0 4px}
.legend span{display:inline-flex;align-items:center;gap:6px}
.legend svg{width:26px;height:14px;flex:none}
.tw{overflow-x:auto;max-width:100%}
svg{display:block;width:100%;height:auto}
svg .grid{stroke:var(--grid);stroke-width:1}svg .axis{stroke:var(--axis);stroke-width:1}
svg .tick{fill:var(--muted);font-size:11px}svg .label{fill:var(--ink2);font-size:12px}
svg .l{fill:none;stroke-width:2;stroke-linejoin:round}
svg .m{stroke:var(--surface);stroke-width:2}
.s1{stroke:var(--s1)}.s2{stroke:var(--s2)}.s3{stroke:var(--s3)}.s4{stroke:var(--s4)}.s5{stroke:var(--s5)}
.m.s1{fill:var(--s1);stroke:var(--surface)}.m.s2{fill:var(--s2);stroke:var(--surface)}.m.s3{fill:var(--s3);stroke:var(--surface)}
.m.s4{fill:var(--s4);stroke:var(--surface)}.m.s5{fill:var(--s5);stroke:var(--surface)}
.dot{fill:var(--s1);fill-opacity:.45}
.shots{display:flex;flex-wrap:wrap;gap:10px;margin:8px 0 4px}
.shot{width:200px;font-size:12.5px;color:var(--ink2)}
.shot img,.shot .none{display:block;width:200px;border:1px solid var(--grid);background:var(--surface)}
.shot .none{display:flex;align-items:center;justify-content:center;color:var(--muted)}
.shot b{color:var(--ink);font-weight:600}
.note{font-size:13px;color:var(--muted)}
details{margin:8px 0}summary{cursor:pointer;color:var(--ink2)}
"#;

fn img_tag(img: &Image, alt: &str) -> String {
    let f = img.w.div_ceil(400).max(1);
    let small = img.downscale(f);
    format!(
        "<img alt=\"{}\" src=\"data:image/png;base64,{}\">",
        esc(alt),
        base64(&crate::png::encode(&small))
    )
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len().max(1) as f64
}

/// Spectrogram of `x` as a picture: time left to right, 0-12 kHz bottom to top.
fn spectrogram(x: &[f32]) -> Image {
    let n = 1024;
    let hop = 512;
    let fft = Fft::new(n);
    let cols = ((x.len().saturating_sub(n)) / hop).clamp(1, 640);
    let rows = 256;
    let mut db = vec![-200f64; cols * rows];
    let mut peak = -200f64;
    for c in 0..cols {
        let mut buf: Vec<Cpx> = (0..n)
            .map(|i| {
                let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
                Cpx::new(x.get(c * hop + i).copied().unwrap_or(0.0) as f64 * w, 0.0)
            })
            .collect();
        fft.forward(&mut buf);
        for r in 0..rows {
            let v = 10.0 * (buf[r].norm2() + 1e-20).log10();
            db[c * rows + r] = v;
            peak = peak.max(v);
        }
    }
    // One hue, light to dark, over a 60 dB range.
    let ramp = [
        [252.0, 252.0, 251.0],
        [205.0, 226.0, 251.0],
        [42.0, 120.0, 214.0],
        [13.0, 54.0, 107.0],
    ];
    let mut img = Image::new(cols, rows);
    for c in 0..cols {
        for r in 0..rows {
            let t = ((db[c * rows + r] - (peak - 60.0)) / 60.0).clamp(0.0, 1.0) * 3.0;
            let i = (t.floor() as usize).min(2);
            let a = t - i as f64;
            let px = [0, 1, 2].map(|k| (ramp[i][k] * (1.0 - a) + ramp[i + 1][k] * a) as u8);
            img.set(c, rows - 1 - r, px);
        }
    }
    img
}

struct Agg {
    label: String,
    ours: bool,
    cons: Constellation,
    source_bytes: f64,
    delivered: f64,
    /// Per time: mean PSNR, mean SSIM, runs with a picture, runs.
    by_time: Vec<(f64, f64, f64, usize, usize)>,
}

fn aggregate(runs: &[&E2eRun], grey: &dyn Fn(&str) -> (f64, f64), label: &str, ours: bool) -> Agg {
    let nt = runs[0].points.len();
    let by_time = (0..nt)
        .map(|t| {
            let ps: Vec<f64> = runs.iter().map(|r| r.points[t].psnr.unwrap_or(grey(&r.image).0)).collect();
            let ss: Vec<f64> = runs.iter().map(|r| r.points[t].ssim.unwrap_or(grey(&r.image).1)).collect();
            let have = runs.iter().filter(|r| r.points[t].psnr.is_some()).count();
            (runs[0].points[t].time, mean(&ps), mean(&ss), have, runs.len())
        })
        .collect();
    Agg {
        label: label.into(),
        ours,
        cons: runs[0].cons,
        source_bytes: mean(&runs.iter().map(|r| r.source_bytes as f64).collect::<Vec<_>>()),
        delivered: mean(&runs.iter().map(|r| r.delivered).collect::<Vec<_>>()),
        by_time,
    }
}

pub fn generate(cfg: &ReportConfig) -> ReportOutput {
    let th = cfg.threads;
    let q = cfg.quick;
    let m = modem();
    let p = m.p;
    let t0 = std::time::Instant::now();
    let log = |s: &str| eprintln!("[report {:6.1}s] {s}", t0.elapsed().as_secs_f64());

    // ---------------------------------------------------------- modem measurements
    log("error rate against theory");
    let ber_frames = if q { 25 } else { 170 };
    let ber: Vec<(Constellation, Vec<ex::BerPoint>)> = [
        (Constellation::Qpsk, vec![1.0, 2.0, 3.0, 3.5, 4.0, 4.5, 5.0, 6.0, 8.0, 10.0]),
        (
            Constellation::Qam16,
            vec![6.0, 7.0, 8.0, 9.0, 9.5, 10.0, 10.5, 11.0, 12.0, 14.0, 16.0],
        ),
    ]
    .into_iter()
    .map(|(c, es)| (c, ex::ber_curve(c, &es, ber_frames, th)))
    .collect();
    log("robustness sweeps");
    let sweeps = ex::robustness_sweeps(if q { 6 } else { 24 }, th);
    log("numerology sweep");
    let numerology = ex::numerology_sweep(if q { 5 } else { 20 }, th);

    // ---------------------------------------------------------- channels and the constellation rule
    let channels = vec![Channel::good(), Channel::fair(), Channel::poor()];
    let probes: Vec<(f64, Constellation)> = parallel_map(channels.clone(), th, |ch| {
        let (_, snr) = ex::delivery(m, Constellation::Qpsk, &ch, 6, &RxOptions::default());
        (snr, recommend(snr))
    });

    // ---------------------------------------------------------- end to end
    log("end-to-end runs");
    let images: Vec<(String, Arc<Image>)> = cfg.images.iter().map(|(n, i)| (n.clone(), Arc::new(i.clone()))).collect();
    let greys: Vec<(String, f64, f64)> = images
        .iter()
        .map(|(n, i)| {
            let g = Image::filled(i.w, i.h, [128, 128, 128]);
            (n.clone(), psnr(i, &g), ssim(i, &g))
        })
        .collect();
    let grey = |name: &str| -> (f64, f64) { greys.iter().find(|g| g.0 == name).map(|g| (g.1, g.2)).unwrap() };
    let step = if q { 5.0 } else { 2.5 };
    let times: Vec<f64> = (0..=(75.0 / step) as usize).map(|i| i as f64 * step).collect();
    let starts: Vec<f64> = if q { vec![13.7] } else { vec![3.3, 21.9, 40.1, 61.7] };
    let shot_times = [5.0, 15.0, 30.0, 75.0];
    let gallery_ch = 1usize;
    let mut jobs = Vec::new();
    // (variant, use the rule's constellation?, label, ours)
    let lines: Vec<(Variant, bool, &str, bool)> = vec![
        (Variant::Windowed, true, "chirpix", true),
        (Variant::Windowed, false, "chirpix, other constellation", true),
        (Variant::CarouselProgressive, true, "progressive, in order, no fountain", false),
        (Variant::FlatAtomic, true, "flat fountain, all-or-nothing", false),
        (Variant::CarouselAtomic, true, "plain file, one pass in 75 s", false),
        (Variant::CarouselHalfAtomic, true, "plain file, half size, two passes", false),
    ];
    for (ii, (name, img)) in images.iter().enumerate() {
        for (ci, ch) in channels.iter().enumerate() {
            for (li, (variant, rule, _, _)) in lines.iter().enumerate() {
                let cons = if *rule {
                    probes[ci].1
                } else if probes[ci].1 == Constellation::Qpsk {
                    Constellation::Qam16
                } else {
                    Constellation::Qpsk
                };
                for (si, &start) in starts.iter().enumerate() {
                    let keep = ii == 0 && ci == gallery_ch && si == 0 && matches!(li, 0 | 2 | 4);
                    jobs.push((
                        (ii, ci, li, si),
                        E2eJob {
                            image_name: name.clone(),
                            image: img.clone(),
                            channel: Channel {
                                seed: ch.seed + 31 * si as u64 + 7 * ii as u64,
                                ..ch.clone()
                            },
                            variant: *variant,
                            cons,
                            design_seconds: 75.0,
                            start,
                            listen: 75.0,
                            times: times.clone(),
                            keep_images: if keep { shot_times.to_vec() } else { Vec::new() },
                        },
                    ));
                }
            }
        }
    }
    let results: Vec<((usize, usize, usize, usize), E2eRun)> = parallel_map(jobs, th, |(key, job)| (key, ex::run_e2e(&job)));
    let pick = |ci: usize, li: usize, ii: Option<usize>| -> Vec<&E2eRun> {
        results
            .iter()
            .filter(|(k, _)| k.1 == ci && k.2 == li && ii.is_none_or(|i| k.0 == i))
            .map(|(_, r)| r)
            .collect()
    };

    log("start-offset sweep");
    let off_step = if q { 15.0 } else { 5.0 };
    let off_starts: Vec<f64> = (0..=(85.0 / off_step) as usize).map(|i| i as f64 * off_step + 0.4).collect();
    let off_lines = [
        (Variant::Windowed, "chirpix", 1usize),
        (Variant::CarouselProgressive, "progressive, in order, no fountain", 2),
        (Variant::CarouselAtomic, "plain file, one pass in 75 s", 3),
    ];
    let mut off_jobs = Vec::new();
    for (li, (variant, _, _)) in off_lines.iter().enumerate() {
        for &start in &off_starts {
            off_jobs.push((
                (li, start),
                E2eJob {
                    image_name: images[0].0.clone(),
                    image: images[0].1.clone(),
                    channel: Channel {
                        seed: 900 + start as u64,
                        ..channels[gallery_ch].clone()
                    },
                    variant: *variant,
                    cons: probes[gallery_ch].1,
                    design_seconds: 75.0,
                    start,
                    listen: 30.0,
                    times: vec![30.0],
                    keep_images: Vec::new(),
                },
            ));
        }
    }
    let off_results: Vec<((usize, f64), E2eRun)> = parallel_map(off_jobs, th, |(key, job)| (key, ex::run_e2e(&job)));

    log("signal pictures");
    let demo_tx = Transmission::new(
        &images[0].1,
        &TxConfig::new(probes[gallery_ch].1, crate::fountain::Scheme::Windowed),
    );
    let demo_audio = channels[gallery_ch].apply(&demo_tx.audio(7, 5));
    let (demo_rx, _) = m.receive(
        &demo_audio,
        FS,
        &RxOptions {
            keep_debug: true,
            ..Default::default()
        },
    );
    let spec_img = spectrogram(&demo_audio[..demo_audio.len().min(6 * FS as usize)]);

    // ---------------------------------------------------------- write
    log("writing");
    let mut h = String::new();
    let mut summary = String::new();
    h += "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">";
    h += &format!("<title>chirpix report</title><style>{CSS}</style></head><body><main>");
    h += "<h1>chirpix: a picture sent as sound</h1>";
    h += &format!(
        "<p class=\"lead\">Simulated-channel measurements of the modem, the fountain code and the image codec. {} Everything on this page was computed by <code>chirpix report{}</code>; nothing here was recorded over the air.</p>",
        esc(&cfg.image_note),
        if q { " --quick" } else { "" }
    );
    h += "<h2>Signal</h2><table><tr><th>Parameter</th><th>Value</th></tr>";
    let rows = [
        ("Sample rate", format!("{} Hz", FS)),
        (
            "Band",
            format!(
                "{:.0}-{:.0} Hz, {} carriers {:.2} Hz apart",
                p.first_bin() as f64 * p.carrier_spacing_hz(),
                (p.first_bin() + p.ncar()) as f64 * p.carrier_spacing_hz(),
                p.ncar(),
                p.carrier_spacing_hz()
            ),
        ),
        (
            "OFDM symbol",
            format!(
                "{:.1} ms + {:.1} ms cyclic prefix",
                p.nfft as f64 * 1e3 / FS as f64,
                p.cp as f64 * 1e3 / FS as f64
            ),
        ),
        (
            "Frame",
            format!(
                "{:.3} s: chirp, training symbol, header symbol, {} data symbols",
                p.frame_seconds(),
                p.data_syms()
            ),
        ),
        (
            "Pilots",
            format!("every {}th carrier of every data symbol", crate::modem::PILOT_STEP),
        ),
        (
            "Error correction",
            "convolutional K=7 (133,171) rate 1/2, soft Viterbi; CRC-32 per packet".to_string(),
        ),
        (
            "Payload rate",
            format!(
                "QPSK {:.0} B/s, 16-QAM {:.0} B/s",
                p.byte_rate(Constellation::Qpsk),
                p.byte_rate(Constellation::Qam16)
            ),
        ),
    ];
    for (k, v) in rows {
        h += &format!("<tr><td>{k}</td><td style=\"text-align:left\">{}</td></tr>", esc(&v));
    }
    h += "</table>";

    // ---- gallery
    let gname = &images[0].0;
    h += &format!(
        "<h2>What the listener sees</h2><p>Image <b>{}</b>, channel <b>{}</b> ({}), {}. The listener starts recording {:.1} s into an endless transmission, in the middle of a frame.</p>",
        esc(gname),
        esc(&channels[gallery_ch].name),
        esc(&channels[gallery_ch].describe()),
        probes[gallery_ch].1.name(),
        starts[0]
    );
    for li in [0usize, 2, 4] {
        if let Some((_, run)) = results.iter().find(|(k, _)| *k == (0, gallery_ch, li, 0)) {
            h += &format!("<h3>{}</h3><div class=\"shots\">", esc(lines[li].2));
            for (t, img) in &run.images {
                let pt = run.points.iter().find(|pt| (pt.time - t).abs() < 1e-6).unwrap();
                let ar = images[0].1.h as f64 / images[0].1.w as f64;
                match img {
                    Some(i) => {
                        h += &format!(
                            "<div class=\"shot\">{}<b>{t:.0} s</b> · {:.1} dB · SSIM {:.3} · {:.1} kB</div>",
                            img_tag(i, &format!("{} after {t:.0} s", lines[li].2)),
                            pt.psnr.unwrap_or(0.0),
                            pt.ssim.unwrap_or(0.0),
                            pt.bytes as f64 / 1000.0
                        )
                    }
                    None => h += &format!("<div class=\"shot\"><div class=\"none\" style=\"height:{:.0}px\">nothing yet</div><b>{t:.0} s</b> · no picture</div>", 200.0 * ar),
                }
            }
            if li == 0 {
                h += &format!("<div class=\"shot\">{}<b>original</b></div>", img_tag(&images[0].1, "original"));
            }
            h += "</div>";
        }
    }

    // ---- headline tables
    h += "<h2>Quality against listening time</h2>";
    h += &format!(
        "<p>Mean over {} image{} and {} start time{} per channel. PSNR is over all RGB samples; SSIM is on luma (11x11 Gaussian window). A run with no picture yet is scored as a flat grey image, which is what a blank screen would get (mean {:.1} dB, SSIM {:.2} for these images); the small figure is how many runs had a picture. Every scheme uses the same modem, the same codec and the same seconds of audio.</p>",
        images.len(),
        if images.len() == 1 { "" } else { "s" },
        starts.len(),
        if starts.len() == 1 { "" } else { "s" },
        mean(&greys.iter().map(|g| g.1).collect::<Vec<_>>()),
        mean(&greys.iter().map(|g| g.2).collect::<Vec<_>>())
    );
    let tidx: Vec<usize> = shot_times
        .iter()
        .map(|t| times.iter().position(|x| (x - t).abs() < 1e-6).unwrap())
        .collect();
    let mut charts_psnr = String::new();
    for (ci, ch) in channels.iter().enumerate() {
        h += &format!(
            "<h3>Channel “{}”: {}</h3><p class=\"note\">Receiver measured {:.1} dB per carrier on a probe, so the rule (16-QAM from {:.0} dB) picks {}.</p>",
            esc(&ch.name),
            esc(&ch.describe()),
            probes[ci].0,
            QAM16_MIN_SNR_DB,
            probes[ci].1.name()
        );
        summary += &format!("Channel {}: {} -> {}\n", ch.name, ch.describe(), probes[ci].1.name());
        h += "<div class=\"tw\"><table><tr><th>Scheme</th><th>Mode</th><th>File</th><th>Packets delivered</th>";
        for t in shot_times {
            h += &format!("<th>{t:.0} s</th>");
        }
        h += "</tr>";
        let mut series = Vec::new();
        for (li, (_, _, label, ours)) in lines.iter().enumerate() {
            let runs = pick(ci, li, None);
            if runs.is_empty() {
                continue;
            }
            let a = aggregate(&runs, &grey, label, *ours);
            h += &format!(
                "<tr><td{}>{}</td><td>{}</td><td>{:.1} kB</td><td>{:.0}%</td>",
                if a.ours { " class=\"ours\"" } else { "" },
                esc(&a.label),
                a.cons.name(),
                a.source_bytes / 1000.0,
                a.delivered * 100.0
            );
            summary += &format!("  {:<36} {:<7} {:5.1} kB", a.label, a.cons.name(), a.source_bytes / 1000.0);
            for &ti in &tidx {
                let (_, ps, ss, have, n) = a.by_time[ti];
                h += &format!("<td>{ps:.1} dB · {ss:.3} <small>{have}/{n}</small></td>");
                summary += &format!("  {ps:5.1} dB {ss:.3} ({have}/{n})");
            }
            h += "</tr>";
            summary += "\n";
            if li != 1 && li != 5 {
                let slot = [1, 0, 2, 4, 3, 0][li];
                series.push(Series::new(label, slot, a.by_time.iter().map(|b| (b.0, b.1)).collect()));
            }
        }
        h += "</table></div>";
        let mut chart = Chart::new(&format!("Mean PSNR, channel “{}”", ch.name), "seconds listened", "PSNR (dB)");
        chart.series = series;
        chart.decimals = 1;
        chart.x_ticks = Some(vec![0.0, 5.0, 15.0, 30.0, 45.0, 60.0, 75.0]);
        chart.width = 360.0;
        chart.height = 270.0;
        charts_psnr += &render(&chart).replace("class=\"chart\"", "class=\"chart small\"");
    }
    h += &format!("<div class=\"row\">{charts_psnr}</div>");
    h += "<details><summary>Per-image numbers for chirpix</summary><table><tr><th>Image</th><th>Channel</th><th>Mode</th>";
    for t in shot_times {
        h += &format!("<th>{t:.0} s</th>");
    }
    h += "</tr>";
    for (ii, (name, _)) in images.iter().enumerate() {
        for (ci, ch) in channels.iter().enumerate() {
            let runs = pick(ci, 0, Some(ii));
            if runs.is_empty() {
                continue;
            }
            let a = aggregate(&runs, &grey, "chirpix", true);
            h += &format!("<tr><td>{}</td><td>{}</td><td>{}</td>", esc(name), esc(&ch.name), a.cons.name());
            summary += &format!("  per-image {:<10} {:<5} {:<7}", name, ch.name, a.cons.name());
            for &ti in &tidx {
                let (_, ps, ss, have, n) = a.by_time[ti];
                h += &format!("<td>{ps:.1} dB · {ss:.3} <small>{have}/{n}</small></td>");
                summary += &format!("  {ps:5.1} dB {ss:.3}");
            }
            h += "</tr>";
            summary += "\n";
        }
    }
    h += "</table></details>";

    // ---- start offset
    h += &format!(
        "<h2>Quality against the moment the listener starts</h2><p>Image {}, channel “{}”, every scheme sized for 75 s. Each point is one run: the listener starts at that time into the transmission and listens for 30 s.</p>",
        esc(gname),
        esc(&channels[gallery_ch].name)
    );
    let mut chart = Chart::new("PSNR after 30 s of listening", "start time (s into the transmission)", "PSNR (dB)");
    chart.decimals = 1;
    let g0 = grey(gname).0;
    h += "<div class=\"row\">";
    let mut table = String::from("<table><tr><th>Start (s)</th>");
    for (_, label, _) in &off_lines {
        table += &format!("<th>{}</th>", esc(label));
    }
    table += "</tr>";
    for &start in &off_starts {
        table += &format!("<tr><td>{start:.0}</td>");
        for li in 0..off_lines.len() {
            let r = &off_results.iter().find(|(k, _)| k.0 == li && k.1 == start).unwrap().1;
            match r.points[0].psnr {
                Some(v) => table += &format!("<td>{v:.1} dB</td>"),
                None => table += "<td>no picture</td>",
            }
        }
        table += "</tr>";
    }
    table += "</table>";
    for (li, (_, label, slot)) in off_lines.iter().enumerate() {
        let pts = off_starts
            .iter()
            .map(|&s| {
                (
                    s,
                    off_results.iter().find(|(k, _)| k.0 == li && k.1 == s).unwrap().1.points[0]
                        .psnr
                        .unwrap_or(g0),
                )
            })
            .collect();
        chart.series.push(Series::new(label, *slot, pts));
    }
    let ours: Vec<f64> = off_starts
        .iter()
        .map(|&s| {
            off_results.iter().find(|(k, _)| k.0 == 0 && k.1 == s).unwrap().1.points[0]
                .psnr
                .unwrap_or(g0)
        })
        .collect();
    let (omin, omax) = ours.iter().fold((f64::MAX, f64::MIN), |a, v| (a.0.min(*v), a.1.max(*v)));
    summary += &format!(
        "Start-offset sweep (30 s of listening, channel {}): chirpix PSNR between {omin:.1} and {omax:.1} dB over {} start times\n",
        channels[gallery_ch].name,
        off_starts.len()
    );
    h += &render(&chart);
    h += &format!("<details><summary>Table</summary>{table}</details></div>");
    h += &format!("<p class=\"note\">A run with no picture is drawn at the flat-grey score ({g0:.1} dB).</p>");

    // ---- BER
    h += "<h2>Error rate against theory</h2><p>White noise only. The x axis is the signal-to-noise ratio per carrier (Es/N0). “Uncoded” counts raw bit decisions before the Viterbi decoder; “coded” counts information bits after it. The ideal curve is the same mapping, soft-bit rule and decoder fed with perfect timing and channel knowledge, so the gap between it and the modem is the cost of synchronisation and channel estimation.</p><div class=\"row\">";
    for (cons, pts) in &ber {
        let mut chart = Chart::new(&format!("{}: bit error rate", cons.name()), "Es/N0 (dB)", "bit error rate");
        chart.log_y = true;
        chart.y_range = Some((1e-6, 1.0));
        chart
            .series
            .push(Series::new("uncoded, theory", 1, pts.iter().map(|b| (b.esn0_db, b.uncoded_theory)).collect()).dashed());
        chart.series.push(Series::new(
            "uncoded, modem",
            1,
            pts.iter().map(|b| (b.esn0_db, b.uncoded_modem)).collect(),
        ));
        if *cons == Constellation::Qpsk {
            chart.series.push(
                Series::new(
                    "coded, union bound",
                    3,
                    pts.iter()
                        .filter(|b| b.coded_bound >= 1e-6)
                        .map(|b| (b.esn0_db, b.coded_bound))
                        .collect(),
                )
                .dashed(),
            );
        }
        chart.series.push(Series::new(
            "coded, ideal receiver",
            2,
            pts.iter().map(|b| (b.esn0_db, b.coded_ideal)).collect(),
        ));
        chart.series.push(Series::new(
            "coded, modem",
            4,
            pts.iter().map(|b| (b.esn0_db, b.coded_modem)).collect(),
        ));
        h += &render(&chart);
    }
    h += "</div><div class=\"tw\"><table><tr><th>Constellation</th><th>Uncoded BER 1e-2: theory</th><th>modem</th><th>loss</th><th>Coded BER 1e-4: ideal receiver</th><th>modem</th><th>implementation loss</th><th>Packet loss &lt; 1% from</th></tr>";
    for (cons, pts) in &ber {
        let c = |f: &dyn Fn(&ex::BerPoint) -> f64, target: f64| {
            ex::crossing(&pts.iter().map(|b| (b.esn0_db, f(b))).collect::<Vec<_>>(), target)
        };
        let (ut, um) = (c(&|b| b.uncoded_theory, 1e-2), c(&|b| b.uncoded_modem, 1e-2));
        let (ci, cm) = (c(&|b| b.coded_ideal, 1e-4), c(&|b| b.coded_modem, 1e-4));
        let pl = pts.iter().find(|b| b.packet_loss < 0.01).map(|b| b.esn0_db);
        let f = |v: Option<f64>| v.map(|x| format!("{x:.1} dB")).unwrap_or_else(|| "n/a".into());
        let d = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) => format!("{:.1} dB", b - a),
            _ => "n/a".into(),
        };
        h += &format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            cons.name(),
            f(ut),
            f(um),
            d(ut, um),
            f(ci),
            f(cm),
            d(ci, cm),
            f(pl)
        );
        summary += &format!(
            "{}: uncoded BER 1e-2 at {} (theory {}), coded BER 1e-4 at {} (ideal receiver {}): implementation loss {}; packet loss < 1% from {}\n",
            cons.name(),
            f(um),
            f(ut),
            f(cm),
            f(ci),
            d(ci, cm),
            f(pl)
        );
    }
    h += &format!("</table></div><p class=\"note\">{} frames per point ({} information bits for QPSK). Eb/N0 = Es/N0 for QPSK at rate 1/2 and Es/N0 − 3 dB for 16-QAM at rate 1/2; these figures do not include the cyclic prefix, pilots and frame overhead, which cost a further {:.1} dB of transmitted energy.</p>",
        ber_frames,
        ber_frames * crate::modem::BLOCKS * 890,
        -10.0 * ((p.data_syms() * p.nfft) as f64 * (p.ndata() as f64 / p.ncar() as f64) / p.frame_len() as f64).log10());

    // ---- constellation rule
    h += &format!("<h2>Choosing the constellation</h2><p>There is no return channel, so the sender must choose before transmitting. The rule: send QPSK unless a test recording decoded with <code>chirpix decode</code> reports at least {QAM16_MIN_SNR_DB:.0} dB per carrier, in which case 16-QAM doubles the rate. The receiver's figure already includes echo that outlasts the cyclic prefix, which is why it saturates in reverberant rooms however loud the signal is. The sweeps below show where each constellation stops working.</p>");

    // ---- sweeps
    h += "<h2>Where it breaks</h2><p>Share of packets delivered, QPSK and 16-QAM, one impairment varied at a time. Each point is a separate run.</p><div class=\"row\">";
    for sw in &sweeps {
        let mut chart = Chart::new(&sw.title, &sw.x_label, "packets delivered");
        chart.y_range = Some((0.0, 1.0));
        chart.width = 360.0;
        chart.height = 240.0;
        chart
            .series
            .push(Series::new("QPSK", 1, sw.points.iter().map(|pt| (pt.x, pt.qpsk)).collect()));
        chart
            .series
            .push(Series::new("16-QAM", 2, sw.points.iter().map(|pt| (pt.x, pt.qam16)).collect()));
        h += &render(&chart).replace("class=\"chart\"", "class=\"chart small\"").replace(
            "</svg></figure>",
            &format!("</svg><div class=\"note\">{}</div></figure>", esc(&sw.note)),
        );
    }
    h += "</div><details><summary>Tables</summary>";
    for sw in &sweeps {
        h += &format!(
            "<h3>{}</h3><table><tr><th>{}</th><th>QPSK</th><th>16-QAM</th><th>receiver SNR (QPSK run)</th></tr>",
            esc(&sw.title),
            esc(&sw.x_label)
        );
        for pt in &sw.points {
            h += &format!(
                "<tr><td>{}</td><td>{:.0}%</td><td>{:.0}%</td><td>{}</td></tr>",
                esc(&pt.label),
                pt.qpsk * 100.0,
                pt.qam16 * 100.0,
                if pt.rx_snr_db.is_nan() {
                    "no frame".into()
                } else {
                    format!("{:.1} dB", pt.rx_snr_db)
                }
            );
        }
        h += "</table>";
    }
    h += "</details>";

    // ---- numerology
    h += "<h2>Why the symbols are this long</h2><p>The same modem at four symbol lengths, with the cyclic prefix a quarter of the symbol. Echo that arrives after the cyclic prefix is interference no matter how loud the signal is, so short symbols hit a ceiling in a reverberant room; long symbols cost rate (more of each frame is preamble) and are more sensitive to clock offset and movement. Cells: packets delivered QPSK / 16-QAM. The row in bold is the numerology used everywhere else on this page.</p>";
    let head = |h: &mut String| *h += "<div class=\"tw\"><table><tr><th>Symbol + prefix</th><th>Carrier spacing</th><th>QPSK rate</th>";
    let lead = |h: &mut String, r: &ex::NumerologyRow| {
        let pr: Params = r.params;
        *h += &format!(
            "<tr><td{}>{:.0} + {:.0} ms</td><td>{:.1} Hz</td><td>{:.0} B/s</td>",
            if pr == p { " class=\"ours\"" } else { "" },
            pr.nfft as f64 * 1e3 / FS as f64,
            pr.cp as f64 * 1e3 / FS as f64,
            pr.carrier_spacing_hz(),
            r.byte_rate
        );
    };
    h += "<h3>Against echo: RT60 0.45 s, noise 25 dB down, by direct-to-reverberant ratio</h3>";
    head(&mut h);
    for (d, ..) in &numerology[0].by_drr {
        h += &format!("<th>DRR {d:+.0} dB</th>");
    }
    h += "</tr>";
    for r in &numerology {
        lead(&mut h, r);
        for (_, qp, qa, snr) in &r.by_drr {
            h += &format!(
                "<td>{:.0}% / {:.0}% <small>{}</small></td>",
                qp * 100.0,
                qa * 100.0,
                if snr.is_nan() { "no frame".into() } else { format!("{snr:.0} dB") }
            );
        }
        h += "</tr>";
    }
    h += "</table></div><p class=\"note\">The small figure is the SNR the receiver measured.</p><h3>Against clock offset, with the resampling pass switched off: SNR 20 dB, light reverberation</h3>";
    head(&mut h);
    for (ppm, ..) in &numerology[0].by_ppm {
        h += &format!("<th>{ppm:.0} ppm</th>");
    }
    h += "</tr>";
    for r in &numerology {
        lead(&mut h, r);
        for (_, qp, qa) in &r.by_ppm {
            h += &format!("<td>{:.0}% / {:.0}%</td>", qp * 100.0, qa * 100.0);
        }
        h += "</tr>";
    }
    h += "</table></div>";

    // ---- signal pictures
    h += &format!(
        "<h2>The signal itself</h2><p>Five frames through channel “{}”.</p><div class=\"row\">",
        esc(&channels[gallery_ch].name)
    );
    h += &format!(
        "<figure class=\"chart\"><figcaption>Spectrogram of the received audio</figcaption><img style=\"width:100%;image-rendering:pixelated\" alt=\"spectrogram\" src=\"data:image/png;base64,{}\"><div class=\"note\">Time left to right ({:.1} s), 0 to 12 kHz bottom to top, 60 dB range. The sweeps are the chirp and the training symbol at the start of each frame.</div></figure>",
        base64(&crate::png::encode(&spec_img)),
        spec_img.w as f64 * 512.0 / FS as f64
    );
    if let Some(f) = demo_rx.frames.iter().find(|f| !f.points.is_empty()) {
        let mut svg = String::from("<figure class=\"chart small\"><figcaption>Equalised constellation, one code block</figcaption><svg viewBox=\"0 0 300 300\" role=\"img\" aria-label=\"constellation\"><line class=\"axis\" x1=\"150\" x2=\"150\" y1=\"6\" y2=\"294\"/><line class=\"axis\" x1=\"6\" x2=\"294\" y1=\"150\" y2=\"150\"/>");
        for (re, im) in f.points.iter() {
            let (x, y) = (150.0 + *re as f64 * 85.0, 150.0 - *im as f64 * 85.0);
            if x > 4.0 && x < 296.0 && y > 4.0 && y < 296.0 {
                svg += &format!("<circle class=\"dot\" cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"1.6\"/>");
            }
        }
        svg += &format!(
            "</svg><div class=\"note\">{} points, {}, receiver SNR {:.1} dB, clock offset {:+.0} ppm.</div></figure>",
            f.points.len(),
            f.spec.cons.name(),
            f.snr_db,
            f.ppm
        );
        h += &svg;
        let mut chart = Chart::new("Channel as the receiver sees it", "frequency (kHz)", "dB");
        chart.decimals = 1;
        let fk = |c: usize| (p.first_bin() + c) as f64 * p.carrier_spacing_hz() / 1000.0;
        let dec = (p.ncar() / 128).max(1);
        let mut gain = Series::new(
            "channel gain",
            1,
            (0..p.ncar())
                .step_by(dec)
                .map(|c| (fk(c), 20.0 * (f.channel_mag[c].max(1e-6) as f64).log10()))
                .collect(),
        );
        gain.markers = false;
        let mut snr = Series::new(
            "SNR per carrier",
            2,
            (0..p.ncar()).step_by(dec).map(|c| (fk(c), f.snr_per_carrier[c] as f64)).collect(),
        );
        snr.markers = false;
        chart.series = vec![gain, snr];
        h += &render(&chart);
    }
    h += "</div>";

    h += "<h2>Reading these numbers</h2><ul><li>All channels here are simulated: band-limiting, a synthetic room impulse response (exponentially decaying noise with the stated RT60 and direct-to-reverberant ratio), a resampler for clock offset, white noise, clicks, deleted audio and clipping. A real loudspeaker, room and microphone also add non-linear distortion, automatic gain control and movement, none of which is modelled.</li><li>SNR means signal power over the noise power inside the modem's 6 kHz band.</li><li>The plain-file and flat-fountain baselines carry the same codec's output; they differ only in how packets are scheduled and in showing nothing until the file is complete.</li><li>No analogue SSTV baseline was run.</li></ul>";
    h += &format!(
        "<p class=\"note\">Generated in {:.0} s on {} thread{}.</p>",
        t0.elapsed().as_secs_f64(),
        th,
        if th == 1 { "" } else { "s" }
    );
    h += "</main></body></html>";
    ReportOutput { html: h, summary }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectrogram_shows_a_tone_at_the_right_height() {
        let x: Vec<f32> = (0..48_000)
            .map(|i| (std::f64::consts::TAU * 3000.0 * i as f64 / 48_000.0).sin() as f32)
            .collect();
        let img = spectrogram(&x);
        assert_eq!(img.h, 256);
        // 3 kHz is bin 64 of 1024 at 48 kHz: row 255 - 64 from the top is the darkest.
        let col = img.w / 2;
        let darkest = (0..256)
            .min_by_key(|&r| img.px(col, r).iter().map(|&v| v as u32).sum::<u32>())
            .unwrap();
        assert!((darkest as i32 - 191).abs() <= 1, "row {darkest}");
    }

    #[test]
    fn rule_switches_at_the_threshold() {
        assert_eq!(recommend(11.9), Constellation::Qpsk);
        assert_eq!(recommend(12.0), Constellation::Qam16);
    }
}
