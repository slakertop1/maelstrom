// Dynamic value providers for load generation. Fields (URL, headers, body,
// multipart text) may contain `{{$...}}` placeholders that are expanded fresh
// on every request:
//
//   {{$randomInt(1,1000)}}     random integer in [min, max]
//   {{$randomFrom(a|b|c)}}     random pick from an inline list
//   {{$randomString(12)}}      random alphanumeric string
//   {{$uuid}}                  random UUID v4
//   {{$timestamp}}             unix seconds (stable within one request)
//   {{$counter}}               increments once per request
//   {{$data.users.email}}      value from a named dataset (a "collection")
//
// A dataset is a collection of rows loaded once from an inline list, a local
// file (CSV/JSON), or a URL (e.g. an S3 object). All `{{$data.NAME.*}}`
// references in one request read the SAME row of NAME.
use crate::types::{DatasetSpec, FilePoolSpec};
use rand::Rng;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub enum PickMode {
    Sequential,
    Random,
}

fn pick(mode: &PickMode, len: usize, cursor: &AtomicUsize) -> usize {
    if len == 0 {
        return 0;
    }
    match mode {
        PickMode::Sequential => cursor.fetch_add(1, Ordering::Relaxed) % len,
        PickMode::Random => rand::thread_rng().gen_range(0..len),
    }
}

pub struct Dataset {
    pub mode: PickMode,
    pub rows: Vec<HashMap<String, String>>,
    pub cursor: AtomicUsize,
}

impl Dataset {
    fn pick_index(&self) -> usize {
        pick(&self.mode, self.rows.len(), &self.cursor)
    }
}

/// One file loaded into memory, ready to be attached to a multipart request.
/// `Bytes` so the pooled file is shared across requests without copying.
pub struct PreparedFile {
    pub bytes: bytes::Bytes,
    pub filename: String,
    pub mime: Option<String>,
}

/// A named set of files a multipart part draws from — one file per request.
pub struct FilePool {
    pub mode: PickMode,
    pub files: Vec<PreparedFile>,
    pub cursor: AtomicUsize,
}

impl FilePool {
    fn pick_index(&self) -> usize {
        pick(&self.mode, self.files.len(), &self.cursor)
    }
}

#[derive(Default)]
pub struct DynState {
    counter: AtomicU64,
    datasets: HashMap<String, Dataset>,
    file_pools: HashMap<String, FilePool>,
}

impl DynState {
    /// True when there is anything dynamic to expand — lets callers skip work.
    pub fn is_empty(&self) -> bool {
        self.datasets.is_empty() && self.file_pools.is_empty()
    }

    pub fn request(&self) -> RequestCtx<'_> {
        RequestCtx {
            state: self,
            counter: self.counter.fetch_add(1, Ordering::Relaxed),
            ts: now_secs(),
            picked: RefCell::new(HashMap::new()),
        }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Per-request expansion context: a fixed counter/timestamp and a stable row
/// choice per dataset for the lifetime of one request.
pub struct RequestCtx<'a> {
    state: &'a DynState,
    counter: u64,
    ts: u64,
    picked: RefCell<HashMap<String, usize>>,
}

