//! Request-chaining load engine ("streams").
//!
//! A run is a set of independent **streams** driven in parallel. Each stream is
//! fired at its own target rate of ITERATIONS per second (open model); one
//! iteration runs the stream's `steps` in order, threading values extracted from
//! each response into `{{vars}}` used by later steps (per iteration — so virtual
//! users never share a token/id). A single-request stream is just one step.
//!
//! Metrics come out three levels deep: overall → per stream (whole-chain
//! success-rate + end-to-end latency) → per step (per-endpoint, so the funnel is
//! visible as later steps see fewer requests than earlier ones).
//!
//! The existing `scenario` engine (parallel single-step targets) is untouched;
//! this is an additive path. The per-request accumulator (`TargetAcc`) and
//! finalization are reused from `scenario` so per-step stats never drift.

use crate::dynval::{apply_chain_vars, DynState};
use crate::multipart::{form_from_prepared_vars, prepare_parts, PreparedPart};
use crate::scenario::{LogFn, TargetAcc};
use crate::types::*;
use hdrhistogram::Histogram;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub type StreamsProgressFn = Arc<dyn Fn(&StreamsProgress) + Send + Sync>;

/// One sample flowing to the aggregator: either a single step's outcome, or a
/// whole-iteration (chain) outcome.
enum Sample {
    Step { stream: usize, step: usize, latency_us: u64, status: u16 },
    Iter { stream: usize, ok: bool, e2e_us: u64 },
}

/// A step made ready to fire many times: client (with TLS), parsed method,
/// prepared multipart, and flags for whether we must read the body/headers to
/// satisfy this step's extract rules (so bodies are only materialized when used).
struct PreparedStep {
    client: reqwest::Client,
    method: reqwest::Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    multipart: Option<Arc<Vec<PreparedPart>>>,
    extract: Vec<PreparedExtract>,
    needs_body: bool,
    needs_headers: bool,
}

/// An extract rule with its regex compiled ONCE (regex compilation costs far
/// more than matching; doing it per response would cap throughput and inflate
/// the reported end-to-end latency). Non-regex rules carry `regex: None`.
struct PreparedExtract {
    rule: ExtractRule,
    regex: Option<regex::Regex>,
}

struct PreparedStream {
    name: String,
    rps: u32,
    steps: Arc<Vec<PreparedStep>>,
}

