// Standalone HTML report for a scenario run, ported from the app's TypeScript
// report so the CLI produces the same artifact with no JS runtime.
use crate::types::{LoadTestResult, ScenarioResult, TimelinePoint};

const ENDPOINT_COLORS: [&str; 10] = [
    "#4c9df3", "#3fb950", "#d29922", "#a371f7", "#f0649a", "#39c5cf", "#ff9d5c", "#f85149",
    "#7ee0ff", "#b0bec5",
];

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let a = v.abs();
    if a >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if a >= 10_000.0 {
        format!("{:.1}k", v / 1000.0)
    } else if a >= 100.0 {
        format!("{}", v.round() as i64)
    } else if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else if a >= 10.0 {
        format!("{:.1}", v)
    } else {
        format!("{:.2}", v)
    }
}

fn fmt_ms(v: f64) -> String {
    if v >= 1000.0 {
        format!("{:.2} с", v / 1000.0)
    } else {
        format!("{} мс", fmt_num(v))
    }
}

struct Series {
    name: String,
    color: &'static str,
    points: Vec<(f64, f64)>,
    fill: bool,
}

/// Round the axis top up to a nice step that always contains the peak.
fn nice_ticks(max: f64) -> Vec<f64> {
    let max = if max > 0.0 { max } else { 1.0 };
    let rough = max / 5.0;
    let mag = 10f64.powf(rough.log10().floor());
    let norm = rough / mag;
    let step = if norm >= 5.0 {
        10.0
    } else if norm >= 2.0 {
        5.0
    } else if norm >= 1.0 {
        2.0
    } else {
        1.0
    } * mag;
    let nice_max = (max / step - 1e-9).ceil() * step;
    let mut ticks = Vec::new();
    let mut v = 0.0;
    while v <= nice_max + step * 0.001 {
        ticks.push(v);
        v += step;
    }
    ticks
}

