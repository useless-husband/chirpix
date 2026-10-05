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
        _ => eprintln!("usage"),
    }
}