/// Run a streams (request-chaining) load test to completion or until `cancel`.
/// Validates and builds everything up front so a bad config fails fast.
pub async fn run_streams(
    mut spec: StreamScenarioSpec,
    cancel: CancellationToken,
    on_progress: StreamsProgressFn,
    on_log: LogFn,
) -> Result<StreamsResult, String> {
    let log = |m: String| on_log(m);
    let log_err = |m: String| {
        on_log(format!("✗ {m}"));
        m
    };

    spec.duration_secs = spec.duration_secs.clamp(1, 86_400);
    spec.streams.retain(|s| s.rps > 0 && !s.steps.is_empty());
    if spec.streams.is_empty() {
        return Err(log_err("Не задано ни одного потока с RPS > 0 и шагами".to_string()));
    }
    if spec.streams.len() > 100 {
        return Err(log_err("Слишком много потоков (максимум 100)".to_string()));
    }

    let timeout = Duration::from_millis(spec.timeout_ms.max(100));

    // Build every stream's steps (validate methods/URLs, clients with TLS,
    // prepare multipart) up front.
    let mut prepared: Vec<PreparedStream> = Vec::with_capacity(spec.streams.len());
    for s in &spec.streams {
        let rps = s.rps.clamp(1, 100_000);
        let mut steps = Vec::with_capacity(s.steps.len());
        for st in &s.steps {
            let method = reqwest::Method::from_bytes(st.method.as_bytes())
                .map_err(|_| log_err(format!("Поток «{}»: неверный метод у шага «{}»", s.name, st.name)))?;
            let url = st.url.trim().to_string();
            // Literal URLs only — {{...}} is expanded per request.
            if !url.contains("{{") && reqwest::Url::parse(&url).is_err() {
                return Err(log_err(format!("Поток «{}»: неверный URL у шага «{}»", s.name, st.name)));
            }
            let headers: Vec<(String, String)> = st
                .headers
                .iter()
                .filter(|(k, v)| {
                    reqwest::header::HeaderName::from_bytes(k.trim().as_bytes()).is_ok()
                        && reqwest::header::HeaderValue::from_str(v).is_ok()
                })
                .cloned()
                .collect();
            let mut builder = reqwest::Client::builder()
                .timeout(timeout)
                .pool_max_idle_per_host((rps as usize).clamp(8, 512))
                .user_agent("Maelstrom-Streams/0.1");
            builder = crate::tls::apply_tls(builder, &st.tls).map_err(&log_err)?;
            let client = builder.build().map_err(|e| log_err(e.to_string()))?;

            let multipart = match &st.multipart {
                Some(parts) if parts.iter().any(|p| p.enabled && !p.name.trim().is_empty()) => Some(
                    Arc::new(
                        prepare_parts(parts)
                            .map_err(|e| log_err(format!("Поток «{}»/«{}»: {e}", s.name, st.name)))?,
                    ),
                ),
                _ => None,
            };

            let needs_headers =
                st.extract.iter().any(|e| e.from.trim().eq_ignore_ascii_case("header"));
            let needs_body =
                st.extract.iter().any(|e| !e.from.trim().eq_ignore_ascii_case("header"));

            // Compile each regex extract rule once, up front. A bad pattern is
            // logged here (not silently unset on every request) and disables
            // just that rule. `from` is normalized (trim/lowercase) and
            // validated here too — an unknown source used to compare
            // case-sensitively and then fall silently into the JSON-path
            // branch of `extract_value`, so a typo (or "Header") quietly
            // corrupted a chain instead of failing the run up front.
            let mut extract: Vec<PreparedExtract> = Vec::with_capacity(st.extract.len());
            for r in &st.extract {
                let norm_from = normalize_extract_from(&r.from).map_err(|e| {
                    log_err(format!("Поток «{}»/«{}»/«{}»: {e}", s.name, st.name, r.name))
                })?;
                let regex = if norm_from == "regex" {
                    match regex::Regex::new(&r.expr) {
                        Ok(re) => Some(re),
                        Err(e) => {
                            log(format!(
                                "⚠ поток «{}»/«{}»: неверное regex-правило «{}»: {e}",
                                s.name, st.name, r.name
                            ));
                            None
                        }
                    }
                } else {
                    None
                };
                let mut rule = r.clone();
                rule.from = norm_from;
                extract.push(PreparedExtract { rule, regex });
            }

            steps.push(PreparedStep {
                client,
                method,
                url,
                headers,
                body: st.body.clone(),
                multipart,
                extract,
                needs_body,
                needs_headers,
            });
        }
        prepared.push(PreparedStream { name: s.name.clone(), rps, steps: Arc::new(steps) });
    }

    // Data providers (datasets + file pools) once, up front.
    let dyn_state = Arc::new(
        crate::dynval::resolve(&spec.datasets, &spec.file_pools, &cancel)
            .await
            .map_err(&log_err)?,
    );

    log(format!(
        "старт потоков: {} потоков на {}с",
        prepared.len(),
        spec.duration_secs
    ));
    for s in &prepared {
        log(format!("  поток «{}»: {} шаг(ов) @ {} цепочек/с", s.name, s.steps.len(), s.rps));
    }

    let started_wall = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(spec.duration_secs);

    let dropped: Vec<Arc<AtomicU64>> =
        (0..prepared.len()).map(|_| Arc::new(AtomicU64::new(0))).collect();

    let (tx, rx) = mpsc::unbounded_channel::<Sample>();
    for (i, s) in prepared.iter().enumerate() {
        tokio::spawn(stream_dispatcher(
            i,
            s.steps.clone(),
            s.rps,
            deadline,
            cancel.clone(),
            tx.clone(),
            dyn_state.clone(),
            dropped[i].clone(),
        ));
    }
    drop(tx);

    let names: Vec<String> = prepared.iter().map(|s| s.name.clone()).collect();
    let step_meta: Vec<Vec<(String, String)>> = prepared
        .iter()
        .map(|s| s.steps.iter().map(|st| (st.url.clone(), st.method.to_string())).collect())
        .collect();

    let mut result =
        aggregate(rx, &names, &step_meta, spec.duration_secs, started, &cancel, on_progress).await;
    // Capture BEFORE the cleanup `cancel()` below. The run is only "stopped
    // early" if the token was already cancelled (user Stop / abort) by the time
    // aggregation finished; a normal deadline completion leaves it un-cancelled.
    // Reading it *after* `cancel.cancel()` would always be true and mislabel
    // every full run as "(остановлено)". (The JSON `stopped_early`, computed
    // inside `aggregate`, is already correct — this only fixes the log line.)
    let stopped_early = cancel.is_cancelled();
    cancel.cancel();
    result.started_at = started_wall;

    let mut total_dropped = 0u64;
    for (i, d) in dropped.iter().enumerate() {
        let n = d.load(Ordering::Relaxed);
        total_dropped += n;
        if let Some(s) = result.streams.get_mut(i) {
            s.dropped = n;
        }
    }
    result.overall.dropped = total_dropped;
    if total_dropped > 0 {
        log(format!("⚠ недодано {total_dropped} итераций — целевой темп цепочек не достигнут"));
    }
    log(format!(
        "потоки завершены: запросов={} ошибок={} ({:.2}%){}",
        result.overall.total_requests,
        result.overall.errors,
        result.overall.error_rate,
        if stopped_early { " (остановлено)" } else { "" }
    ));
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn stream_dispatcher(
    stream_idx: usize,
    steps: Arc<Vec<PreparedStep>>,
    rps: u32,
    deadline: tokio::time::Instant,
    token: CancellationToken,
    tx: mpsc::UnboundedSender<Sample>,
    dyn_state: Arc<DynState>,
    dropped: Arc<AtomicU64>,
) {
    // Cap in-flight ITERATIONS at ~2s of arrivals so a slow chain doesn't grow
    // memory without bound; excess arrivals are counted as dropped.
    let cap = ((rps as usize) * 2).clamp(50, 20_000);
    let inflight = Arc::new(AtomicUsize::new(0));
    let started = tokio::time::Instant::now();
    let mut generated: u64 = 0;
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = token.cancelled() => break,
            _ = tokio::time::sleep_until(deadline) => break,
            _ = interval.tick() => {}
        }
        // Target arrivals come from wall-clock elapsed time, not from counting
        // realized `interval.tick()` events: under load, MissedTickBehavior::
        // Skip silently drops missed ticks, and a per-tick fixed increment
        // would lose that time's worth of iterations with no trace anywhere.
        // Basing it on elapsed time means a late/skipped tick still catches up
        // to the correct cumulative total; any of that catch-up the inflight
        // cap can't absorb is counted as dropped below, same as before.
        let target_total = (started.elapsed().as_secs_f64() * rps as f64).floor() as u64;
        let mut n = target_total.saturating_sub(generated);
        generated = target_total;
        while n > 0 {
            if inflight.load(Ordering::Relaxed) >= cap {
                dropped.fetch_add(n, Ordering::Relaxed);
                break;
            }
            inflight.fetch_add(1, Ordering::Relaxed);
            let steps = steps.clone();
            let tx = tx.clone();
            let inflight = inflight.clone();
            let dyn_state = dyn_state.clone();
            let cancel = token.clone();
            tokio::spawn(async move {
                run_iteration(stream_idx, &steps, &tx, &dyn_state, &cancel).await;
                inflight.fetch_sub(1, Ordering::Relaxed);
            });
            n -= 1;
        }
    }
}

