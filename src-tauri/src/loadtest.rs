use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct LoadTestState {
    running: Arc<AtomicBool>,
    cancel: Mutex<Option<CancellationToken>>,
}

impl LoadTestState {
    /// Reserve the single load-test slot. Returns a fresh cancellation token
    /// and a guard handle that must be used to release the slot.
    pub fn try_start(&self) -> Result<(CancellationToken, Arc<AtomicBool>), String> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err("Нагрузочный тест уже выполняется".to_string());
        }
        let token = CancellationToken::new();
        *self.cancel.lock().unwrap() = Some(token.clone());
        Ok((token, self.running.clone()))
    }
}

// Result types and helpers live in the shared engine crate.
pub use maelstrom_core::histogram::build_histogram;
pub use maelstrom_core::types::{LoadTestResult, RunMeta, TimelinePoint};

#[derive(Deserialize, Clone)]
pub struct LoadTestSpec {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub vus: usize,
    pub duration_secs: u64,
    pub rps_limit: Option<u32>,
    pub timeout_ms: u64,
    #[serde(default)]
    pub tls: Option<crate::tls::TlsConfig>,
    #[serde(default)]
    pub auth_refresh: Option<crate::oauth::OAuthTokenRequest>,
    #[serde(default)]
    pub multipart: Option<Vec<maelstrom_core::types::MultipartPart>>,
    #[serde(default)]
    pub datasets: Vec<maelstrom_core::types::DatasetSpec>,
    #[serde(default)]
    pub file_pools: Vec<maelstrom_core::types::FilePoolSpec>,
}

#[derive(Serialize, Clone)]
pub struct ProgressSnapshot {
    pub elapsed_secs: f64,
    pub total_requests: u64,
    pub errors: u64,
    pub rps_current: f64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    pub point: TimelinePoint,
}

/// A single sample: latency in microseconds, HTTP status (0 = network error).
pub(crate) type Sample = (u64, u16);

