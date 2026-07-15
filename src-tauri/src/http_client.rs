use base64::Engine;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Deserialize)]
pub struct HttpRequestSpec {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub tls: Option<crate::tls::TlsConfig>,
    #[serde(default)]
    pub multipart: Option<Vec<maelstrom_core::types::MultipartPart>>,
    #[serde(default)]
    pub file_pools: Vec<maelstrom_core::types::FilePoolSpec>,
    #[serde(default)]
    pub datasets: Vec<maelstrom_core::types::DatasetSpec>,
}

#[derive(Serialize)]
pub struct HttpResponseData {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub body_base64: bool,
    pub size_bytes: usize,
    pub duration_ms: f64,
}

/// Same cap as `storage::read_text_file` uses for local files — big enough for
/// any real API response, small enough that one huge/streamed reply can't
/// blow up memory before we ever look at it.
const MAX_RESPONSE_BODY_BYTES: usize = 40 * 1024 * 1024;

/// Read the response body in chunks, aborting as soon as the accumulated size
/// crosses `max_bytes` instead of buffering the whole thing via `bytes()`.
async fn read_body_limited(mut resp: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        buf.extend_from_slice(&chunk);
        if buf.len() > max_bytes {
            return Err(format!(
                "Тело ответа превышает лимит {} МБ",
                max_bytes / (1024 * 1024)
            ));
        }
    }
    Ok(buf)
}

/// Reqwest only strips a fixed set of *standard* sensitive headers
/// (Authorization, Cookie, …) when a redirect crosses hosts — custom headers
/// like `X-Api-Key` are not covered and would otherwise be replayed against
/// whatever host `Location` points to. `redirect::Policy` in this reqwest
/// version can only follow/stop/error a redirect (no hook to rewrite
/// headers), so the safe equivalent is: stop following as soon as the
/// redirect target's scheme, host, or port differs from the previous hop's —
/// no request, and therefore no header, ever reaches the new origin. Scheme
/// is checked too (not just host+port): an https→http redirect to the same
/// host:port would otherwise pass as "same host" and replay secret headers
/// over an unencrypted connection, which defeats the whole point of this
/// check. Same-origin redirects (trailing slash, path changes, …) keep
/// working, up to the same 10-hop cap reqwest's own default uses.
fn same_host_redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        const MAX_REDIRECTS: usize = 10;
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        let same_host = match attempt.previous().last() {
            Some(prev) => {
                prev.scheme() == attempt.url().scheme()
                    && prev.host_str() == attempt.url().host_str()
                    && prev.port_or_known_default() == attempt.url().port_or_known_default()
            }
            None => true,
        };
        if same_host {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

#[tauri::command]
pub async fn send_request(
    app: tauri::AppHandle,
    spec: HttpRequestSpec,
) -> Result<HttpResponseData, String> {
    let method = reqwest::Method::from_bytes(spec.method.as_bytes())
        .map_err(|_| format!("Неверный HTTP-метод: {}", spec.method))?;

    // Resolve data providers so a single "Send" behaves like one load iteration:
    // dynamic generators ({{$uuid}}, …), file pools, AND datasets ({{$data.*}})
    // all work here too. DB-backed datasets are turned into inline rows first.
    let datasets = crate::db::resolve_db_datasets(&app, &spec.datasets).await?;
    // A single "Send" has no Stop button of its own to cancel against — a
    // fresh token here just lets `resolve` share the same cancellation-aware
    // code path as the load-test/scenario callers (d1b) instead of forking a
    // separate uncancellable variant.
    let dyn_state = maelstrom_core::dynval::resolve(
        &datasets,
        &spec.file_pools,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await?;
    let ctx = dyn_state.request();

    let expanded_url = ctx.expand(spec.url.trim());
    crate::log::write(
        &app,
        "HTTP →",
        &format!(
            "{} {} | headers: {}",
            spec.method,
            crate::log::safe_url(&expanded_url),
            crate::log::safe_headers(&spec.headers)
        ),
    );
    let url = reqwest::Url::parse(expanded_url.trim())
        .map_err(|e| format!("Неверный URL: {e}"))?;

    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(spec.timeout_ms.unwrap_or(30_000)))
        .user_agent("Maelstrom/0.1")
        .redirect(same_host_redirect_policy());
    builder = crate::tls::apply_tls(builder, &spec.tls)?;
    let client = builder.build().map_err(|e| e.to_string())?;

    let has_multipart = spec
        .multipart
        .as_ref()
        .is_some_and(|p| p.iter().any(|f| f.enabled && !f.name.trim().is_empty()));

    let mut req = client.request(method, url);
    for (k, v) in &spec.headers {
        let k = k.trim();
        if k.is_empty() {
            continue;
        }
        // reqwest sets the multipart Content-Type (with boundary) itself.
        if has_multipart && k.eq_ignore_ascii_case("content-type") {
            continue;
        }
        req = req.header(k, ctx.expand(v));
    }
    if has_multipart {
        let prepared = maelstrom_core::multipart::prepare_parts(spec.multipart.as_ref().unwrap())?;
        req = req.multipart(maelstrom_core::multipart::form_from_prepared(&prepared, &ctx));
    } else if let Some(b) = spec.body {
        if !b.is_empty() {
            req = req.body(ctx.expand(&b));
        }
    }

    let start = Instant::now();
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = format_reqwest_error(&e);
            crate::log::write(&app, "HTTP ✗", &format!("{} {} | {msg}", spec.method, crate::log::safe_url(&expanded_url)));
            return Err(msg);
        }
    };
    let status = resp.status();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| {
            (
                k.to_string(),
                v.to_str().unwrap_or("<binary>").to_string(),
            )
        })
        .collect();
    let bytes = match read_body_limited(resp, MAX_RESPONSE_BODY_BYTES).await {
        Ok(b) => b,
        Err(msg) => {
            crate::log::write(&app, "HTTP ✗", &format!("{} {} | {msg}", spec.method, crate::log::safe_url(&expanded_url)));
            return Err(msg);
        }
    };
    let duration_ms = start.elapsed().as_secs_f64() * 1000.0;
    let size_bytes = bytes.len();

    crate::log::write(
        &app,
        "HTTP ←",
        &format!(
            "{} {} → {} {} | {} байт | {:.0} мс",
            spec.method,
            crate::log::safe_url(&expanded_url),
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
            size_bytes,
            duration_ms
        ),
    );

    let (body, body_base64) = match String::from_utf8(bytes.to_vec()) {
        Ok(text) => (text, false),
        Err(_) => (
            base64::engine::general_purpose::STANDARD.encode(&bytes),
            true,
        ),
    };

    Ok(HttpResponseData {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or("").to_string(),
        headers,
        body,
        body_base64,
        size_bytes,
        duration_ms,
    })
}

