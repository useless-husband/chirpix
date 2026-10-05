use chirpix::channel::Channel;
use chirpix::fountain::Scheme;
use chirpix::image::{psnr, ssim, synthetic, Image};
use chirpix::link::{frames_for, receive_recording, Transmission, TxConfig};
use chirpix::modem::{modem, Constellation, RxOptions, FS};
use chirpix::report::{generate, recommend, ReportConfig, QAM16_MIN_SNR_DB};
use chirpix::{png, wav};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const USAGE: &str = "chirpix: send a picture as sound

USAGE
  chirpix encode <image.png> [-o tx.wav] [--mode robust|fast] [--seconds 90]
                 [--design 75] [--scheme windowed|flat|carousel]
      Make the audio to play. robust = QPSK, fast = 16-QAM (twice the rate,
      needs a cleaner channel). --seconds is how much audio to write;
      --design is the listening time the picture is sized for.

  chirpix decode <recording.wav> [-o out_dir] [--every 5] [--ref image.png]
      Recover the picture from a recording (any sample rate, any start
      point). Writes the picture as it stood every --every seconds, the
      final picture, and timeline.csv. With --ref, reports PSNR and SSIM.

  chirpix simulate <tx.wav> [-o rx.wav] [--channel ideal|good|fair|poor]
                 [--snr dB] [--rt60 s] [--drr dB] [--ppm n] [--lowpass Hz]
                 [--highpass Hz] [--clip xRMS] [--clicks per_s]
                 [--dropouts per_min] [--skip s] [--rate Hz] [--seed n]
      Pass audio through the simulated loudspeaker-room-microphone channel.

  chirpix report [-o out/report] [--quick] [--threads 4] [image.png ...]
      Run the measurements and write report.html (self-contained).

  chirpix testimage <scene|chart|clouds> [-o image.png] [--size 384x256]
      Write one of the built-in procedural test pictures.

  chirpix info
      Print the modem parameters.
";

struct Args {
    pos: Vec<String>,
    opt: HashMap<String, String>,
}

fn parse_args(raw: &[String], flags: &[&str]) -> Result<Args, String> {
    let mut a = Args { pos: Vec::new(), opt: HashMap::new() };
    let mut i = 0;
    while i < raw.len() {
        let s = &raw[i];
        if s == "-o" || s.starts_with("--") {
            let key = if s == "-o" { "out".to_string() } else { s[2..].to_string() };
            if flags.contains(&key.as_str()) {
                a.opt.insert(key, "1".into());
            } else {
                i += 1;
                let v = raw.get(i).ok_or_else(|| format!("option {s} needs a value"))?;
                a.opt.insert(key, v.clone());
            }
        } else {
            a.pos.push(s.clone());
        }
        i += 1;
    }
    Ok(a)
}

impl Args {
    fn num(&self, key: &str) -> Result<Option<f64>, String> {
        match self.opt.get(key) {
            None => Ok(None),
            Some(v) => v.parse::<f64>().map(Some).map_err(|_| format!("--{key}: '{v}' is not a number")),
        }
    }
    fn known(&self, allowed: &[&str]) -> Result<(), String> {
        for k in self.opt.keys() {
            if !allowed.contains(&k.as_str()) {
                return Err(format!("unknown option --{k}"));
            }
        }
        Ok(())
    }
}

fn load_image(path: &str) -> Result<Image, String> {
    let img = png::read(Path::new(path))?;
    // Very large pictures are shrunk: at these data rates the extra pixels
    // would never be filled in.
    let f = img.w.max(img.h).div_ceil(1024).max(1);
    if f > 1 {
        eprintln!("note: {path} is {}x{}; shrinking by {f} to {}x{}", img.w, img.h, img.w / f, img.h / f);
    }
    Ok(img.downscale(f))
}

