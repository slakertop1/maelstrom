// End-to-end tests of the request-chaining (streams) engine against an
// in-process HTTP server. Covers: value threading (json/header/regex),
// PER-ITERATION variable isolation (no token bleed between concurrent chains),
// funnel/abort-on-failure, timeouts, mixed chain+single runs, and that the OLD
// engine features (dynval generators, datasets, multipart) still work inside
// the new streams engine.
use maelstrom_core::scenario::no_log;
use maelstrom_core::streams::run_streams;
use maelstrom_core::types::{
    DatasetSource, DatasetSpec, ExtractRule, MultipartPart, StreamScenarioSpec, StreamSpec,
    StreamStep,
};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// Shared mock-server state the tests assert against.
#[derive(Default)]
struct MockState {
    /// tokens issued by /login and not yet consumed by /use
    issued: Mutex<HashSet<String>>,
    /// tokens successfully consumed exactly once
    used: Mutex<HashSet<String>>,
    /// /use called with an already-consumed token (would mean var bleed) → 409
    reuse_conflicts: AtomicU64,
    /// /use called with an unknown/absent token → 401
    bad_token: AtomicU64,
    /// full request targets seen on /echo (query strings recorded)
    echoes: Mutex<Vec<String>>,
    /// Content-Type header values seen on /multi
    multi_content_types: Mutex<Vec<String>>,
    login_seq: AtomicU64,
}

/// Minimal HTTP/1.1 mock:
///   POST /login → 200 {"data":{"token":"T-<unique>"}} (token recorded as issued)
///   GET  /use   → 200 if Bearer <issued, unused>; 409 if reused; 401 otherwise
///   GET  /hdr   → 200, header `X-Next: NEXT-42`, body {"note":"code=ABC77"}
///   GET  /echo?…→ 200, records the full target (path?query)
///   GET  /fail  → 500 always
///   GET  /never → 200 (chains must never reach it after a failed step)
///   GET  /slow  → sleeps 3 s, then 200
///   POST /multi → 200, records the Content-Type header
async fn spawn_mock() -> (String, Arc<MockState>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(MockState::default());
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let st = st.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = req.lines().next().unwrap_or("").to_string();
                let target = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let header = |name: &str| -> Option<String> {
                    req.lines()
                        .skip(1)
                        .take_while(|l| !l.is_empty())
                        .find(|l| l.to_ascii_lowercase().starts_with(&format!("{name}:")))
                        .map(|l| l.split_once(':').map_or("", |(_, v)| v).trim().to_string())
                };

                let mut extra_headers = String::new();
                let (status, body): (&str, String) = if target.starts_with("/login") {
                    let tok = format!("T-{}", st.login_seq.fetch_add(1, Ordering::Relaxed));
                    st.issued.lock().unwrap().insert(tok.clone());
                    ("200 OK", format!("{{\"data\":{{\"token\":\"{tok}\"}}}}"))
                } else if target.starts_with("/use") {
                    let auth = header("authorization").unwrap_or_default();
                    let tok = auth.strip_prefix("Bearer ").unwrap_or("").to_string();
                    let was_issued = st.issued.lock().unwrap().remove(&tok);
                    if was_issued {
                        st.used.lock().unwrap().insert(tok);
                        ("200 OK", "{\"ok\":true}".to_string())
                    } else if st.used.lock().unwrap().contains(&tok) {
                        st.reuse_conflicts.fetch_add(1, Ordering::Relaxed);
                        ("409 Conflict", "{\"error\":\"token reused\"}".to_string())
                    } else {
                        st.bad_token.fetch_add(1, Ordering::Relaxed);
                        ("401 Unauthorized", "{\"error\":\"bad token\"}".to_string())
                    }
                } else if target.starts_with("/hdr") {
                    extra_headers = "X-Next: NEXT-42\r\n".to_string();
                    ("200 OK", "{\"note\":\"code=ABC77\"}".to_string())
                } else if target.starts_with("/echo") {
                    st.echoes.lock().unwrap().push(target.clone());
                    ("200 OK", "{\"ok\":true}".to_string())
                } else if target.starts_with("/fail") {
                    ("500 Internal Server Error", "{\"error\":\"boom\"}".to_string())
                } else if target.starts_with("/never") {
                    ("200 OK", "{\"ok\":true}".to_string())
                } else if target.starts_with("/slow") {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    ("200 OK", "{\"ok\":true}".to_string())
                } else if target.starts_with("/multi") {
                    let ct = header("content-type").unwrap_or_default();
                    st.multi_content_types.lock().unwrap().push(ct);
                    ("200 OK", "{\"ok\":true}".to_string())
                } else {
                    ("404 Not Found", "{}".to_string())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), state)
}

