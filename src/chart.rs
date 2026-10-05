//! Static SVG charts for the report: no scripts, tooltips are native
//! `<title>` elements, colours come from CSS variables so the page can
//! follow the reader's light or dark setting.

pub struct Series {
    pub name: String,
    pub points: Vec<(f64, f64)>,
    /// Palette slot 1..=5 (fixed per entity across the whole report).
    pub slot: usize,
    pub dashed: bool,
    pub markers: bool,
}

impl Series {
    pub fn new(name: &str, slot: usize, points: Vec<(f64, f64)>) -> Series {
        Series { name: name.into(), points, slot, dashed: false, markers: true }
    }
    pub fn dashed(mut self) -> Series {
        self.dashed = true;
        self.markers = false;
        self
    }
}

pub struct Chart {
    pub title: String,
    pub x_label: String,
    pub y_label: String,
    pub series: Vec<Series>,
    pub log_y: bool,
    pub y_range: Option<(f64, f64)>,
    pub x_ticks: Option<Vec<f64>>,
    pub width: f64,
    pub height: f64,
    /// Decimal places in tooltips.
    pub decimals: usize,
}

impl Chart {
    pub fn new(title: &str, x_label: &str, y_label: &str) -> Chart {
        Chart {
            title: title.into(),
            x_label: x_label.into(),
            y_label: y_label.into(),
            series: Vec::new(),
            log_y: false,
            y_range: None,
            x_ticks: None,
            width: 520.0,
            height: 300.0,
            decimals: 2,
        }
    }
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn nice_ticks(lo: f64, hi: f64, target: usize) -> Vec<f64> {
    let span = (hi - lo).max(1e-12);
    let raw = span / target as f64;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * mag).find(|s| span / s <= target as f64 + 0.5).unwrap_or(10.0 * mag);
    let mut t = (lo / step).ceil() * step;
    let mut out = Vec::new();
    while t <= hi + step * 1e-6 {
        out.push(if t.abs() < step * 1e-9 { 0.0 } else { t });
        t += step;
    }
    out
}

fn fmt_num(v: f64) -> String {
    if v == 0.0 {
        "0".into()
    } else if v.abs() >= 100.0 || (v.fract().abs() < 1e-9 && v.abs() >= 1.0) {
        format!("{v:.0}")
    } else if v.abs() >= 0.1 {
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        format!("{v:.0e}")
    }
}

fn marker(slot: usize, x: f64, y: f64) -> String {
    // A different shape per slot, so identity never rests on colour alone.
    let c = format!("class=\"m s{slot}\"");
    match slot {
        2 => format!("<rect {c} x=\"{:.1}\" y=\"{:.1}\" width=\"8\" height=\"8\"/>", x - 4.0, y - 4.0),
        3 => format!("<path {c} d=\"M{:.1} {:.1} l5 9 h-10 z\"/>", x, y - 5.5),
        4 => format!("<path {c} d=\"M{:.1} {:.1} l5 5 l-5 5 l-5 -5 z\"/>", x, y - 5.0),
        5 => format!("<path {c} d=\"M{:.1} {:.1} l5 -9 h-10 z\"/>", x, y + 5.5),
        _ => format!("<circle {c} cx=\"{x:.1}\" cy=\"{y:.1}\" r=\"4\"/>"),
    }
}

pub fn legend(series: &[Series]) -> String {
    if series.len() < 2 {
        return String::new();
    }
    let mut s = String::from("<div class=\"legend\">");
    for se in series {
        let line = if se.dashed {
            format!("<line class=\"l s{}\" x1=\"0\" y1=\"7\" x2=\"26\" y2=\"7\" stroke-dasharray=\"5 4\"/>", se.slot)
        } else {
            format!("<line class=\"l s{}\" x1=\"0\" y1=\"7\" x2=\"26\" y2=\"7\"/>{}", se.slot, if se.markers { marker(se.slot, 13.0, 7.0) } else { String::new() })
        };
        s += &format!("<span><svg width=\"26\" height=\"14\" aria-hidden=\"true\">{line}</svg>{}</span>", esc(&se.name));
    }
    s + "</div>"
}