fn line_chart(series: &[Series], y_is_ms: bool) -> String {
    let w = 900.0;
    let h = 260.0;
    let pad_l = 56.0;
    let pad_r = 16.0;
    let pad_t = 30.0;
    let pad_b = 34.0;
    let iw = w - pad_l - pad_r;
    let ih = h - pad_t - pad_b;

    let all: Vec<(f64, f64)> = series.iter().flat_map(|s| s.points.iter().copied()).collect();
    if all.is_empty() {
        return String::new();
    }
    let x_min = all.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let x_max = all.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max);
    let y_max_raw = all.iter().map(|p| p.1).fold(0.0_f64, f64::max);
    let ticks = nice_ticks(y_max_raw * 1.08);
    let y_max = *ticks.last().unwrap_or(&1.0);
    let x_span = if x_max - x_min != 0.0 { x_max - x_min } else { 1.0 };
    let y_max = if y_max > 0.0 { y_max } else { 1.0 };

    let sx = |x: f64| pad_l + (x - x_min) / x_span * iw;
    let sy = |y: f64| pad_t + ih - (y / y_max) * ih;
    let fmt_y = |v: f64| if y_is_ms { fmt_ms(v) } else { fmt_num(v) };

    let mut out = format!(
        "<svg viewBox=\"0 0 {w} {h}\" width=\"100%\" xmlns=\"http://www.w3.org/2000/svg\" font-family=\"Segoe UI, Arial, sans-serif\">"
    );
    for t in &ticks {
        let y = sy(*t);
        out.push_str(&format!(
            "<line x1=\"{pad_l}\" y1=\"{y:.1}\" x2=\"{:.1}\" y2=\"{y:.1}\" stroke=\"rgba(255,255,255,0.08)\"/>",
            w - pad_r
        ));
        out.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"end\" font-size=\"11\" fill=\"#9aa0ab\">{}</text>",
            pad_l - 8.0,
            y + 4.0,
            esc(&fmt_y(*t))
        ));
    }
    // x labels
    let x_ticks = 6;
    for i in 0..=x_ticks {
        let xv = x_min + x_span * i as f64 / x_ticks as f64;
        out.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\" font-size=\"11\" fill=\"#9aa0ab\">{}с</text>",
            sx(xv),
            h - 10.0,
            xv.round() as i64
        ));
    }
    out.push_str(&format!(
        "<line x1=\"{pad_l}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"rgba(255,255,255,0.18)\"/>",
        pad_t + ih,
        w - pad_r,
        pad_t + ih
    ));

    for (si, s) in series.iter().enumerate() {
        if s.points.is_empty() {
            continue;
        }
        let mut pts: Vec<(f64, f64)> = s.points.clone();
        pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        let poly: String = pts
            .iter()
            .map(|p| format!("{:.1},{:.1}", sx(p.0), sy(p.1)))
            .collect::<Vec<_>>()
            .join(" ");
        if s.fill {
            let gid = format!("g{si}");
            out.push_str(&format!(
                "<defs><linearGradient id=\"{gid}\" x1=\"0\" y1=\"0\" x2=\"0\" y2=\"1\"><stop offset=\"0%\" stop-color=\"{c}\" stop-opacity=\"0.35\"/><stop offset=\"100%\" stop-color=\"{c}\" stop-opacity=\"0.02\"/></linearGradient></defs>",
                c = s.color
            ));
            out.push_str(&format!(
                "<polygon points=\"{:.1},{:.1} {poly} {:.1},{:.1}\" fill=\"url(#{gid})\"/>",
                sx(pts.first().unwrap().0),
                pad_t + ih,
                sx(pts.last().unwrap().0),
                pad_t + ih
            ));
        }
        out.push_str(&format!(
            "<polyline points=\"{poly}\" fill=\"none\" stroke=\"{}\" stroke-width=\"2\" stroke-linejoin=\"round\"/>",
            s.color
        ));
    }

    // legend
    let mut lx = pad_l;
    for s in series {
        out.push_str(&format!(
            "<circle cx=\"{:.1}\" cy=\"14\" r=\"5\" fill=\"{}\"/><text x=\"{:.1}\" y=\"18\" font-size=\"12\" fill=\"#9aa0ab\">{}</text>",
            lx + 5.0,
            s.color,
            lx + 15.0,
            esc(&s.name)
        ));
        lx += 15.0 + s.name.chars().count() as f64 * 7.0 + 22.0;
    }
    out.push_str("</svg>");
    out
}