#[tauri::command]
pub async fn start_load_test(
    app: AppHandle,
    state: State<'_, LoadTestState>,
    spec: LoadTestSpec,
) -> Result<(), String> {
    // Validate everything up front so workers never panic.
    let mut spec = spec;
    if reqwest::Method::from_bytes(spec.method.as_bytes()).is_err() {
        return Err(format!("Неверный HTTP-метод: {}", spec.method));
    }
    spec.url = spec.url.trim().to_string();
    // Skip strict URL validation for templated URLs ({{$...}} is expanded per request).
    if !spec.url.contains("{{") && reqwest::Url::parse(&spec.url).is_err() {
        return Err("Неверный URL".to_string());
    }
    spec.headers.retain(|(k, v)| {
        reqwest::header::HeaderName::from_bytes(k.trim().as_bytes()).is_ok()
            && reqwest::header::HeaderValue::from_str(v).is_ok()
    });
    spec.vus = spec.vus.clamp(1, 10_000);
    spec.duration_secs = spec.duration_secs.clamp(1, 3600);

    // Resolve DB-backed datasets to inline rows (fail fast before the slot).
    spec.datasets = crate::db::resolve_db_datasets(&app, &spec.datasets).await?;

    let (token, running) = state.try_start()?;

    crate::log::write(
        &app,
        "LOAD ▶",
        &format!(
            "{} {} | VUs={} {}с | rps_limit={} | multipart={} | datasets={}",
            spec.method,
            crate::log::safe_url(&spec.url),
            spec.vus,
            spec.duration_secs,
            spec.rps_limit.map(|r| r.to_string()).unwrap_or_else(|| "∞".into()),
            spec.multipart.is_some(),
            spec.datasets.len()
        ),
    );

    // Open model (RPS set) reuses connections up to the concurrency cap, not VUs.
    let pool_size = spec.vus.max(spec.rps_limit.unwrap_or(0) as usize).min(2048);
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_millis(spec.timeout_ms.max(100)))
        .pool_max_idle_per_host(pool_size)
        .user_agent("Maelstrom-LoadTest/0.1");
    builder = crate::tls::apply_tls(builder, &spec.tls).map_err(|e| {
        running.store(false, Ordering::SeqCst);
        e
    })?;
    let client = builder.build().map_err(|e| {
        running.store(false, Ordering::SeqCst);
        e.to_string()
    })?;

    // Optional auto-refreshing OAuth token: fetched now (fail fast) and kept
    // fresh in the background for the whole run.
    let live_token = if let Some(cfg) = spec.auth_refresh.clone() {
        let app_emit = app.clone();
        let on_refresh: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |n| {
            let _ = app_emit.emit("token_refreshed", n);
            crate::log::write(&app_emit, "TOKEN", &format!("автообновление токена #{n}"));
        });
        crate::log::write(&app, "TOKEN", "получение токена для нагрузки…");
        match maelstrom_core::oauth::start_token_refresher(cfg, token.clone(), on_refresh).await {
            Ok(t) => {
                crate::log::write(&app, "TOKEN", "токен получен");
                Some(t)
            }
            Err(e) => {
                token.cancel();
                running.store(false, Ordering::SeqCst);
                return Err(format!("Не удалось получить токен: {e}"));
            }
        }
    } else {
        None
    };

    // Load data providers (datasets + file pools) once (fail fast).
    if !spec.datasets.is_empty() || !spec.file_pools.is_empty() {
        crate::log::write(
            &app,
            "DATA",
            &format!(
                "загрузка провайдеров: датасетов={}, наборов файлов={}",
                spec.datasets.len(),
                spec.file_pools.len()
            ),
        );
    }
    let dyn_state = match maelstrom_core::dynval::resolve(&spec.datasets, &spec.file_pools).await {
        Ok(d) => Arc::new(d),
        Err(e) => {
            // Stop the already-started token refresher too.
            token.cancel();
            running.store(false, Ordering::SeqCst);
            crate::log::write(&app, "DATA ✗", &e);
            return Err(e);
        }
    };

    // Read any multipart file parts into memory once (fail fast).
    let prepared_multipart = match &spec.multipart {
        Some(parts) if parts.iter().any(|p| p.enabled && !p.name.trim().is_empty()) => {
            match maelstrom_core::multipart::prepare_parts(parts) {
                Ok(p) => Some(Arc::new(p)),
                Err(e) => {
                    token.cancel();
                    running.store(false, Ordering::SeqCst);
                    return Err(e);
                }
            }
        }
        _ => None,
    };

    // Scheduler shortfall: target-rate arrivals that could not be launched.
    let dropped = Arc::new(AtomicU64::new(0));

    tauri::async_runtime::spawn(async move {
        let started_wall = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let started = Instant::now();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(spec.duration_secs);

        let (tx, rx) = mpsc::unbounded_channel::<Sample>();
        if let Some(rps) = spec.rps_limit.filter(|r| *r > 0) {
            // OPEN MODEL — the configured RPS is the arrival rate, fired on
            // schedule regardless of how slowly the target responds (like the
            // multi-endpoint scenario and the CLI). Concurrency grows as needed
            // up to a cap; VUs do NOT bound throughput here.
            tokio::spawn(open_dispatcher(
                client.clone(),
                spec.clone(),
                rps,
                deadline,
                token.clone(),
                tx.clone(),
                live_token.clone(),
                prepared_multipart.clone(),
                dyn_state.clone(),
                dropped.clone(),
            ));
        } else {
            // CLOSED MODEL — no target rate: the VUs hammer back-to-back, so
            // throughput is bounded by VUs × latency.
            for _ in 0..spec.vus {
                let client = client.clone();
                let spec = spec.clone();
                let token = token.clone();
                let tx = tx.clone();
                let live_token = live_token.clone();
                let mp = prepared_multipart.clone();
                let ds = dyn_state.clone();
                tokio::spawn(worker(client, spec, deadline, token, tx, live_token, mp, ds));
            }
        }
        drop(tx);

        let meta = RunMeta {
            target: spec.url.clone(),
            kind: spec.method.clone(),
            vus: spec.vus,
            duration_secs: spec.duration_secs,
            rps_limit: spec.rps_limit,
        };
        let mut result = aggregate(&app, rx, meta, started, started_wall, &token).await;
        // Stop the background token refresher.
        token.cancel();
        // Attach the rate-limiter shortfall (0 when unlimited or fully kept up).
        result.dropped = dropped.load(Ordering::Relaxed);
        crate::log::write(
            &app,
            "LOAD ■",
            &format!(
                "{} {} | запросов={} ошибок={} ({:.2}%) | rps={:.0} p95={:.0}мс p99={:.0}мс{}{}",
                result.method,
                crate::log::safe_url(&result.url),
                result.total_requests,
                result.errors,
                result.error_rate,
                result.rps_avg,
                result.p95_ms,
                result.p99_ms,
                if result.dropped > 0 {
                    format!(" | недодано={}", result.dropped)
                } else {
                    String::new()
                },
                if result.stopped_early { " (остановлен)" } else { "" }
            ),
        );
        let _ = app.emit("load_finished", &result);
        running.store(false, Ordering::SeqCst);
    });

    Ok(())
}

