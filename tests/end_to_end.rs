//! Whole-system tests: a picture goes in, audio goes through the simulated
//! channel, and pictures must come out, with the properties the project
//! claims. Fixed seeds throughout.

use chirpix::channel::Channel;
use chirpix::experiments::{run_e2e, E2eJob, Variant};
use chirpix::image::synthetic;
use chirpix::modem::Constellation;
use std::process::Command;
use std::sync::Arc;

fn job(variant: Variant, channel: Channel, cons: Constellation, start: f64, listen: f64, times: &[f64]) -> E2eJob {
    E2eJob {
        image_name: "scene".into(),
        image: Arc::new(synthetic("scene", 256, 192)),
        channel,
        variant,
        cons,
        design_seconds: 40.0,
        start,
        listen,
        times: times.to_vec(),
        keep_images: Vec::new(),
    }
}

#[test]
fn picture_sharpens_with_listening_time_in_an_ordinary_room() {
    let times = [6.0, 12.0, 24.0, 44.0];
    let run = run_e2e(&job(Variant::Windowed, Channel::fair(), Constellation::Qpsk, 17.3, 44.0, &times));
    let q: Vec<f64> = run.points.iter().map(|p| p.psnr.unwrap_or(0.0)).collect();
    assert!(q[0] > 15.0, "a picture within 6 s: {q:?}");
    assert!(q.windows(2).all(|w| w[1] >= w[0]), "quality must never fall: {q:?}");
    assert!(q[3] > q[0] + 4.0, "and must clearly improve: {q:?}");
    assert!(run.delivered > 0.9, "packet delivery {:.2}", run.delivered);
    assert!((run.rx_ppm + 70.0).abs() < 10.0, "clock offset estimate {:.1} ppm", run.rx_ppm);
}

#[test]
fn start_time_does_not_matter_for_the_windowed_scheme_but_does_for_plain_order() {
    // After 20 s of listening, at three different points in the transmission.
    let mut windowed = Vec::new();
    let mut in_order = Vec::new();
    for start in [1.1, 13.9, 29.3] {
        let w = run_e2e(&job(Variant::Windowed, Channel::good(), Constellation::Qpsk, start, 20.0, &[20.0]));
        let o = run_e2e(&job(
            Variant::CarouselProgressive,
            Channel::good(),
            Constellation::Qpsk,
            start,
            20.0,
            &[20.0],
        ));
        windowed.push(w.points[0].psnr);
        in_order.push(o.points[0].psnr);
    }
    let w: Vec<f64> = windowed
        .iter()
        .map(|p| p.expect("windowed scheme always has a picture after 20 s"))
        .collect();
    let spread = w.iter().cloned().fold(f64::MIN, f64::max) - w.iter().cloned().fold(f64::MAX, f64::min);
    assert!(spread < 5.0, "windowed quality should barely depend on the start: {w:?}");
    assert!(
        in_order.iter().any(|p| p.is_none()),
        "in-order sending has nothing for some start times: {in_order:?}"
    );
}

#[test]
fn plain_file_shows_nothing_until_complete_and_wins_when_it_completes() {
    let times = [20.0, 44.0];
    let plain = run_e2e(&job(
        Variant::CarouselAtomic,
        Channel::good(),
        Constellation::Qpsk,
        5.0,
        44.0,
        &times,
    ));
    let ours = run_e2e(&job(Variant::Windowed, Channel::good(), Constellation::Qpsk, 5.0, 44.0, &times));
    assert!(plain.points[0].psnr.is_none() && ours.points[0].psnr.is_some());
    // The honest other half: with enough clean listening the plain file is the better picture.
    assert!(plain.points[1].psnr.unwrap() > ours.points[1].psnr.unwrap());
}

#[test]
fn a_bad_channel_still_yields_a_picture_where_the_plain_file_yields_none() {
    let times = [60.0];
    let ours = run_e2e(&job(Variant::Windowed, Channel::poor(), Constellation::Qpsk, 9.0, 60.0, &times));
    let plain = run_e2e(&job(
        Variant::CarouselAtomic,
        Channel::poor(),
        Constellation::Qpsk,
        9.0,
        60.0,
        &times,
    ));
    assert!(
        ours.delivered < 0.97,
        "the poor channel should lose packets (delivered {:.2})",
        ours.delivered
    );
    assert!(
        ours.points[0].psnr.unwrap_or(0.0) > 18.0,
        "picture after 60 s: {:?}",
        ours.points[0].psnr
    );
    assert!(plain.points[0].psnr.is_none(), "one pass with losses cannot complete");
}

#[test]
fn sixteen_qam_doubles_the_data_on_a_clean_channel_and_fails_on_a_poor_one() {
    let good = run_e2e(&job(Variant::Windowed, Channel::good(), Constellation::Qam16, 3.0, 30.0, &[30.0]));
    let base = run_e2e(&job(Variant::Windowed, Channel::good(), Constellation::Qpsk, 3.0, 30.0, &[30.0]));
    assert!(
        good.points[0].bytes as f64 > 1.5 * base.points[0].bytes as f64,
        "{} vs {}",
        good.points[0].bytes,
        base.points[0].bytes
    );
    let bad = run_e2e(&job(Variant::Windowed, Channel::poor(), Constellation::Qam16, 3.0, 30.0, &[30.0]));
    assert!(
        bad.delivered < 0.2,
        "16-QAM should not survive the poor channel (delivered {:.2})",
        bad.delivered
    );
}

#[test]
fn command_line_round_trip_at_another_sample_rate() {
    let bin = env!("CARGO_BIN_EXE_chirpix");
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("cli round trip");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = |name: &str| dir.join(name).to_string_lossy().to_string();
    let run = |args: &[&str]| -> String {
        let out = Command::new(bin).args(args).output().expect("run chirpix");
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success(), "chirpix {args:?} failed:\n{text}");
        text
    };
    run(&["testimage", "chart", "-o", &p("in.png"), "--size", "200x150"]);
    let enc = run(&["encode", &p("in.png"), "-o", &p("tx.wav"), "--design", "20", "--seconds", "26"]);
    assert!(enc.contains("QPSK") && enc.contains("layers"));
    run(&[
        "simulate",
        &p("tx.wav"),
        "-o",
        &p("rx.wav"),
        "--channel",
        "good",
        "--skip",
        "2.5",
        "--rate",
        "44100",
        "--ppm",
        "90",
    ]);
    let dec = run(&["decode", &p("rx.wav"), "-o", &p("decoded"), "--every", "8", "--ref", &p("in.png")]);
    assert!(
        dec.contains("44100 Hz") && dec.contains("[complete]") && dec.contains("PSNR"),
        "{dec}"
    );
    let csv = std::fs::read_to_string(dir.join("decoded/timeline.csv")).unwrap();
    assert!(csv.lines().count() >= 4);
    let img = chirpix::png::read(&dir.join("decoded/final.png")).unwrap();
    assert_eq!((img.w, img.h), (200, 150));
    // Errors are reported, not panicked.
    let out = Command::new(bin).args(["decode", &p("in.png")]).output().unwrap();
    assert!(!out.status.success() && String::from_utf8_lossy(&out.stderr).contains("error:"));
    let out = Command::new(bin).args(["encode", &p("missing.png")]).output().unwrap();
    assert!(!out.status.success());
}
