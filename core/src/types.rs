use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------- data providers ----------

/// AWS credentials for reading a URL source from a PRIVATE S3 bucket. When set,
/// the GET is signed with Signature Version 4 (see `awssig`) instead of being a
/// plain request — so private objects work, not only public / presigned URLs.
/// In a CLI config the secret fields are typically `${VAR}` placeholders that
/// `expand_env` fills from the environment, keeping keys out of the file.
#[derive(Deserialize, Serialize, Clone)]
pub struct AwsAuth {
    pub access_key_id: String,
    pub secret_access_key: String,
    #[serde(default)]
    pub session_token: Option<String>,
    #[serde(default = "default_aws_region")]
    pub region: String,
    #[serde(default)]
    pub service: Option<String>, // defaults to "s3"
}

fn default_aws_region() -> String {
    "us-east-1".to_string()
}

#[derive(Deserialize, Serialize, Clone)]
pub struct DatasetSource {
    pub kind: String, // "inline" | "file" | "url" | "db"
    #[serde(default)]
    pub rows: Option<Vec<HashMap<String, String>>>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub format: Option<String>, // "csv" | "json" (auto when absent)
    #[serde(default)]
    pub query: Option<String>, // SQL for kind = "db"
    #[serde(default)]
    pub aws: Option<AwsAuth>, // sign the URL GET for a private S3 bucket
}

#[derive(Deserialize, Serialize, Clone)]
pub struct DatasetSpec {
    pub name: String,
    #[serde(default)]
    pub mode: String, // "sequential" | "random"
    pub source: DatasetSource,
}

// ---------- file pools (a set of files a multipart part draws from per request) ----------

#[derive(Deserialize, Serialize, Clone)]
pub struct FilePoolSource {
    pub kind: String, // "folder" | "list" | "url"
    #[serde(default)]
    pub path: Option<String>, // folder
    #[serde(default)]
    pub mask: Option<String>, // e.g. "*.jpg,*.png" (folder); empty/"*" = all
    #[serde(default)]
    pub paths: Option<Vec<String>>, // explicit local file list
    #[serde(default)]
    pub urls: Option<Vec<String>>, // remote objects (S3 presigned / public http)
    #[serde(default)]
    pub aws: Option<AwsAuth>, // sign the URL GETs for a private S3 bucket
}

#[derive(Deserialize, Serialize, Clone)]
pub struct FilePoolSpec {
    pub name: String,
    #[serde(default)]
    pub mode: String, // "random" | "sequential"
    pub source: FilePoolSource,
}

// ---------- TLS ----------

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct TlsConfig {
    pub client_cert_pem: Option<String>,
    pub client_key_pem: Option<String>,
    pub ca_cert_pem: Option<String>,
    #[serde(default)]
    pub insecure: bool,
}

// ---------- OAuth ----------

#[derive(Deserialize, Serialize, Clone)]
pub struct OAuthTokenRequest {
    pub grant_type: String, // client_credentials | password | refresh_token
    pub token_url: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub scope: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub refresh_token: Option<String>,
    pub client_auth: String, // "basic" | "body"
}

#[derive(Serialize, Clone)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: Option<u64>,
    pub refresh_token: Option<String>,
    pub scope: Option<String>,
}

// ---------- multipart ----------

#[derive(Deserialize, Serialize, Clone)]
pub struct MultipartPart {
    pub name: String,
    pub kind: String, // "text" | "file"
    pub value: String, // text value, or file path for file parts
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

// ---------- results ----------

#[derive(Serialize, Deserialize, Clone)]
pub struct TimelinePoint {
    pub sec: u64,
    pub requests: u64,
    pub errors: u64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct HistBucket {
    pub from_ms: f64,
    pub to_ms: f64,
    pub count: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct LoadTestResult {
    pub url: String,
    pub method: String,
    pub vus: usize,
    pub duration_secs: u64,
    pub rps_limit: Option<u32>,
    pub started_at: String,
    pub actual_duration_ms: f64,
    pub total_requests: u64,
    pub errors: u64,
    pub error_rate: f64,
    pub rps_avg: f64,
    pub latency_min_ms: f64,
    pub latency_max_ms: f64,
    pub latency_avg_ms: f64,
    pub p50_ms: f64,
    pub p75_ms: f64,
    pub p90_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub status_counts: Vec<(String, u64)>,
    pub timeline: Vec<TimelinePoint>,
    pub histogram: Vec<HistBucket>,
    pub stopped_early: bool,
    /// Requests the open-model scheduler wanted to send but couldn't place —
    /// the target fell behind its configured RPS and the in-flight cap was hit.
    /// A non-zero value means the achieved RPS is below target; surfaced so the
    /// shortfall isn't silent.
    #[serde(default)]
    pub dropped: u64,
}

/// Describes what is being load-tested; feeds the result metadata.
pub struct RunMeta {
    pub target: String,
    pub kind: String, // HTTP method, "SQL" for DB runs, "MIX" for scenario overall
    pub vus: usize,
    pub duration_secs: u64,
    pub rps_limit: Option<u32>,
}

// ---------- scenario (multi-endpoint) ----------

#[derive(Deserialize, Serialize, Clone)]
pub struct ScenarioTarget {
    pub name: String,
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: Option<String>,
    pub rps: u32,
    #[serde(default)]
    pub tls: Option<TlsConfig>,
    #[serde(default)]
    pub auth_refresh: Option<OAuthTokenRequest>,
    #[serde(default)]
    pub multipart: Option<Vec<MultipartPart>>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ScenarioSpec {
    pub duration_secs: u64,
    pub timeout_ms: u64,
    pub targets: Vec<ScenarioTarget>,
    #[serde(default)]
    pub datasets: Vec<DatasetSpec>,
    #[serde(default)]
    pub file_pools: Vec<FilePoolSpec>,
}

#[derive(Serialize, Clone)]
pub struct TargetProgress {
    pub name: String,
    pub rps_current: f64,
    pub total: u64,
    pub errors: u64,
}

#[derive(Serialize, Clone)]
pub struct ScenarioProgress {
    pub elapsed_secs: f64,
    pub overall_total: u64,
    pub overall_errors: u64,
    pub overall_rps: f64,
    pub overall_p95_ms: f64,
    pub targets: Vec<TargetProgress>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ScenarioResult {
    pub started_at: String,
    pub duration_secs: u64,
    pub actual_duration_ms: f64,
    pub overall: LoadTestResult,
    pub targets: Vec<LoadTestResult>,
    pub stopped_early: bool,
}
