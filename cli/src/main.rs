//! Maelstrom headless load-test runner.
//!
//! Runs a multi-endpoint scenario exported from the desktop app (or hand-written
//! JSON) from a CI pipeline or a Kubernetes Job — i.e. from inside the network
//! that is allowed to reach prod, not a laptop. Emits a JSON + HTML report and
//! exits non-zero when a threshold is breached, so pipelines gate on it.

use maelstrom_core::report::build_scenario_report;
use maelstrom_core::scenario::run_scenario;
use maelstrom_core::streams::run_streams;
use maelstrom_core::types::{ScenarioSpec, StreamScenarioSpec, StreamSpec};
use clap::Parser;
use serde::Deserialize;
use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "maelstrom",
    about = "Headless нагрузочный раннер Maelstrom (CI / Kubernetes)",
    version
)]
struct Args {
    /// Путь к JSON-конфигу сценария (экспорт из приложения или ручной).
    config: String,

    /// Куда записать JSON-отчёт.
    #[arg(long, default_value = "maelstrom-report.json")]
    out_json: String,

    /// Куда записать HTML-отчёт (по умолчанию не пишется).
    #[arg(long)]
    out_html: Option<String>,

    /// Переопределить длительность из конфига (секунды).
    #[arg(long)]
    duration: Option<u64>,

    /// Гейт: максимально допустимая доля ошибок в процентах (иначе exit 1).
    #[arg(long)]
    max_error_rate: Option<f64>,

    /// Гейт: максимально допустимый общий p95 в миллисекундах (иначе exit 1).
    #[arg(long)]
    max_p95: Option<f64>,

    /// Гейт (потоки/цепочки): минимальная доля завершённых цепочек в процентах —
    /// проверяется для КАЖДОГО потока (иначе exit 1).
    #[arg(long)]
    min_success_rate: Option<f64>,

    /// Не печатать посекундный прогресс.
    #[arg(long)]
    quiet: bool,

    /// Дублировать все логи (фазы, токены, ошибки) в файл — секреты маскируются.
    #[arg(long)]
    log_file: Option<String>,
}

/// Timestamped log line to stderr and, if configured, appended to `--log-file`.
/// Messages are already secret-redacted by the engine / callers.
fn log_line(log_file: &Option<String>, category: &str, msg: &str) {
    let ts = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let line = format!("{ts} [{category}] {msg}");
    eprintln!("{line}");
    if let Some(p) = log_file {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{line}");
        }
    }
}

#[derive(Deserialize)]
struct CliConfig {
    #[serde(default)]
    name: Option<String>,
    duration_secs: u64,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    /// Independent HTTP targets (classic parallel scenario). Optional so a
    /// streams-only config doesn't have to carry an empty array.
    #[serde(default)]
    targets: Vec<maelstrom_core::types::ScenarioTarget>,
    #[serde(default)]
    datasets: Vec<maelstrom_core::types::DatasetSpec>,
    #[serde(default)]
    file_pools: Vec<maelstrom_core::types::FilePoolSpec>,
    #[serde(default)]
    thresholds: Option<Thresholds>,
    /// When present, run a gRPC load test instead of the HTTP scenario.
    #[serde(default)]
    grpc: Option<GrpcCliConfig>,
    /// When present, run a WebSocket load test instead of the HTTP scenario.
    #[serde(default)]
    websocket: Option<WsCliConfig>,
    /// When present, run request-chaining streams: each stream fires its steps
    /// in order at its own iterations-per-second rate, threading {{vars}}
    /// extracted from responses. Single-step streams are plain load.
    #[serde(default)]
    streams: Vec<StreamSpec>,
}