#[tauri::command]
pub fn stop_load_test(state: State<'_, LoadTestState>) {
    if let Some(token) = state.cancel.lock().unwrap().as_ref() {
        token.cancel();
    }
}

pub(crate) fn spawn_refill_task(
    sem: Arc<Semaphore>,
    rps: u32,
    token: CancellationToken,
    duration_secs: u64,
    dropped: Arc<AtomicU64>,
) {
    tauri::async_runtime::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(duration_secs);
        let per_tick = rps as f64 / 20.0;
        let mut acc = 0.0_f64;
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = interval.tick() => {}
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            acc += per_tick;
            let add = acc.floor() as usize;
            acc -= add as f64;
            // Cap outstanding permits at ~1 second of budget to bound bursts.
            let cap = (rps as usize).max(1);
            let room = cap.saturating_sub(sem.available_permits());
            let grant = add.min(room);
            if grant > 0 {
                sem.add_permits(grant);
            }
            // Budget that couldn't be granted (permits already at the cap because
            // the VUs aren't consuming fast enough) means the target rate isn't
            // being met — count it so the RPS shortfall is surfaced, not silent.
            let missed = add - grant;
            if missed > 0 {
                dropped.fetch_add(missed as u64, Ordering::Relaxed);
            }
        }
    });
}

/// Open-model dispatcher: fires `rps` requests per second on schedule (50 ms
/// ticks, fractional budget carried between ticks), spawning a task per request
/// — mirroring `core::scenario::dispatcher`. In-flight concurrency is capped at
/// 2×RPS (bounded memory on a stalled target); arrivals that cannot launch at
/// the cap are counted in `dropped` so the shortfall is visible, never silent.
#[allow(clippy::too_many_arguments)]
async fn open_dispatcher(
    client: reqwest::Client,
    spec: LoadTestSpec,
    rps: u32,
    deadline: tokio::time::Instant,
    token: CancellationToken,
    tx: mpsc::UnboundedSender<Sample>,
    live_token: Option<Arc<tokio::sync::RwLock<String>>>,
    multipart: Option<Arc<Vec<maelstrom_core::multipart::PreparedPart>>>,
    dyn_state: Arc<maelstrom_core::dynval::DynState>,
    dropped: Arc<AtomicU64>,
) {
    let per_tick = rps as f64 / 20.0;
    let cap = ((rps as usize) * 2).clamp(50, 20_000);
    let inflight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let spec = Arc::new(spec);
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
                dropped.fetch_add(n as u64, Ordering::Relaxed);
                break;
            }
            inflight.fetch_add(1, Ordering::Relaxed);
            let client = client.clone();
            let spec = spec.clone();
            let tx = tx.clone();
            let inflight = inflight.clone();
            let live_token = live_token.clone();
            let multipart = multipart.clone();
            let dyn_state = dyn_state.clone();
            tokio::spawn(async move {
                let start = Instant::now();
                let status = send_once(&client, &spec, &live_token, &multipart, &dyn_state).await;
                let latency_us = start.elapsed().as_micros().max(1) as u64;
                inflight.fetch_sub(1, Ordering::Relaxed);
                let _ = tx.send((latency_us, status));
            });
            n -= 1;
        }
    }
}

