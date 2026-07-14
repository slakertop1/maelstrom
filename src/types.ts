export interface KV {
  id: string;
  key: string;
  value: string;
  enabled: boolean;
  /// Environment variables only: secret values are masked in the UI and exported
  /// to CI configs as ${KEY} placeholders (injected per-system via OS env).
  secret?: boolean;
}

export type BodyType = "none" | "json" | "text" | "form" | "multipart";

export interface MultipartField {
  id: string;
  name: string;
  kind: "text" | "file";
  value: string; // text value, or file path for file parts
  filename: string;
  content_type: string;
  enabled: boolean;
  // File parts only: draw one file per request from a set instead of a fixed path.
  source?: "fixed" | "pool";
  pool_mode?: "random" | "sequential";
  pool_kind?: "folder" | "list" | "url";
  pool_path?: string; // folder
  pool_mask?: string; // folder mask, e.g. "*.jpg,*.png"
  pool_paths?: string; // list — one path per line
  pool_urls?: string; // url/S3 — one URL per line
}

export interface FilePoolSource {
  kind: string; // "folder" | "list" | "url"
  path?: string | null;
  mask?: string | null;
  paths?: string[] | null;
  urls?: string[] | null;
}

export interface FilePoolSpec {
  name: string;
  mode: string; // "random" | "sequential"
  source: FilePoolSource;
}

export interface MultipartPartSpec {
  name: string;
  kind: string;
  value: string;
  filename: string | null;
  content_type: string | null;
  enabled: boolean;
}
export type AuthType = "none" | "bearer" | "basic" | "oauth2";

export type OAuthGrant =
  | "client_credentials"
  | "password"
  | "authorization_code"
  | "refresh_token";

export interface OAuth2Config {
  grant: OAuthGrant;
  auth_url: string; // authorization endpoint (authorization_code / SSO)
  token_url: string;
  client_id: string;
  client_secret: string;
  scope: string;
  username: string;
  password: string;
  client_auth: "basic" | "body";
  // keep the token fresh automatically during load tests
  auto_refresh: boolean;
  // filled in after a successful token fetch
  access_token: string;
  refresh_token: string;
  expires_at: number | null; // epoch ms
}

/// Sent to the backend so it can re-issue the token while a load test runs.
export interface OAuthRefreshSpec {
  grant_type: string;
  token_url: string;
  client_id: string;
  client_secret: string | null;
  scope: string | null;
  username: string | null;
  password: string | null;
  refresh_token: string | null;
  client_auth: string;
}

export interface AuthConfig {
  type: AuthType;
  token: string;
  username: string;
  password: string;
  oauth2: OAuth2Config;
}

/// A named, reusable auth setup: fill the credentials once, save, then apply
/// them to any other request from a dropdown instead of retyping.
export interface AuthProfile {
  id: string;
  name: string;
  auth: AuthConfig;
}

/// Default display name for a profile derived from what identifies it best:
/// OAuth2 → client_id @ token-url host, Basic → username, Bearer → token tail.
export function authProfileName(auth: AuthConfig): string {
  if (auth.type === "oauth2") {
    const o = auth.oauth2;
    let host = "";
    try {
      host = o.token_url ? new URL(o.token_url).host : "";
    } catch {
      host = o.token_url;
    }
    const id = o.client_id || "oauth2";
    return host ? `${id} @ ${host}` : id;
  }
  if (auth.type === "basic") return `basic: ${auth.username || "user"}`;
  if (auth.type === "bearer") {
    const tail = auth.token.trim().slice(-4);
    return tail ? `bearer …${tail}` : "bearer";
  }
  return "auth";
}

export interface TlsConfig {
  enabled: boolean;
  client_cert_pem: string;
  client_key_pem: string;
  ca_cert_pem: string;
  insecure: boolean;
}

export type RequestKind = "http" | "db" | "grpc" | "ws";

export interface WsConfig {
  url: string;
  message: string;
}

export interface WsCallResult {
  messages: string[];
  duration_ms: number;
}

export interface DbConfig {
  driver: "postgres" | "mysql" | "sqlite";
  url: string;
  username: string;
  password: string;
  query: string;
}

export interface GrpcConfig {
  proto_path: string;
  endpoint: string;
  service: string;
  method: string;
  body: string;
  /** Import root dirs (like protoc -I), one per line — for resolving imports. */
  import_paths: string;
}

export interface GrpcMethodInfo {
  service: string;
  method: string;
  path: string;
  client_streaming: boolean;
  server_streaming: boolean;
  input_type: string;
  output_type: string;
}

export interface GrpcCallResult {
  responses: string[];
  server_streaming: boolean;
  duration_ms: number;
}

export interface RequestConfig {
  id: string;
  name: string;
  kind: RequestKind;
  method: string;
  url: string;
  params: KV[];
  headers: KV[];
  body_type: BodyType;
  body: string;
  form_body: KV[];
  multipart_body: MultipartField[];
  auth: AuthConfig;
  tls: TlsConfig;
  db: DbConfig;
  grpc: GrpcConfig;
  ws: WsConfig;
  assertions: import("./assertions").Assertion[];
}