#[derive(Deserialize)]
struct WsCliConfig {
    url: String,
    #[serde(default)]
    message: String,
    #[serde(default = "default_vus")]
    vus: usize,
    #[serde(default)]
    rps_limit: Option<u32>,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

#[derive(Deserialize)]
struct GrpcCliConfig {
    endpoint: String,
    proto_path: String,
    #[serde(default)]
    includes: Vec<String>,
    service: String,
    method: String,
    #[serde(default)]
    body: String,
    /// Custom CA / mTLS client identity for `https://` endpoints — see the
    /// HTTP path's `ScenarioTarget::tls` (maelstrom_core::types::TlsConfig).
    /// Absent = as before: native root CAs only, no client identity.
    #[serde(default)]
    tls: Option<maelstrom_core::types::TlsConfig>,
    #[serde(default = "default_vus")]
    vus: usize,
    #[serde(default)]
    rps_limit: Option<u32>,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

fn default_vus() -> usize {
    10
}

fn default_timeout() -> u64 {
    10_000
}

/// A threshold is breached when the measured value strictly exceeds the limit.
/// `None` means the threshold isn't set, so it can never fail the run.
/// A NaN value (e.g. 0/0 success-rate when zero iterations completed) can't be
/// meaningfully compared with `<`/`>` — both would silently return `false` and
/// let the gate pass. Treat NaN as a breach whenever a limit is actually set.
fn breached(value: f64, max: Option<f64>) -> bool {
    match max {
        Some(m) => value.is_nan() || value > m,
        None => false,
    }
}

/// Floor threshold (e.g. chain success-rate): breached when the value falls
/// strictly BELOW the limit. `None` — not set, never fails. Same NaN handling
/// as `breached`: NaN counts as a breach, not a silent pass.
fn below(value: f64, min: Option<f64>) -> bool {
    match min {
        Some(m) => value.is_nan() || value < m,
        None => false,
    }
}

/// A threshold can't be confirmed against an empty sample: 0 requests makes
/// `error_rate` read as a trivial 0.0 (see `histogram.rs`: `if total > 0 {...}
/// else { 0.0 }`), and 0 chain iterations makes a stream's `success_rate` read
/// as 0.0 too (see `streams.rs`) — either would silently satisfy (or, for a
/// lax floor, "pass") any gate, even though nothing was actually measured.
/// When a threshold is set and the run executed zero requests/iterations,
/// that must be a gate failure, not a silent green light — you can't prove a
/// threshold holds with zero data points.
const ZERO_SAMPLE_MSG: &str = "0 запросов выполнено — порог не может быть подтверждён";

/// True when at least one threshold is set but the run executed zero
/// requests/iterations — the exact condition that must fail the gate instead
/// of silently passing (see `ZERO_SAMPLE_MSG`). Deliberately ignores the
/// threshold's *value*: even a maximally lax threshold (e.g. `max_error_rate:
/// 100` or `min_success_rate: 0`) can't be confirmed against zero data.
fn zero_sample_breach(executed: u64, any_threshold_set: bool) -> bool {
    any_threshold_set && executed == 0
}

#[derive(Deserialize, Default)]
struct Thresholds {
    #[serde(default)]
    max_error_rate: Option<f64>,
    #[serde(default)]
    max_p95_ms: Option<f64>,
    /// Streams only: minimum completed-chain rate (%), checked per stream.
    #[serde(default)]
    min_success_rate: Option<f64>,
}

/// Escape a value for a JSON string context: the config is JSON, and `${VAR}`
/// almost always sits inside a string, so a secret containing `"`, `\` or a
/// control char would otherwise break the config (or inject structure).
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Expand ${VAR} references from the environment so secrets (tokens, client
/// secrets) stay out of the committed config file. Slices on byte positions from
/// `find` (char boundaries) so non-ASCII in the config is preserved; substituted
/// values are JSON-escaped for the surrounding string context.
fn expand_env(input: &str) -> String {
    if !input.contains("${") {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find("${") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 2..];
        match after.find('}') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(val) => out.push_str(&json_escape(&val)),
                    Err(_) => {
                        eprintln!("⚠ переменная окружения {name} не задана — оставлена как есть");
                        out.push_str("${");
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                // No closing "}" — leave the marker literally and stop scanning.
                out.push_str("${");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = Args::parse();
    let log_file = args.log_file.clone();
    macro_rules! log {
        ($cat:expr, $($a:tt)*) => { log_line(&log_file, $cat, &format!($($a)*)) };
    }

    let raw = match std::fs::read_to_string(&args.config) {
        Ok(s) => s,
        Err(e) => {
            log!("CLI", "Не удалось прочитать конфиг {}: {e}", args.config);
            return ExitCode::from(2);
        }
    };
    let mut cfg: CliConfig = match serde_json::from_str(&expand_env(&raw)) {
        Ok(c) => c,
        Err(e) => {
            log!("CLI", "Неверный конфиг: {e}");
            return ExitCode::from(2);
        }
    };

    let duration = args.duration.unwrap_or(cfg.duration_secs);

    // A config with none of targets/streams/grpc/websocket has nothing to run —
    // without this check it would silently exit 0 having done no work at all.
    if cfg.targets.is_empty() && cfg.streams.is_empty() && cfg.grpc.is_none() && cfg.websocket.is_none() {
        log!(
            "CLI",
            "Пустой конфиг: нет ни targets, ни streams, ни grpc, ни websocket — нечего запускать"
        );
        return ExitCode::from(2);
    }

    // gRPC / WebSocket / streams load paths run and return before the HTTP
    // scenario.
    if let Some(g) = cfg.grpc.take() {
        return run_grpc_load(g, duration, &args, cfg.thresholds.as_ref(), &log_file).await;
    }
    if let Some(w) = cfg.websocket.take() {
        return run_ws_load(w, duration, &args, cfg.thresholds.as_ref(), &log_file).await;
    }
    if !cfg.streams.is_empty() {
        return run_streams_load(cfg, duration, &args, &log_file).await;
    }

    // Resolve DB-backed datasets (SELECT → inline rows) before the run, so the
    // engine stays database-free. Streamed and capped for huge result sets.
    let has_db = cfg.datasets.iter().any(|d| d.source.kind == "db");
    if has_db {
        log!("CLI", "резолв БД-датасетов…");
    }
    let datasets =
        match maelstrom_db::resolve_db_datasets(&cfg.datasets, maelstrom_db::DB_DATASET_MAX_ROWS)
            .await
        {
            Ok((d, warnings)) => {
                for w in &warnings {
                    log!("CLI", "⚠ {w}");
                    eprintln!("⚠ {w}");
                }
                if has_db {
                    for ds in &d {
                        if let Some(rows) = ds.source.rows.as_ref() {
                            log!("CLI", "  датасет «{}»: {} строк", ds.name, rows.len());
                        }
                    }
                }
                d
            }
            Err(e) => {
                log!("CLI", "Ошибка БД-датасета: {e}");
                return ExitCode::from(2);
            }
        };

    let spec = ScenarioSpec {
        duration_secs: duration,
        timeout_ms: cfg.timeout_ms,
        targets: cfg.targets,
        datasets,
        file_pools: cfg.file_pools,
    };

    log!(
        "CLI",
        "конфиг «{}» загружен: {} ручек, {}с, датасетов={}, наборов файлов={}",
        cfg.name.as_deref().unwrap_or("scenario"),
        spec.targets.len(),
        duration,
        spec.datasets.len(),
        spec.file_pools.len()
    );
    println!(
        "⚡ Maelstrom — нагрузка сервиса «{}»: {} ручек, {} с",
        cfg.name.as_deref().unwrap_or("scenario"),
        spec.targets.len(),
        duration
    );

    let cancel = CancellationToken::new();
    let cancel_sig = cancel.clone();
    let log_sig = log_file.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        log_line(&log_sig, "CLI", "получен сигнал остановки (Ctrl-C)");
        cancel_sig.cancel();
    });

    let quiet = args.quiet;
    let log_prog = log_file.clone();
    let on_progress = Arc::new(move |p: &maelstrom_core::types::ScenarioProgress| {
        // Every tick goes to the log; stdout only when not quiet.
        log_line(
            &log_prog,
            "ПРОГРЕСС",
            &format!(
                "{:.0}с rps={:.0} всего={} ошибок={} p95={:.0}мс",
                p.elapsed_secs, p.overall_rps, p.overall_total, p.overall_errors, p.overall_p95_ms
            ),
        );
        if quiet {
            return;
        }
        println!(
            "[{:>4.0}с] rps={:>7} всего={:>9} ошибок={:>6} p95={:.0}мс",
            p.elapsed_secs,
            fmt(p.overall_rps),
            fmt(p.overall_total as f64),
            fmt(p.overall_errors as f64),
            p.overall_p95_ms
        );
    });
    let log_ref = log_file.clone();
    let on_refresh = Arc::new(move |n: u64| {
        log_line(&log_ref, "ТОКЕН", &format!("токен обновлён автоматически #{n}"));
    });
    let log_eng = log_file.clone();
    let on_log = Arc::new(move |m: String| log_line(&log_eng, "ДВИЖОК", &m));

    let result = match run_scenario(spec, cancel, on_progress, on_refresh, on_log).await {
        Ok(r) => r,
        Err(e) => {
            log!("CLI", "Ошибка запуска: {e}");
            return ExitCode::from(2);
        }
    };

    // write reports
    match serde_json::to_string_pretty(&result) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&args.out_json, json) {
                log!("CLI", "Не удалось записать {}: {e}", args.out_json);
            } else {
                log!("CLI", "JSON-отчёт записан: {}", args.out_json);
                println!("JSON-отчёт: {}", args.out_json);
            }
        }
        Err(e) => {
            log!("CLI", "Не удалось сериализовать JSON-отчёт: {e}");
        }
    }
    if let Some(html_path) = &args.out_html {
        let html = build_scenario_report(&result);
        if let Err(e) = std::fs::write(html_path, html) {
            log!("CLI", "Не удалось записать {html_path}: {e}");
        } else {
            log!("CLI", "HTML-отчёт записан: {html_path}");
            println!("HTML-отчёт: {html_path}");
        }
    }

    // summary
    let o = &result.overall;
    log!(
        "ИТОГ",
        "{} запросов, {:.2}% ошибок, rps(средн.)={:.0}, p95={:.0}мс, p99={:.0}мс",
        o.total_requests,
        o.error_rate,
        o.rps_avg,
        o.p95_ms,
        o.p99_ms
    );
    println!(
        "\nИтог: {} запросов, {:.2}% ошибок, RPS(средний)={:.0}, p95={:.0}мс, p99={:.0}мс",
        o.total_requests, o.error_rate, o.rps_avg, o.p95_ms, o.p99_ms
    );
    if o.dropped > 0 {
        log!("ИТОГ", "недодано {} запросов (целевой RPS не достигнут)", o.dropped);
        println!(
            "⚠ Недодано {} запросов — целевой RPS не достигнут (ручка(и) не успевают)",
            o.dropped
        );
    }
    println!("По ручкам:");
    for t in &result.targets {
        // The URL may be a presigned link — redact before logging.
        let safe = maelstrom_core::redact::safe_url(&t.url);
        log!(
            "ИТОГ",
            "  {} {} запросов={} rps={:.0} ошибок={:.2}% p95={:.0}мс",
            t.method,
            safe,
            t.total_requests,
            t.rps_avg,
            t.error_rate,
            t.p95_ms
        );
        println!(
            "  {:<7} {:<48} запросов={:<9} rps={:<7.0} ошибок={:.2}%  p95={:.0}мс",
            t.method,
            truncate(&safe, 48),
            t.total_requests,
            t.rps_avg,
            t.error_rate,
            t.p95_ms
        );
    }

    // thresholds → exit code
    let max_err = args.max_error_rate.or(cfg.thresholds.as_ref().and_then(|t| t.max_error_rate));
    let max_p95 = args.max_p95.or(cfg.thresholds.as_ref().and_then(|t| t.max_p95_ms));
    if args.min_success_rate.is_some()
        || cfg.thresholds.as_ref().and_then(|t| t.min_success_rate).is_some()
    {
        log!(
            "CLI",
            "⚠ --min-success-rate/min_success_rate применяется только к потокам (streams) — для HTTP-конфига порог игнорируется"
        );
        eprintln!(
            "⚠ --min-success-rate/min_success_rate применяется только к потокам (streams) — для HTTP-конфига порог игнорируется"
        );
    }
    let mut failed = false;
    if zero_sample_breach(o.total_requests, max_err.is_some() || max_p95.is_some()) {
        // 0 requests total -> error_rate is a trivial 0.0, which would pass
        // any max_error_rate/max_p95 gate silently. Fail loudly instead.
        log!("ГЕЙТ", "✖ {}", ZERO_SAMPLE_MSG);
        failed = true;
    } else {
        if let Some(me) = max_err {
            if breached(o.error_rate, Some(me)) {
                log!("ГЕЙТ", "✖ доля ошибок {:.2}% > {:.2}%", o.error_rate, me);
                failed = true;
            } else {
                log!("ГЕЙТ", "✓ доля ошибок {:.2}% ≤ {:.2}%", o.error_rate, me);
            }
        }
        if let Some(mp) = max_p95 {
            if breached(o.p95_ms, Some(mp)) {
                log!("ГЕЙТ", "✖ p95 {:.0}мс > {:.0}мс", o.p95_ms, mp);
                failed = true;
            } else {
                log!("ГЕЙТ", "✓ p95 {:.0}мс ≤ {:.0}мс", o.p95_ms, mp);
            }
        }
    }
    if failed {
        log!("CLI", "завершение: код выхода 1 (порог превышен)");
        return ExitCode::from(1);
    }
    if max_err.is_some() || max_p95.is_some() {
        println!("✔ Все пороги пройдены");
    }
    log!("CLI", "завершение: код выхода 0");
    ExitCode::SUCCESS
}