impl<'a> RequestCtx<'a> {
    pub fn expand(&self, input: &str) -> String {
        if !input.contains("{{$") {
            return input.to_string();
        }
        // Slice on byte positions returned by `find` (always char boundaries) so
        // non-ASCII (UTF-8) literals between placeholders are preserved verbatim.
        let mut out = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(pos) = rest.find("{{$") {
            out.push_str(&rest[..pos]);
            let after = &rest[pos + 3..];
            match after.find("}}") {
                Some(end) => {
                    out.push_str(&self.eval(&after[..end]));
                    rest = &after[end + 2..];
                }
                None => {
                    // No closing "}}" — leave the marker visible and stop scanning.
                    out.push_str("{{$");
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    fn eval(&self, expr: &str) -> String {
        let expr = expr.trim();
        let (name, args) = match expr.find('(') {
            Some(p) if expr.ends_with(')') => (&expr[..p], &expr[p + 1..expr.len() - 1]),
            _ => (expr, ""),
        };
        match name {
            "randomInt" => {
                let (a, b) = parse_two_ints(args).unwrap_or((0, 100));
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                rand::thread_rng().gen_range(lo..=hi).to_string()
            }
            "randomFrom" => {
                let opts: Vec<&str> = args.split('|').collect();
                if opts.is_empty() {
                    String::new()
                } else {
                    opts[rand::thread_rng().gen_range(0..opts.len())]
                        .trim()
                        .to_string()
                }
            }
            "randomString" => {
                let n: usize = args.trim().parse().unwrap_or(8);
                random_string(n.min(4096))
            }
            "uuid" => uuid_v4(),
            "timestamp" => self.ts.to_string(),
            "counter" => self.counter.to_string(),
            _ if name.starts_with("data.") => self.data_value(&name["data.".len()..]),
            _ => format!("{{{{${expr}}}}}"), // unknown — leave visible for debugging
        }
    }

    /// The file this request should send for the given pool. Stable within one
    /// request (repeated references pick the same file); varies across requests.
    pub fn pick_file(&self, pool: &str) -> Option<&'a PreparedFile> {
        let p = self.state.file_pools.get(pool)?;
        if p.files.is_empty() {
            return None;
        }
        let idx = *self
            .picked
            .borrow_mut()
            .entry(format!("$file:{pool}"))
            .or_insert_with(|| p.pick_index());
        p.files.get(idx)
    }

    fn data_value(&self, path: &str) -> String {
        // path is "datasetName.column"
        let (ds_name, col) = match path.split_once('.') {
            Some(x) => x,
            None => return String::new(),
        };
        let Some(ds) = self.state.datasets.get(ds_name) else {
            return String::new();
        };
        if ds.rows.is_empty() {
            return String::new();
        }
        let idx = *self
            .picked
            .borrow_mut()
            .entry(ds_name.to_string())
            .or_insert_with(|| ds.pick_index());
        ds.rows[idx].get(col).cloned().unwrap_or_default()
    }
}

/// Substitute `{{name}}` with a known chain variable (from request chaining).
/// Unknown names and the dynval generators (`{{$...}}`, whose name starts with
/// `$`) are left intact for the per-request [`RequestCtx::expand`] pass. Empty
/// var maps short-circuit to a borrow-free no-op.
pub fn apply_chain_vars(s: &str, vars: &HashMap<String, String>) -> String {
    if vars.is_empty() || !s.contains("{{") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("{{") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 2..];
        match after.find("}}") {
            Some(end) => {
                let inner = after[..end].trim();
                match vars.get(inner) {
                    Some(val) => out.push_str(val),
                    None => {
                        out.push_str("{{");
                        out.push_str(&after[..end]);
                        out.push_str("}}");
                    }
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_two_ints(args: &str) -> Option<(i64, i64)> {
    let mut it = args.split(',');
    let a = it.next()?.trim().parse().ok()?;
    let b = it.next()?.trim().parse().ok()?;
    Some((a, b))
}

fn random_string(n: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..n).map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char).collect()
}

fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

// ---------- dataset loading ----------

fn pick_mode(mode: &str) -> PickMode {
    if mode == "random" {
        PickMode::Random
    } else {
        PickMode::Sequential
    }
}

/// Resolve dataset and file-pool specs into an in-memory DynState (reads files /
/// fetches URLs once, up front). DB-backed datasets are resolved by the caller
/// into inline rows before this is called. `cancel` lets a Stop hit during a
/// slow/large load abort the wait instead of blocking it until the 10s/300s
/// transport timeouts (or a blocking fs call) finish on their own — see the
/// per-source notes on `load_rows`/`load_pool`/`fetch_signed` for exactly where
/// cancellation is (and, for single blocking fs calls, isn't) checked.
pub async fn resolve(
    specs: &[DatasetSpec],
    pools: &[FilePoolSpec],
    cancel: &CancellationToken,
) -> Result<DynState, String> {
    let mut datasets = HashMap::new();
    for spec in specs {
        if spec.name.trim().is_empty() {
            continue;
        }
        if cancel.is_cancelled() {
            return Err("Загрузка данных отменена".to_string());
        }
        let rows = load_rows(spec, cancel).await?;
        datasets.insert(
            spec.name.clone(),
            Dataset { mode: pick_mode(&spec.mode), rows, cursor: AtomicUsize::new(0) },
        );
    }
    let mut file_pools = HashMap::new();
    for spec in pools {
        if spec.name.trim().is_empty() {
            continue;
        }
        if cancel.is_cancelled() {
            return Err("Загрузка данных отменена".to_string());
        }
        let files = load_pool(spec, cancel).await?;
        if files.is_empty() {
            return Err(format!("Набор файлов «{}»: не найдено ни одного файла", spec.name));
        }
        file_pools.insert(
            spec.name.clone(),
            FilePool { mode: pick_mode(&spec.mode), files, cursor: AtomicUsize::new(0) },
        );
    }
    Ok(DynState { counter: AtomicU64::new(0), datasets, file_pools })
}

/// A `fetch_signed` failure: either the transport itself errored, or `cancel`
/// fired while the request was in flight. Kept separate from `reqwest::Error`
/// (which has no "cancelled" variant of its own) so callers can render a
/// dedicated message instead of a misleading transport-error string.
enum FetchErr {
    Cancelled,
    Http(reqwest::Error),
}

/// GET a URL, optionally signing it with AWS Signature V4 (for a private S3
/// object). Without creds it is a plain GET — a public object or a presigned
/// link, as before. The client sets `Host` to match what the signer signed.
/// Races the request against `cancel` so a Stop hit while connecting/sending
/// returns promptly instead of waiting out the 10s connect timeout. Note this
/// only covers `req.send()`, which reqwest resolves as soon as the response
/// *headers* arrive — it does NOT race the body. Callers that go on to read
/// the body (`resp.bytes()`/`resp.text()`) must race that read against
/// `cancel` themselves (see the "url" branches of `load_rows`/`load_pool`
/// below) so a Stop hit mid-download doesn't sit out the 300s overall
/// timeout.
async fn fetch_signed(
    url: &str,
    aws: Option<&crate::types::AwsAuth>,
    cancel: &CancellationToken,
) -> Result<reqwest::Response, FetchErr> {
    // Bounded, unlike `Client::new()` — a dataset/pool URL that never answers
    // (or a TCP handshake that never completes) used to hang the whole run's
    // start-up forever. `connect_timeout` fails fast on an unreachable host,
    // while the overall `timeout` is generous (5 min) so a legitimately large
    // dataset over a slow link still downloads — `cancel` below is what lets
    // Stop pre-empt that 5-minute cap instead of waiting for it.
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(FetchErr::Http)?;
    let mut req = client.get(url);
    if let Some(a) = aws {
        if let Ok(parsed) = reqwest::Url::parse(url) {
            for (k, v) in crate::awssig::sign_get(&parsed, a, chrono::Utc::now()) {
                req = req.header(k, v);
            }
        }
    }
    tokio::select! {
        _ = cancel.cancelled() => Err(FetchErr::Cancelled),
        r = req.send() => r.map_err(FetchErr::Http),
    }
}

/// Cap on files loaded per pool — a guard against picking a huge directory and
/// exhausting memory.
const MAX_POOL_FILES: usize = 2000;

fn mask_allows(mask: &str, filename: &str) -> bool {
    let mask = mask.trim();
    if mask.is_empty() || mask == "*" || mask == "*.*" {
        return true;
    }
    let lower = filename.to_lowercase();
    mask.split(',').any(|m| {
        let m = m.trim().trim_start_matches('*').to_lowercase();
        !m.is_empty() && lower.ends_with(&m)
    })
}

fn prepared_from_bytes(bytes: Vec<u8>, name_hint: &str) -> PreparedFile {
    let filename = crate::multipart::basename(name_hint);
    let mime = crate::multipart::guess_mime(&filename).map(|s| s.to_string());
    PreparedFile { bytes: bytes::Bytes::from(bytes), filename, mime }
}

async fn load_pool(spec: &FilePoolSpec, cancel: &CancellationToken) -> Result<Vec<PreparedFile>, String> {
    let src = &spec.source;
    let mut out = Vec::new();
    let err = |e: String| format!("Набор файлов «{}»: {e}", spec.name);
    let cancelled = || err("отменено".into());
    match src.kind.as_str() {
        "folder" => {
            let dir = src.path.as_deref().unwrap_or("").trim();
            if dir.is_empty() {
                return Err(err("не указана папка".into()));
            }
            let mask = src.mask.as_deref().unwrap_or("");
            // A directory listing is a single blocking syscall that, once
            // started, cannot be interrupted mid-flight — `cancel` is only
            // checked before it starts and between the per-file reads below.
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            let entries =
                std::fs::read_dir(dir).map_err(|e| err(format!("не читается папка {dir}: {e}")))?;
            let mut paths: Vec<std::path::PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| mask_allows(mask, n))
                        .unwrap_or(false)
                })
                .collect();
            paths.sort();
            for path in paths.into_iter().take(MAX_POOL_FILES) {
                if cancel.is_cancelled() {
                    return Err(cancelled());
                }
                // Same caveat as above: `std::fs::read` of one file cannot be
                // interrupted once it starts — only the gap between files is
                // cancellable.
                let bytes = std::fs::read(&path)
                    .map_err(|e| err(format!("не читается {}: {e}", path.display())))?;
                out.push(prepared_from_bytes(bytes, &path.to_string_lossy()));
            }
        }
        "list" => {
            for path in src.paths.clone().unwrap_or_default().into_iter().take(MAX_POOL_FILES) {
                if cancel.is_cancelled() {
                    return Err(cancelled());
                }
                let path = path.trim();
                if path.is_empty() {
                    continue;
                }
                // See the "folder" branch above: a single blocking read can't
                // be interrupted mid-flight, only between files.
                let bytes = std::fs::read(path)
                    .map_err(|e| err(format!("не читается {path}: {e}")))?;
                out.push(prepared_from_bytes(bytes, path));
            }
        }
        "url" => {
            for url in src.urls.clone().unwrap_or_default().into_iter().take(MAX_POOL_FILES) {
                let url = url.trim();
                if url.is_empty() {
                    continue;
                }
                // Redact the URL in errors — presigned S3 links carry a signature.
                let safe = crate::redact::safe_url(url);
                let resp = fetch_signed(url, spec.source.aws.as_ref(), cancel)
                    .await
                    .map_err(|e| err(format!("{safe} — {}", transport_reason(&e))))?;
                if !resp.status().is_success() {
                    return Err(err(format!("{safe}: HTTP {}", resp.status().as_u16())));
                }
                // `resp` only means the headers arrived — race the body read
                // against `cancel` too, or a Stop hit mid-download would sit
                // out the 300s overall timeout (see fetch_signed's doc).
                let bytes = tokio::select! {
                    _ = cancel.cancelled() => return Err(cancelled()),
                    r = resp.bytes() => {
                        r.map_err(|e| err(format!("{safe} — {}", transport_reason(&FetchErr::Http(e)))))?
                    }
                };
                // Strip any query string when guessing the name/extension.
                let name = url.split(['?', '#']).next().unwrap_or(url);
                out.push(prepared_from_bytes(bytes.to_vec(), name));
            }
        }
        other => return Err(err(format!("неизвестный источник {other}"))),
    }
    Ok(out)
}

/// Coarse, secret-free reason for a failed HTTP fetch. reqwest's own Display
/// embeds the full (possibly presigned) URL, so we never surface it directly.
fn transport_reason(e: &FetchErr) -> &'static str {
    match e {
        FetchErr::Cancelled => "отменено",
        FetchErr::Http(e) if e.is_timeout() => "таймаут",
        FetchErr::Http(e) if e.is_connect() => "ошибка соединения (DNS/TLS/отказ)",
        FetchErr::Http(e) if e.is_body() || e.is_decode() => "ошибка чтения ответа",
        FetchErr::Http(_) => "ошибка запроса",
    }
}

