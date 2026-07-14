// Regression test: when a later target's initial token fetch fails,
// run_scenario must cancel the refreshers already started for earlier
// targets — otherwise an orphaned task keeps re-POSTing credentials to the
// token endpoint for the lifetime of the process.
use maelstrom_core::scenario::run_scenario;
use maelstrom_core::types::{OAuthTokenRequest, ScenarioProgress, ScenarioSpec, ScenarioTarget};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

async fn spawn_token_server() -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let count_bg = count.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            count_bg.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let body = b"{\"access_token\":\"tok\",\"expires_in\":1}";
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}/token"), count)
}

fn auth(token_url: &str) -> Option<OAuthTokenRequest> {
    Some(OAuthTokenRequest {
        grant_type: "client_credentials".into(),
        token_url: token_url.into(),
        client_id: "id".into(),
        client_secret: Some("secret".into()),
        scope: None,
        username: None,
        password: None,
        refresh_token: None,
        client_auth: "body".into(),
    })
}

fn target(name: &str, token_url: &str) -> ScenarioTarget {
    ScenarioTarget {
        name: name.into(),
        method: "GET".into(),
        url: "http://127.0.0.1:9/never-hit".into(),
        headers: vec![],
        body: None,
        rps: 1,
        tls: None,
        auth_refresh: auth(token_url),
        multipart: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresher_is_cancelled_when_later_target_token_init_fails() {
    let (good_url, count) = spawn_token_server().await;

    let spec = ScenarioSpec {
        duration_secs: 1,
        timeout_ms: 1000,
        datasets: vec![],
        file_pools: vec![],
        targets: vec![
            target("a", &good_url),
            // Port 1 is closed: initial fetch_token fails fast.
            target("b", "http://127.0.0.1:1/token"),
        ],
    };

    let cancel = CancellationToken::new();
    let on_p: Arc<dyn Fn(&ScenarioProgress) + Send + Sync> = Arc::new(|_| {});
    let on_r: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(|_| {});
    let res = run_scenario(
        spec,
        cancel.clone(),
        on_p,
        on_r,
        maelstrom_core::scenario::no_log(),
    )
    .await;
    assert!(res.is_err(), "scenario must fail on target b token init");
    assert!(
        cancel.is_cancelled(),
        "run_scenario must cancel already-started refreshers on Err"
    );

    // Let any refresh already in flight at cancel time finish, then take the
    // baseline. Exact count is timing-dependent (target b's failure may take
    // longer than one 0.8s refresh interval) — what matters is no growth after.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let after_err = count.load(Ordering::SeqCst);
    assert!(after_err >= 1, "target a fetched its initial token");

    // expires_in=1 -> a leaked refresher would POST again every 0.8s.
    tokio::time::sleep(std::time::Duration::from_millis(2600)).await;
    let later = count.load(Ordering::SeqCst);
    assert_eq!(
        later, after_err,
        "orphaned refresher kept POSTing credentials after Err"
    );
}
