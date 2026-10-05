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
        Some("dbg") => {
            use chirpix::modem::*;
            use chirpix::util::Rng;
            let cons = Constellation::Qpsk;
            let m = modem();
            let seed = 0xBE5 + 50u64;
            let mut rng = Rng::new(seed);
            let mut audio = vec![0f32; 3000];
            for i in 0..300 {
                let spec = FrameSpec { cons, scheme: 0, session: 1, counter: i as u32, k: 100 };
                let pk: Vec<[u8; 107]> = (0..4).map(|_| { let mut p = [0u8; 107]; for v in p.iter_mut() { *v = rng.next_u64() as u8; } p }).collect();
                audio.extend(m.modulate_frame(&spec, &pk));
            }
            audio.extend(vec![0f32; 3000]);
            let g = 10f64.powf(0.5);
            let sigma = (CARRIER_AMP * CARRIER_AMP * NFFT as f64 / (4.0 * g)).sqrt();
            for v in audio.iter_mut() { *v += (sigma * rng.gauss()) as f32; }
            let rep = m.demodulate(&audio, &RxOptions::default());
            println!("frames {} false {}", rep.frames.len(), rep.false_starts);
            let mut prev: i64 = -1;
            for f in &rep.frames {
                if f.spec.counter as i64 != prev + 1 { println!("gap before counter {} start {} (expected start {})", f.spec.counter, f.start, 3000 + (prev + 1) * FRAME_LEN as i64); }
                prev = f.spec.counter as i64;
                if (f.start as i64 - (3000 + f.spec.counter as i64 * FRAME_LEN as i64)).abs() > 20 { println!("counter {} start {} off by {}", f.spec.counter, f.start, f.start as i64 - (3000 + f.spec.counter as i64 * FRAME_LEN as i64)); }
            }
        }
        _ => eprintln!("usage"),
    }
}
