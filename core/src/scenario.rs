// Multi-endpoint load engine: each selected handler is driven at its own target
// RPS, concurrently, using an open-model dispatcher. UI-agnostic — progress and
// token-refresh notifications are delivered through callbacks.
use crate::dynval::DynState;
use crate::histogram::finalize_result;
use crate::multipart::{form_from_prepared, prepare_parts, PreparedPart};
use crate::oauth::start_token_refresher;
use crate::types::*;
use hdrhistogram::Histogram;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

/// (target index, latency µs, HTTP status; 0 = network error)
type Sample = (usize, u64, u16);

pub type ProgressFn = Arc<dyn Fn(&ScenarioProgress) + Send + Sync>;
pub type RefreshFn = Arc<dyn Fn(u64) + Send + Sync>;
/// Sink for step-by-step engine logs (already secret-redacted). The app writes
/// these to its log file; the CLI to stderr / --log-file.
pub type LogFn = Arc<dyn Fn(String) + Send + Sync>;

/// A [`LogFn`] that discards everything — for callers/tests that don't log.
pub fn no_log() -> LogFn {
    Arc::new(|_| {})
}

/// Run a multi-endpoint scenario to completion (or until `cancel` fires) and
/// return the aggregated result. Validates and fetches initial tokens up front,
/// so a bad config fails fast. Every significant step is reported through
/// `on_log` (secrets redacted) so both the app and the CLI get a full trace.
pub async fn run_scenario(
    mut spec: ScenarioSpec,
    cancel: CancellationToken,
    on_progress: ProgressFn,
    on_token_refresh: RefreshFn,
    on_log: LogFn,
) -> Result<ScenarioResult, String> {
    // Log helper + a version that also returns the error it just logged.
    let log = |m: String| on_log(m);
    let log_err = |m: String| {
        on_log(format!("✗ {m}"));
        m
    };

    spec.duration_secs = spec.duration_secs.clamp(1, 86_400);
    spec.targets.retain(|t| t.rps > 0 && !t.url.trim().is_empty());
    if spec.targets.is_empty() {
        return Err(log_err("Не выбрано ни одной ручки с RPS > 0".to_string()));
    }
    if spec.targets.len() > 100 {
        return Err(log_err("Слишком много ручек (максимум 100)".to_string()));
    }

    log(format!(
        "старт сценария: {} ручек, {}с, таймаут {}мс, датасетов={}, наборов файлов={}",
        spec.targets.len(),
        spec.duration_secs,
        spec.timeout_ms,
        spec.datasets.len(),
        spec.file_pools.len()
    ));
    for t in &spec.targets {
        log(format!(
            "  ручка «{}»: {} {} @ {} rps{}{}{}",
            t.name,
            t.method,
            crate::redact::safe_url(&t.url),
            t.rps,
            if t.tls.is_some() { " +TLS" } else { "" },
            if t.auth_refresh.is_some() { " +OAuth" } else { "" },
            if t.multipart.is_some() { " +multipart" } else { "" },
        ));
    }

    let timeout = Duration::from_millis(spec.timeout_ms.max(100));
    let mut clients = Vec::with_capacity(spec.targets.len());
    for t in &mut spec.targets {
        if reqwest::Method::from_bytes(t.method.as_bytes()).is_err() {
            return Err(log_err(format!("Неверный метод у «{}»: {}", t.name, t.method)));
        }
        t.url = t.url.trim().to_string();
        // Templated URLs ({{$...}}) are expanded per request, so only validate literals.
        if !t.url.contains("{{") && reqwest::Url::parse(&t.url).is_err() {
            return Err(log_err(format!("Неверный URL у «{}»", t.name)));
        }
        t.headers.retain(|(k, v)| {
            reqwest::header::HeaderName::from_bytes(k.trim().as_bytes()).is_ok()
                && reqwest::header::HeaderValue::from_str(v).is_ok()
        });
        t.rps = t.rps.clamp(1, 100_000);
        let mut builder = reqwest::Client::builder()
            .timeout(timeout)
            .pool_max_idle_per_host((t.rps as usize).clamp(8, 512))
            .user_agent("Maelstrom-Scenario/0.1");
        builder = crate::tls::apply_tls(builder, &t.tls).map_err(&log_err)?;
        clients.push(builder.build().map_err(|e| log_err(e.to_string()))?);
    }

    // Load data providers (datasets + file pools) once, up front (fail fast).
    if !spec.datasets.is_empty() || !spec.file_pools.is_empty() {
        log("загрузка провайдеров данных…".to_string());
        for d in &spec.datasets {
            log(format!("  датасет «{}» ({}, {})", d.name, d.source.kind, mode_label(&d.mode)));
        }
        for p in &spec.file_pools {
            log(format!(
                "  набор файлов «{}» ({}, {})",
                p.name,
                p.source.kind,
                mode_label(&p.mode)
            ));
        }
    }
    let dyn_state = Arc::new(
        crate::dynval::resolve(&spec.datasets, &spec.file_pools)
            .await
            .map_err(&log_err)?,
    );
    if !spec.datasets.is_empty() || !spec.file_pools.is_empty() {
        log("провайдеры данных готовы".to_string());
    }

    // Read any multipart file parts into memory once, up front (fail fast).
    let mut prepared_multipart: Vec<Option<Arc<Vec<PreparedPart>>>> =
        Vec::with_capacity(spec.targets.len());
    for t in &spec.targets {
        match &t.multipart {
            Some(parts) if parts.iter().any(|p| p.enabled && !p.name.trim().is_empty()) => {
                let prepared =
                    prepare_parts(parts).map_err(|e| log_err(format!("«{}»: {e}", t.name)))?;
                prepared_multipart.push(Some(Arc::new(prepared)));
            }
            _ => prepared_multipart.push(None),
        }
    }

    // Per-target auto-refreshing OAuth token (initial fetch now, fail fast).
    let mut live_tokens: Vec<Option<Arc<RwLock<String>>>> = Vec::with_capacity(spec.targets.len());
    for t in &spec.targets {
        if let Some(cfg) = t.auth_refresh.clone() {
            log(format!(
                "получение OAuth-токена для «{}» ({})…",
                t.name,
                crate::redact::safe_url(&cfg.token_url)
            ));
            let cell = start_token_refresher(cfg, cancel.clone(), on_token_refresh.clone())
                .await
                .map_err(|e| {
                    // Refreshers already started for earlier targets share this
                    // token — cancel them so none keeps re-POSTing credentials.
                    cancel.cancel();
                    log_err(format!("Токен для «{}»: {e}", t.name))
                })?;
            log(format!("токен для «{}» получен", t.name));
            live_tokens.push(Some(cell));
        } else {
            live_tokens.push(None);
        }
    }

    let started_wall = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(spec.duration_secs);
    let targets = Arc::new(spec.targets.clone());
    let clients = Arc::new(clients);

    log(format!(
        "старт нагрузки: {} ручек на {}с",
        targets.len(),
        spec.duration_secs
    ));

    // Per-target counter of requests the scheduler couldn't place (target slower
    // than its configured RPS). Surfaced in the result so low throughput isn't
    // silently indistinguishable from a low target.
    let dropped: Vec<Arc<AtomicU64>> =
        (0..targets.len()).map(|_| Arc::new(AtomicU64::new(0))).collect();

    let (tx, rx) = mpsc::unbounded_channel::<Sample>();
    for (i, target) in targets.iter().enumerate() {
        let client = clients[i].clone();
        let target = target.clone();
        let token = cancel.clone();
        let tx = tx.clone();
        let live = live_tokens[i].clone();
        let mp = prepared_multipart[i].clone();
        let dyn_state = dyn_state.clone();
        let dropped = dropped[i].clone();
        tokio::spawn(dispatcher(i, client, target, deadline, token, tx, live, mp, dyn_state, dropped));
    }
    drop(tx);

    let mut result = aggregate(rx, &targets, spec.duration_secs, started, &cancel, on_progress).await;
    // Stop the background token refreshers.
    cancel.cancel();
    result.started_at = started_wall;

    // Attach the scheduler-shortfall counts (per target + overall).
    let mut total_dropped = 0u64;
    for (i, d) in dropped.iter().enumerate() {
        let n = d.load(Ordering::Relaxed);
        total_dropped += n;
        if let Some(t) = result.targets.get_mut(i) {
            t.dropped = n;
        }
    }
    result.overall.dropped = total_dropped;
    if total_dropped > 0 {
        log(format!(
            "⚠ недодано {total_dropped} запросов — целевой RPS не достигнут (ручка(и) не успевают)"
        ));
    }
    log(format!(
        "нагрузка завершена: всего={} ошибок={} ({:.2}%) rps(средн.)={:.0}{}",
        result.overall.total_requests,
        result.overall.errors,
        result.overall.error_rate,
        result.overall.rps_avg,
        if cancel.is_cancelled() { " (остановлено)" } else { "" }
    ));
    Ok(result)
}

