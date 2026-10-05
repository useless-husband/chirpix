use chirpix::{codec, image, png};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("codec") => {
            let img = png::read(Path::new(&args[1])).unwrap();
            let t = std::time::Instant::now();
            let full = codec::encode(&img, 70_000);
            let te = t.elapsed();
            for n in [600, 1500, 3000, 6000, 12000, 18000, 24000, 33000, 66000] {
                let t = std::time::Instant::now();
                let out = codec::decode(&full[..n.min(full.len())]).unwrap();
                let td = t.elapsed();
                println!("{n:6} B  PSNR {:.2}  SSIM {:.4}  dec {:?} enc {:?}", image::psnr(&img, &out), image::ssim(&img, &out), td, te);
            }
        }
        Some("ber") => {
            use chirpix::{experiments as ex, modem::Constellation};
            let frames: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100);
            for (cons, es) in [
                (Constellation::Qpsk, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 10.0]),
                (Constellation::Qam16, vec![7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 16.0]),
            ] {
                println!("{cons:?}: Es/N0  unc_theory unc_modem  bound    ideal    modem   pkt_loss lost");
                let pts = ex::ber_curve(cons, &es, frames, 4);
                for p in &pts {
                    println!("  {:5.1}  {:.2e} {:.2e}  {:.2e} {:.2e} {:.2e}  {:.3} {}", p.esn0_db, p.uncoded_theory, p.uncoded_modem, p.coded_bound, p.coded_ideal, p.coded_modem, p.packet_loss, p.frames_lost);
                }
                let ideal: Vec<(f64, f64)> = pts.iter().map(|p| (p.esn0_db, p.coded_ideal)).collect();
                let md: Vec<(f64, f64)> = pts.iter().map(|p| (p.esn0_db, p.coded_modem)).collect();
                println!("  coded BER 1e-4: ideal {:?} modem {:?}", ex::crossing(&ideal, 1e-4), ex::crossing(&md, 1e-4));
            }
        }
        Some("sweeps") => {
            let frames: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
            for sw in chirpix::experiments::robustness_sweeps(frames, 4) {
                println!("{} [{}]", sw.title, sw.x_label);
                for p in &sw.points {
                    println!("  {:>7}  qpsk {:.2}  qam16 {:.2}  rx snr {:.1}", p.label, p.qpsk, p.qam16, p.rx_snr_db);
                }
            }
        }
        Some("numerology") => {
            let frames: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(20);
            for r in chirpix::experiments::numerology_sweep(frames, 4) {
                println!("N={} CP={} frame {:.3}s rate {:.0} B/s", r.params.nfft, r.params.cp, r.params.frame_seconds(), r.byte_rate);
                for (d, q, h, snr) in &r.by_drr {
                    println!("   DRR {d:+.0}: qpsk {q:.2} qam16 {h:.2} rx snr {snr:.1}");
                }
                for (ppm, q, h) in &r.by_ppm {
                    println!("   ppm {ppm:.0} (no resample): qpsk {q:.2} qam16 {h:.2}");
                }
            }
        }
        _ => eprintln!("usage"),
    }
}