fn cmd_encode(raw: &[String]) -> Result<(), String> {
    let a = parse_args(raw, &[])?;
    a.known(&["out", "mode", "seconds", "design", "scheme"])?;
    let input = a.pos.first().ok_or("encode: which image?")?;
    let img = load_image(input)?;
    let cons = match a.opt.get("mode").map(|s| s.as_str()).unwrap_or("robust") {
        "robust" | "qpsk" => Constellation::Qpsk,
        "fast" | "qam16" | "16qam" => Constellation::Qam16,
        other => return Err(format!("--mode: '{other}' is not robust or fast")),
    };
    let scheme = match a.opt.get("scheme").map(|s| s.as_str()).unwrap_or("windowed") {
        "windowed" => Scheme::Windowed,
        "flat" => Scheme::Flat,
        "carousel" => Scheme::Carousel,
        other => return Err(format!("--scheme: '{other}' is not windowed, flat or carousel")),
    };
    let seconds = a.num("seconds")?.unwrap_or(90.0);
    let design = a.num("design")?.unwrap_or(75.0);
    if !(2.0..=3600.0).contains(&seconds) || !(4.0..=3600.0).contains(&design) {
        return Err("--seconds must be 2..3600 and --design 4..3600".into());
    }
    let cfg = TxConfig { cons, scheme, design_seconds: design, k: None };
    let tx = Transmission::new(&img, &cfg);
    let mut audio = vec![0f32; FS as usize / 4];
    audio.extend(tx.audio(0, frames_for(seconds)));
    audio.extend(vec![0f32; FS as usize / 4]);
    let out = PathBuf::from(a.opt.get("out").cloned().unwrap_or_else(|| "tx.wav".into()));
    wav::write(&out, &audio, FS).map_err(|e| format!("{}: {e}", out.display()))?;
    let p = modem().p;
    println!("image        {input}  {}x{}", img.w, img.h);
    println!("mode         {} rate 1/2, {:.0} bytes/s of payload", cons.name(), p.byte_rate(cons));
    println!("stream       {} bytes in {} packets ({scheme:?} scheme, sized for {design:.0} s of listening)", tx.stream.len(), tx.k());
    if scheme == Scheme::Windowed {
        let w: Vec<String> = tx.encoder.plan.windows.iter().map(|k| format!("{:.1} kB", (*k * chirpix::fountain::T) as f64 / 1000.0)).collect();
        println!("layers       {} (each decodable on its own, coarsest first)", w.join(", "));
    }
    println!("audio        {} : {:.1} s, {} frames of {:.3} s, 48 kHz 16-bit mono", out.display(), audio.len() as f64 / FS as f64, frames_for(seconds), p.frame_seconds());
    Ok(())
}

fn cmd_decode(raw: &[String]) -> Result<(), String> {
    let a = parse_args(raw, &[])?;
    a.known(&["out", "every", "ref"])?;
    let input = a.pos.first().ok_or("decode: which recording?")?;
    let w = wav::read(Path::new(input)).map_err(|e| format!("{input}: {e}"))?;
    if !(8000..=192_000).contains(&w.rate) {
        return Err(format!("{input}: sample rate {} Hz is outside 8000..192000", w.rate));
    }
    let every = a.num("every")?.unwrap_or(5.0);
    if every < 0.5 {
        return Err("--every must be at least 0.5".into());
    }
    let reference = match a.opt.get("ref") {
        Some(p) => Some(load_image(p)?),
        None => None,
    };
    let out = PathBuf::from(a.opt.get("out").cloned().unwrap_or_else(|| "decoded".into()));
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let duration = w.samples.len() as f64 / w.rate as f64;
    println!("recording    {input}: {duration:.1} s at {} Hz, {} channel(s)", w.rate, w.channels);
    if w.rate < 16_000 {
        println!("warning      the signal reaches 7 kHz; a {} Hz recording cannot hold it", w.rate);
    }
    let rec = receive_recording(&w.samples, w.rate, &RxOptions::default());
    let frames = &rec.report.frames;
    let Some(spec) = rec.spec else {
        println!("frames       none found ({} chirp candidates rejected)", rec.report.false_starts);
        return Err("no chirpix signal found in this recording".into());
    };
    let snr = frames.iter().map(|f| f.snr_db).sum::<f64>() / frames.len() as f64;
    let ok: usize = frames.iter().map(|f| f.packet_ok.iter().filter(|&&b| b).count()).sum();
    let total: usize = frames.iter().map(|f| f.packet_ok.len()).sum();
    println!("frames       {} found, first at {:.2} s", frames.len(), frames[0].start as f64 / FS as f64);
    println!("signal       {}, {} source packets, scheme {:?}", spec.cons.name(), spec.k, Scheme::from_u8(spec.scheme).unwrap_or(Scheme::Windowed));
    println!("quality      {snr:.1} dB per carrier, recorder clock {:+.0} ppm, packets ok {ok}/{total}", rec.ppm);
    let advice = recommend(snr);
    println!(
        "advice       {} ({} needs about {QAM16_MIN_SNR_DB:.0} dB)",
        if advice == Constellation::Qam16 { "this channel can carry --mode fast" } else { "stay with --mode robust" },
        Constellation::Qam16.name()
    );
    let mut times: Vec<f64> = (1..).map(|i| i as f64 * every).take_while(|t| *t < duration).collect();
    times.push(duration);
    let snaps = rec.snapshots(&times, false);
    let mut csv = String::from("seconds,packets,rank,stream_bytes,complete,psnr_db,ssim\n");
    println!("\n  seconds  packets  stream bytes  picture");
    let mut last_bytes = usize::MAX;
    for s in &snaps {
        let mut line = format!("  {:7.1}  {:7}  {:12}  ", s.time, s.packets, s.prefix_bytes);
        let mut q = (String::new(), String::new());
        match &s.image {
            Some(img) => {
                if s.prefix_bytes != last_bytes {
                    let name = out.join(format!("t{:04.0}.png", s.time));
                    png::write(&name, img).map_err(|e| format!("{}: {e}", name.display()))?;
                    line += &format!("{}", name.display());
                } else {
                    line += "(unchanged)";
                }
                if let Some(r) = &reference {
                    if (r.w, r.h) == (img.w, img.h) {
                        q = (format!("{:.2}", psnr(r, img)), format!("{:.4}", ssim(r, img)));
                        line += &format!("  PSNR {} dB  SSIM {}", q.0, q.1);
                    }
                }
            }
            None => line += "nothing yet",
        }
        if s.complete && s.prefix_bytes != last_bytes {
            line += "  [complete]";
        }
        last_bytes = s.prefix_bytes;
        println!("{line}");
        csv += &format!("{:.2},{},{},{},{},{},{}\n", s.time, s.packets, s.rank, s.prefix_bytes, s.complete as u8, q.0, q.1);
    }
    std::fs::write(out.join("timeline.csv"), csv).map_err(|e| e.to_string())?;
    match snaps.last().and_then(|s| s.image.as_ref()) {
        Some(img) => {
            let name = out.join("final.png");
            png::write(&name, img).map_err(|e| e.to_string())?;
            println!("\nfinal picture {}  ({}x{})", name.display(), img.w, img.h);
            Ok(())
        }
        None => Err("frames were found but not enough packets survived for even the coarsest picture".into()),
    }
}