fn mode_label(mode: &str) -> &str {
    if mode == "random" {
        "случайно"
    } else {
        "по кругу"
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatcher(
    idx: usize,
    client: reqwest::Client,
    target: ScenarioTarget,
    deadline: tokio::time::Instant,
    token: CancellationToken,
    tx: mpsc::UnboundedSender<Sample>,
    live_token: Option<Arc<RwLock<String>>>,
    multipart: Option<Arc<Vec<PreparedPart>>>,
    dyn_state: Arc<DynState>,
    dropped: Arc<AtomicU64>,
) {
    let per_tick = target.rps as f64 / 20.0;
    let cap = ((target.rps as usize) * 2).clamp(50, 20_000);
    let inflight = Arc::new(AtomicUsize::new(0));
    let target = Arc::new(target);
    let mut acc = 0.0_f64;
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = token.cancelled() => break,
            _ = tokio::time::sleep_until(deadline) => break,
            _ = interval.tick() => {}
        }
        acc += per_tick;
        let mut n = acc.floor() as usize;
        acc -= n as f64;
        while n > 0 {
            if inflight.load(Ordering::Relaxed) >= cap {
                // Target can't keep up: the remaining arrivals for this tick are
                // dropped (open model). Count them instead of silently discarding
                // so the reported RPS shortfall is attributable.
                dropped.fetch_add(n as u64, Ordering::Relaxed);
                break;
            }
            inflight.fetch_add(1, Ordering::Relaxed);
            let client = client.clone();
            let target = target.clone();
            let tx = tx.clone();
            let inflight = inflight.clone();
            let live_token = live_token.clone();
            let multipart = multipart.clone();
            let dyn_state = dyn_state.clone();
            tokio::spawn(async move {
                let start = Instant::now();
                let status = send_once(&client, &target, &live_token, &multipart, &dyn_state).await;
                let latency_us = start.elapsed().as_micros().max(1) as u64;
                inflight.fetch_sub(1, Ordering::Relaxed);
                let _ = tx.send((idx, latency_us, status));
            });
            n -= 1;
        }
    }
}

