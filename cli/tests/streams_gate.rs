// Integration test for the streams gate: build the real `maelstrom` binary,
// point it at an in-process mock service with a JSON streams config, and assert
// the exit code reflects the PER-STREAM min-success-rate floor (the flagship
// gating feature — a regression here would silently green-light a broken deploy).
use std::net::TcpListener as StdListener;
use std::process::Command;

/// Spawn a tiny mock HTTP server on a background tokio runtime; returns its base
/// URL. `/bad*` always 500s; everything else 200s. Binding the std listener
/// first means the OS queues connections into the backlog before the async
/// accept loop starts, so there's no connect-before-ready race.
fn spawn_mock() -> String {
    let std_listener = StdListener::bind("127.0.0.1:0").unwrap();
    std_listener.set_nonblocking(true).unwrap();
    let addr = std_listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { continue };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let line = String::from_utf8_lossy(&buf[..n])
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_string();
                    let target = line.split_whitespace().nth(1).unwrap_or("");
                    let (status, body) = if target.starts_with("/bad") {
                        ("500 Internal Server Error", "{\"error\":true}")
                    } else {
                        ("200 OK", "{\"ok\":true}")
                    };
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
    });
    format!("http://{addr}")
}

/// Write `config_json` to a temp file and run the built maelstrom binary; return
/// its exit code.
fn run_cli(tag: &str, config_json: &str, extra_args: &[&str]) -> i32 {
    let dir = std::env::temp_dir().join(format!("maelstrom-gate-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg_path = dir.join("scenario.json");
    std::fs::write(&cfg_path, config_json).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_maelstrom"))
        .arg(&cfg_path)
        .arg("--out-json")
        .arg(dir.join("report.json"))
        .arg("--quiet")
        .args(extra_args)
        .status()
        .expect("run maelstrom binary");
    status.code().unwrap_or(-1)
}

#[test]
fn per_stream_success_rate_floor_gates_the_run() {
    let base = spawn_mock();

    // Two streams: one always-200 (100% completed), one always-500 (0% completed).
    let mixed = format!(
        r#"{{
            "duration_secs": 2, "timeout_ms": 3000,
            "streams": [
                {{"name":"good","rps":20,"steps":[{{"name":"ok","method":"GET","url":"{base}/ok"}}]}},
                {{"name":"bad","rps":20,"steps":[{{"name":"boom","method":"GET","url":"{base}/bad"}}]}}
            ]
        }}"#
    );
    // Floor 99% → the "bad" stream (0% completed) breaches → exit 1.
    assert_eq!(run_cli("mixed", &mixed, &["--min-success-rate", "99"]), 1, "any stream below floor → exit 1");

    // Healthy-only config → all streams complete → exit 0.
    let healthy = format!(
        r#"{{
            "duration_secs": 2, "timeout_ms": 3000,
            "streams": [{{"name":"good","rps":20,"steps":[{{"name":"ok","method":"GET","url":"{base}/ok"}}]}}]
        }}"#
    );
    assert_eq!(run_cli("healthy", &healthy, &["--min-success-rate", "99"]), 0, "all streams pass floor → exit 0");
}

#[test]
fn cli_flag_overrides_a_laxer_config_floor() {
    let base = spawn_mock();
    // Config sets a permissive floor (0) that alone would pass a 0%-completion stream.
    let cfg = format!(
        r#"{{
            "duration_secs": 2, "timeout_ms": 3000,
            "thresholds": {{"min_success_rate": 0}},
            "streams": [{{"name":"bad","rps":20,"steps":[{{"name":"boom","method":"GET","url":"{base}/bad"}}]}}]
        }}"#
    );
    // Config floor 0 → 0% >= 0 → pass (exit 0).
    assert_eq!(run_cli("lax", &cfg, &[]), 0, "config floor of 0 passes");
    // Stricter CLI flag overrides the config → 0% < 99 → breach (exit 1).
    assert_eq!(run_cli("override", &cfg, &["--min-success-rate", "99"]), 1, "--min-success-rate overrides config");
}
