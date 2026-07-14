//! Tauri commands for WebSocket: a single connect/send/receive, and a load test.
//! The WS client + load runner live in the shared `maelstrom-core` crate.
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, State};

use crate::loadtest::LoadTestState;

#[derive(Deserialize)]
pub struct WsCallSpec {
    pub url: String,
    #[serde(default)]
    pub message: String,
    pub timeout_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct WsCallResult {
    pub messages: Vec<String>,
    pub duration_ms: f64,
}

/// Connect, send one message, and collect replies until the socket goes idle.
#[tauri::command]
pub async fn ws_call(app: AppHandle, spec: WsCallSpec) -> Result<WsCallResult, String> {
    crate::log::write(&app, "WS →", &crate::log::safe_url(&spec.url));
    let res = maelstrom_core::ws::ws_call(
        &spec.url,
        &spec.message,
        spec.timeout_ms.unwrap_or(5000),
        1000,
    )
    .await;
    match res {
        Ok(r) => {
            crate::log::write(&app, "WS ←", &format!("{} сообщений", r.messages.len()));
            Ok(WsCallResult { messages: r.messages, duration_ms: r.duration_ms })
        }
        Err(e) => {
            crate::log::write(&app, "WS ✗", &e);
            Err(e)
        }
    }
}

#[derive(Deserialize)]
pub struct WsLoadSpec {
    pub url: String,
    #[serde(default)]
    pub message: String,
    pub vus: usize,
    pub duration_secs: u64,
    pub rps_limit: Option<u32>,
    pub timeout_ms: u64,
}

/// Run a WebSocket load test using the shared load-test slot; emits `load_finished`.
#[tauri::command]
pub async fn ws_start_load(
    app: AppHandle,
    state: State<'_, LoadTestState>,
    spec: WsLoadSpec,
) -> Result<(), String> {
    let (token, running) = state.try_start()?;
    crate::log::write(
        &app,
        "WS LOAD ▶",
        &format!(
            "{} | VUs={} {}с | rps_limit={}",
            crate::log::safe_url(&spec.url),
            spec.vus,
            spec.duration_secs,
            spec.rps_limit.map(|r| r.to_string()).unwrap_or_else(|| "∞".into())
        ),
    );

    tauri::async_runtime::spawn(async move {
        let result = maelstrom_core::ws::ws_load(
            &spec.url,
            &spec.message,
            spec.vus,
            spec.duration_secs,
            spec.rps_limit,
            spec.timeout_ms,
            token.clone(),
        )
        .await;
        match result {
            Ok(r) => {
                crate::log::write(
                    &app,
                    "WS LOAD ■",
                    &format!(
                        "запросов={} ошибок={} ({:.2}%) rps={:.0} p95={:.0}мс",
                        r.total_requests, r.errors, r.error_rate, r.rps_avg, r.p95_ms
                    ),
                );
                let _ = app.emit("load_finished", &r);
            }
            Err(e) => {
                crate::log::write(&app, "WS LOAD ✗", &e);
                let _ = app.emit("load_error", &e);
            }
        }
        running.store(false, Ordering::SeqCst);
    });

    Ok(())
}