async fn send_once(
    client: &reqwest::Client,
    target: &ScenarioTarget,
    live_token: &Option<Arc<RwLock<String>>>,
    multipart: &Option<Arc<Vec<PreparedPart>>>,
    dyn_state: &DynState,
) -> u16 {
    // Per-request dynamic values ({{$randomInt}}, {{$data.*}}, …).
    let ctx = dyn_state.request();
    let method =
        reqwest::Method::from_bytes(target.method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut req = client.request(method, ctx.expand(&target.url));
    for (k, v) in &target.headers {
        if live_token.is_some() && k.trim().eq_ignore_ascii_case("authorization") {
            continue;
        }
        // reqwest sets the multipart Content-Type (with boundary) itself.
        if multipart.is_some() && k.trim().eq_ignore_ascii_case("content-type") {
            continue;
        }
        req = req.header(k.trim(), ctx.expand(v));
    }
    if let Some(cell) = live_token {
        let t = cell.read().await.clone();
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    if let Some(parts) = multipart {
        req = req.multipart(form_from_prepared(parts, &ctx));
    } else if let Some(b) = &target.body {
        if !b.is_empty() {
            req = req.body(ctx.expand(b));
        }
    }
    match req.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let _ = resp.bytes().await;
            status
        }
        Err(_) => 0,
    }
}

struct TargetAcc {
    hist: Histogram<u64>,
    sec_hist: Histogram<u64>,
    status_counts: HashMap<u16, u64>,
    total: u64,
    errors: u64,
    sum_us: u128,
    sec_requests: u64,
    sec_errors: u64,
    sec_sum_us: u128,
    timeline: Vec<TimelinePoint>,
}

