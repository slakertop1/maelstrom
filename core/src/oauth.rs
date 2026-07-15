use crate::types::{OAuthTokenRequest, OAuthTokenResponse};
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
struct RawTokenResponse {
    access_token: String,
    #[serde(default)]
    token_type: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

/// POST an arbitrary token request (form-encoded) and parse the response. Used
/// by the non-interactive grants here and by the app's authorization_code flow.
pub async fn post_token(
    token_url: &str,
    params: Vec<(String, String)>,
    basic: Option<(&str, Option<&str>)>,
) -> Result<OAuthTokenResponse, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client
        .post(token_url)
        .header("Accept", "application/json")
        .form(&params);
    if let Some((id, secret)) = basic {
        req = req.basic_auth(id, secret);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("Запрос токена не удался: {}", crate::error_chain(&e)))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    // Never echo the raw response body: token-endpoint error/parse-failure
    // bodies can contain an echoed client_secret, refresh_token or password
    // (some providers reflect the request back), and this message ends up in
    // logs. Surface only the status code and body length.
    if !status.is_success() {
        return Err(format!(
            "Сервер токенов вернул {} (тело ответа скрыто, {} байт)",
            status.as_u16(),
            text.len()
        ));
    }
    let raw: RawTokenResponse = serde_json::from_str(&text).map_err(|_| {
        format!(
            "Не удалось разобрать ответ сервера токенов (статус {}, {} байт)",
            status.as_u16(),
            text.len()
        )
    })?;
    Ok(OAuthTokenResponse {
        access_token: raw.access_token,
        token_type: raw.token_type.unwrap_or_else(|| "Bearer".to_string()),
        expires_in: raw.expires_in,
        refresh_token: raw.refresh_token,
        scope: raw.scope,
    })
}

/// Fetch a token for a non-interactive grant (client_credentials / password /
/// refresh_token).
pub async fn fetch_token(spec: &OAuthTokenRequest) -> Result<OAuthTokenResponse, String> {
    let mut params: Vec<(String, String)> = vec![("grant_type".into(), spec.grant_type.clone())];
    match spec.grant_type.as_str() {
        "client_credentials" => {}
        "password" => {
            params.push(("username".into(), spec.username.clone().unwrap_or_default()));
            params.push(("password".into(), spec.password.clone().unwrap_or_default()));
        }
        "refresh_token" => {
            params.push((
                "refresh_token".into(),
                spec.refresh_token.clone().ok_or("Нет refresh_token")?,
            ));
        }
        other => return Err(format!("Неподдерживаемый grant_type: {other}")),
    }
    if let Some(scope) = &spec.scope {
        if !scope.trim().is_empty() {
            params.push(("scope".into(), scope.clone()));
        }
    }

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

    post_token(&spec.token_url, params, basic).await
}

/// Fetch an initial token, then keep it fresh in the background until `cancel`
/// fires. Refreshes at 80% of the token's TTL and invokes `on_refresh(count)`
/// after each successful renewal, or `on_error(message)` after each failed one
/// (in addition to the `eprintln!` trace, since a packaged GUI build has no
/// visible stderr — `on_error` is how the failure reaches the app's own log/UI).
/// Returns the shared token cell that workers read on every request.
pub async fn start_token_refresher(
    mut cfg: OAuthTokenRequest,
    cancel: CancellationToken,
    on_refresh: Arc<dyn Fn(u64) + Send + Sync>,
    on_error: Arc<dyn Fn(String) + Send + Sync>,
) -> Result<Arc<RwLock<String>>, String> {
    let first = fetch_token(&cfg).await?;
    let value = Arc::new(RwLock::new(first.access_token));
    let value_bg = value.clone();

    if let Some(rt) = first.refresh_token {
        cfg.refresh_token = Some(rt);
        if cfg.grant_type == "authorization_code" {
            cfg.grant_type = "refresh_token".to_string();
        }
    }
    // TTL (seconds) from the server; refresh at 80% of it. Clamp the wait, not
    // the TTL, so short-lived tokens are still renewed in time.
    let mut ttl = first.expires_in.unwrap_or(3600);

    tokio::spawn(async move {
        let mut count: u64 = 0;
        // Last TTL that came from a successful fetch: after a transient failure
        // (ttl=5 for a quick retry) a success without expires_in must restore
        // it, not keep hammering the token endpoint every 4 seconds.
        let mut good_ttl = ttl;
        // Capped exponential backoff for consecutive refresh failures: 5s,
        // 10s, 20s, ... up to 300s, so a token-endpoint outage doesn't get
        // hammered at a fixed 5s interval. Reset to 5s after any success.
        let mut fail_backoff: u64 = 5;
        loop {
            let wait = (ttl as f64 * 0.8).max(0.5);
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(std::time::Duration::from_secs_f64(wait)) => {}
            }
            if cancel.is_cancelled() {
                break;
            }
            match fetch_token(&cfg).await {
                Ok(r) => {
                    *value_bg.write().await = r.access_token;
                    if let Some(rt) = r.refresh_token {
                        cfg.refresh_token = Some(rt);
                    }
                    ttl = r.expires_in.unwrap_or(good_ttl);
                    good_ttl = ttl;
                    fail_backoff = 5;
                    count += 1;
                    on_refresh(count);
                }
                Err(e) => {
                    // fetch_token's error text is already stripped of any
                    // raw response body (see post_token), so it's safe to
                    // log as-is.
                    let msg = format!(
                        "[oauth] фоновое обновление токена не удалось, повтор через {fail_backoff}с: {e}"
                    );
                    eprintln!("{msg}");
                    on_error(msg);
                    ttl = fail_backoff;
                    fail_backoff = (fail_backoff * 2).min(300);
                }
            }
        }
    });

    Ok(value)
}