export interface Collection {
  id: string;
  name: string;
  requests: RequestConfig[];
}

export interface Environment {
  id: string;
  name: string;
  variables: KV[];
}

export type DatasetSourceKind = "file" | "url" | "db";

/// AWS credentials to sign a URL/S3 GET (SigV4) so a private bucket works.
export interface AwsAuth {
  access_key_id: string;
  secret_access_key: string;
  session_token?: string | null;
  region: string;
  service?: string | null;
}

export interface Dataset {
  id: string;
  name: string;
  mode: "sequential" | "random";
  source_kind: DatasetSourceKind;
  path: string; // file
  url: string; // s3 / http
  query: string; // db SQL
  db_url: string; // db connection string
  format: "" | "csv" | "json";
  // URL/S3 source only: sign the GET with AWS credentials (private bucket).
  aws_enabled?: boolean;
  aws_region?: string;
  aws_access_key_id?: string;
  aws_secret_access_key?: string;
  aws_session_token?: string;
}

export interface DatasetSpec {
  name: string;
  mode: string;
  source: {
    kind: string;
    rows?: Record<string, string>[] | null;
    path?: string | null;
    url?: string | null;
    format?: string | null;
    query?: string | null;
    aws?: AwsAuth | null;
  };
}

export interface PersistedState {
  collections: Collection[];
  environments: Environment[];
  active_env_id: string | null;
  datasets?: Dataset[];
  /** Недавние Token URL (OAuth) — общая подсказка для всех запросов. */
  token_urls?: string[];
  /** Сохранённые профили авторизации — переиспользуются между ручками. */
  auth_profiles?: AuthProfile[];
}

// ---- mirrors of Rust structs (snake_case) ----

export interface HttpResponseData {
  status: number;
  status_text: string;
  headers: [string, string][];
  body: string;
  body_base64: boolean;
  size_bytes: number;
  duration_ms: number;
}

export interface TlsSpec {
  client_cert_pem: string | null;
  client_key_pem: string | null;
  ca_cert_pem: string | null;
  insecure: boolean;
}

export interface LoadTestSpec {
  method: string;
  url: string;
  headers: [string, string][];
  body: string | null;
  vus: number;
  duration_secs: number;
  rps_limit: number | null;
  timeout_ms: number;
  tls: TlsSpec | null;
  auth_refresh: OAuthRefreshSpec | null;
  multipart: MultipartPartSpec[] | null;
  datasets: DatasetSpec[];
  file_pools: FilePoolSpec[];
}

export interface DbLoadTestSpec {
  url: string;
  query: string;
  vus: number;
  duration_secs: number;
  rps_limit: number | null;
  timeout_ms: number;
  username: string;
  password: string;
}

export interface DbResponse {
  columns: string[];
  rows: string[][];
  row_count: number;
  rows_affected: number | null;
  truncated: boolean;
  duration_ms: number;
}

export interface OAuthTokenResponse {
  access_token: string;
  token_type: string;
  expires_in: number | null;
  refresh_token: string | null;
  scope: string | null;
}

// ---- multi-endpoint scenario load ----

export interface ScenarioTargetSpec {
  name: string;
  method: string;
  url: string;
  headers: [string, string][];
  body: string | null;
  rps: number;
  tls: TlsSpec | null;
  auth_refresh: OAuthRefreshSpec | null;
  multipart: MultipartPartSpec[] | null;
}

export interface ScenarioSpec {
  duration_secs: number;
  timeout_ms: number;
  targets: ScenarioTargetSpec[];
  datasets: DatasetSpec[];
  file_pools: FilePoolSpec[];
}

export interface TargetProgress {
  name: string;
  rps_current: number;
  total: number;
  errors: number;
}

export interface ScenarioProgress {
  elapsed_secs: number;
  overall_total: number;
  overall_errors: number;
  overall_rps: number;
  overall_p95_ms: number;
  targets: TargetProgress[];
}

export interface ScenarioResult {
  started_at: string;
  duration_secs: number;
  actual_duration_ms: number;
  overall: LoadTestResult;
  targets: LoadTestResult[];
  stopped_early: boolean;
}

export interface TimelinePoint {
  sec: number;
  requests: number;
  errors: number;
  avg_ms: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
}

export interface ProgressSnapshot {
  elapsed_secs: number;
  total_requests: number;
  errors: number;
  rps_current: number;
  avg_ms: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
  max_ms: number;
  point: TimelinePoint;
}

export interface HistBucket {
  from_ms: number;
  to_ms: number;
  count: number;
}

export interface LoadTestResult {
  url: string;
  method: string;
  vus: number;
  duration_secs: number;
  rps_limit: number | null;
  started_at: string;
  actual_duration_ms: number;
  total_requests: number;
  errors: number;
  error_rate: number;
  rps_avg: number;
  latency_min_ms: number;
  latency_max_ms: number;
  latency_avg_ms: number;
  p50_ms: number;
  p75_ms: number;
  p90_ms: number;
  p95_ms: number;
  p99_ms: number;
  status_counts: [string, number][];
  timeline: TimelinePoint[];
  histogram: HistBucket[];
  stopped_early: boolean;
  /** Requests the scheduler couldn't place — the target fell behind its RPS. */
  dropped?: number;
}