/// Run one chain iteration: steps in order, threading extracted `{{vars}}`.
/// Aborts remaining steps on the first failure (network error / status ≥ 400)
/// or as soon as `cancel` fires — checked between steps AND inside the wait
/// for each step's response, so a chain in flight when Stop is hit doesn't
/// keep running to completion (or worse, hang on a slow/dead target).
async fn run_iteration(
    stream_idx: usize,
    steps: &[PreparedStep],
    tx: &mpsc::UnboundedSender<Sample>,
    dyn_state: &DynState,
    cancel: &CancellationToken,
) {
    let iter_start = Instant::now();
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut ok = true;
    for (i, step) in steps.iter().enumerate() {
        if cancel.is_cancelled() {
            ok = false;
            break;
        }
        let start = Instant::now();
        let (status, body, headers) = send_step(step, &vars, dyn_state, cancel).await;
        let latency_us = start.elapsed().as_micros().max(1) as u64;
        let _ = tx.send(Sample::Step { stream: stream_idx, step: i, latency_us, status });
        if status == 0 || status >= 400 {
            ok = false;
            break;
        }
        for pe in &step.extract {
            if let Some(val) = extract_value(pe, body.as_deref(), &headers) {
                vars.insert(pe.rule.name.clone(), val);
            }
        }
    }
    let e2e_us = iter_start.elapsed().as_micros().max(1) as u64;
    let _ = tx.send(Sample::Iter { stream: stream_idx, ok, e2e_us });
}