fn fetch_error(name: &str, url: &str, e: &FetchErr) -> String {
    format!("Датасет «{name}»: {} — {}", crate::redact::safe_url(url), transport_reason(e))
}

async fn load_rows(
    spec: &DatasetSpec,
    cancel: &CancellationToken,
) -> Result<Vec<HashMap<String, String>>, String> {
    let src = &spec.source;
    match src.kind.as_str() {
        "inline" => Ok(src.rows.clone().unwrap_or_default()),
        "file" => {
            let path = src.path.as_deref().unwrap_or("").trim();
            if path.is_empty() {
                return Err(format!("Датасет «{}»: не указан путь к файлу", spec.name));
            }
            if cancel.is_cancelled() {
                return Err(format!("Датасет «{}»: отменено", spec.name));
            }
            // A single std::fs::read_to_string is one blocking syscall that
            // can't be interrupted mid-flight — cancellation is only checked
            // before it starts (above).
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("Датасет «{}»: {e}", spec.name))?;
            parse_rows(&text, src.format.as_deref(), path)
        }
        "url" => {
            let url = src.url.as_deref().unwrap_or("").trim();
            if url.is_empty() {
                return Err(format!("Датасет «{}»: не указан URL", spec.name));
            }
            // Never echo the raw URL (may be a presigned S3 link with a signature)
            // or reqwest's Display (which embeds it) — redact + coarse reason.
            let resp = fetch_signed(url, src.aws.as_ref(), cancel)
                .await
                .map_err(|e| fetch_error(&spec.name, url, &e))?;
            // A private S3 GET that isn't authorised returns an XML error *body*
            // with a 4xx status; without this check we'd parse that XML as CSV
            // and silently produce garbage rows.
            if !resp.status().is_success() {
                return Err(format!(
                    "Датасет «{}»: {} — HTTP {}",
                    spec.name,
                    crate::redact::safe_url(url),
                    resp.status().as_u16()
                ));
            }
            // `resp` only means the headers arrived — race the body read
            // against `cancel` too, or a Stop hit mid-download would sit out
            // the 300s overall timeout (see fetch_signed's doc).
            let text = tokio::select! {
                _ = cancel.cancelled() => return Err(fetch_error(&spec.name, url, &FetchErr::Cancelled)),
                r = resp.text() => r.map_err(|e| fetch_error(&spec.name, url, &FetchErr::Http(e)))?,
            };
            parse_rows(&text, src.format.as_deref(), url)
        }
        other => Err(format!("Датасет «{}»: неизвестный источник {other}", spec.name)),
    }
}

