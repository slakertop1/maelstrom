// End-to-end test of the scenario engine against an in-process HTTP server.
// Exercises per-target RPS, aggregation, dynamic generators and datasets, and
// the HTML report — the whole path the app and CLI share.
use maelstrom_core::report::build_scenario_report;
use maelstrom_core::scenario::run_scenario;
use maelstrom_core::types::{
    DatasetSource, DatasetSpec, ScenarioProgress, ScenarioSpec, ScenarioTarget,
};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// Minimal HTTP/1.1 server that records each request target and replies 200.
async fn spawn_mock() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen_bg = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let seen = seen_bg.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                if let Some(line) = req.lines().next() {
                    // "GET /a?n=1 HTTP/1.1" -> "/a?n=1"
                    if let Some(target) = line.split_whitespace().nth(1) {
                        seen.lock().unwrap().push(target.to_string());
                    }
                }
                let body = b"{\"ok\":true}";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), seen)
}

fn target(name: &str, url: String, rps: u32) -> ScenarioTarget {
    ScenarioTarget {
        name: name.into(),
        method: "GET".into(),
        url,
        headers: vec![],
        body: None,
        rps,
        tls: None,
        auth_refresh: None,
        multipart: None,
    }
}

fn noop_progress() -> Arc<dyn Fn(&ScenarioProgress) + Send + Sync> {
    Arc::new(|_p| {})
}
fn noop_refresh() -> Arc<dyn Fn(u64) + Send + Sync> {
    Arc::new(|_n| {})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scenario_runs_endpoints_with_generators_and_dataset() {
    let (base, seen) = spawn_mock().await;

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

    let spec = ScenarioSpec {
        duration_secs: 2,
        timeout_ms: 3000,
        datasets: vec![people],
        file_pools: vec![],
        targets: vec![
            target("a", format!("{base}/a?n={{{{$counter}}}}"), 40),
            target("b", format!("{base}/b?who={{{{$data.people.name}}}}"), 30),
        ],
    };

    let result = run_scenario(
        spec,
        CancellationToken::new(),
        noop_progress(),
        noop_refresh(),
        maelstrom_core::scenario::no_log(),
    )
    .await
    .expect("scenario should run");

    // Two endpoints, each near its target volume (40 & 30 rps over ~2s).
    assert_eq!(result.targets.len(), 2);
    assert_eq!(result.overall.errors, 0, "no request should fail");
    // A fast mock keeps up, so nothing should be dropped by the scheduler.
    assert_eq!(result.overall.dropped, 0, "no requests should be dropped");
    assert_eq!(result.targets[0].dropped + result.targets[1].dropped, 0);
    let total = result.overall.total_requests;
    assert!((100..=160).contains(&total), "total {total} not near (40+30)*2");

    let a = &result.targets[0];
    let b = &result.targets[1];
    assert!(a.total_requests >= 60 && a.total_requests <= 100, "a={}", a.total_requests);
    assert!(b.total_requests >= 40 && b.total_requests <= 80, "b={}", b.total_requests);

    // Inspect what the server actually received.
    let reqs = seen.lock().unwrap().clone();
    let a_ns: Vec<i64> = reqs
        .iter()
        .filter_map(|r| r.strip_prefix("/a?n="))
        .filter_map(|n| n.parse().ok())
        .collect();
    // {{$counter}} produced distinct, increasing values
    assert!(a_ns.len() >= 60);
    let distinct: std::collections::HashSet<_> = a_ns.iter().collect();
    assert_eq!(distinct.len(), a_ns.len(), "counter values must be unique");

    // {{$data.people.name}} cycled through both rows
    let whos: std::collections::HashSet<String> = reqs
        .iter()
        .filter_map(|r| r.strip_prefix("/b?who=").map(|s| s.to_string()))
        .collect();
    assert!(whos.contains("alice") && whos.contains("bob"), "dataset rows: {whos:?}");

    // Report renders without panicking and carries the brand + a chart.
    let html = build_scenario_report(&result);
    assert!(html.contains("Maelstrom"));
    assert!(html.contains("<svg"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_targets_is_rejected() {
    let spec = ScenarioSpec {
        duration_secs: 1,
        timeout_ms: 1000,
        datasets: vec![],
        file_pools: vec![],
        targets: vec![],
    };
    let err = run_scenario(
        spec,
        CancellationToken::new(),
        noop_progress(),
        noop_refresh(),
        maelstrom_core::scenario::no_log(),
    )
    .await;
    assert!(err.is_err());
}