pub fn render(ch: &Chart) -> String {
    let (w, h) = (ch.width, ch.height);
    let (ml, mr, mt, mb) = (56.0, 14.0, 10.0, 42.0);
    let (pw, ph) = (w - ml - mr, h - mt - mb);
    let all: Vec<(f64, f64)> = ch.series.iter().flat_map(|s| s.points.iter().copied()).filter(|p| p.0.is_finite() && p.1.is_finite()).collect();
    let (mut x0, mut x1) = all.iter().fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.0), a.1.max(p.0)));
    if all.is_empty() || x0 == x1 {
        x0 -= 1.0;
        x1 += 1.0;
    }
    let ty = |v: f64| if ch.log_y { v.max(1e-300).log10() } else { v };
    let (y0, y1) = match ch.y_range {
        Some((a, b)) => (ty(a), ty(b)),
        None => {
            let (lo, hi) = all.iter().filter(|p| !ch.log_y || p.1 > 0.0).fold((f64::MAX, f64::MIN), |a, p| (a.0.min(ty(p.1)), a.1.max(ty(p.1))));
            if lo > hi {
                (0.0, 1.0)
            } else if ch.log_y {
                (lo.floor(), hi.ceil().max(lo.floor() + 1.0))
            } else {
                let pad = ((hi - lo) * 0.08).max(1e-9);
                (lo - pad, hi + pad)
            }
        }
    };
    let sx = |x: f64| ml + (x - x0) / (x1 - x0) * pw;
    let sy = |y: f64| mt + ph - ((ty(y) - y0) / (y1 - y0)).clamp(-0.02, 1.02) * ph;
    let mut s = format!(
        "<figure class=\"chart\"><figcaption>{}</figcaption>{}<svg viewBox=\"0 0 {w} {h}\" role=\"img\" aria-label=\"{}\">",
        esc(&ch.title),
        legend(&ch.series),
        esc(&ch.title)
    );
    // grid and y ticks
    let yticks: Vec<f64> = if ch.log_y { (y0.ceil() as i32..=y1.floor() as i32).map(|e| e as f64).collect() } else { nice_ticks(y0, y1, 5) };
    for t in &yticks {
        let y = mt + ph - (t - y0) / (y1 - y0) * ph;
        let label = if ch.log_y { format!("1e{}", *t as i32) } else { fmt_num(*t) };
        s += &format!("<line class=\"grid\" x1=\"{ml}\" x2=\"{:.1}\" y1=\"{y:.1}\" y2=\"{y:.1}\"/><text class=\"tick\" x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\">{label}</text>", ml + pw, ml - 6.0, y + 4.0);
    }
    let xticks = ch.x_ticks.clone().unwrap_or_else(|| nice_ticks(x0, x1, 7));
    for t in &xticks {
        let x = sx(*t);
        s += &format!("<line class=\"axis\" x1=\"{x:.1}\" x2=\"{x:.1}\" y1=\"{:.1}\" y2=\"{:.1}\"/><text class=\"tick\" x=\"{x:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text>", mt + ph, mt + ph + 4.0, mt + ph + 17.0, fmt_num(*t));
    }
    s += &format!("<line class=\"axis\" x1=\"{ml}\" x2=\"{:.1}\" y1=\"{:.1}\" y2=\"{:.1}\"/>", ml + pw, mt + ph, mt + ph);
    s += &format!("<text class=\"label\" x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text>", ml + pw / 2.0, h - 6.0, esc(&ch.x_label));
    s += &format!("<text class=\"label\" transform=\"translate(13 {:.1}) rotate(-90)\" text-anchor=\"middle\">{}</text>", mt + ph / 2.0, esc(&ch.y_label));
    for se in &ch.series {
        let pts: Vec<(f64, f64)> = se.points.iter().copied().filter(|p| p.0.is_finite() && p.1.is_finite() && (!ch.log_y || p.1 > 0.0)).collect();
        if pts.is_empty() {
            continue;
        }
        let path: Vec<String> = pts.iter().enumerate().map(|(i, p)| format!("{}{:.1} {:.1}", if i == 0 { "M" } else { "L" }, sx(p.0), sy(p.1))).collect();
        s += &format!("<path class=\"l s{}\" d=\"{}\"{}/>", se.slot, path.join(" "), if se.dashed { " stroke-dasharray=\"5 4\"" } else { "" });
        if se.markers {
            for p in &pts {
                let tip = if ch.log_y { format!("{:.2e}", p.1) } else { format!("{:.*}", ch.decimals, p.1) };
                s += &format!("<g>{}<title>{}: {} at {}</title></g>", marker(se.slot, sx(p.0), sy(p.1)), esc(&se.name), tip, fmt_num(p.0));
            }
        }
    }
    s + "</svg></figure>"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_are_round_numbers_inside_the_range() {
        assert_eq!(nice_ticks(0.0, 75.0, 7), vec![0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0]);
        let t = nice_ticks(-200.0, 200.0, 5);
        assert!(t.contains(&0.0) && t.iter().all(|v| (-200.0..=200.0).contains(v)));
    }

    #[test]
    fn chart_markup_is_well_formed_and_escaped() {
        let mut ch = Chart::new("A <b> & c", "x", "y");
        ch.series.push(Series::new("one", 1, vec![(0.0, 1.0), (1.0, 2.0)]));
        ch.series.push(Series::new("two", 2, vec![(0.0, f64::NAN), (1.0, 3.0)]).dashed());
        ch.log_y = true;
        let svg = render(&ch);
        assert!(svg.contains("A &lt;b&gt; &amp; c") && !svg.contains("NaN"));
        assert_eq!(svg.matches("<svg").count(), svg.matches("</svg>").count());
        assert_eq!(svg.matches("<g>").count(), svg.matches("</g>").count());
        assert!(svg.contains("class=\"legend\""));
    }
}
