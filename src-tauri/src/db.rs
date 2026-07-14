//! Tauri commands for ad-hoc SQL and DB load testing. The SQL layer (native
//! drivers, value stringification, URL helpers, DB-backed datasets) lives in the
//! shared `maelstrom-db` crate so the CLI can resolve DB datasets too.
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;

use crate::loadtest::{aggregate, spawn_refill_task, LoadTestState, RunMeta, Sample};
use maelstrom_db::{build_db_url, is_query, mask_db_url, Db, DB_DATASET_MAX_ROWS};

/// Resolve any `db`-sourced datasets into inline rows before the engine runs.
/// Non-fatal notes (e.g. a truncated result set) are written to the app log so
/// the user sees them — there is no stderr in the GUI.
pub async fn resolve_db_datasets(
    app: &AppHandle,
    specs: &[maelstrom_core::types::DatasetSpec],
) -> Result<Vec<maelstrom_core::types::DatasetSpec>, String> {
    let (out, warnings) = maelstrom_db::resolve_db_datasets(specs, DB_DATASET_MAX_ROWS).await?;
    for w in warnings {
        crate::log::write(app, "DATA ⚠", &w);
    }
    Ok(out)
}

/// Collapse a SQL string to a single truncated line for logging.
fn one_line(sql: &str) -> String {
    let s: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 200 {
        format!("{}…", s.chars().take(200).collect::<String>())
    } else {
        s
    }
}

// ---------- single query ----------

#[derive(Deserialize, Clone)]
pub struct DbRequestSpec {
    pub url: String,
    pub query: String,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

#[derive(Serialize)]
pub struct DbResponse {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub row_count: u64,
    pub rows_affected: Option<u64>,
    pub truncated: bool,
    pub duration_ms: f64,
}

const MAX_PREVIEW_ROWS: usize = 200;

#[tauri::command]
pub async fn db_execute(app: AppHandle, spec: DbRequestSpec) -> Result<DbResponse, String> {
    let url = build_db_url(&spec.url, &spec.username, &spec.password);
    crate::log::write(&app, "DB →", &format!("{} | {}", mask_db_url(&url), one_line(&spec.query)));
    let timeout = Duration::from_millis(spec.timeout_ms.unwrap_or(30_000));
    let db = Db::connect(&url, 1, timeout)
        .await
        .map_err(|e| format!("Не удалось подключиться к БД: {e}"))?;

    let start = Instant::now();
    let result = if is_query(&spec.query) {
        let table = tokio::time::timeout(timeout, db.fetch(&spec.query, MAX_PREVIEW_ROWS))
            .await
            .map_err(|_| "Таймаут запроса".to_string())?
            .map_err(|e| format!("Ошибка запроса: {e}"))?;
        DbResponse {
            columns: table.columns,
            row_count: table.row_count,
            rows: table.rows,
            rows_affected: None,
            truncated: table.truncated,
            duration_ms: start.elapsed().as_secs_f64() * 1000.0,
        }
    } else {
        let affected = tokio::time::timeout(timeout, db.execute(&spec.query))
            .await
            .map_err(|_| "Таймаут запроса".to_string())?
            .map_err(|e| format!("Ошибка запроса: {e}"))?;
        DbResponse {
            columns: vec![],
            rows: vec![],
            row_count: 0,
            rows_affected: Some(affected),
            truncated: false,
            duration_ms: start.elapsed().as_secs_f64() * 1000.0,
        }
    };
    db.close().await;
    Ok(result)
}

// ---------- DB load test ----------

#[derive(Deserialize, Clone)]
pub struct DbLoadTestSpec {
    pub url: String,
    pub query: String,
    pub vus: usize,
    pub duration_secs: u64,
    pub rps_limit: Option<u32>,
    pub timeout_ms: u64,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

#[tauri::command]
pub async fn start_db_load_test(
    app: AppHandle,
    state: State<'_, LoadTestState>,
    spec: DbLoadTestSpec,
) -> Result<(), String> {
    let mut spec = spec;
    spec.url = build_db_url(&spec.url, &spec.username, &spec.password);
    spec.vus = spec.vus.clamp(1, 1_000);
    spec.duration_secs = spec.duration_secs.clamp(1, 3600);
    if spec.query.trim().is_empty() {
        return Err("Пустой SQL-запрос".to_string());
    }

    crate::log::write(
        &app,
        "DB LOAD ▶",
        &format!(
            "{} | VUs={} {}с | {}",
            mask_db_url(spec.url.trim()),
            spec.vus,
            spec.duration_secs,
            one_line(&spec.query)
        ),
    );

    let timeout = Duration::from_millis(spec.timeout_ms.max(100));
    // Connect before reserving the slot so a bad URL fails fast with a clear error.
    let db = Db::connect(&spec.url, spec.vus as u32, timeout)
        .await
        .map_err(|e| format!("Не удалось подключиться к БД: {e}"))?;

    let (token, running) = state.try_start()?;

    // Counts rate-limiter budget the VUs couldn't keep up with (RPS shortfall).
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let limiter = spec.rps_limit.filter(|r| *r > 0).map(|rps| {
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        spawn_refill_task(sem.clone(), rps, token.clone(), spec.duration_secs, dropped.clone());
        sem
    });

    tauri::async_runtime::spawn(async move {
        let started_wall = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let started = Instant::now();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(spec.duration_secs);

        let (tx, rx) = mpsc::unbounded_channel::<Sample>();
        for _ in 0..spec.vus {
            let db = db.clone();
            let spec = spec.clone();
            let token = token.clone();
            let tx = tx.clone();
            let limiter = limiter.clone();
            tokio::spawn(db_worker(db, spec, deadline, token, tx, limiter, timeout));
        }
        drop(tx);

        let meta = RunMeta {
            target: mask_db_url(spec.url.trim()),
            kind: "SQL".to_string(),
            vus: spec.vus,
            duration_secs: spec.duration_secs,
            rps_limit: spec.rps_limit,
        };
        let mut result = aggregate(&app, rx, meta, started, started_wall, &token).await;
        db.close().await;
        result.dropped = dropped.load(std::sync::atomic::Ordering::Relaxed);
        let _ = app.emit("load_finished", &result);
        running.store(false, std::sync::atomic::Ordering::SeqCst);
    });

    Ok(())
}

async fn db_worker(
    db: Db,
    spec: DbLoadTestSpec,
    deadline: tokio::time::Instant,
    token: tokio_util::sync::CancellationToken,
    tx: mpsc::UnboundedSender<Sample>,
    limiter: Option<std::sync::Arc<tokio::sync::Semaphore>>,
    timeout: Duration,
) {
    let query_is_select = is_query(&spec.query);
    loop {
        if token.is_cancelled() || tokio::time::Instant::now() >= deadline {
            break;
        }
        if let Some(sem) = &limiter {
            tokio::select! {
                biased;
                _ = token.cancelled() => break,
                _ = tokio::time::sleep_until(deadline) => break,
                permit = sem.acquire() => match permit {
                    Ok(p) => p.forget(),
                    Err(_) => break,
                },
            }
        }
        let start = Instant::now();
        let ok = matches!(
            tokio::time::timeout(timeout, db.run_ok(&spec.query, query_is_select)).await,
            Ok(true)
        );
        let latency_us = start.elapsed().as_micros().max(1) as u64;
        let status: u16 = if ok { 200 } else { 0 };
        if tx.send((latency_us, status)).is_err() {
            break;
        }
        if status == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}