fn cmd_simulate(raw: &[String]) -> Result<(), String> {
    let a = parse_args(raw, &[])?;
    a.known(&["out", "channel", "snr", "rt60", "drr", "ppm", "lowpass", "highpass", "clip", "clicks", "dropouts", "skip", "rate", "seed"])?;
    let input = a.pos.first().ok_or("simulate: which WAV?")?;
    let w = wav::read(Path::new(input)).map_err(|e| format!("{input}: {e}"))?;
    if w.rate != FS {
        return Err(format!("{input}: expected the 48 kHz file written by encode, got {} Hz", w.rate));
    }
    let mut ch = match a.opt.get("channel").map(|s| s.as_str()).unwrap_or("ideal") {
        "ideal" => Channel::clean(),
        "good" => Channel::good(),
        "fair" => Channel::fair(),
        "poor" => Channel::poor(),
        other => return Err(format!("--channel: '{other}' is not ideal, good, fair or poor")),
    };
    if let Some(v) = a.num("snr")? {
        ch.snr_db = Some(v);
    }
    if let Some(v) = a.num("rt60")? {
        ch.rt60 = v.clamp(0.0, 3.0);
    }
    if let Some(v) = a.num("drr")? {
        ch.drr_db = v;
    }
    if let Some(v) = a.num("ppm")? {
        ch.ppm = v.clamp(-5000.0, 5000.0);
    }
    let (lo, hi) = (a.num("highpass")?, a.num("lowpass")?);
    if lo.is_some() || hi.is_some() {
        let (blo, bhi) = ch.band.unwrap_or((50.0, 20_000.0));
        ch.band = Some((lo.unwrap_or(blo).clamp(1.0, 20_000.0), hi.unwrap_or(bhi).clamp(100.0, 23_000.0)));
    }
    if let Some(v) = a.num("clip")? {
        ch.clip = Some(v.max(0.01));
    }
    if let Some(v) = a.num("clicks")? {
        ch.impulses_per_s = v.max(0.0);
    }
    if let Some(v) = a.num("dropouts")? {
        ch.dropouts_per_min = v.max(0.0);
    }
    if let Some(v) = a.num("seed")? {
        ch.seed = v as u64;
    }
    let mut y = ch.apply(&w.samples);
    let skip = (a.num("skip")?.unwrap_or(0.0).max(0.0) * FS as f64) as usize;
    y.drain(..skip.min(y.len()));
    let rate = a.num("rate")?.unwrap_or(FS as f64) as u32;
    if !(8000..=192_000).contains(&rate) {
        return Err("--rate must be 8000..192000".into());
    }
    if rate != FS {
        y = chirpix::dsp::resample(&y, FS as f64 / rate as f64);
    }
    // Leave headroom the way a recorder's gain setting would.
    let peak = y.iter().fold(0f32, |m, v| m.max(v.abs()));
    if peak > 0.98 {
        for v in y.iter_mut() {
            *v *= 0.98 / peak;
        }
    }
    let out = PathBuf::from(a.opt.get("out").cloned().unwrap_or_else(|| "rx.wav".into()));
    wav::write(&out, &y, rate).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("channel      {}", ch.describe());
    println!("recording    {} : {:.1} s at {rate} Hz (started {:.1} s into the transmission)", out.display(), y.len() as f64 / rate as f64, skip as f64 / FS as f64);
    Ok(())
}

