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
    let dyn_state = maelstrom_core::dynval::resolve(&datasets, &spec.file_pools).await?;
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
        .user_agent("Maelstrom/0.1");
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
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
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