impl TargetAcc {
    fn new() -> Self {
        Self {
            hist: Histogram::new(3).expect("hist"),
            sec_hist: Histogram::new(3).expect("hist"),
            status_counts: HashMap::new(),
            total: 0,
            errors: 0,
            sum_us: 0,
            sec_requests: 0,
            sec_errors: 0,
            sec_sum_us: 0,
            timeline: Vec::new(),
        }
    }

    fn record(&mut self, latency_us: u64, status: u16) {
        self.total += 1;
        self.sum_us += latency_us as u128;
        self.sec_requests += 1;
        self.sec_sum_us += latency_us as u128;
        let _ = self.hist.record(latency_us);
        let _ = self.sec_hist.record(latency_us);
        *self.status_counts.entry(status).or_insert(0) += 1;
        if status == 0 || status >= 400 {
            self.errors += 1;
            self.sec_errors += 1;
        }
    }

    fn tick(&mut self) {
        self.timeline.push(TimelinePoint {
            sec: self.timeline.len() as u64 + 1,
            requests: self.sec_requests,
            errors: self.sec_errors,
            avg_ms: if self.sec_requests > 0 {
                self.sec_sum_us as f64 / self.sec_requests as f64 / 1000.0
            } else {
                0.0
            },
            p50_ms: self.sec_hist.value_at_quantile(0.5) as f64 / 1000.0,
            p95_ms: self.sec_hist.value_at_quantile(0.95) as f64 / 1000.0,
            p99_ms: self.sec_hist.value_at_quantile(0.99) as f64 / 1000.0,
        });
        self.sec_requests = 0;
        self.sec_errors = 0;
        self.sec_sum_us = 0;
        self.sec_hist.reset();
    }
}

async fn aggregate(
    mut rx: mpsc::UnboundedReceiver<Sample>,
    targets: &[ScenarioTarget],
    duration_secs: u64,
    started: Instant,
    token: &CancellationToken,
    on_progress: ProgressFn,
) -> ScenarioResult {
    let mut accs: Vec<TargetAcc> = (0..targets.len()).map(|_| TargetAcc::new()).collect();
    let mut overall = TargetAcc::new();

    let mut ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            sample = rx.recv() => match sample {
                Some((idx, latency_us, status)) => {
                    if let Some(acc) = accs.get_mut(idx) {
                        acc.record(latency_us, status);
                    }
                    overall.record(latency_us, status);
                }
                None => break,
            },
            _ = ticker.tick() => {
                let progress = ScenarioProgress {
                    elapsed_secs: started.elapsed().as_secs_f64(),
                    overall_total: overall.total,
                    overall_errors: overall.errors,
                    overall_rps: overall.sec_requests as f64,
                    overall_p95_ms: overall.hist.value_at_quantile(0.95) as f64 / 1000.0,
                    targets: accs
                        .iter()
                        .enumerate()
                        .map(|(i, a)| TargetProgress {
                            name: targets[i].name.clone(),
                            rps_current: a.sec_requests as f64,
                            total: a.total,
                            errors: a.errors,
                        })
                        .collect(),
                };
                for a in accs.iter_mut() {
                    a.tick();
                }
                overall.tick();
                on_progress(&progress);
            }
        }
    }

    let actual_duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    let stopped_early = token.is_cancelled();

    let target_results: Vec<LoadTestResult> = accs
        .into_iter()
        .enumerate()
        .map(|(i, a)| {
            finalize_result(
                &a.hist,
                a.status_counts,
                a.total,
                a.errors,
                a.sum_us,
                a.timeline,
                RunMeta {
                    target: targets[i].url.clone(),
                    kind: targets[i].method.clone(),
                    vus: 0,
                    duration_secs,
                    rps_limit: Some(targets[i].rps),
                },
                actual_duration_ms,
                stopped_early,
            )
        })
        .collect();

    let overall_result = finalize_result(
        &overall.hist,
        overall.status_counts,
        overall.total,
        overall.errors,
        overall.sum_us,
        overall.timeline,
        RunMeta {
            target: format!("{} ручек", targets.len()),
            kind: "MIX".to_string(),
            vus: 0,
            duration_secs,
            rps_limit: None,
        },
        actual_duration_ms,
        stopped_early,
    );

    ScenarioResult {
        started_at: String::new(),
        duration_secs,
        actual_duration_ms,
        overall: overall_result,
        targets: target_results,
        stopped_early,
    }
}