fn cmd_report(raw: &[String]) -> Result<(), String> {
    let a = parse_args(raw, &["quick"])?;
    a.known(&["out", "quick", "threads"])?;
    let quick = a.opt.contains_key("quick");
    let threads = a.num("threads")?.unwrap_or(4.0).clamp(1.0, 4.0) as usize;
    let mut images = Vec::new();
    for p in &a.pos {
        let name = Path::new(p).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| p.clone());
        images.push((name, load_image(p)?));
    }
    let image_note = if images.is_empty() {
        let (w, h) = if quick { (192, 128) } else { (384, 256) };
        for n in ["scene", "chart", "clouds"] {
            images.push((n.to_string(), synthetic(n, w, h)));
        }
        format!("Test pictures: three procedural images generated by the program ({w}x{h}).")
    } else {
        format!("Test pictures: {}.", images.iter().map(|(n, i)| format!("{n} ({}x{})", i.w, i.h)).collect::<Vec<_>>().join(", "))
    };
    let out = PathBuf::from(a.opt.get("out").cloned().unwrap_or_else(|| "out/report".into()));
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let r = generate(&ReportConfig { quick, images, image_note, threads });
    std::fs::write(out.join("report.html"), &r.html).map_err(|e| e.to_string())?;
    std::fs::write(out.join("summary.txt"), &r.summary).map_err(|e| e.to_string())?;
    println!("{}", r.summary);
    println!("report       {}  ({:.1} MB)", out.join("report.html").display(), r.html.len() as f64 / 1e6);
    Ok(())
}

fn cmd_testimage(raw: &[String]) -> Result<(), String> {
    let a = parse_args(raw, &[])?;
    a.known(&["out", "size"])?;
    let name = a.pos.first().map(|s| s.as_str()).unwrap_or("scene");
    if !["scene", "chart", "clouds"].contains(&name) {
        return Err(format!("testimage: '{name}' is not scene, chart or clouds"));
    }
    let size = a.opt.get("size").cloned().unwrap_or_else(|| "384x256".into());
    let (w, h) = size.split_once('x').and_then(|(w, h)| Some((w.parse::<usize>().ok()?, h.parse::<usize>().ok()?))).ok_or("--size wants WIDTHxHEIGHT")?;
    if !(8..=2048).contains(&w) || !(8..=2048).contains(&h) {
        return Err("--size: each side must be 8..2048".into());
    }
    let out = PathBuf::from(a.opt.get("out").cloned().unwrap_or_else(|| format!("{name}.png")));
    png::write(&out, &synthetic(name, w, h)).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("wrote {} ({w}x{h})", out.display());
    Ok(())
}

fn cmd_info() {
    let p = modem().p;
    let f = |bin: usize| bin as f64 * p.carrier_spacing_hz();
    println!("sample rate      {FS} Hz");
    println!("band             {:.0}-{:.0} Hz ({} carriers, {:.2} Hz apart)", f(p.first_bin()), f(p.first_bin() + p.ncar()), p.ncar(), p.carrier_spacing_hz());
    println!("symbol           {:.1} ms + {:.1} ms cyclic prefix", p.nfft as f64 * 1e3 / FS as f64, p.cp as f64 * 1e3 / FS as f64);
    println!("frame            {:.3} s ({} samples): chirp, training, header, {} data symbols", p.frame_seconds(), p.frame_len(), p.data_syms());
    for c in [Constellation::Qpsk, Constellation::Qam16] {
        println!("{:<16} {} packets of {} bytes per frame, {:.0} bytes/s", c.name(), c.packets_per_frame(), chirpix::fountain::T, p.byte_rate(c));
    }
    println!("16-QAM threshold {QAM16_MIN_SNR_DB:.0} dB per carrier at the receiver");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = if args.is_empty() { &args[..] } else { &args[1..] };
    let result = match args.first().map(|s| s.as_str()) {
        Some("encode") => cmd_encode(rest),
        Some("decode") => cmd_decode(rest),
        Some("simulate") => cmd_simulate(rest),
        Some("report") => cmd_report(rest),
        Some("testimage") => cmd_testimage(rest),
        Some("info") => {
            cmd_info();
            Ok(())
        }
        Some("help") | Some("--help") | Some("-h") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command '{other}'\n\n{USAGE}")),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
