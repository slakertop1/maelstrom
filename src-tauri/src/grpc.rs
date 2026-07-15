//! Tauri commands for gRPC: introspect a `.proto`, make a single call, and run a
//! load test. The dynamic gRPC engine lives in the shared `maelstrom-grpc` crate.
use maelstrom_grpc::{grpc_load, MethodInfo, Proto};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, State};

use crate::loadtest::LoadTestState;

#[derive(Deserialize)]
pub struct ProtoRef {
    pub proto_path: String,
    #[serde(default)]
    pub includes: Vec<String>,
}

fn load_proto(r: &ProtoRef) -> Result<Proto, String> {
    Proto::from_file(&r.proto_path, &r.includes)
}

/// Parse a `.proto` and list its callable methods (for the service/method picker).
#[tauri::command]
pub fn grpc_list_methods(app: AppHandle, proto: ProtoRef) -> Result<Vec<MethodInfo>, String> {
    crate::log::write(&app, "GRPC", &format!("разбор {}", proto.proto_path));
    let p = load_proto(&proto)?;
    Ok(p.methods())
}

/// A JSON skeleton of a method's request message — prefilled in the editor.
#[tauri::command]
pub fn grpc_request_template(
    proto: ProtoRef,
    service: String,
    method: String,
) -> Result<String, String> {
    load_proto(&proto)?.request_template(&service, &method)
}

#[derive(Deserialize)]
pub struct GrpcCallSpec {
    pub endpoint: String,
    #[serde(flatten)]
    pub proto: ProtoRef,
    pub service: String,
    pub method: String,
    pub body: String,
    pub timeout_ms: Option<u64>,
    /// Custom CA / mTLS client identity for an `https://` endpoint — same
    /// shape the HTTP engine uses (see `http_client::HttpRequestSpec::tls`).
    /// Optional for backward compatibility: absent/`null` behaves exactly as
    /// before (native root CAs only, no client identity).
    #[serde(default)]
    pub tls: Option<crate::tls::TlsConfig>,
}

#[derive(Serialize)]
pub struct GrpcCallResult {
    pub responses: Vec<String>,
    pub server_streaming: bool,
    pub duration_ms: f64,
}

/// Make a single unary or server-streaming call and return the JSON response(s).
#[tauri::command]
pub async fn grpc_call(app: AppHandle, spec: GrpcCallSpec) -> Result<GrpcCallResult, String> {
    let proto = load_proto(&spec.proto)?;
    crate::log::write(
        &app,
        "GRPC →",
        &format!("{} {}/{}", crate::log::safe_url(&spec.endpoint), spec.service, spec.method),
    );
    let res = proto
        .call_json_with_tls(
            &spec.endpoint,
            &spec.service,
            &spec.method,
            &spec.body,
            spec.timeout_ms.unwrap_or(30_000),
            spec.tls.clone(),
        )
        .await;
    match res {
        Ok(r) => {
            crate::log::write(
                &app,
                "GRPC ←",
                &format!("{} сообщений за {:.0}мс", r.responses.len(), r.duration_ms),
            );
            Ok(GrpcCallResult {
                responses: r.responses,
                server_streaming: r.server_streaming,
                duration_ms: r.duration_ms,
            })
        }
        Err(e) => {
            crate::log::write(&app, "GRPC ✗", &format!("{}/{}: {e}", spec.service, spec.method));
            Err(e)
        }
    }
}

#[derive(Deserialize)]
pub struct GrpcLoadSpec {
    pub endpoint: String,
    #[serde(flatten)]
    pub proto: ProtoRef,
    pub service: String,
    pub method: String,
    pub body: String,
    pub vus: usize,
    pub duration_secs: u64,
    pub rps_limit: Option<u32>,
    pub timeout_ms: u64,
    /// Same TLS config as [`GrpcCallSpec::tls`] — optional, `None` behaves
    /// exactly as before.
    #[serde(default)]
    pub tls: Option<crate::tls::TlsConfig>,
}

/// Run a gRPC load test using the shared load-test slot; emits `load_finished`.
#[tauri::command]
pub async fn grpc_start_load(
    app: AppHandle,
    state: State<'_, LoadTestState>,
    spec: GrpcLoadSpec,
) -> Result<(), String> {
    let proto = load_proto(&spec.proto)?;
    // Build the call up front so a bad proto/body/method fails fast (before the slot).
    let call = proto.build_call_with_tls(
        &spec.endpoint,
        &spec.service,
        &spec.method,
        &spec.body,
        spec.timeout_ms,
        spec.tls.clone(),
    )?;

    let (token, running) = state.try_start()?;
    crate::log::write(
        &app,
        "GRPC LOAD ▶",
        &format!(
            "{} {}/{} | VUs={} {}с | rps_limit={}",
            crate::log::safe_url(&spec.endpoint),
            spec.service,
            spec.method,
            spec.vus,
            spec.duration_secs,
            spec.rps_limit.map(|r| r.to_string()).unwrap_or_else(|| "∞".into())
        ),
    );

    tauri::async_runtime::spawn(async move {
        let result = grpc_load(call, spec.vus, spec.duration_secs, spec.rps_limit, token.clone()).await;
        match result {
            Ok(mut r) => {
                r.started_at = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                crate::log::write(
                    &app,
                    "GRPC LOAD ■",
                    &format!(
                        "запросов={} ошибок={} ({:.2}%) rps={:.0} p95={:.0}мс",
                        r.total_requests, r.errors, r.error_rate, r.rps_avg, r.p95_ms
                    ),
                );
                let _ = app.emit("load_finished", &r);
            }
            Err(e) => {
                crate::log::write(&app, "GRPC LOAD ✗", &e);
                let _ = app.emit("load_error", &e);
            }
        }
        running.store(false, Ordering::SeqCst);
    });

    Ok(())
}