fn format_reqwest_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "Таймаут запроса".to_string()
    } else if e.is_connect() {
        format!("Ошибка соединения: {e}")
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Minimal raw-socket HTTP mock (same pattern `loadtest.rs` tests use):
    /// counts connections and replies with `response` to every one of them.
    async fn spawn_counting_mock(
        hits: Arc<AtomicUsize>,
        response: String,
    ) -> std::net::SocketAddr {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                hits.fetch_add(1, Ordering::SeqCst);
                let response = response.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(response.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn read_body_limited_allows_body_under_the_cap() {
        let hits = Arc::new(AtomicUsize::new(0));
        let addr = spawn_counting_mock(
            hits,
            "HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world".to_string(),
        )
        .await;

        let resp = reqwest::get(format!("http://{addr}/x")).await.unwrap();
        let body = read_body_limited(resp, 1024).await.unwrap();
        assert_eq!(body, b"hello world");
    }

    #[tokio::test]
    async fn read_body_limited_rejects_body_over_the_cap() {
        let big = vec![b'x'; 5000];
        let hits = Arc::new(AtomicUsize::new(0));
        let addr = spawn_counting_mock(
            hits,
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                big.len(),
                String::from_utf8(big).unwrap()
            ),
        )
        .await;

        let resp = reqwest::get(format!("http://{addr}/x")).await.unwrap();
        let err = read_body_limited(resp, 1024).await.unwrap_err();
        assert!(err.contains("превышает"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn redirect_policy_stops_before_leaking_headers_cross_host() {
        // Server B stands in for a different host (different port ⇒ different
        // authority): it must never be contacted, or the redirect would have
        // replayed our request — including any custom secret headers — to it.
        let b_hits = Arc::new(AtomicUsize::new(0));
        let b_addr = spawn_counting_mock(
            b_hits.clone(),
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_string(),
        )
        .await;

        let a_hits = Arc::new(AtomicUsize::new(0));
        let a_addr = spawn_counting_mock(
            a_hits,
            format!(
                "HTTP/1.1 302 Found\r\nLocation: http://{b_addr}/target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            ),
        )
        .await;

        let client = reqwest::Client::builder()
            .redirect(same_host_redirect_policy())
            .build()
            .unwrap();
        let resp = client
            .get(format!("http://{a_addr}/start"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 302, "cross-host redirect must not be followed");
        assert_eq!(
            b_hits.load(Ordering::SeqCst),
            0,
            "the cross-host target must never have been contacted"
        );
    }

    #[tokio::test]
    async fn redirect_policy_stops_on_scheme_downgrade_same_host() {
        // Location keeps host:port identical but switches http -> https:
        // even though a scheme-blind check would call this "same host", it
        // must still be stopped, or secret headers would replay onto
        // whatever answers on that port — here, plaintext instead of TLS.
        // The mock only speaks plain HTTP, so if the policy wrongly followed
        // it, the TLS handshake against it would fail and `send()` would
        // error out instead of cleanly returning the original 302 with a
        // single hit.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_for_mock = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                hits_for_mock.fetch_add(1, Ordering::SeqCst);
                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: https://{addr}/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(response.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });

        let client = reqwest::Client::builder()
            .redirect(same_host_redirect_policy())
            .build()
            .unwrap();
        let resp = client
            .get(format!("http://{addr}/start"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 302, "scheme-changing redirect must not be followed");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "the target must not have been re-contacted over a different scheme"
        );
    }

    #[tokio::test]
    async fn redirect_policy_follows_same_host_redirect() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_for_mock = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { break };
                let n = hits_for_mock.fetch_add(1, Ordering::SeqCst);
                let response = if n == 0 {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://{addr}/next\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_string()
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = sock.read(&mut buf).await;
                    let _ = sock.write_all(response.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });

        let client = reqwest::Client::builder()
            .redirect(same_host_redirect_policy())
            .build()
            .unwrap();
        let resp = client
            .get(format!("http://{addr}/start"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200, "same-host redirect must be followed");
        assert!(hits.load(Ordering::SeqCst) >= 2);
    }
}