/// Run request-chaining streams (parallel chains + singles) and gate on
/// thresholds: overall error rate / p95 + per-stream completed-chain rate.
async fn run_streams_load(
    cfg: CliConfig,
    duration: u64,
    args: &Args,
    log_file: &Option<String>,
) -> ExitCode {
    let log = |cat: &str, msg: String| log_line(log_file, cat, &msg);

    // Resolve DB-backed datasets to inline rows, as in the HTTP path.
    let has_db = cfg.datasets.iter().any(|d| d.source.kind == "db");
    if has_db {
        log("CLI", "резолв БД-датасетов…".to_string());
    }
    let datasets = match maelstrom_db::resolve_db_datasets(
        &cfg.datasets,
        maelstrom_db::DB_DATASET_MAX_ROWS,
    )
    .await
    {
        Ok((d, warnings)) => {
            for w in &warnings {
                log("CLI", format!("⚠ {w}"));
                eprintln!("⚠ {w}");
            }
            d
        }
        Err(e) => {
            log("CLI", format!("Ошибка БД-датасета: {e}"));
            return ExitCode::from(2);
        }
    };

    let spec = StreamScenarioSpec {
        duration_secs: duration,
        timeout_ms: cfg.timeout_ms,
        streams: cfg.streams,
        datasets,
        file_pools: cfg.file_pools,
    };
    let total_steps: usize = spec.streams.iter().map(|s| s.steps.len()).sum();
    log(
        "CLI",
        format!(
            "конфиг «{}»: {} потоков ({} шагов суммарно), {}с",
            cfg.name.as_deref().unwrap_or("streams"),
            spec.streams.len(),
            total_steps,
            duration
        ),
    );
    println!(
        "⚡ Maelstrom — потоки (цепочки): «{}», {} потоков / {} шагов, {} с",
        cfg.name.as_deref().unwrap_or("streams"),
        spec.streams.len(),
        total_steps,
        duration
    );

    let cancel = CancellationToken::new();
    let cancel_sig = cancel.clone();
    let lf = log_file.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        log_line(&lf, "CLI", "получен сигнал остановки (Ctrl-C)");
        cancel_sig.cancel();
    });

    let quiet = args.quiet;
    let log_prog = log_file.clone();
    let on_progress = Arc::new(move |p: &maelstrom_core::types::StreamsProgress| {
        let chains: String = p
            .streams
            .iter()
            .map(|s| format!("{}={:.0}/с", truncate(&s.name, 14), s.iters_per_sec))
            .collect::<Vec<_>>()
            .join(" ");
        log_line(
            &log_prog,
            "ПРОГРЕСС",
            &format!(
                "{:.0}с Σrps={:.0} всего={} ошибок={} p95={:.0}мс | целевой rps: {chains}",
                p.elapsed_secs, p.overall_rps, p.overall_total, p.overall_errors, p.overall_p95_ms
            ),
        );
        if quiet {
            return;
        }
        println!(
            "[{:>4.0}с] Σrps={:>7} всего={:>9} ошибок={:>6} p95={:.0}мс | целевой rps: {chains}",
            p.elapsed_secs,
            fmt(p.overall_rps),
            fmt(p.overall_total as f64),
            fmt(p.overall_errors as f64),
            p.overall_p95_ms
        );
    });
    let log_eng = log_file.clone();
    let on_log = Arc::new(move |m: String| log_line(&log_eng, "ДВИЖОК", &m));

    let result = match run_streams(spec, cancel, on_progress, on_log).await {
        Ok(r) => r,
        Err(e) => {
            log("CLI", format!("Ошибка запуска: {e}"));
            return ExitCode::from(2);
        }
    };

    // JSON report.
    match serde_json::to_string_pretty(&result) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&args.out_json, json) {
                log("CLI", format!("Не удалось записать {}: {e}", args.out_json));
            } else {
                log("CLI", format!("JSON-отчёт записан: {}", args.out_json));
                println!("JSON-отчёт: {}", args.out_json);
            }
        }
        Err(e) => {
            log("CLI", format!("Не удалось сериализовать JSON-отчёт: {e}"));
        }
    }
    if args.out_html.is_some() {
        // The three-level HTML report ships with the app-side report work.
        log("CLI", "HTML-отчёт для потоков пока не поддерживается — записан JSON".to_string());
        eprintln!("⚠ HTML-отчёт для потоков пока не поддерживается — записан JSON");
    }

    // Three-level summary: overall → per stream (chain) → per step (endpoint).
    let o = &result.overall;
    log(
        "ИТОГ",
        format!(
            "{} запросов (сумма по всем шагам), {:.2}% ошибок, Σrps={:.0}, p95={:.0}мс, p99={:.0}мс",
            o.total_requests, o.error_rate, o.rps_avg, o.p95_ms, o.p99_ms
        ),
    );
    println!(
        "\nИтог (сумма по всем шагам всех потоков): {} запросов, {:.2}% ошибок, суммарный RPS={:.0}, p95={:.0}мс, p99={:.0}мс",
        o.total_requests, o.error_rate, o.rps_avg, o.p95_ms, o.p99_ms
    );
    println!("Потоки (RPS — на целевой endpoint каждой цепочки):");
    for s in &result.streams {
        // The chain's "target" endpoint is its last step; its rps is the load
        // that actually reaches the endpoint you usually care about — unlike the
        // Σrps above, which sums every step of every stream.
        let target_rps = s.steps.last().map(|st| st.rps_avg).unwrap_or(0.0);
        let head = format!(
            "«{}»: {:.0} rps на целевой endpoint · цепочек {}/{} завершено ({:.1}%) · e2e p95={:.0}мс{}",
            s.name,
            target_rps,
            s.iterations_completed,
            s.iterations_started,
            s.success_rate,
            s.e2e_p95_ms,
            if s.dropped > 0 { format!(" · недодано={}", s.dropped) } else { String::new() }
        );
        log("ИТОГ", format!("  {head}"));
        println!("  {head}");
        let last = s.steps.len().saturating_sub(1);
        for (j, st) in s.steps.iter().enumerate() {
            let safe = maelstrom_core::redact::safe_url(&st.url);
            let marker = if j == last { "  ← целевой endpoint" } else { "" };
            let line = format!(
                "{}. {:<7} {:<40} запросов={:<9} rps={:<7.0} ошибок={:.2}%  p95={:.0}мс{}",
                j + 1,
                st.method,
                truncate(&safe, 40),
                st.total_requests,
                st.rps_avg,
                st.error_rate,
                st.p95_ms,
                marker
            );
            log("ИТОГ", format!("    {line}"));
            println!("    {line}");
        }
    }

    // Thresholds → exit code.
    let t = cfg.thresholds.as_ref();
    let max_err = args.max_error_rate.or(t.and_then(|t| t.max_error_rate));
    let max_p95 = args.max_p95.or(t.and_then(|t| t.max_p95_ms));
    let min_sr = args.min_success_rate.or(t.and_then(|t| t.min_success_rate));
    let mut failed = false;
    if zero_sample_breach(o.total_requests, max_err.is_some() || max_p95.is_some()) {
        // Same empty-sample guard as the HTTP path: 0 requests overall makes
        // error_rate a trivial 0.0, which would pass any max_* gate silently.
        log("ГЕЙТ", format!("✖ {}", ZERO_SAMPLE_MSG));
        failed = true;
    } else {
        if let Some(me) = max_err {
            if breached(o.error_rate, Some(me)) {
                log("ГЕЙТ", format!("✖ доля ошибок {:.2}% > {:.2}%", o.error_rate, me));
                failed = true;
            } else {
                log("ГЕЙТ", format!("✓ доля ошибок {:.2}% ≤ {:.2}%", o.error_rate, me));
            }
        }
        if let Some(mp) = max_p95 {
            if breached(o.p95_ms, Some(mp)) {
                log("ГЕЙТ", format!("✖ p95 {:.0}мс > {:.0}мс", o.p95_ms, mp));
                failed = true;
            } else {
                log("ГЕЙТ", format!("✓ p95 {:.0}мс ≤ {:.0}мс", o.p95_ms, mp));
            }
        }
    }
    if let Some(ms) = min_sr {
        for s in &result.streams {
            if zero_sample_breach(s.iterations_started, true) {
                // Zero chain attempts for this stream -> success_rate reads as
                // a trivial 0.0 (see streams.rs), which a lax floor (even 0)
                // would otherwise pass. Zero iterations can't confirm any
                // floor, no matter its value.
                log("ГЕЙТ", format!("✖ поток «{}»: {}", s.name, ZERO_SAMPLE_MSG));
                failed = true;
            } else if below(s.success_rate, Some(ms)) {
                log(
                    "ГЕЙТ",
                    format!("✖ поток «{}»: завершено {:.1}% < {:.1}%", s.name, s.success_rate, ms),
                );
                failed = true;
            } else {
                log(
                    "ГЕЙТ",
                    format!("✓ поток «{}»: завершено {:.1}% ≥ {:.1}%", s.name, s.success_rate, ms),
                );
            }
        }
    }
    if failed {
        log("CLI", "завершение: код выхода 1 (порог превышен)".to_string());
        return ExitCode::from(1);
    }
    if max_err.is_some() || max_p95.is_some() || min_sr.is_some() {
        println!("✔ Все пороги пройдены");
    }
    log("CLI", "завершение: код выхода 0".to_string());
    ExitCode::SUCCESS
}

