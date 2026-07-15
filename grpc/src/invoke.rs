//! Calling dynamic gRPC methods: unary and server-streaming, JSON in/out, plus a
//! load runner that reuses the shared histogram/result types.

use crate::codec::DynCodec;
use crate::Proto;
use http::uri::PathAndQuery;
use maelstrom_core::histogram::finalize_result;
use maelstrom_core::types::{LoadTestResult, RunMeta, TimelinePoint};
use prost_reflect::{DynamicMessage, MessageDescriptor, SerializeOptions};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Semaphore};
use tokio_util::sync::CancellationToken;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};

fn status_msg(s: &tonic::Status) -> String {
    format!("{:?}: {}", s.code(), s.message())
}

/// A failed call: a human-readable message (for logs/UI) plus the real gRPC
/// status code when the failure came from a `tonic::Status`. Non-gRPC
/// failures (connect errors, bad paths, local validation) carry no code.
/// `grpc_load` uses `code` to bucket load-test results by actual status
/// instead of a collapsed success/failure bool.
struct CallError {
    message: String,
    code: Option<tonic::Code>,
}

impl CallError {
    fn from_status(s: &tonic::Status) -> Self {
        CallError { message: status_msg(s), code: Some(s.code()) }
    }

    fn other(message: String) -> Self {
        CallError { message, code: None }
    }
}

impl From<CallError> for String {
    fn from(e: CallError) -> String {
        e.message
    }
}

/// Parse proto3 JSON into a message of the given type.
pub fn json_to_message(desc: &MessageDescriptor, json: &str) -> Result<DynamicMessage, String> {
    let json = if json.trim().is_empty() { "{}" } else { json };
    let mut de = serde_json::Deserializer::from_str(json);
    let msg = DynamicMessage::deserialize(desc.clone(), &mut de)
        .map_err(|e| format!("Тело запроса не подходит под {}: {e}", desc.full_name()))?;
    de.end().map_err(|e| format!("Лишние данные в JSON: {e}"))?;
    Ok(msg)
}

/// Parse one message, or a JSON array of messages (for client-/bidi-streaming).
pub fn json_to_messages(desc: &MessageDescriptor, json: &str) -> Result<Vec<DynamicMessage>, String> {
    let trimmed = json.trim();
    if trimmed.starts_with('[') {
        let items: Vec<serde_json::Value> =
            serde_json::from_str(trimmed).map_err(|e| format!("Ожидался JSON-массив сообщений: {e}"))?;
        items
            .iter()
            .map(|v| json_to_message(desc, &v.to_string()))
            .collect()
    } else {
        Ok(vec![json_to_message(desc, json)?])
    }
}

/// Serialize a message to proto3 JSON (default field names, includes defaults).
pub fn message_to_json(msg: &DynamicMessage) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::pretty(&mut buf);
    let opts = SerializeOptions::new().skip_default_fields(false);
    msg.serialize_with_options(&mut ser, &opts)
        .map_err(|e| format!("сериализация ответа: {e}"))?;
    String::from_utf8(buf).map_err(|e| e.to_string())
}

async fn connect(endpoint: &str, timeout: Duration) -> Result<Channel, String> {
    let mut ep = Endpoint::from_shared(endpoint.to_string())
        .map_err(|e| format!("Неверный адрес «{endpoint}»: {e}"))?
        .timeout(timeout)
        .connect_timeout(timeout);
    if endpoint.trim_start().starts_with("https") {
        ep = ep
            .tls_config(ClientTlsConfig::new().with_native_roots())
            .map_err(|e| format!("TLS: {e}"))?;
    }
    ep.connect().await.map_err(|e| format!("Подключение к {endpoint}: {e}"))
}

async fn do_unary(
    channel: Channel,
    path: &str,
    input: DynamicMessage,
    output: MessageDescriptor,
) -> Result<DynamicMessage, CallError> {
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready().await.map_err(|e| CallError::other(format!("сервис не готов: {e}")))?;
    let path = PathAndQuery::from_str(path).map_err(|e| CallError::other(format!("путь метода: {e}")))?;
    let resp = grpc
        .unary(tonic::Request::new(input), path, DynCodec { output })
        .await
        .map_err(|e| CallError::from_status(&e))?;
    Ok(resp.into_inner())
}