fn parse_rows(
    text: &str,
    format: Option<&str>,
    hint: &str,
) -> Result<Vec<HashMap<String, String>>, String> {
    let is_json = match format {
        Some("json") => true,
        Some("csv") => false,
        _ => hint.trim_end().ends_with(".json") || text.trim_start().starts_with('['),
    };
    if is_json {
        parse_json_rows(text)
    } else {
        Ok(parse_csv_rows(text))
    }
}

fn parse_json_rows(text: &str) -> Result<Vec<HashMap<String, String>>, String> {
    let val: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("JSON-датасет: {e}"))?;
    let arr = val.as_array().ok_or("JSON-датасет должен быть массивом объектов")?;
    let mut rows = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        // Reject non-object elements loudly. Silently pushing an empty row here
        // turned `[1,2,3]` into rows whose {{$data.*}} expanded to "" — data that
        // looked like the target misbehaving under load.
        let obj = item.as_object().ok_or_else(|| {
            format!("JSON-датасет: элемент #{} не объект (нужен массив объектов)", i + 1)
        })?;
        let mut row = HashMap::new();
        for (k, v) in obj {
            row.insert(k.clone(), json_scalar(v));
        }
        rows.push(row);
    }
    Ok(rows)
}

fn json_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Minimal CSV: first row is the header. Handles double-quoted fields with
/// embedded commas and doubled quotes.
fn parse_csv_rows(text: &str) -> Vec<HashMap<String, String>> {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let headers: Vec<String> = match lines.next() {
        Some(h) => split_csv_line(h),
        None => return Vec::new(),
    };
    lines
        .map(|line| {
            let cells = split_csv_line(line);
            headers
                .iter()
                .enumerate()
                .map(|(i, h)| (h.clone(), cells.get(i).cloned().unwrap_or_default()))
                .collect()
        })
        .collect()
}