export function uid(): string {
  return Math.random().toString(36).slice(2, 10) + Date.now().toString(36);
}

export function newKV(): KV {
  return { id: uid(), key: "", value: "", enabled: true };
}

export function newOAuth2(): OAuth2Config {
  return {
    grant: "client_credentials",
    auth_url: "",
    token_url: "",
    client_id: "",
    client_secret: "",
    scope: "",
    username: "",
    password: "",
    client_auth: "body",
    auto_refresh: true,
    access_token: "",
    refresh_token: "",
    expires_at: null,
  };
}

export function newDataset(): Dataset {
  return {
    id: uid(),
    name: "",
    mode: "sequential",
    source_kind: "file",
    path: "",
    url: "",
    query: "",
    db_url: "",
    format: "",
    aws_enabled: false,
    aws_region: "us-east-1",
    aws_access_key_id: "",
    aws_secret_access_key: "",
    aws_session_token: "",
  };
}

export function toDatasetSpec(d: Dataset): DatasetSpec {
  const source: DatasetSpec["source"] = { kind: d.source_kind };
  if (d.source_kind === "file") source.path = d.path;
  else if (d.source_kind === "url") {
    source.url = d.url;
    // Sign the GET for a private S3 bucket when credentials are supplied.
    if (d.aws_enabled && (d.aws_access_key_id ?? "").trim()) {
      source.aws = {
        access_key_id: (d.aws_access_key_id ?? "").trim(),
        secret_access_key: (d.aws_secret_access_key ?? "").trim(),
        region: (d.aws_region ?? "").trim() || "us-east-1",
        session_token: (d.aws_session_token ?? "").trim() || null,
      };
    }
  } else if (d.source_kind === "db") {
    source.url = d.db_url;
    source.query = d.query;
  }
  if (d.format) source.format = d.format;
  return { name: d.name, mode: d.mode, source };
}

/// Split a multi-line textarea into trimmed, non-empty lines.
export function splitLines(text?: string): string[] {
  return (text ?? "")
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter(Boolean);
}

/// Build the engine FilePoolSpec for a "pool"-mode multipart file field.
export function toFilePoolSpec(name: string, f: MultipartField): FilePoolSpec {
  const kind = f.pool_kind ?? "folder";
  const source: FilePoolSource = { kind };
  if (kind === "folder") {
    source.path = f.pool_path ?? "";
    source.mask = f.pool_mask ?? "";
  } else if (kind === "list") {
    source.paths = splitLines(f.pool_paths);
  } else if (kind === "url") {
    source.urls = splitLines(f.pool_urls);
  }
  return { name, mode: f.pool_mode ?? "random", source };
}

export function newMultipartField(kind: "text" | "file" = "text"): MultipartField {
  return {
    id: uid(),
    name: "",
    kind,
    value: "",
    filename: "",
    content_type: "",
    enabled: true,
  };
}

export function newTls(): TlsConfig {
  return {
    enabled: false,
    client_cert_pem: "",
    client_key_pem: "",
    ca_cert_pem: "",
    insecure: false,
  };
}

function defaultAuth(): AuthConfig {
  return { type: "none", token: "", username: "", password: "", oauth2: newOAuth2() };
}

/// Backfill an AuthConfig that may come from an older persisted file (missing
/// oauth2 sub-fields, absent keys) — used for both requests and auth profiles,
/// so applying an old profile can never inject `undefined` into the editor.
export function migrateAuth(a: unknown): AuthConfig {
  const base = defaultAuth();
  const src = (a ?? {}) as Partial<AuthConfig>;
  return { ...base, ...src, oauth2: { ...base.oauth2, ...(src.oauth2 ?? {}) } };
}

export function newRequest(name = "New request"): RequestConfig {
  return {
    id: uid(),
    name,
    kind: "http",
    method: "GET",
    url: "",
    params: [newKV()],
    headers: [newKV()],
    body_type: "none",
    body: "",
    form_body: [newKV()],
    multipart_body: [newMultipartField()],
    auth: defaultAuth(),
    tls: newTls(),
    db: {
      driver: "postgres",
      url: "",
      username: "",
      password: "",
      query: "",
    },
    grpc: {
      proto_path: "",
      endpoint: "",
      service: "",
      method: "",
      body: "",
      import_paths: "",
    },
    ws: {
      url: "",
      message: "",
    },
    assertions: [],
  };
}

/// Backfill fields that older persisted requests may lack.
export function migrateRequest(r: any): RequestConfig {
  const base = newRequest(r?.name ?? "Request");
  return {
    ...base,
    ...r,
    auth: migrateAuth(r?.auth),
    tls: { ...base.tls, ...(r?.tls ?? {}) },
    db: { ...base.db, ...(r?.db ?? {}) },
    grpc: { ...base.grpc, ...(r?.grpc ?? {}) },
    ws: { ...base.ws, ...(r?.ws ?? {}) },
    assertions: Array.isArray(r?.assertions) ? r.assertions : [],
    kind: r?.kind ?? "http",
  };
}
