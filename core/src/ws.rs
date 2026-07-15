//! WebSocket client: a single connect/send/receive call and a load runner that
//! models request→response over WS (send a frame, await a reply, measure latency).

use crate::histogram::finalize_result;
use crate::types::{LoadTestResult, RunMeta, TimelinePoint};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Semaphore};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

/// Result of one WS interaction: the messages received after sending.
pub struct WsCallResult {
    pub messages: Vec<String>,
    pub duration_ms: f64,
}

fn render(msg: &Message) -> Option<String> {
    match msg {
        Message::Text(t) => Some(t.to_string()),
        Message::Binary(b) => Some(format!("<binary {} байт>", b.len())),
        _ => None,
    }
}

/// Connect, optionally send one text message, then collect replies until the
/// socket goes idle for `idle_ms`, it closes, or `max_msgs` is reached.
pub async fn ws_call(
    url: &str,
    message: &str,
    timeout_ms: u64,
    max_msgs: usize,
) -> Result<WsCallResult, String> {
    let started = Instant::now();
    let connect_to = Duration::from_millis(timeout_ms.max(100));
    let (mut ws, _resp) = tokio::time::timeout(connect_to, tokio_tungstenite::connect_async(url))
        .await
        .map_err(|_| "Таймаут подключения".to_string())?
        .map_err(|e| format!("Подключение WS: {e}"))?;

    if !message.is_empty() {
        ws.send(Message::Text(message.into()))
            .await
            .map_err(|e| format!("Отправка: {e}"))?;
    }

    let idle = Duration::from_millis(timeout_ms.clamp(100, 3000));
    let mut messages = Vec::new();
    loop {
        match tokio::time::timeout(idle, ws.next()).await {
            Ok(Some(Ok(msg))) => {
                if matches!(msg, Message::Close(_)) {
                    break;
                }
                if let Some(s) = render(&msg) {
                    messages.push(s);
                    if messages.len() >= max_msgs {
                        break;
                    }
                }
            }
            Ok(Some(Err(e))) => return Err(format!("Приём: {e}")),
            Ok(None) => break,  // socket closed
            Err(_) => break,    // idle — stop waiting for more
        }
    }
    let _ = ws.close(None).await;
    Ok(WsCallResult {
        messages,
        duration_ms: started.elapsed().as_secs_f64() * 1000.0,
    })
}

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// One send→reply round-trip on an existing connection. Returns latency (µs) and
/// whether it succeeded (a reply arrived in time).
async fn round_trip(ws: &mut Ws, message: &str, timeout: Duration, cancel: &CancellationToken) -> (u64, bool) {
    let start = Instant::now();
    if ws.send(Message::Text(message.into())).await.is_err() {
        return (start.elapsed().as_micros().max(1) as u64, false);
    }
    // One deadline for the WHOLE round-trip, set once — NOT re-armed on every
    // control frame. `tokio::time::timeout` per-iteration used to reset the
    // clock on each ping/pong, so a chatty peer that never sends a real reply
    // could stall this indefinitely. Cancellation is checked inside the wait
    // too (not just between round-trips in the caller), so Stop takes effect
    // immediately instead of only after the current wait gives up on its own.
    let deadline = tokio::time::Instant::now() + timeout;
    let ok = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break false,
            _ = tokio::time::sleep_until(deadline) => break false,
            msg = ws.next() => match msg {
                Some(Ok(m)) => {
                    if matches!(m, Message::Close(_)) {
                        break false;
                    }
                    if render(&m).is_some() {
                        break true;
                    }
                    // control frame (ping/pong) — keep waiting, deadline unchanged
                }
                _ => break false,
            },
        }
    };
    ((start.elapsed().as_micros().max(1)) as u64, ok)
}