fn split_csv_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            }
            '"' => in_quotes = true,
            ',' if !in_quotes => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DatasetSource, DatasetSpec};

    fn state_with(name: &str, mode: PickMode, rows: Vec<HashMap<String, String>>) -> DynState {
        let mut datasets = HashMap::new();
        datasets.insert(name.to_string(), Dataset { mode, rows, cursor: AtomicUsize::new(0) });
        DynState { counter: AtomicU64::new(0), datasets, file_pools: HashMap::new() }
    }

    fn row(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn plain_text_passes_through() {
        let s = DynState::default();
        assert_eq!(s.request().expand("no placeholders"), "no placeholders");
    }

    #[test]
    fn random_int_stays_in_range() {
        let s = DynState::default();
        for _ in 0..500 {
            let v: i64 = s.request().expand("{{$randomInt(5,9)}}").parse().unwrap();
            assert!((5..=9).contains(&v), "got {v}");
        }
    }

    #[test]
    fn random_int_handles_reversed_bounds() {
        let s = DynState::default();
        let v: i64 = s.request().expand("{{$randomInt(9,5)}}").parse().unwrap();
        assert!((5..=9).contains(&v));
    }

    #[test]
    fn random_from_picks_listed_option() {
        let s = DynState::default();
        let opts = ["red", "green", "blue"];
        for _ in 0..200 {
            let v = s.request().expand("{{$randomFrom(red|green|blue)}}");
            assert!(opts.contains(&v.as_str()), "got {v}");
        }
    }

    #[test]
    fn uuid_v4_shape() {
        let s = DynState::default();
        let u = s.request().expand("{{$uuid}}");
        assert_eq!(u.len(), 36);
        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), vec![8, 4, 4, 4, 12]);
        assert_eq!(&u[14..15], "4"); // version nibble
    }

    #[test]
    fn random_string_length() {
        let s = DynState::default();
        assert_eq!(s.request().expand("{{$randomString(20)}}").len(), 20);
    }

    #[test]
    fn counter_is_stable_within_request_and_increments_across() {
        let s = DynState::default();
        let a = s.request().expand("{{$counter}}-{{$counter}}");
        assert_eq!(a, "0-0"); // same value twice in one request
        let b = s.request().expand("{{$counter}}");
        assert_eq!(b, "1");
    }

    #[test]
    fn unknown_generator_left_visible() {
        let s = DynState::default();
        assert_eq!(s.request().expand("{{$nope}}"), "{{$nope}}");
    }

    #[test]
    fn preserves_non_ascii_around_placeholders() {
        // Regression: byte-by-byte expansion corrupted multibyte UTF-8 when the
        // string also contained a {{$...}} placeholder. Fresh state per case so
        // {{$counter}} is 0 each time.
        assert_eq!(
            DynState::default().request().expand("Привет {{$counter}} мир — café ünïcode"),
            "Привет 0 мир — café ünïcode"
        );
        // Placeholder at the very end, non-ASCII before it.
        assert_eq!(
            DynState::default().request().expand("значение={{$counter}}"),
            "значение=0"
        );
        // Unclosed marker keeps the non-ASCII intact.
        assert_eq!(
            DynState::default().request().expand("тест {{$oops"),
            "тест {{$oops"
        );
    }

    #[test]
    fn dataset_sequential_cycles_and_row_is_consistent() {
        let rows = vec![
            row(&[("name", "alice"), ("city", "Berlin")]),
            row(&[("name", "bob"), ("city", "Prague")]),
        ];
        let s = state_with("people", PickMode::Sequential, rows);
        // within one request, both refs to the dataset use the SAME row
        assert_eq!(s.request().expand("{{$data.people.name}}@{{$data.people.city}}"), "alice@Berlin");
        assert_eq!(s.request().expand("{{$data.people.name}}"), "bob");
        assert_eq!(s.request().expand("{{$data.people.name}}"), "alice"); // wraps around
    }

    #[test]
    fn missing_dataset_or_column_is_empty() {
        let s = state_with("people", PickMode::Sequential, vec![row(&[("name", "x")])]);
        assert_eq!(s.request().expand("{{$data.people.nope}}"), "");
        assert_eq!(s.request().expand("{{$data.other.name}}"), "");
    }

    #[test]
    fn parse_csv_basic_and_quoted() {
        let csv = "name,note\nalice,hello\n\"bob,jr\",\"has, comma\"\n";
        let rows = parse_csv_rows(csv);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "alice");
        assert_eq!(rows[1]["name"], "bob,jr");
        assert_eq!(rows[1]["note"], "has, comma");
    }

    #[test]
    fn parse_json_array_of_objects() {
        let json = r#"[{"sku":"A","price":10},{"sku":"B","price":20}]"#;
        let rows = parse_json_rows(json).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["sku"], "A");
        assert_eq!(rows[0]["price"], "10"); // numbers stringified
    }

    #[test]
    fn parse_json_rejects_non_object_elements() {
        // A bare-scalar array must error, not silently become empty rows.
        let err = parse_json_rows("[1,2,3]").unwrap_err();
        assert!(err.contains("не объект"), "{err}");
        // A mix (object then scalar) also errors, pointing at the bad element.
        assert!(parse_json_rows(r#"[{"a":1},"oops"]"#).is_err());
        // Not an array at all → the existing top-level error.
        assert!(parse_json_rows(r#"{"a":1}"#).is_err());
    }

    /// Regression: cancelling while the response *body* is still trickling in
    /// (headers already received) used to be a no-op — `fetch_signed` only
    /// raced `cancel` against `req.send()`, which reqwest resolves as soon as
    /// headers arrive, so `load_rows`'s subsequent `resp.text().await` had no
    /// race at all and would sit out the full 300s timeout. The server below
    /// sends headers with a `Content-Length` promise it never fulfils, so a
    /// non-cancelling read would hang for the whole 300s; bounding the test in
    /// `tokio::time::timeout` at 5s proves cancellation — not the transport
    /// timeout — is what unblocks it.
    #[tokio::test]
    async fn load_rows_url_cancel_during_body_read_returns_promptly() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await; // drain the request
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\n")
                    .await;
                // No body bytes follow — simulate a stalled/slow transfer and
                // hold the socket open well past the test's own timeout.
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });

        let spec = DatasetSpec {
            name: "slow".into(),
            mode: "sequential".into(),
            source: DatasetSource {
                kind: "url".into(),
                rows: None,
                path: None,
                url: Some(format!("http://{addr}/data.csv")),
                format: Some("csv".into()),
                query: None,
                aws: None,
            },
        };

        let cancel = CancellationToken::new();
        let cancel2 = cancel.clone();
        tokio::spawn(async move {
            // Give the header round-trip time to land before cancelling, so
            // the race under test is against the body read, not the send.
            tokio::time::sleep(Duration::from_millis(150)).await;
            cancel2.cancel();
        });

        let result = tokio::time::timeout(Duration::from_secs(5), load_rows(&spec, &cancel))
            .await
            .expect("cancel should make load_rows return well before the 300s transport timeout");
        let err = result.unwrap_err();
        assert!(err.contains("отменено"), "{err}");
    }

    #[tokio::test]
    async fn resolve_inline_dataset() {
        let spec = DatasetSpec {
            name: "u".into(),
            mode: "sequential".into(),
            source: DatasetSource {
                kind: "inline".into(),
                rows: Some(vec![row(&[("id", "1")]), row(&[("id", "2")])]),
                path: None,
                url: None,
                format: None,
                query: None,
                aws: None,
            },
        };
        let state = resolve(std::slice::from_ref(&spec), &[], &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(state.request().expand("{{$data.u.id}}"), "1");
        assert_eq!(state.request().expand("{{$data.u.id}}"), "2");
    }

    #[test]
    fn mask_allows_by_extension() {
        assert!(mask_allows("*.jpg,*.png", "photo.JPG"));
        assert!(mask_allows("", "anything.bin"));
        assert!(mask_allows("*", "anything.bin"));
        assert!(!mask_allows("*.png", "doc.pdf"));
    }

    #[tokio::test]
    async fn file_pool_from_folder_picks_per_request() {
        let dir = std::env::temp_dir().join(format!("maelstrom-pool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"AAAA").unwrap();
        std::fs::write(dir.join("b.png"), b"BBBBBB").unwrap();
        std::fs::write(dir.join("skip.txt"), b"nope").unwrap();

        let spec = FilePoolSpec {
            name: "imgs".into(),
            mode: "sequential".into(),
            source: crate::types::FilePoolSource {
                kind: "folder".into(),
                path: Some(dir.to_string_lossy().into_owned()),
                mask: Some("*.png".into()),
                paths: None,
                urls: None,
                aws: None,
            },
        };
        let state = resolve(&[], std::slice::from_ref(&spec), &CancellationToken::new())
            .await
            .unwrap();
        // Only the two .png files are in the pool; sequential order is sorted.
        let r1 = state.request();
        let f1 = r1.pick_file("imgs").unwrap();
        assert_eq!(f1.filename, "a.png");
        assert_eq!(f1.mime.as_deref(), Some("image/png"));
        // Same request -> same file even on repeated reference.
        assert_eq!(r1.pick_file("imgs").unwrap().filename, "a.png");
        // Next request -> next file.
        assert_eq!(state.request().pick_file("imgs").unwrap().filename, "b.png");
        // Unknown pool -> None.
        assert!(state.request().pick_file("nope").is_none());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