const REPORT_CSS: &str = r#"
  :root { color-scheme: dark; }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { background: #141519; color: #e6e8ec; font-family: "Segoe UI", -apple-system, Arial, sans-serif; line-height: 1.5; }
  .wrap { max-width: 1080px; margin: 0 auto; padding: 32px 24px 64px; }
  header { padding: 28px 28px 24px; border-radius: 16px; background: linear-gradient(135deg, #1d1e25 0%, #23242c 60%, #2a2320 100%); border: 1px solid rgba(255,255,255,0.07); margin-bottom: 24px; }
  header h1 { font-size: 22px; font-weight: 650; margin-bottom: 6px; }
  header h1 .accent { color: #ff6c37; }
  .chips { display: flex; gap: 8px; margin-top: 14px; flex-wrap: wrap; }
  .chip { font-size: 12px; color: #9aa0ab; background: rgba(255,255,255,0.05); border: 1px solid rgba(255,255,255,0.08); padding: 4px 12px; border-radius: 999px; }
  .chip b { color: #e6e8ec; font-weight: 600; }
  .verdict { display: flex; align-items: center; gap: 12px; padding: 14px 18px; border-radius: 12px; margin-bottom: 24px; font-weight: 600; }
  .verdict .note { font-weight: 400; color: rgba(230,232,236,0.75); font-size: 13px; }
  .verdict.ok { background: rgba(63,185,80,0.12); border: 1px solid rgba(63,185,80,0.4); color: #56d364; }
  .verdict.warn { background: rgba(210,153,34,0.12); border: 1px solid rgba(210,153,34,0.4); color: #e3b341; }
  .verdict.bad { background: rgba(248,81,73,0.12); border: 1px solid rgba(248,81,73,0.4); color: #ff7b72; }
  .cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 12px; margin-bottom: 24px; }
  .card { background: #1d1e25; border: 1px solid rgba(255,255,255,0.07); border-radius: 12px; padding: 16px 18px; }
  .card .label { font-size: 12px; color: #9aa0ab; margin-bottom: 6px; }
  .card .value { font-size: 22px; font-weight: 650; font-variant-numeric: tabular-nums; }
  .card.ok .value { color: #56d364; }
  .card.bad .value { color: #ff7b72; }
  section { background: #1d1e25; border: 1px solid rgba(255,255,255,0.07); border-radius: 14px; padding: 20px 22px; margin-bottom: 20px; }
  section h2 { font-size: 15px; font-weight: 650; margin-bottom: 14px; color: #cdd2da; }
  table { width: 100%; border-collapse: collapse; font-size: 13px; font-variant-numeric: tabular-nums; }
  th { text-align: left; color: #9aa0ab; font-weight: 600; padding: 8px 10px; border-bottom: 1px solid rgba(255,255,255,0.1); }
  td { padding: 7px 10px; border-bottom: 1px solid rgba(255,255,255,0.05); }
  .table-scroll { overflow-x: auto; }
  .m-tag { font-weight: 700; font-size: 11px; }
  td.err { color: #ff7b72; }
  footer { text-align: center; color: #6b7280; font-size: 12px; margin-top: 32px; }
"#;

fn short_url(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(u) => {
            let mut p = u.path().to_string();
            if let Some(q) = u.query() {
                p.push('?');
                p.push_str(q);
            }
            p
        }
        Err(_) => url.to_string(),
    }
}

pub fn build_scenario_report(s: &ScenarioResult) -> String {
    let o = &s.overall;
    let (mut vcls, mut vtext, mut vnote) = if o.error_rate < 1.0 {
        ("ok", "Нагрузка пройдена успешно", "Доля ошибок ниже 1%".to_string())
    } else if o.error_rate < 5.0 {
        ("warn", "Пройдена с предупреждениями", format!("Доля ошибок {:.2}%", o.error_rate))
    } else {
        ("bad", "Обнаружены проблемы", format!("Доля ошибок {:.2}%", o.error_rate))
    };
    // A large scheduler shortfall means the achieved RPS is below target — flag
    // it even when the error rate is low, so a "green" report doesn't hide it.
    if o.dropped > 0 && vcls == "ok" {
        vcls = "warn";
        vtext = "Пройдена, но целевой RPS не достигнут";
        vnote = format!("Недодано {} запросов (ручка(и) не успевают)", fmt_num(o.dropped as f64));
    }
    let vicon = if vcls == "ok" { "✔" } else if vcls == "warn" { "⚠" } else { "✖" };

    let rps_series: Vec<Series> = s
        .targets
        .iter()
        .enumerate()
        .map(|(i, t)| Series {
            name: short_url(&t.url),
            color: ENDPOINT_COLORS[i % ENDPOINT_COLORS.len()],
            points: t.timeline.iter().map(|p| (p.sec as f64, p.requests as f64)).collect(),
            fill: false,
        })
        .collect();

    let overall_series = vec![
        Series {
            name: "Запросов/с (сумма)".to_string(),
            color: "#4c9df3",
            points: timeline_points(&o.timeline, |p| p.requests as f64),
            fill: true,
        },
        Series {
            name: "Ошибок/с".to_string(),
            color: "#f85149",
            points: timeline_points(&o.timeline, |p| p.errors as f64),
            fill: false,
        },
    ];

    let cards = [
        ("Ручек".to_string(), s.targets.len().to_string(), ""),
        ("Запросов всего".to_string(), fmt_num(o.total_requests as f64), ""),
        ("RPS (средний, сумма)".to_string(), fmt_num(o.rps_avg), ""),
        (
            "Ошибки".to_string(),
            format!("{} · {:.2}%", fmt_num(o.errors as f64), o.error_rate),
            if o.errors > 0 { "bad" } else { "ok" },
        ),
        ("p95 (общий)".to_string(), fmt_ms(o.p95_ms), ""),
        ("p99 (общий)".to_string(), fmt_ms(o.p99_ms), ""),
    ];
    let mut cards_html: String = cards
        .iter()
        .map(|(l, v, c)| {
            format!(
                "<div class=\"card {c}\"><div class=\"label\">{}</div><div class=\"value\">{}</div></div>",
                esc(l),
                esc(v)
            )
        })
        .collect();
    if o.dropped > 0 {
        cards_html.push_str(&format!(
            "<div class=\"card bad\"><div class=\"label\">Недодано (RPS не достигнут)</div><div class=\"value\">{}</div></div>",
            esc(&fmt_num(o.dropped as f64))
        ));
    }

    let rows: String = s
        .targets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let color = ENDPOINT_COLORS[i % ENDPOINT_COLORS.len()];
            format!(
                "<tr><td><span style=\"display:inline-block;width:9px;height:9px;border-radius:2px;background:{color};margin-right:8px\"></span><span class=\"m-tag\">{}</span> <span style=\"font-family:Consolas,monospace\">{}</span></td><td>{}</td><td>{}</td><td class=\"{}\">{} · {:.2}%</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(&t.method),
                esc(&short_url(&t.url)),
                fmt_num(t.total_requests as f64),
                fmt_num(t.rps_avg),
                if t.errors > 0 { "err" } else { "" },
                fmt_num(t.errors as f64),
                t.error_rate,
                fmt_ms(t.p50_ms),
                fmt_ms(t.p95_ms),
                fmt_ms(t.p99_ms),
            )
        })
        .collect();

    format!(
        r#"<!doctype html>
<html lang="ru">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Maelstrom — отчёт о нагрузке сервиса ({} ручек)</title>
<style>{css}</style>
</head>
<body>
<div class="wrap">
  <header>
    <h1><span class="accent">⚡ Maelstrom</span> — отчёт о нагрузке сервиса</h1>
    <div class="chips">
      <span class="chip">Начало: <b>{started}</b></span>
      <span class="chip">Длительность: <b>{dur:.1} с</b>{stopped}</span>
      <span class="chip">Ручек: <b>{n}</b></span>
    </div>
  </header>
  <div class="verdict {vcls}">{vicon} {vtext} <span class="note">{vnote}</span></div>
  <div class="cards">{cards}</div>
  <section>
    <h2>Результаты по ручкам</h2>
    <div class="table-scroll"><table>
      <thead><tr><th>Ручка</th><th>Запросов</th><th>RPS</th><th>Ошибки</th><th>p50</th><th>p95</th><th>p99</th></tr></thead>
      <tbody>{rows}</tbody>
    </table></div>
  </section>
  <section><h2>Пропускная способность по ручкам (запросов/с)</h2>{rps_chart}</section>
  <section><h2>Суммарная пропускная способность и ошибки</h2>{overall_chart}</section>
  <footer>Сгенерировано Maelstrom CLI</footer>
</div>
</body>
</html>"#,
        s.targets.len(),
        css = REPORT_CSS,
        started = esc(&s.started_at),
        dur = s.actual_duration_ms / 1000.0,
        stopped = if s.stopped_early { " (остановлено вручную)" } else { "" },
        n = s.targets.len(),
        vcls = vcls,
        vicon = vicon,
        vtext = vtext,
        vnote = esc(&vnote),
        cards = cards_html,
        rows = rows,
        rps_chart = line_chart(&rps_series, false),
        overall_chart = line_chart(&overall_series, false),
    )
}

fn timeline_points(tl: &[TimelinePoint], f: impl Fn(&TimelinePoint) -> f64) -> Vec<(f64, f64)> {
    tl.iter().map(|p| (p.sec as f64, f(p))).collect()
}

#[allow(dead_code)]
fn _unused(_: &LoadTestResult) {}