fn step(name: &str, method: &str, url: String) -> StreamStep {
    StreamStep {
        name: name.into(),
        method: method.into(),
        url,
        headers: vec![],
        body: None,
        tls: None,
        multipart: None,
        extract: vec![],
    }
}

fn extract(name: &str, from: &str, expr: &str) -> ExtractRule {
    ExtractRule { name: name.into(), from: from.into(), expr: expr.into() }
}

fn spec(streams: Vec<StreamSpec>) -> StreamScenarioSpec {
    StreamScenarioSpec {
        duration_secs: 2,
        timeout_ms: 3000,
        streams,
        datasets: vec![],
        file_pools: vec![],
    }
}

async fn run(spec: StreamScenarioSpec) -> maelstrom_core::types::StreamsResult {
    run_streams(spec, CancellationToken::new(), Arc::new(|_| {}), no_log())
        .await
        .expect("run_streams")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chain_extracts_token_and_reuses_it() {
    let (base, _st) = spawn_mock().await;
    let mut login = step("login", "POST", format!("{base}/login"));
    login.extract = vec![extract("token", "json", "data.token")];
    let mut use_ = step("use", "GET", format!("{base}/use"));
    use_.headers = vec![("Authorization".to_string(), "Bearer {{token}}".to_string())];

    let result = run(spec(vec![StreamSpec {
        name: "login → use".into(),
        rps: 50,
        steps: vec![login, use_],
    }]))
    .await;

    let s = &result.streams[0];
    assert_eq!(s.steps.len(), 2);
    let (login_r, use_r) = (&s.steps[0], &s.steps[1]);
    assert!(login_r.total_requests > 10, "login hit: {}", login_r.total_requests);
    // The decisive check: /use must return 200 (401 would mean the token wasn't threaded).
    assert_eq!(use_r.errors, 0, "use must not 401 — token was passed");
    assert!(s.iterations_completed > 10);
    assert!(s.success_rate > 95.0, "success {}", s.success_rate);
    assert!(s.e2e_p95_ms > 0.0);
    assert!(login_r.total_requests >= use_r.total_requests, "funnel monotonic");
    assert_eq!(result.overall.total_requests, login_r.total_requests + use_r.total_requests);
}

// The sharpest correctness property of chaining under load: variables are
// PER-ITERATION. Every /login issues a unique one-shot token; /use consumes it
// exactly once. If concurrent iterations shared a var map, one iteration would
// overwrite another's token → some tokens consumed twice (409) and some never.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn per_iteration_variables_do_not_bleed_between_chains() {
    let (base, st) = spawn_mock().await;
    let mut login = step("login", "POST", format!("{base}/login"));
    login.extract = vec![extract("token", "json", "data.token")];
    let mut use_ = step("use", "GET", format!("{base}/use"));
    use_.headers = vec![("Authorization".to_string(), "Bearer {{token}}".to_string())];

    let result = run(spec(vec![StreamSpec {
        name: "isolation".into(),
        rps: 80, // enough that iterations overlap heavily
        steps: vec![login, use_],
    }]))
    .await;

    let s = &result.streams[0];
    assert!(s.iterations_completed > 50, "need real concurrency: {}", s.iterations_completed);
    assert_eq!(st.reuse_conflicts.load(Ordering::Relaxed), 0, "a token was used twice — var bleed");
    assert_eq!(st.bad_token.load(Ordering::Relaxed), 0, "a chain used an unknown token");
    // Every completed chain consumed exactly its own token.
    assert_eq!(st.used.lock().unwrap().len() as u64, s.steps[1].total_requests - s.steps[1].errors);
    assert_eq!(s.steps[1].errors, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chain_aborts_on_failed_step_and_funnel_shows_it() {
    let (base, _st) = spawn_mock().await;
    let result = run(spec(vec![StreamSpec {
        name: "abort".into(),
        rps: 30,
        steps: vec![
            step("ok", "GET", format!("{base}/echo?stage=1")),
            step("boom", "GET", format!("{base}/fail")),
            step("after", "GET", format!("{base}/never")),
        ],
    }]))
    .await;

    let s = &result.streams[0];
    assert!(s.steps[0].total_requests > 10, "step1 ran");
    assert_eq!(s.steps[1].error_rate, 100.0, "step2 always 500");
    // The step AFTER the failure must never be reached.
    assert_eq!(s.steps[2].total_requests, 0, "step3 must not run after a failed step");
    assert_eq!(s.iterations_completed, 0);
    assert_eq!(s.success_rate, 0.0);
    // Overall error rate reflects one failing step out of two executed.
    assert!(result.overall.errors > 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extracts_from_header_and_regex_thread_into_next_step() {
    let (base, st) = spawn_mock().await;
    let mut first = step("hdr", "GET", format!("{base}/hdr"));
    first.extract = vec![
        extract("next", "header", "X-Next"),
        extract("code", "regex", r#"code=(\w+)"#),
    ];
    let second = step("echo", "GET", format!("{base}/echo?h={{{{next}}}}&c={{{{code}}}}"));

    let result = run(spec(vec![StreamSpec {
        name: "hdr+regex".into(),
        rps: 20,
        steps: vec![first, second],
    }]))
    .await;

    assert!(result.streams[0].iterations_completed > 5);
    let echoes = st.echoes.lock().unwrap();
    assert!(!echoes.is_empty());
    for e in echoes.iter() {
        assert!(e.contains("h=NEXT-42"), "header extract threaded: {e}");
        assert!(e.contains("c=ABC77"), "regex extract threaded: {e}");
    }
}

// OLD functionality inside the NEW engine: per-request dynval generators
// ({{$counter}}) and dataset values ({{$data.people.name}}) must keep working
// in stream steps, alongside chain vars.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generators_and_datasets_still_work_in_stream_steps() {
    let (base, st) = spawn_mock().await;
    let people = DatasetSpec {
        name: "people".into(),
        mode: "sequential".into(),
        source: DatasetSource {
            kind: "inline".into(),
            rows: Some(vec![
                [("name".to_string(), "alice".to_string())].into_iter().collect(),
                [("name".to_string(), "bob".to_string())].into_iter().collect(),
            ]),
            path: None,
            url: None,
            format: None,
            query: None,
            aws: None,
        },
    };
    let mut sp = spec(vec![StreamSpec {
        name: "gen".into(),
        rps: 30,
        steps: vec![step(
            "echo",
            "GET",
            format!("{base}/echo?n={{{{$counter}}}}&who={{{{$data.people.name}}}}"),
        )],
    }]);
    sp.datasets = vec![people];

    let result = run(sp).await;
    assert!(result.streams[0].iterations_completed > 10);

    let echoes = st.echoes.lock().unwrap();
    // Dataset round-robin delivered both rows.
    assert!(echoes.iter().any(|e| e.contains("who=alice")), "alice seen");
    assert!(echoes.iter().any(|e| e.contains("who=bob")), "bob seen");
    // Counter generated distinct values per request (not one frozen value).
    let ns: HashSet<&str> = echoes
        .iter()
        .filter_map(|e| e.split("n=").nth(1).and_then(|s| s.split('&').next()))
        .collect();
    assert!(ns.len() > 5, "counter varied per request: {} distinct", ns.len());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multipart_step_sends_multipart_content_type() {
    let (base, st) = spawn_mock().await;
    let mut up = step("upload", "POST", format!("{base}/multi"));
    up.multipart = Some(vec![MultipartPart {
        name: "meta".into(),
        kind: "text".into(),
        value: "hello".into(),
        filename: None,
        content_type: None,
        enabled: true,
    }]);

    let result = run(spec(vec![StreamSpec { name: "mp".into(), rps: 15, steps: vec![up] }])).await;
    assert!(result.streams[0].steps[0].total_requests > 5);
    assert_eq!(result.streams[0].steps[0].errors, 0);
    let cts = st.multi_content_types.lock().unwrap();
    assert!(!cts.is_empty());
    assert!(
        cts.iter().all(|ct| ct.starts_with("multipart/form-data")),
        "multipart wiring intact: {:?}",
        cts.first()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mixed_chain_and_single_streams_run_in_parallel() {
    let (base, _st) = spawn_mock().await;
    let mut login = step("login", "POST", format!("{base}/login"));
    login.extract = vec![extract("token", "json", "data.token")];
    let mut use_ = step("use", "GET", format!("{base}/use"));
    use_.headers = vec![("Authorization".to_string(), "Bearer {{token}}".to_string())];

    let result = run(spec(vec![
        StreamSpec { name: "chain".into(), rps: 30, steps: vec![login, use_] },
        StreamSpec { name: "single".into(), rps: 40, steps: vec![step("e", "GET", format!("{base}/echo?s=1"))] },
    ]))
    .await;

    assert_eq!(result.streams.len(), 2);
    let chain = &result.streams[0];
    let single = &result.streams[1];
    assert!(chain.iterations_completed > 10 && chain.success_rate > 95.0);
    assert!(single.iterations_completed > 20);
    // Single stream: 1 step, iterations == requests.
    assert_eq!(single.steps.len(), 1);
    assert_eq!(single.iterations_started, single.steps[0].total_requests);
    // Overall sums every request of both streams.
    let sum: u64 = result
        .streams
        .iter()
        .flat_map(|s| s.steps.iter())
        .map(|st| st.total_requests)
        .sum();
    assert_eq!(result.overall.total_requests, sum);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeout_counts_as_error_and_aborts_chain() {
    let (base, _st) = spawn_mock().await;
    let mut sp = spec(vec![StreamSpec {
        name: "slowpoke".into(),
        rps: 10,
        steps: vec![
            step("slow", "GET", format!("{base}/slow")),
            step("after", "GET", format!("{base}/never")),
        ],
    }]);
    sp.timeout_ms = 300; // mock sleeps 3 s → every request times out

    let result = run(sp).await;
    let s = &result.streams[0];
    assert!(s.steps[0].total_requests > 5);
    assert_eq!(s.steps[0].error_rate, 100.0, "timeouts are errors");
    assert_eq!(s.steps[1].total_requests, 0, "chain aborted on timeout");
    assert_eq!(s.iterations_completed, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn validation_fails_fast() {
    // No streams at all.
    let empty = StreamScenarioSpec {
        duration_secs: 2,
        timeout_ms: 1000,
        streams: vec![],
        datasets: vec![],
        file_pools: vec![],
    };
    assert!(run_streams(empty, CancellationToken::new(), Arc::new(|_| {}), no_log())
        .await
        .is_err());

    // Invalid method.
    let bad_method = spec(vec![StreamSpec {
        name: "bad".into(),
        rps: 10,
        steps: vec![step("x", "NOT A METHOD", "http://127.0.0.1:1/x".into())],
    }]);
    assert!(run_streams(bad_method, CancellationToken::new(), Arc::new(|_| {}), no_log())
        .await
        .is_err());

    // Invalid literal URL.
    let bad_url = spec(vec![StreamSpec {
        name: "bad".into(),
        rps: 10,
        steps: vec![step("x", "GET", "not a url".into())],
    }]);
    assert!(run_streams(bad_url, CancellationToken::new(), Arc::new(|_| {}), no_log())
        .await
        .is_err());

    // Streams with rps=0 / no steps are filtered out → nothing left → error.
    let zero = spec(vec![StreamSpec { name: "z".into(), rps: 0, steps: vec![step("x", "GET", "http://h/x".into())] }]);
    assert!(run_streams(zero, CancellationToken::new(), Arc::new(|_| {}), no_log())
        .await
        .is_err());
}