async fn do_server_streaming(
    channel: Channel,
    path: &str,
    input: DynamicMessage,
    output: MessageDescriptor,
    max: usize,
) -> Result<Vec<DynamicMessage>, CallError> {
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready().await.map_err(|e| CallError::other(format!("сервис не готов: {e}")))?;
    let path = PathAndQuery::from_str(path).map_err(|e| CallError::other(format!("путь метода: {e}")))?;
    let mut stream = grpc
        .server_streaming(tonic::Request::new(input), path, DynCodec { output })
        .await
        .map_err(|e| CallError::from_status(&e))?
        .into_inner();
    let mut out = Vec::new();
    while let Some(msg) = stream.message().await.map_err(|e| CallError::from_status(&e))? {
        out.push(msg);
        if out.len() >= max {
            break;
        }
    }
    Ok(out)
}

async fn do_client_streaming(
    channel: Channel,
    path: &str,
    inputs: Vec<DynamicMessage>,
    output: MessageDescriptor,
) -> Result<DynamicMessage, CallError> {
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready().await.map_err(|e| CallError::other(format!("сервис не готов: {e}")))?;
    let path = PathAndQuery::from_str(path).map_err(|e| CallError::other(format!("путь метода: {e}")))?;
    let req = tonic::Request::new(futures_util::stream::iter(inputs));
    let resp = grpc
        .client_streaming(req, path, DynCodec { output })
        .await
        .map_err(|e| CallError::from_status(&e))?;
    Ok(resp.into_inner())
}

async fn do_bidi_streaming(
    channel: Channel,
    path: &str,
    inputs: Vec<DynamicMessage>,
    output: MessageDescriptor,
    max: usize,
) -> Result<Vec<DynamicMessage>, CallError> {
    let mut grpc = tonic::client::Grpc::new(channel);
    grpc.ready().await.map_err(|e| CallError::other(format!("сервис не готов: {e}")))?;
    let path = PathAndQuery::from_str(path).map_err(|e| CallError::other(format!("путь метода: {e}")))?;
    let req = tonic::Request::new(futures_util::stream::iter(inputs));
    let mut stream = grpc
        .streaming(req, path, DynCodec { output })
        .await
        .map_err(|e| CallError::from_status(&e))?
        .into_inner();
    let mut out = Vec::new();
    while let Some(msg) = stream.message().await.map_err(|e| CallError::from_status(&e))? {
        out.push(msg);
        if out.len() >= max {
            break;
        }
    }
    Ok(out)
}

/// Dispatch a call by its streaming kind, returning all response messages.
async fn execute_call(
    channel: Channel,
    call: &LoadCall,
    max: usize,
) -> Result<Vec<DynamicMessage>, CallError> {
    let path = &call.path;
    let out = call.output.clone();
    match (call.client_streaming, call.server_streaming) {
        (false, false) => {
            let input = call
                .inputs
                .first()
                .cloned()
                .ok_or_else(|| CallError::other("Пустое тело запроса".to_string()))?;
            Ok(vec![do_unary(channel, path, input, out).await?])
        }
        (false, true) => {
            let input = call
                .inputs
                .first()
                .cloned()
                .ok_or_else(|| CallError::other("Пустое тело запроса".to_string()))?;
            do_server_streaming(channel, path, input, out, max).await
        }
        (true, false) => Ok(vec![
            do_client_streaming(channel, path, call.inputs.clone(), out).await?,
        ]),
        (true, true) => do_bidi_streaming(channel, path, call.inputs.clone(), out, max).await,
    }
}

/// One resolved, ready-to-send call (messages built once, replayed under load).
/// `inputs` holds one message for unary/server-streaming, or several for
/// client-/bidi-streaming (body given as a JSON array).
#[derive(Clone)]
pub struct LoadCall {
    pub endpoint: String,
    pub path: String,
    pub inputs: Vec<DynamicMessage>,
    pub output: MessageDescriptor,
    pub client_streaming: bool,
    pub server_streaming: bool,
    pub timeout_ms: u64,
}

/// Result of a single (unary or streaming) JSON call.
#[derive(Debug)]
pub struct CallResult {
    pub responses: Vec<String>,
    pub server_streaming: bool,
    pub duration_ms: f64,
}

