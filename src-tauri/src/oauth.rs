use base64::Engine;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// The token types and non-interactive fetch/refresh live in the shared engine.
pub use maelstrom_core::types::{OAuthTokenRequest, OAuthTokenResponse};

/// Thin command wrapper: fetch a token for a non-interactive grant.
#[tauri::command]
pub async fn fetch_oauth_token(
    app: AppHandle,
    spec: OAuthTokenRequest,
) -> Result<OAuthTokenResponse, String> {
    // Log the attempt — grant, endpoint, client_id — but never the token/secret.
    crate::log::write(
        &app,
        "OAUTH →",
        &format!(
            "grant={} token_url={} client_id={}",
            spec.grant_type,
            crate::log::safe_url(&spec.token_url),
            spec.client_id
        ),
    );
    match maelstrom_core::oauth::fetch_token(&spec).await {
        Ok(r) => {
            crate::log::write(
                &app,
                "OAUTH ←",
                &format!("токен получен (expires_in={:?})", r.expires_in),
            );
            Ok(r)
        }
        Err(e) => {
            crate::log::write(&app, "OAUTH ✗", &e);
            Err(e)
        }
    }
}


// ---------- Authorization Code + PKCE (SSO through the system browser) ----------

#[derive(Deserialize)]
pub struct AuthCodeSpec {
    pub auth_url: String,
    pub token_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scope: Option<String>,
    pub client_auth: String,
}

fn random_urlsafe(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

#[tauri::command]
pub async fn oauth_authorization_code(
    app: AppHandle,
    spec: AuthCodeSpec,
) -> Result<OAuthTokenResponse, String> {
    let verifier = random_urlsafe(48);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let state = random_urlsafe(24);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("Не удалось открыть локальный порт: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let mut auth_url =
        url::Url::parse(spec.auth_url.trim()).map_err(|e| format!("Неверный auth URL: {e}"))?;
    auth_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &spec.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    if let Some(scope) = &spec.scope {
        if !scope.trim().is_empty() {
            auth_url.query_pairs_mut().append_pair("scope", scope);
        }
    }

    app.opener()
        .open_url(auth_url.as_str(), None::<String>)
        .map_err(|e| format!("Не удалось открыть браузер: {e}"))?;

    // Wait for the browser redirect (up to 3 minutes).
    let code = tokio::time::timeout(
        std::time::Duration::from_secs(180),
        wait_for_code(listener, &state),
    )
    .await
    .map_err(|_| "Время ожидания входа истекло (3 мин)".to_string())??;

    let mut params: Vec<(String, String)> = vec![
        ("grant_type".into(), "authorization_code".into()),
        ("code".into(), code),
        ("redirect_uri".into(), redirect_uri),
        ("code_verifier".into(), verifier),
    ];
    let basic = if spec.client_auth == "basic" {
        Some((spec.client_id.as_str(), spec.client_secret.as_deref()))
    } else {
        params.push(("client_id".into(), spec.client_id.clone()));
        if let Some(secret) = &spec.client_secret {
            if !secret.is_empty() {
                params.push(("client_secret".into(), secret.clone()));
            }
        }
        None
    };
    maelstrom_core::oauth::post_token(&spec.token_url, params, basic).await
}

async fn wait_for_code(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String, String> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| format!("Ошибка локального сервера: {e}"))?;
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buf[..n]);
        let first_line = request.lines().next().unwrap_or("");
        // Expect: GET /callback?code=...&state=... HTTP/1.1
        let path = first_line.split_whitespace().nth(1).unwrap_or("");
        if !path.starts_with("/callback") {
            let _ = stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await;
            continue;
        }
        let query = path.split_once('?').map_or("", |x| x.1);
        let result = parse_callback_query(query, expected_state);

        let body = if result.is_ok() {
            "<h2>✓ Вход выполнен</h2><p>Можно закрыть вкладку и вернуться в Maelstrom.</p>"
        } else {
            "<h2>Вход не выполнен</h2><p>Можно закрыть вкладку и вернуться в Maelstrom.</p>"
        };
        let html = format!(
            "<!doctype html><meta charset=utf-8><body style=\"font-family:sans-serif;background:#17181c;color:#e6e8ec;display:flex;align-items:center;justify-content:center;height:100vh;text-align:center\"><div>{body}</div>"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            html.len(),
            html
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;

        return result;
    }
}

/// Parse the OAuth redirect query: validate `state` (CSRF defence), surface a
/// provider `error=`, and return the authorization `code`. Pure (no I/O) so the
/// security-critical checks are unit-testable.
fn parse_callback_query(query: &str, expected_state: &str) -> Result<String, String> {
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        let decoded = percent_decode(v);
        match k {
            "code" => code = Some(decoded),
            "state" => state = Some(decoded),
            "error" => error = Some(decoded),
            _ => {}
        }
    }
    if let Some(err) = error {
        return Err(format!("Сервер авторизации вернул ошибку: {err}"));
    }
    // Reject a missing or mismatched state — this is the CSRF defence for the flow.
    if state.as_deref() != Some(expected_state) {
        return Err("Несовпадение state — возможная CSRF-атака, вход отклонён".to_string());
    }
    match code {
        Some(code) => Ok(code),
        None => Err("В ответе авторизации нет кода".to_string()),
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{parse_callback_query, percent_decode};

    #[test]
    fn accepts_code_when_state_matches() {
        assert_eq!(parse_callback_query("code=abc123&state=xyz", "xyz").unwrap(), "abc123");
    }

    #[test]
    fn rejects_state_mismatch_as_csrf() {
        let err = parse_callback_query("code=abc&state=WRONG", "xyz").unwrap_err();
        assert!(err.contains("CSRF"), "{err}");
        // A missing state must also be rejected, not silently accepted.
        assert!(parse_callback_query("code=abc", "xyz").is_err());
    }

    #[test]
    fn surfaces_provider_error_and_never_returns_a_code() {
        let err = parse_callback_query("error=access_denied&state=xyz", "xyz").unwrap_err();
        assert!(err.contains("access_denied"), "{err}");
        // Even if an attacker also supplies a code, an error response yields no code.
        assert!(parse_callback_query("error=denied&code=evil&state=xyz", "xyz").is_err());
    }

    #[test]
    fn errors_when_code_absent_but_state_ok() {
        assert!(parse_callback_query("state=xyz", "xyz").is_err());
    }

    #[test]
    fn state_is_percent_decoded_before_comparison() {
        // The provider may percent-encode state; it must still match the raw value.
        assert_eq!(parse_callback_query("code=c&state=a%2Fb", "a/b").unwrap(), "c");
    }

    #[test]
    fn percent_decoding_handles_escapes_and_malformed_input() {
        assert_eq!(percent_decode("a%20b+c%2Fd"), "a b c/d");
        // Truncated / invalid escapes are left as-is and never panic.
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%ZZ"), "%ZZ");
        assert_eq!(percent_decode("plain"), "plain");
    }
}