/// Build and send one step. Substitutes chain `{{vars}}` first, then the dynval
/// generators (`{{$...}}`). Returns (status, body?, headers) — body/headers only
/// read when this step has an extract rule that needs them. Races the request
/// against `cancel` so a step in flight when Stop is hit doesn't block the
/// chain from unwinding until the request itself times out.
async fn send_step(
    step: &PreparedStep,
    vars: &HashMap<String, String>,
    dyn_state: &DynState,
    cancel: &CancellationToken,
) -> (u16, Option<String>, Vec<(String, String)>) {
    // Build the request inside a block so the per-request context (which holds a
    // !Sync RefCell) is dropped BEFORE the await below — keeps this future Send.
    let req = {
        let ctx = dyn_state.request();
        let url = ctx.expand(&apply_chain_vars(&step.url, vars));
        let mut req = step.client.request(step.method.clone(), url);
        for (k, v) in &step.headers {
            if step.multipart.is_some() && k.trim().eq_ignore_ascii_case("content-type") {
                continue;
            }
            req = req.header(k.trim(), ctx.expand(&apply_chain_vars(v, vars)));
        }
        if let Some(parts) = &step.multipart {
            // vars threaded into multipart text fields too (login → upload chains).
            req = req.multipart(form_from_prepared_vars(parts, &ctx, vars));
        } else if let Some(b) = &step.body {
            if !b.is_empty() {
                req = req.body(ctx.expand(&apply_chain_vars(b, vars)));
            }
        }
        req
    };
    let sent = tokio::select! {
        biased;
        _ = cancel.cancelled() => return (0, None, Vec::new()),
        r = req.send() => r,
    };
    match sent {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let headers = if step.needs_headers {
                resp.headers()
                    .iter()
                    .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect()
            } else {
                Vec::new()
            };
            let body = if step.needs_body {
                resp.text().await.ok()
            } else {
                let _ = resp.bytes().await; // drain and discard
                None
            };
            (status, body, headers)
        }
        Err(_) => (0, None, Vec::new()),
    }
}

/// Normalize an extract rule's `from` (trim + lowercase, so "Header"/" json "
/// behave like "header"/"json") and check it's one of the supported sources.
fn normalize_extract_from(from: &str) -> Result<String, String> {
    let norm = from.trim().to_lowercase();
    if matches!(norm.as_str(), "header" | "regex" | "json") {
        Ok(norm)
    } else {
        Err(format!("неверный источник extract «{from}» (ожидается header/regex/json)"))
    }
}

fn extract_value(
    pe: &PreparedExtract,
    body: Option<&str>,
    headers: &[(String, String)],
) -> Option<String> {
    let rule = &pe.rule;
    match rule.from.as_str() {
        "header" => headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(rule.expr.trim()))
            .map(|(_, v)| v.clone()),
        "regex" => {
            // Precompiled at build time; None means the pattern was invalid.
            let caps = pe.regex.as_ref()?.captures(body?)?;
            caps.get(1).or_else(|| caps.get(0)).map(|m| m.as_str().to_string())
        }
        // default: JSON dot/bracket path (e.g. data.items.0.token)
        _ => {
            let v: serde_json::Value = serde_json::from_str(body?).ok()?;
            json_path(&v, &rule.expr).map(json_scalar)
        }
    }
}

fn json_path<'a>(v: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let norm = path.replace('[', ".").replace(']', "");
    let mut cur = v;
    for part in norm.split('.').filter(|p| !p.is_empty()) {
        cur = match part.parse::<usize>() {
            // A numeric segment usually means an array index, but the same
            // response shape can carry an OBJECT with numeric-looking string
            // keys (e.g. {"2024": {...}}) — those were unreachable before
            // since the array-index branch's `?` bailed the whole lookup on
            // any object. Fall back to a plain string-key lookup instead.
            Ok(idx) => match cur.get(idx) {
                Some(v) => v,
                None => cur.get(part)?,
            },
            Err(_) => cur.get(part)?,
        };
    }
    Some(cur)
}