/// Run a gRPC load test (unary/server-streaming) and gate on thresholds.
async fn run_grpc_load(
    g: GrpcCliConfig,
    duration: u64,
    args: &Args,
    thresholds: Option<&Thresholds>,
    log_file: &Option<String>,
) -> ExitCode {
    let log = |cat: &str, msg: String| log_line(log_file, cat, &msg);
    log("CLI", format!("gRPC-нагрузка: {} {}/{}", g.endpoint, g.service, g.method));

    let proto = match maelstrom_grpc::Proto::from_file(&g.proto_path, &g.includes) {
        Ok(p) => p,
        Err(e) => {
            log("CLI", format!("gRPC .proto: {e}"));
            return ExitCode::from(2);
        }
    };
    let call = match proto.build_call_with_tls(&g.endpoint, &g.service, &g.method, &g.body, g.timeout_ms, g.tls.clone()) {
        Ok(c) => c,
        Err(e) => {
            log("CLI", format!("gRPC вызов: {e}"));
            return ExitCode::from(2);
        }
    };

    let cancel = CancellationToken::new();
    let cancel_sig = cancel.clone();
    let lf = log_file.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        log_line(&lf, "CLI", "получен сигнал остановки (Ctrl-C)");
        cancel_sig.cancel();
    });

    println!("⚡ Maelstrom gRPC — {} {}/{}: VUs={}, {} с", g.endpoint, g.service, g.method, g.vus, duration);
    log("CLI", format!("старт: VUs={} {}с rps_limit={:?}", g.vus, duration, g.rps_limit));

    let result = match maelstrom_grpc::grpc_load(call, g.vus, duration, g.rps_limit, cancel).await {
        Ok(r) => r,
        Err(e) => {
            log("CLI", format!("Ошибка gRPC-нагрузки: {e}"));
            return ExitCode::from(2);
        }
    };
    finish_load(result, "gRPC", args, thresholds, log_file)
}