impl Proto {
    /// Build a reusable [`LoadCall`] from JSON body for the given method.
    pub fn build_call(
        &self,
        endpoint: &str,
        service: &str,
        method: &str,
        json_body: &str,
        timeout_ms: u64,
    ) -> Result<LoadCall, String> {
        let m = self.find_method(service, method)?;
        let inputs = json_to_messages(&m.input(), json_body)?;
        if m.is_client_streaming() && inputs.is_empty() {
            return Err("Для клиентского стриминга задайте JSON-массив сообщений".to_string());
        }
        if !m.is_client_streaming() && inputs.len() > 1 {
            return Err(format!(
                "Метод «{method}» не клиентский стриминг — ожидается одно сообщение, а не массив из {}",
                inputs.len()
            ));
        }
        Ok(LoadCall {
            endpoint: endpoint.to_string(),
            path: format!("/{}/{}", m.parent_service().full_name(), m.name()),
            inputs,
            output: m.output(),
            client_streaming: m.is_client_streaming(),
            server_streaming: m.is_server_streaming(),
            timeout_ms,
        })
    }

    /// A JSON skeleton of a method's request message (all fields at defaults) —
    /// shown in the UI so the user sees exactly what to fill in. For client-/bidi-
    /// streaming methods the skeleton is a one-element array of messages.
    pub fn request_template(&self, service: &str, method: &str) -> Result<String, String> {
        let m = self.find_method(service, method)?;
        let one = message_to_json(&DynamicMessage::new(m.input()))?;
        if m.is_client_streaming() {
            Ok(format!("[\n{one}\n]"))
        } else {
            Ok(one)
        }
    }

