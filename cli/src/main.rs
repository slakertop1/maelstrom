//! Maelstrom headless load-test runner.
//!
//! Runs a multi-endpoint scenario exported from the desktop app (or hand-written
//! JSON) from a CI pipeline or a Kubernetes Job — i.e. from inside the network
//! that is allowed to reach prod, not a laptop. Emits a JSON + HTML report and
//! exits non-zero when a threshold is breached, so pipelines gate on it.

use maelstrom_core::report::build_scenario_report;
use maelstrom_core::scenario::run_scenario;
use maelstrom_core::types::ScenarioSpec;
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
fn breached(value: f64, max: Option<f64>) -> bool {
    matches!(max, Some(m) if value > m)
}

#[derive(Deserialize, Default)]
struct Thresholds {
    #[serde(default)]
    max_error_rate: Option<f64>,
    #[serde(default)]
    max_p95_ms: Option<f64>,
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

    // gRPC / WebSocket load paths (separate transports) run and return before the
    // HTTP scenario.
    if let Some(g) = cfg.grpc.take() {
        return run_grpc_load(g, duration, &args, cfg.thresholds.as_ref(), &log_file).await;
    }
    if let Some(w) = cfg.websocket.take() {
        return run_ws_load(w, duration, &args, cfg.thresholds.as_ref(), &log_file).await;
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
    if let Ok(json) = serde_json::to_string_pretty(&result) {
        if let Err(e) = std::fs::write(&args.out_json, json) {
            log!("CLI", "Не удалось записать {}: {e}", args.out_json);
        } else {
            log!("CLI", "JSON-отчёт записан: {}", args.out_json);
            println!("JSON-отчёт: {}", args.out_json);
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
    let mut failed = false;
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
    let call = match proto.build_call(&g.endpoint, &g.service, &g.method, &g.body, g.timeout_ms) {
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
    if let Ok(json) = serde_json::to_string_pretty(&result) {
        if std::fs::write(&args.out_json, json).is_ok() {
            log("CLI", format!("JSON-отчёт: {}", args.out_json));
            println!("JSON-отчёт: {}", args.out_json);
        }
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
    let mut failed = false;
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
    use super::{breached, expand_env};

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
}