/// Run a WebSocket load test (send→reply round-trips) and gate on thresholds.
async fn run_ws_load(
    w: WsCliConfig,
    duration: u64,
    args: &Args,
    thresholds: Option<&Thresholds>,
    log_file: &Option<String>,
) -> ExitCode {
    let log = |cat: &str, msg: String| log_line(log_file, cat, &msg);
    log("CLI", format!("WebSocket-нагрузка: {}", w.url));
    println!("⚡ Maelstrom WebSocket — {}: VUs={}, {} с", w.url, w.vus, duration);

    let cancel = CancellationToken::new();
    let cancel_sig = cancel.clone();
    let lf = log_file.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        log_line(&lf, "CLI", "получен сигнал остановки (Ctrl-C)");
        cancel_sig.cancel();
    });

    let result = match maelstrom_core::ws::ws_load(
        &w.url, &w.message, w.vus, duration, w.rps_limit, w.timeout_ms, cancel,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            log("CLI", format!("Ошибка WS-нагрузки: {e}"));
            return ExitCode::from(2);
        }
    };
    finish_load(result, "WebSocket", args, thresholds, log_file)
}

/// Write the JSON report, print + log the summary, evaluate thresholds → exit code.
/// Shared by the gRPC and WebSocket load paths.
fn finish_load(
    result: maelstrom_core::types::LoadTestResult,
    kind: &str,
    args: &Args,
    thresholds: Option<&Thresholds>,
    log_file: &Option<String>,
) -> ExitCode {
    let log = |cat: &str, msg: String| log_line(log_file, cat, &msg);
    match serde_json::to_string_pretty(&result) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&args.out_json, json) {
                log("CLI", format!("Не удалось записать {}: {e}", args.out_json));
            } else {
                log("CLI", format!("JSON-отчёт: {}", args.out_json));
                println!("JSON-отчёт: {}", args.out_json);
            }
        }
        Err(e) => {
            log("CLI", format!("Не удалось сериализовать JSON-отчёт: {e}"));
        }
    }
    if args.out_html.is_some() {
        log("CLI", format!("HTML-отчёт для {kind} пока не поддерживается — записан JSON"));
        eprintln!("⚠ HTML-отчёт для {kind} пока не поддерживается — записан JSON");
    }
    log(
        "ИТОГ",
        format!(
            "{} запросов, {:.2}% ошибок, rps={:.0}, p95={:.0}мс, p99={:.0}мс",
            result.total_requests, result.error_rate, result.rps_avg, result.p95_ms, result.p99_ms
        ),
    );
    println!(
        "\nИтог {}: {} запросов, {:.2}% ошибок, RPS={:.0}, p95={:.0}мс, p99={:.0}мс",
        kind, result.total_requests, result.error_rate, result.rps_avg, result.p95_ms, result.p99_ms
    );

    let max_err = args.max_error_rate.or(thresholds.and_then(|t| t.max_error_rate));
    let max_p95 = args.max_p95.or(thresholds.and_then(|t| t.max_p95_ms));
    if args.min_success_rate.is_some() || thresholds.and_then(|t| t.min_success_rate).is_some() {
        log(
            "CLI",
            format!(
                "⚠ --min-success-rate/min_success_rate применяется только к потокам (streams) — для {kind}-конфига порог игнорируется"
            ),
        );
        eprintln!(
            "⚠ --min-success-rate/min_success_rate применяется только к потокам (streams) — для {kind}-конфига порог игнорируется"
        );
    }
    let mut failed = false;
    if zero_sample_breach(result.total_requests, max_err.is_some() || max_p95.is_some()) {
        // Same empty-sample guard as the HTTP path: 0 requests makes
        // error_rate a trivial 0.0, which would pass any max_* gate silently.
        log("ГЕЙТ", format!("✖ {}", ZERO_SAMPLE_MSG));
        failed = true;
    } else {
        if let Some(me) = max_err {
            if breached(result.error_rate, Some(me)) {
                log("ГЕЙТ", format!("✖ доля ошибок {:.2}% > {:.2}%", result.error_rate, me));
                failed = true;
            } else {
                log("ГЕЙТ", format!("✓ доля ошибок {:.2}% ≤ {:.2}%", result.error_rate, me));
            }
        }
        if let Some(mp) = max_p95 {
            if breached(result.p95_ms, Some(mp)) {
                log("ГЕЙТ", format!("✖ p95 {:.0}мс > {:.0}мс", result.p95_ms, mp));
                failed = true;
            } else {
                log("ГЕЙТ", format!("✓ p95 {:.0}мс ≤ {:.0}мс", result.p95_ms, mp));
            }
        }
    }
    if failed {
        log("CLI", "завершение: код выхода 1 (порог превышен)".to_string());
        return ExitCode::from(1);
    }
    if max_err.is_some() || max_p95.is_some() {
        println!("✔ Все пороги пройдены");
    }
    log("CLI", "завершение: код выхода 0".to_string());
    ExitCode::SUCCESS
}

