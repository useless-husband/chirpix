//! The Robot 36 encoder and decoder against two independent implementations:
//! pySSTV 0.5.9 (encoder) and the `sstv` 0.2.0 Python package (decoder).
//! They need Python packages, so they are ignored by `cargo test`; run them
//! with `make sstv-check`, which installs the packages into a private virtual
//! environment, makes the fixtures (scripts/sstv_crosscheck.py) and then runs
//! these tests with CHIRPIX_SSTV_FIXTURES pointing at them.

use chirpix::channel::Channel;
use chirpix::image::{psnr, Image};
use chirpix::modem::FS;
use chirpix::sstv::{self, Convention, Placement};
use chirpix::{png, wav};
use std::path::PathBuf;

const NAMES: [&str; 2] = ["scene", "clouds"];

fn file(name: &str) -> PathBuf {
    let dir = std::env::var("CHIRPIX_SSTV_FIXTURES").expect("run through `make sstv-check`, which makes the fixtures");
    PathBuf::from(dir).join(name)
}

fn picture(name: &str) -> Image {
    png::read(&file(name)).unwrap()
}

fn audio(name: &str) -> Vec<f32> {
    let w = wav::read(&file(name)).unwrap();
    assert_eq!(w.rate, FS);
    w.samples
}

fn decoded(samples: &[f32], conv: Convention, smoothing: Option<(f64, f64)>) -> Image {
    let rx = sstv::receive_with(samples, FS, smoothing);
    assert_eq!(rx.headers.len(), 1);
    assert_eq!((rx.headers[0].code, rx.headers[0].parity_ok), (sstv::VIS_CODE, true));
    assert_eq!(rx.rows_shown(rx.duration, Placement::FromVis), sstv::HEIGHT);
    rx.picture(rx.duration, Placement::FromVis, conv).unwrap()
}

#[test]
#[ignore]
fn encoder_matches_pysstv_sample_for_sample() {
    for name in NAMES {
        // Pillow's own YCbCr values through our timing and synthesis.
        let ycc: Vec<[f64; 3]> = picture(&format!("{name}_ycbcr.png"))
            .data
            .chunks_exact(3)
            .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
            .collect();
        let ours = sstv::synthesize(&sstv::frame_tones_ycc(&ycc, false), FS, 1.0);
        let theirs = audio(&format!("pysstv_{name}.wav"));
        assert_eq!(ours.len(), theirs.len(), "{name}: length");
        // pySSTV truncates to 16 bits (and we compare in f32); anything beyond
        // one step is a timing or frequency difference.
        let worst = ours
            .iter()
            .zip(&theirs)
            .map(|(a, b)| ((a - b) * 32768.0).abs())
            .fold(0f32, f32::max);
        println!(
            "{name}: {} samples, largest difference from pySSTV {worst:.3} of a 16-bit step",
            ours.len()
        );
        assert!(worst <= 1.01, "{name}: differs from pySSTV by {worst} steps");
        // Our float version of Pillow's conversion is within one level of it.
        let pic = picture(&format!("{name}.png"));
        let mut off = 0usize;
        for (p, want) in pic.data.chunks_exact(3).zip(&ycc) {
            let got = Convention::Pysstv.to_ycc([p[0] as f64, p[1] as f64, p[2] as f64]);
            let d = (0..3).map(|i| (got[i] - want[i]).abs()).fold(0.0, f64::max);
            assert!(d <= 1.0, "{name}: {got:?} against Pillow's {want:?}");
            off += (d > 0.0) as usize;
        }
        println!(
            "{name}: our full-range YCbCr differs from Pillow's by one level in {off} of {} pixels",
            ycc.len()
        );
    }
}

#[test]
#[ignore]
fn decoder_reads_pysstv_audio() {
    for name in NAMES {
        let pic = picture(&format!("{name}.png"));
        let theirs = audio(&format!("pysstv_{name}.wav"));
        let reference = psnr(&pic, &picture(&format!("sstvpy_pysstv_{name}.png")));
        let auto = psnr(&pic, &decoded(&theirs, Convention::Pysstv, None));
        let light = psnr(&pic, &decoded(&theirs, Convention::Pysstv, Some((1.0, 4.0))));
        let wrong = psnr(&pic, &decoded(&theirs, Convention::Spec, Some((1.0, 4.0))));
        println!(
            "{name}: pySSTV audio decoded by the sstv package {reference:.2} dB; by chirpix {auto:.2} dB (smoothing chosen for noise), {light:.2} dB (smoothing 1,4), {wrong:.2} dB (smoothing 1,4, assuming studio swing)"
        );
        assert!(light >= reference - 1.0, "{name}: {light:.2} dB against {reference:.2} dB");
        assert!(auto >= reference - 4.0, "{name}: {auto:.2} dB against {reference:.2} dB");
        assert!(wrong < light, "{name}: the colour convention should matter");
    }
}

#[test]
#[ignore]
fn independent_decoder_reads_our_audio() {
    for name in NAMES {
        let pic = picture(&format!("{name}.png"));
        let theirs = psnr(&pic, &picture(&format!("sstvpy_pysstv_{name}.png")));
        let ours_py = psnr(&pic, &picture(&format!("sstvpy_chirpix_{name}_pysstv.png")));
        // The sstv package assumes full-range colour. Its picture of our
        // studio-swing audio is wrong in contrast, but the Y, B-Y and R-Y it
        // measured can be read back out and put through the right equations.
        let spec = picture(&format!("sstvpy_chirpix_{name}_spec.png"));
        let mut fixed = spec.clone();
        for p in fixed.data.chunks_exact_mut(3) {
            let rgb = [p[0] as f64, p[1] as f64, p[2] as f64];
            let ycc = Convention::Pysstv.to_ycc(rgb);
            let back = Convention::Spec.to_rgb(ycc);
            for i in 0..3 {
                p[i] = back[i].round().clamp(0.0, 255.0) as u8;
            }
        }
        let (ours_spec, ours_fixed) = (psnr(&pic, &spec), psnr(&pic, &fixed));
        println!(
            "{name}: decoded by the sstv package: pySSTV audio {theirs:.2} dB; chirpix --convention pysstv {ours_py:.2} dB; chirpix --convention spec {ours_spec:.2} dB as it shows it, {ours_fixed:.2} dB with studio-swing equations"
        );
        assert!((ours_py - theirs).abs() < 0.5, "{name}: {ours_py:.2} against {theirs:.2} dB");
        assert!(ours_fixed > theirs - 1.5, "{name}: {ours_fixed:.2} against {theirs:.2} dB");
    }
}

#[test]
#[ignore]
fn either_encoder_gives_the_same_result_through_the_simulated_room() {
    for name in NAMES {
        let pic = picture(&format!("{name}.png"));
        let ours: Vec<f32> = audio(&format!("chirpix_{name}_pysstv.wav"));
        let theirs = audio(&format!("pysstv_{name}.wav"));
        for ch in [Channel::good(), Channel::fair()] {
            let a = psnr(&pic, &decoded(&ch.apply(&ours), Convention::Pysstv, None));
            let b = psnr(&pic, &decoded(&ch.apply(&theirs), Convention::Pysstv, None));
            println!("{name}, channel {}: chirpix audio {a:.2} dB, pySSTV audio {b:.2} dB", ch.name);
            assert!((a - b).abs() < 1.0, "{name} {}: {a:.2} against {b:.2} dB", ch.name);
        }
    }
}