/// Load-test a WebSocket endpoint: `vus` persistent connections, each repeatedly
/// sending `message` and awaiting a reply. Reconnects on failure.
pub async fn ws_load(
    url: &str,
    message: &str,
    vus: usize,
    duration_secs: u64,
    rps_limit: Option<u32>,
    timeout_ms: u64,
    cancel: CancellationToken,
) -> Result<LoadTestResult, String> {
    let vus = vus.clamp(1, 10_000);
    let duration_secs = duration_secs.clamp(1, 3600);
    let timeout = Duration::from_millis(timeout_ms.max(100));

    // Validate the endpoint up front (fail fast before the slot / workers).
    {
        let (ws, _) = tokio::time::timeout(timeout, tokio_tungstenite::connect_async(url))
            .await
            .map_err(|_| "Таймаут подключения".to_string())?
            .map_err(|e| format!("Подключение WS: {e}"))?;
        let mut ws = ws;
        let _ = ws.close(None).await;
    }

    let started_wall = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let limiter = rps_limit.filter(|r| *r > 0).map(|rps| {
        let sem = Arc::new(Semaphore::new(0));
        spawn_rps_refill(sem.clone(), rps, cancel.clone(), duration_secs);
        sem
    });

    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(duration_secs);
    // Bounded, unlike an unbounded channel — the aggregator is a tight
    // in-memory loop so this should rarely fill, but a bound means a stalled
    // aggregator applies backpressure to VUs instead of letting latency
    // samples pile up in memory without limit.
    const RESULT_CHANNEL_CAP: usize = 4096;
    let (tx, rx) = mpsc::channel::<(u64, bool)>(RESULT_CHANNEL_CAP);
    let url = Arc::new(url.to_string());
    let message = Arc::new(message.to_string());

    for _ in 0..vus {
        let url = url.clone();
        let message = message.clone();
        let cancel = cancel.clone();
        let tx = tx.clone();
        let limiter = limiter.clone();
        tokio::spawn(async move {
            let mut conn: Option<Ws> = None;
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
                // Ensure a live connection.
                if conn.is_none() {
                    match tokio::time::timeout(timeout, tokio_tungstenite::connect_async(&*url)).await {
                        Ok(Ok((ws, _))) => conn = Some(ws),
                        _ => {
                            let _ = tx.send((timeout.as_micros() as u64, false)).await;
                            tokio::time::sleep(Duration::from_millis(25)).await;
                            continue;
                        }
                    }
                }
                let ws = conn.as_mut().unwrap();
                let (latency_us, ok) = round_trip(ws, &message, timeout, &cancel).await;
                if !ok {
                    // Best-effort Close before dropping — the peer previously
                    // just saw a bare TCP drop on every failed round-trip.
                    // Bounded by its own short timeout so a stuck/dead peer
                    // can't stall the reconnect loop waiting for it.
                    if let Some(mut dead) = conn.take() {
                        let _ = tokio::time::timeout(Duration::from_millis(200), dead.close(None)).await;
                    }
                }
                if tx.send((latency_us, ok)).await.is_err() {
                    break;
                }
                if !ok {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            }
            if let Some(mut ws) = conn {
                let _ = ws.close(None).await;
            }
        });
    }
    drop(tx);

    let meta = RunMeta {
        target: url.to_string(),
        kind: "WS".to_string(),
        vus,
        duration_secs,
        rps_limit,
    };
    let mut result = aggregate(rx, meta, started).await;
    cancel.cancel();
    result.started_at = started_wall;
    Ok(result)
}

fn spawn_rps_refill(sem: Arc<Semaphore>, rps: u32, cancel: CancellationToken, duration_secs: u64) {
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

async fn aggregate(
    mut rx: mpsc::Receiver<(u64, bool)>,
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
                Some((latency_us, ok)) => {
                    total += 1;
                    sum_us += latency_us as u128;
                    sec_req += 1;
                    sec_sum += latency_us as u128;
                    let _ = hist.record(latency_us);
                    let _ = sec_hist.record(latency_us);
                    *status.entry(if ok { 200 } else { 0 }).or_insert(0) += 1;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    // Minimal echo WS server on a random port; returns its ws:// URL.
    async fn spawn_echo() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut ws = match tokio_tungstenite::accept_async(stream).await {
                        Ok(ws) => ws,
                        Err(_) => return,
                    };
                    while let Some(Ok(msg)) = ws.next().await {
                        match msg {
                            Message::Text(t) => {
                                let _ = ws.send(Message::Text(format!("echo:{t}"))).await;
                            }
                            Message::Close(_) => break,
                            _ => {}
                        }
                    }
                });
            }
        });
        format!("ws://{addr}")
    }

    #[tokio::test]
    async fn ws_call_receives_echo() {
        let url = spawn_echo().await;
        let res = ws_call(&url, "hello", 1000, 4).await.unwrap();
        assert!(res.messages.iter().any(|m| m == "echo:hello"), "got {:?}", res.messages);
    }

    #[tokio::test]
    async fn ws_load_runs_against_echo() {
        let url = spawn_echo().await;
        let cancel = CancellationToken::new();
        let result = ws_load(&url, "ping", 4, 1, None, 1000, cancel).await.unwrap();
        assert_eq!(result.method, "WS");
        assert!(result.total_requests > 0, "no round-trips");
        assert_eq!(result.errors, 0, "unexpected errors");
    }
}