fn fmt(v: f64) -> String {
    if v >= 10_000.0 {
        format!("{:.1}k", v / 1000.0)
    } else {
        format!("{}", v.round() as i64)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{below, breached, expand_env, zero_sample_breach};

    #[test]
    fn threshold_gate() {
        // no threshold set -> never fails the run
        assert!(!breached(9999.0, None));
        // strictly greater breaches; exactly at the limit passes
        assert!(!breached(1.0, Some(1.0)));
        assert!(breached(1.01, Some(1.0)));
        assert!(!breached(0.99, Some(1.0)));
        // p95-in-ms style
        assert!(breached(557.0, Some(500.0)));
        assert!(!breached(500.0, Some(500.0)));
    }

    #[test]
    fn env_expansion() {
        std::env::set_var("MAELSTROM_TEST_SECRET", "s3cret");
        assert_eq!(expand_env("client_secret=${MAELSTROM_TEST_SECRET}"), "client_secret=s3cret");
        // unknown var is left as-is (with a warning to stderr)
        assert_eq!(expand_env("x=${MAELSTROM_TEST_UNSET_XYZ}"), "x=${MAELSTROM_TEST_UNSET_XYZ}");
        // no placeholders
        assert_eq!(expand_env("plain text"), "plain text");
        // multiple
        std::env::set_var("MAELSTROM_TEST_A", "1");
        assert_eq!(expand_env("${MAELSTROM_TEST_A}-${MAELSTROM_TEST_A}"), "1-1");
        // non-ASCII around a placeholder must survive intact (regression)
        assert_eq!(expand_env("тело=${MAELSTROM_TEST_A} мир café"), "тело=1 мир café");
        // substituted values are JSON-escaped for the string context
        std::env::set_var("MAELSTROM_TEST_Q", "a\"b\\c");
        assert_eq!(expand_env("\"t\":\"${MAELSTROM_TEST_Q}\""), "\"t\":\"a\\\"b\\\\c\"");
    }

    #[test]
    fn parses_http_config_with_defaults() {
        let json = r#"{"duration_secs":30,"targets":[{"name":"t","method":"GET","url":"http://x","rps":10}]}"#;
        let cfg: super::CliConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.duration_secs, 30);
        assert_eq!(cfg.timeout_ms, 10_000); // default_timeout
        assert_eq!(cfg.targets.len(), 1);
        assert_eq!(cfg.targets[0].rps, 10);
        assert!(cfg.datasets.is_empty() && cfg.file_pools.is_empty());
        assert!(cfg.grpc.is_none() && cfg.websocket.is_none());
    }

    #[test]
    fn parses_grpc_config_and_dispatches() {
        let json = r#"{"duration_secs":10,"targets":[],"grpc":{"endpoint":"http://h:50051","proto_path":"s.proto","service":"S","method":"M"}}"#;
        let cfg: super::CliConfig = serde_json::from_str(json).unwrap();
        let g = cfg.grpc.expect("grpc block present → gRPC path");
        assert_eq!(g.service, "S");
        assert_eq!(g.vus, 10); // default_vus
        assert_eq!(g.timeout_ms, 10_000);
    }

    #[test]
    fn parses_websocket_config_and_dispatches() {
        let json = r#"{"duration_secs":10,"targets":[],"websocket":{"url":"ws://h"}}"#;
        let cfg: super::CliConfig = serde_json::from_str(json).unwrap();
        let w = cfg.websocket.expect("websocket block present → WS path");
        assert_eq!(w.url, "ws://h");
        assert_eq!(w.vus, 10);
        assert!(w.message.is_empty());
    }

    #[test]
    fn parses_streams_config_without_targets() {
        // A streams-only config: no "targets" key at all (it defaults to []),
        // extract rules threaded, thresholds carry min_success_rate.
        let json = r#"{
            "duration_secs": 60,
            "streams": [{
                "name": "checkout",
                "rps": 100,
                "steps": [
                    {"name":"login","method":"POST","url":"http://x/login",
                     "extract":[{"name":"token","from":"json","expr":"data.token"}]},
                    {"name":"order","method":"POST","url":"http://x/order",
                     "headers":[["Authorization","Bearer {{token}}"]]}
                ]
            }],
            "thresholds": {"max_error_rate": 1.0, "min_success_rate": 99.0}
        }"#;
        let cfg: super::CliConfig = serde_json::from_str(json).unwrap();
        assert!(cfg.targets.is_empty(), "targets defaults to empty");
        assert_eq!(cfg.streams.len(), 1);
        let s = &cfg.streams[0];
        assert_eq!(s.rps, 100);
        assert_eq!(s.steps.len(), 2);
        assert_eq!(s.steps[0].extract.len(), 1);
        assert_eq!(s.steps[0].extract[0].name, "token");
        assert_eq!(s.steps[1].headers[0].1, "Bearer {{token}}");
        let t = cfg.thresholds.unwrap();
        assert_eq!(t.min_success_rate, Some(99.0));
    }

    #[test]
    fn streams_take_precedence_over_targets_when_both_present() {
        // Both blocks parse side by side; main() dispatches to the streams path
        // whenever `streams` is non-empty (documented precedence).
        let json = r#"{
            "duration_secs": 10,
            "targets": [{"name":"t","method":"GET","url":"http://x","rps":5}],
            "streams": [{"name":"s","rps":10,"steps":[{"name":"a","method":"GET","url":"http://y"}]}]
        }"#;
        let cfg: super::CliConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.targets.len(), 1);
        assert_eq!(cfg.streams.len(), 1);
        assert!(!cfg.streams.is_empty(), "non-empty streams → streams path wins");
    }

    #[test]
    fn success_rate_floor_gate() {
        // not set -> never fails
        assert!(!below(0.0, None));
        // strictly below the floor breaches; exactly at the floor passes
        assert!(below(98.9, Some(99.0)));
        assert!(!below(99.0, Some(99.0)));
        assert!(!below(100.0, Some(99.0)));
    }

    #[test]
    fn zero_sample_gate() {
        // c1-deeper: a run that executed 0 requests/iterations must breach
        // the gate whenever a threshold was actually set — 0/0 reads as a
        // trivial error_rate/success_rate of 0.0 (see histogram.rs /
        // streams.rs), which `breached`/`below` alone would treat as a pass.
        assert!(zero_sample_breach(0, true));
        // Any nonzero sample is never a zero-sample breach, no matter how
        // small — the normal breached()/below() value checks take over.
        assert!(!zero_sample_breach(1, true));
        assert!(!zero_sample_breach(1_000, true));
        // No threshold set at all -> nothing to confirm, so 0 executed is
        // fine (matches how breached()/below() treat `None`).
        assert!(!zero_sample_breach(0, false));
    }
}