fn json_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Whole-chain accumulator: per-step request stats + iteration success + e2e.
struct StreamAcc {
    steps: Vec<TargetAcc>,
    e2e: Histogram<u64>,
    iters_started: u64,
    iters_completed: u64,
    e2e_sum_us: u128,
    sec_iters: u64,
}

impl StreamAcc {
    fn new(n_steps: usize) -> Self {
        Self {
            steps: (0..n_steps).map(|_| TargetAcc::new()).collect(),
            e2e: Histogram::new(3).expect("hist"),
            iters_started: 0,
            iters_completed: 0,
            e2e_sum_us: 0,
            sec_iters: 0,
        }
    }
    fn errors(&self) -> u64 {
        self.steps.iter().map(|a| a.errors).sum()
    }
}

#[allow(clippy::too_many_arguments)]
async fn aggregate(
    mut rx: mpsc::UnboundedReceiver<Sample>,
    names: &[String],
    step_meta: &[Vec<(String, String)>],
    duration_secs: u64,
    started: Instant,
    token: &CancellationToken,
    on_progress: StreamsProgressFn,
) -> StreamsResult {
    let mut accs: Vec<StreamAcc> = step_meta.iter().map(|s| StreamAcc::new(s.len())).collect();
    let mut overall = TargetAcc::new();

    let mut ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            sample = rx.recv() => match sample {
                Some(Sample::Step { stream, step, latency_us, status }) => {
                    if let Some(acc) = accs.get_mut(stream).and_then(|s| s.steps.get_mut(step)) {
                        acc.record(latency_us, status);
                    }
                    overall.record(latency_us, status);
                }
                Some(Sample::Iter { stream, ok, e2e_us }) => {
                    if let Some(s) = accs.get_mut(stream) {
                        s.iters_started += 1;
                        s.sec_iters += 1;
                        if ok {
                            s.iters_completed += 1;
                        }
                        let _ = s.e2e.record(e2e_us);
                        s.e2e_sum_us += e2e_us as u128;
                    }
                }
                None => break,
            },
            _ = ticker.tick() => {
                let progress = StreamsProgress {
                    elapsed_secs: started.elapsed().as_secs_f64(),
                    overall_total: overall.total,
                    overall_errors: overall.errors,
                    overall_rps: overall.sec_requests as f64,
                    overall_p95_ms: overall.hist.value_at_quantile(0.95) as f64 / 1000.0,
                    streams: accs
                        .iter()
                        .enumerate()
                        .map(|(i, s)| StreamProgress {
                            name: names[i].clone(),
                            iterations: s.iters_started,
                            iters_per_sec: s.sec_iters as f64,
                            errors: s.errors(),
                        })
                        .collect(),
                };
                for s in accs.iter_mut() {
                    for a in s.steps.iter_mut() {
                        a.tick();
                    }
                    s.sec_iters = 0;
                }
                overall.tick();
                on_progress(&progress);
            }
        }
    }

    let actual_duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    let stopped_early = token.is_cancelled();

    let stream_results: Vec<StreamResult> = accs
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let started_n = s.iters_started;
            let completed_n = s.iters_completed;
            let e2e_sum = s.e2e_sum_us;
            let e2e = &s.e2e;
            let e2e_avg_ms = if started_n > 0 {
                e2e_sum as f64 / started_n as f64 / 1000.0
            } else {
                0.0
            };
            let e2e_p50_ms = e2e.value_at_quantile(0.5) as f64 / 1000.0;
            let e2e_p95_ms = e2e.value_at_quantile(0.95) as f64 / 1000.0;
            let e2e_p99_ms = e2e.value_at_quantile(0.99) as f64 / 1000.0;
            let steps: Vec<LoadTestResult> = s
                .steps
                .into_iter()
                .enumerate()
                .map(|(j, a)| {
                    let (url, method) = step_meta[i][j].clone();
                    a.finalize(
                        RunMeta { target: url, kind: method, vus: 0, duration_secs, rps_limit: None },
                        actual_duration_ms,
                        stopped_early,
                    )
                })
                .collect();
            StreamResult {
                name: names[i].clone(),
                steps,
                iterations_started: started_n,
                iterations_completed: completed_n,
                success_rate: if started_n > 0 {
                    completed_n as f64 / started_n as f64 * 100.0
                } else {
                    0.0
                },
                e2e_avg_ms,
                e2e_p50_ms,
                e2e_p95_ms,
                e2e_p99_ms,
                dropped: 0, // filled by the caller from the dispatcher shortfall
            }
        })
        .collect();

    let overall_result = overall.finalize(
        RunMeta {
            target: format!("{} потоков", names.len()),
            kind: "CHAIN".to_string(),
            vus: 0,
            duration_secs,
            rps_limit: None,
        },
        actual_duration_ms,
        stopped_early,
    );

    StreamsResult {
        started_at: String::new(),
        duration_secs,
        actual_duration_ms,
        overall: overall_result,
        streams: stream_results,
        stopped_early,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_vars_replaces_known_leaves_generators() {
        let mut vars = HashMap::new();
        vars.insert("token".to_string(), "abc123".to_string());
        // known var replaced
        assert_eq!(apply_chain_vars("Bearer {{token}}", &vars), "Bearer abc123");
        // generator ({{$...}}) and unknown name left intact for the next pass
        assert_eq!(apply_chain_vars("{{$uuid}}/{{missing}}/{{token}}", &vars), "{{$uuid}}/{{missing}}/abc123");
        // whitespace tolerated
        assert_eq!(apply_chain_vars("{{ token }}", &vars), "abc123");
        // no braces → unchanged
        assert_eq!(apply_chain_vars("plain", &vars), "plain");
    }

    #[test]
    fn apply_vars_edge_cases() {
        let mut vars = HashMap::new();
        vars.insert("a".to_string(), "1".to_string());
        vars.insert("b".to_string(), "2".to_string());
        // adjacent placeholders
        assert_eq!(apply_chain_vars("{{a}}{{b}}", &vars), "12");
        // dataset refs ({{$data...}}) untouched — expanded later by dynval
        assert_eq!(apply_chain_vars("{{$data.u.id}}-{{a}}", &vars), "{{$data.u.id}}-1");
        // unclosed marker survives verbatim
        assert_eq!(apply_chain_vars("x {{a}} tail {{oops", &vars), "x 1 tail {{oops");
        // empty vars → fast path, unchanged
        assert_eq!(apply_chain_vars("{{a}}", &HashMap::new()), "{{a}}");
        // non-ASCII around placeholders preserved
        assert_eq!(apply_chain_vars("токен={{a}} мир", &vars), "токен=1 мир");
    }

    #[test]
    fn json_path_navigates_objects_and_arrays() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"data":{"items":[{"token":"T1"},{"token":"T2"}]},"n":5}"#).unwrap();
        assert_eq!(json_path(&v, "data.items.0.token").map(json_scalar), Some("T1".to_string()));
        assert_eq!(json_path(&v, "data.items[1].token").map(json_scalar), Some("T2".to_string()));
        assert_eq!(json_path(&v, "n").map(json_scalar), Some("5".to_string()));
        assert!(json_path(&v, "data.missing").is_none());
    }

    /// Build a PreparedExtract the same way the engine does (regex compiled up
    /// front; invalid regex → None, disabling just that rule).
    fn pe(from: &str, expr: &str) -> PreparedExtract {
        PreparedExtract {
            rule: ExtractRule { name: "x".into(), from: from.into(), expr: expr.into() },
            regex: if from == "regex" { regex::Regex::new(expr).ok() } else { None },
        }
    }

    #[test]
    fn extract_value_by_source() {
        let headers = vec![("X-Token".to_string(), "H1".to_string())];
        let body = r#"{"id":42,"name":"bob"}"#;
        assert_eq!(extract_value(&pe("json", "id"), Some(body), &[]), Some("42".to_string()));
        // header (case-insensitive)
        assert_eq!(extract_value(&pe("header", "x-token"), None, &headers), Some("H1".to_string()));
        // regex capture group 1
        assert_eq!(
            extract_value(&pe("regex", r#""name":"(\w+)""#), Some(body), &[]),
            Some("bob".to_string())
        );
    }

    #[test]
    fn extract_value_edge_cases() {
        let body = r#"{"data":{"deep":null},"n":7}"#;
        // missing json path → None (var stays unset, later steps keep the literal)
        assert_eq!(extract_value(&pe("json", "data.nope"), Some(body), &[]), None);
        // null json value → empty string (set but empty)
        assert_eq!(extract_value(&pe("json", "data.deep"), Some(body), &[]), Some(String::new()));
        // body absent → None
        assert_eq!(extract_value(&pe("json", "n"), None, &[]), None);
        // body not JSON → None
        assert_eq!(extract_value(&pe("json", "n"), Some("<html>"), &[]), None);
        // invalid regex → None (compiled to None up front), never a panic
        assert_eq!(extract_value(&pe("regex", "([unclosed"), Some(body), &[]), None);
        // regex with no capture group falls back to the whole match
        assert_eq!(extract_value(&pe("regex", r#"\d+"#), Some(body), &[]), Some("7".to_string()));
        // header missing → None
        assert_eq!(extract_value(&pe("header", "X-None"), None, &[]), None);
    }

    #[test]
    fn json_path_edge_cases() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"arr":[10,20],"num":3.5,"flag":true,"nul":null}"#).unwrap();
        // array index at root level
        assert_eq!(json_path(&v, "arr.1").map(json_scalar), Some("20".to_string()));
        assert_eq!(json_path(&v, "arr[0]").map(json_scalar), Some("10".to_string()));
        // scalars stringify without quotes
        assert_eq!(json_path(&v, "num").map(json_scalar), Some("3.5".to_string()));
        assert_eq!(json_path(&v, "flag").map(json_scalar), Some("true".to_string()));
        // null → empty string
        assert_eq!(json_path(&v, "nul").map(json_scalar), Some(String::new()));
        // out-of-range index → None
        assert!(json_path(&v, "arr.5").is_none());
    }

    // s4 regression: a numeric path segment must fall back to an object's
    // string key when it isn't a valid array index — {"2024": ...} used to be
    // unreachable because the array-index branch always won and bailed via `?`.
    #[test]
    fn json_path_numeric_segment_falls_back_to_object_key() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"2024":{"total":42},"data":[{"id":"A"},{"id":"B"}],"0":"root-zero"}"#,
        )
        .unwrap();
        // numeric-looking object key, nested
        assert_eq!(json_path(&v, "2024.total").map(json_scalar), Some("42".to_string()));
        // numeric-looking object key at the root
        assert_eq!(json_path(&v, "0").map(json_scalar), Some("root-zero".to_string()));
        // real array indices still resolve as indices, not as string keys
        assert_eq!(json_path(&v, "data.0.id").map(json_scalar), Some("A".to_string()));
        assert_eq!(json_path(&v, "data[1].id").map(json_scalar), Some("B".to_string()));
    }

    // s2 regression: `from` is normalized (trim/lowercase) and restricted to
    // header/regex/json — a typo must be rejected, not silently treated as json.
    #[test]
    fn extract_from_normalizes_case_and_whitespace() {
        assert_eq!(normalize_extract_from(" JSON ").unwrap(), "json");
        assert_eq!(normalize_extract_from("Header").unwrap(), "header");
        assert_eq!(normalize_extract_from("REGEX").unwrap(), "regex");
    }

    #[test]
    fn extract_from_rejects_unknown_source() {
        let err = normalize_extract_from("xml").unwrap_err();
        assert!(err.contains("xml"), "{err}");
        assert!(normalize_extract_from("").is_err());
    }

    #[tokio::test]
    async fn run_streams_rejects_unknown_extract_from_up_front() {
        let bad_step = StreamStep {
            name: "s".into(),
            method: "GET".into(),
            url: "http://127.0.0.1:1/x".into(),
            headers: vec![],
            body: None,
            tls: None,
            multipart: None,
            extract: vec![ExtractRule { name: "v".into(), from: "xml".into(), expr: "a".into() }],
        };
        let spec = StreamScenarioSpec {
            duration_secs: 1,
            timeout_ms: 500,
            streams: vec![StreamSpec { name: "s".into(), rps: 1, steps: vec![bad_step] }],
            datasets: vec![],
            file_pools: vec![],
        };
        // Must fail at build time — before any dispatch against the (bogus)
        // target — so this returns immediately instead of running for 1s.
        // (StreamsResult isn't Debug, so match manually instead of unwrap_err.)
        let result = run_streams(
            spec,
            CancellationToken::new(),
            Arc::new(|_: &StreamsProgress| {}),
            Arc::new(|_: String| {}),
        )
        .await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected run_streams to reject an unknown extract `from`"),
        };
        assert!(err.contains("xml"), "{err}");
    }
}