    /// Invoke a method once with a JSON request, returning JSON response(s).
    pub async fn call_json(
        &self,
        endpoint: &str,
        service: &str,
        method: &str,
        json_body: &str,
        timeout_ms: u64,
    ) -> Result<CallResult, String> {
        let call = self.build_call(endpoint, service, method, json_body, timeout_ms)?;
        let timeout = Duration::from_millis(timeout_ms.max(100));
        let channel = connect(&call.endpoint, timeout).await?;
        let started = Instant::now();
        let responses = execute_call(channel, &call, 10_000)
            .await?
            .iter()
            .map(message_to_json)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CallResult {
            responses,
            server_streaming: call.server_streaming,
            duration_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

pub type GrpcLoadResult = LoadTestResult;

/// Run a gRPC method under load and return an aggregated result. Mirrors the
/// HTTP load model: `vus` concurrent workers, optional overall RPS cap, replaying
/// the same request; success/failure and latency are recorded per call.
pub async fn grpc_load(
    call: LoadCall,
    vus: usize,
    duration_secs: u64,
    rps_limit: Option<u32>,
    cancel: CancellationToken,
) -> Result<LoadTestResult, String> {
    let vus = vus.clamp(1, 10_000);
    let duration_secs = duration_secs.clamp(1, 3600);
    let timeout = Duration::from_millis(call.timeout_ms.max(100));

    // Connect once up front so a bad endpoint fails fast; workers clone the channel.
    let channel = connect(&call.endpoint, timeout).await?;
    let started_wall = chrono_now();

    let limiter = rps_limit.filter(|r| *r > 0).map(|rps| {
        let sem = Arc::new(Semaphore::new(0));
        spawn_refill(sem.clone(), rps, cancel.clone(), duration_secs);
        sem
    });

    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(duration_secs);
    // Second element is a status code for the aggregated status breakdown,
    // not a bool: 200 on success, a real gRPC status code (1..=16) on
    // failure. NOT tonic::Code::Ok's raw discriminant (0) — status_label()
    // treats 0 as the HTTP-runner's "network error" sentinel for non-DB
    // runners, so reusing it for gRPC success would mislabel every
    // successful call as an error in the CLI/HTML report and UI.
    let (tx, rx) = mpsc::unbounded_channel::<(u64, u16)>();
    let call = Arc::new(call);

    for _ in 0..vus {
        let channel = channel.clone();
        let call = call.clone();
        let cancel = cancel.clone();
        let tx = tx.clone();
        let limiter = limiter.clone();
        tokio::spawn(async move {
            loop {
                if cancel.is_cancelled() || tokio::time::Instant::now() >= deadline {
                    break;
                }
                if let Some(sem) = &limiter {
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => break,
                        _ = tokio::time::sleep_until(deadline) => break,
                        p = sem.acquire() => match p { Ok(p) => p.forget(), Err(_) => break },
                    }
                }
                let start = Instant::now();
                let outcome = execute_call(channel.clone(), &call, usize::MAX).await;
                let ok = outcome.is_ok();
                // 200 on success (the HTTP-report convention status_label()
                // renders as a real code, keeping 0 free for "network
                // error"); the real gRPC status code (1..=16) from a
                // tonic::Status on failure; fall back to Unknown for
                // non-gRPC failures (e.g. connect errors) so they still
                // land in the error bucket without colliding with 0/200.
                let code: u16 = match &outcome {
                    Ok(_) => 200,
                    Err(e) => e.code.map(|c| c as u16).unwrap_or(tonic::Code::Unknown as u16),
                };
                let latency_us = start.elapsed().as_micros().max(1) as u64;
                if tx.send((latency_us, code)).is_err() {
                    break;
                }
                if !ok {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
        });
    }
    drop(tx);

    let meta = RunMeta {
        target: call.endpoint.clone(),
        kind: "gRPC".to_string(),
        vus,
        duration_secs,
        rps_limit,
    };
    let mut result = aggregate(rx, meta, started).await;
    cancel.cancel();
    result.started_at = started_wall;
    Ok(result)
}

async fn aggregate(
    mut rx: mpsc::UnboundedReceiver<(u64, u16)>,
    meta: RunMeta,
    started: Instant,
) -> LoadTestResult {
    let mut hist = hdrhistogram::Histogram::<u64>::new(3).expect("hist");
    let mut sec_hist = hdrhistogram::Histogram::<u64>::new(3).expect("hist");
    let mut status: HashMap<u16, u64> = HashMap::new();
    let mut timeline: Vec<TimelinePoint> = Vec::new();
    let (mut total, mut errors, mut sum_us) = (0u64, 0u64, 0u128);
    let (mut sec_req, mut sec_err, mut sec_sum) = (0u64, 0u64, 0u128);
    let mut ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            s = rx.recv() => match s {
                Some((latency_us, code)) => {
                    total += 1;
                    sum_us += latency_us as u128;
                    sec_req += 1;
                    sec_sum += latency_us as u128;
                    let _ = hist.record(latency_us);
                    let _ = sec_hist.record(latency_us);
                    *status.entry(code).or_insert(0) += 1;
                    // 200 == success (see the mapping above); everything
                    // else (real gRPC 1..=16, or Unknown for non-gRPC
                    // failures) counts as an error.
                    let ok = code == 200;
                    if !ok { errors += 1; sec_err += 1; }
                }
                None => break,
            },
            _ = ticker.tick() => {
                timeline.push(TimelinePoint {
                    sec: timeline.len() as u64 + 1,
                    requests: sec_req,
                    errors: sec_err,
                    avg_ms: if sec_req > 0 { sec_sum as f64 / sec_req as f64 / 1000.0 } else { 0.0 },
                    p50_ms: sec_hist.value_at_quantile(0.5) as f64 / 1000.0,
                    p95_ms: sec_hist.value_at_quantile(0.95) as f64 / 1000.0,
                    p99_ms: sec_hist.value_at_quantile(0.99) as f64 / 1000.0,
                });
                sec_req = 0; sec_err = 0; sec_sum = 0;
                sec_hist.reset();
            }
        }
    }

    let actual_ms = started.elapsed().as_secs_f64() * 1000.0;
    finalize_result(&hist, status, total, errors, sum_us, timeline, meta, actual_ms, false)
}

fn spawn_refill(sem: Arc<Semaphore>, rps: u32, cancel: CancellationToken, duration_secs: u64) {
    tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(duration_secs);
        let per_tick = rps as f64 / 20.0;
        let mut acc = 0.0f64;
        let mut interval = tokio::time::interval(Duration::from_millis(50));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {}
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            acc += per_tick;
            let add = acc.floor() as usize;
            acc -= add as f64;
            let cap = (rps as usize).max(1);
            let grant = add.min(cap.saturating_sub(sem.available_permits()));
            if grant > 0 {
                sem.add_permits(grant);
            }
        }
    });
}

fn chrono_now() -> String {
    // Local timestamp without pulling chrono into this crate.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("unix:{secs}")
}