async fn worker(
    client: reqwest::Client,
    spec: LoadTestSpec,
    deadline: tokio::time::Instant,
    token: CancellationToken,
    tx: mpsc::UnboundedSender<Sample>,
    live_token: Option<Arc<tokio::sync::RwLock<String>>>,
    multipart: Option<Arc<Vec<maelstrom_core::multipart::PreparedPart>>>,
    dyn_state: Arc<maelstrom_core::dynval::DynState>,
) {
    loop {
        if token.is_cancelled() || tokio::time::Instant::now() >= deadline {
            break;
        }
        let start = Instant::now();
        let status = send_once(&client, &spec, &live_token, &multipart, &dyn_state).await;
        let latency_us = start.elapsed().as_micros().max(1) as u64;
        if tx.send((latency_us, status)).is_err() {
            break;
        }
        // Back off briefly on network errors so a dead target doesn't spin the CPU.
        if status == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

async fn send_once(
    client: &reqwest::Client,
    spec: &LoadTestSpec,
    live_token: &Option<Arc<tokio::sync::RwLock<String>>>,
    multipart: &Option<Arc<Vec<maelstrom_core::multipart::PreparedPart>>>,
    dyn_state: &maelstrom_core::dynval::DynState,
) -> u16 {
    let ctx = dyn_state.request();
    let method = reqwest::Method::from_bytes(spec.method.as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let mut req = client.request(method, ctx.expand(&spec.url));
    for (k, v) in &spec.headers {
        // When an auto-refreshing token is active, its live value replaces any
        // baked-in Authorization header.
        if live_token.is_some() && k.trim().eq_ignore_ascii_case("authorization") {
            continue;
        }
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
        req = req.multipart(maelstrom_core::multipart::form_from_prepared(parts, &ctx));
    } else if let Some(b) = &spec.body {
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

pub(crate) async fn aggregate(
    app: &AppHandle,
    mut rx: mpsc::UnboundedReceiver<Sample>,
    meta: RunMeta,
    started: Instant,
    started_wall: String,
    token: &CancellationToken,
) -> LoadTestResult {
    let mut hist = Histogram::<u64>::new(3).expect("histogram");
    let mut sec_hist = Histogram::<u64>::new(3).expect("histogram");
    let mut status_counts: HashMap<u16, u64> = HashMap::new();
    let mut timeline: Vec<TimelinePoint> = Vec::new();

    let mut total: u64 = 0;
    let mut errors: u64 = 0;
    let mut sum_us: u128 = 0;
    let mut sec_requests: u64 = 0;
    let mut sec_errors: u64 = 0;
    let mut sec_sum_us: u128 = 0;

    let mut ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            sample = rx.recv() => match sample {
                Some((latency_us, status)) => {
                    total += 1;
                    sum_us += latency_us as u128;
                    sec_requests += 1;
                    sec_sum_us += latency_us as u128;
                    let _ = hist.record(latency_us);
                    let _ = sec_hist.record(latency_us);
                    *status_counts.entry(status).or_insert(0) += 1;
                    if status == 0 || status >= 400 {
                        errors += 1;
                        sec_errors += 1;
                    }
                }
                None => break,
            },
            _ = ticker.tick() => {
                let point = TimelinePoint {
                    sec: timeline.len() as u64 + 1,
                    requests: sec_requests,
                    errors: sec_errors,
                    avg_ms: if sec_requests > 0 {
                        sec_sum_us as f64 / sec_requests as f64 / 1000.0
                    } else { 0.0 },
                    p50_ms: sec_hist.value_at_quantile(0.5) as f64 / 1000.0,
                    p95_ms: sec_hist.value_at_quantile(0.95) as f64 / 1000.0,
                    p99_ms: sec_hist.value_at_quantile(0.99) as f64 / 1000.0,
                };
                let snapshot = ProgressSnapshot {
                    elapsed_secs: started.elapsed().as_secs_f64(),
                    total_requests: total,
                    errors,
                    rps_current: sec_requests as f64,
                    avg_ms: if total > 0 { sum_us as f64 / total as f64 / 1000.0 } else { 0.0 },
                    p50_ms: hist.value_at_quantile(0.5) as f64 / 1000.0,
                    p95_ms: hist.value_at_quantile(0.95) as f64 / 1000.0,
                    p99_ms: hist.value_at_quantile(0.99) as f64 / 1000.0,
                    max_ms: hist.max() as f64 / 1000.0,
                    point: point.clone(),
                };
                timeline.push(point);
                let _ = app.emit("load_progress", &snapshot);
                sec_requests = 0;
                sec_errors = 0;
                sec_sum_us = 0;
                sec_hist.reset();
            }
        }
    }

    // Flush the final partial second.
    if sec_requests > 0 {
        timeline.push(TimelinePoint {
            sec: timeline.len() as u64 + 1,
            requests: sec_requests,
            errors: sec_errors,
            avg_ms: sec_sum_us as f64 / sec_requests as f64 / 1000.0,
            p50_ms: sec_hist.value_at_quantile(0.5) as f64 / 1000.0,
            p95_ms: sec_hist.value_at_quantile(0.95) as f64 / 1000.0,
            p99_ms: sec_hist.value_at_quantile(0.99) as f64 / 1000.0,
        });
    }

    let actual_duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    let is_db = meta.kind == "SQL";
    let mut counts: Vec<(String, u64)> = status_counts
        .into_iter()
        .map(|(status, count)| (maelstrom_core::histogram::status_label(status, is_db), count))
        .collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1));

    LoadTestResult {
        url: meta.target,
        method: meta.kind.clone(),
        vus: meta.vus,
        duration_secs: meta.duration_secs,
        rps_limit: meta.rps_limit,
        started_at: started_wall,
        actual_duration_ms,
        total_requests: total,
        errors,
        error_rate: if total > 0 { errors as f64 / total as f64 * 100.0 } else { 0.0 },
        rps_avg: if actual_duration_ms > 0.0 {
            total as f64 / (actual_duration_ms / 1000.0)
        } else {
            0.0
        },
        latency_min_ms: if total > 0 { hist.min() as f64 / 1000.0 } else { 0.0 },
        latency_max_ms: hist.max() as f64 / 1000.0,
        latency_avg_ms: if total > 0 { sum_us as f64 / total as f64 / 1000.0 } else { 0.0 },
        p50_ms: hist.value_at_quantile(0.5) as f64 / 1000.0,
        p75_ms: hist.value_at_quantile(0.75) as f64 / 1000.0,
        p90_ms: hist.value_at_quantile(0.9) as f64 / 1000.0,
        p95_ms: hist.value_at_quantile(0.95) as f64 / 1000.0,
        p99_ms: hist.value_at_quantile(0.99) as f64 / 1000.0,
        status_counts: counts,
        timeline,
        histogram: build_histogram(&hist),
        stopped_early: token.is_cancelled(),
        dropped: 0, // set by the caller from the rate limiter's shortfall counter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_slot_guard_rejects_a_concurrent_run() {
        let state = LoadTestState::default();
        let (_token, running) = state.try_start().expect("first start reserves the slot");
        // A second start while running must be rejected, not silently allowed.
        assert!(state.try_start().is_err(), "concurrent start must be rejected");
        // Releasing the slot lets a new run start.
        running.store(false, Ordering::SeqCst);
        assert!(state.try_start().is_ok(), "start after release must succeed");
    }

    /// Mock HTTP server that sleeps `delay_ms` before every 200 reply — slow on
    /// purpose, to prove throughput does not depend on response time.
    async fn spawn_slow_mock(delay_ms: u64) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        )
                        .await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        format!("http://{addr}/x")
    }

    // The user-visible contract of the RPS field: "I said 200 — fire 200/s",
    // even when every response takes 100 ms. A closed (VU-bound) model with
    // vus=1 would manage ~10 rps here; the open dispatcher must hit ~200.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn open_model_hits_target_rps_despite_slow_target() {
        let url = spawn_slow_mock(100).await;
        let spec = LoadTestSpec {
            method: "GET".into(),
            url,
            headers: vec![],
            body: None,
            vus: 1, // deliberately tiny: must NOT bound throughput in open model
            duration_secs: 2,
            rps_limit: Some(200),
            timeout_ms: 5000,
            tls: None,
            auth_refresh: None,
            multipart: None,
            datasets: vec![],
            file_pools: vec![],
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel::<Sample>();
        let dropped = Arc::new(AtomicU64::new(0));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        open_dispatcher(
            client,
            spec,
            200,
            deadline,
            CancellationToken::new(),
            tx,
            None,
            None,
            Arc::new(maelstrom_core::dynval::DynState::default()),
            dropped.clone(),
        )
        .await;

        // The dispatcher has returned (deadline); in-flight tasks finish and drop
        // their tx clones, closing the channel.
        let mut delivered = 0u64;
        let mut ok = 0u64;
        while let Some((_lat, status)) = rx.recv().await {
            delivered += 1;
            if status == 200 {
                ok += 1;
            }
        }
        // 2 s at 200 rps ≈ 400 arrivals. Generous CI tolerance, but far above
        // anything a VU-bound model could deliver at 100 ms latency.
        assert!(
            (300..=450).contains(&delivered),
            "expected ~400 dispatched requests, got {delivered}"
        );
        assert_eq!(delivered, ok, "all requests must succeed against the mock");
        assert_eq!(dropped.load(Ordering::Relaxed), 0, "no shortfall expected");
    }
}
