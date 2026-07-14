import { invoke } from "@tauri-apps/api/core";
import type {
  DatasetSpec,
  DbLoadTestSpec,
  DbResponse,
  FilePoolSpec,
  GrpcCallResult,
  GrpcMethodInfo,
  HttpResponseData,
  LoadTestSpec,
  OAuth2Config,
  MultipartPartSpec,
  OAuthTokenResponse,
  PersistedState,
  ScenarioSpec,
  TlsSpec,
  WsCallResult,
} from "./types";

export async function sendRequest(spec: {
  method: string;
  url: string;
  headers: [string, string][];
  body: string | null;
  timeout_ms?: number;
  tls?: TlsSpec | null;
  multipart?: MultipartPartSpec[] | null;
  file_pools?: FilePoolSpec[];
  datasets?: DatasetSpec[];
}): Promise<HttpResponseData> {
  return invoke<HttpResponseData>("send_request", { spec });
}

export async function startLoadTest(spec: LoadTestSpec): Promise<void> {
  return invoke("start_load_test", { spec });
}

export async function stopLoadTest(): Promise<void> {
  return invoke("stop_load_test");
}

export async function startScenarioLoadTest(spec: ScenarioSpec): Promise<void> {
  return invoke("start_scenario_load_test", { spec });
}

export async function dbExecute(spec: {
  url: string;
  query: string;
  timeout_ms?: number;
  username?: string;
  password?: string;
}): Promise<DbResponse> {
  return invoke<DbResponse>("db_execute", { spec });
}

export async function startDbLoadTest(spec: DbLoadTestSpec): Promise<void> {
  return invoke("start_db_load_test", { spec });
}

// ---- gRPC ----

export async function grpcListMethods(
  proto_path: string,
  includes: string[] = []
): Promise<GrpcMethodInfo[]> {
  return invoke<GrpcMethodInfo[]>("grpc_list_methods", { proto: { proto_path, includes } });
}

export async function grpcRequestTemplate(
  proto_path: string,
  service: string,
  method: string,
  includes: string[] = []
): Promise<string> {
  return invoke<string>("grpc_request_template", {
    proto: { proto_path, includes },
    service,
    method,
  });
}

export async function grpcCall(spec: {
  endpoint: string;
  proto_path: string;
  includes?: string[];
  service: string;
  method: string;
  body: string;
  timeout_ms?: number;
}): Promise<GrpcCallResult> {
  return invoke<GrpcCallResult>("grpc_call", { spec });
}

export async function grpcStartLoad(spec: {
  endpoint: string;
  proto_path: string;
  includes?: string[];
  service: string;
  method: string;
  body: string;
  vus: number;
  duration_secs: number;
  rps_limit: number | null;
  timeout_ms: number;
}): Promise<void> {
  return invoke("grpc_start_load", { spec });
}

// ---- WebSocket ----

export async function wsCall(spec: {
  url: string;
  message: string;
  timeout_ms?: number;
}): Promise<WsCallResult> {
  return invoke<WsCallResult>("ws_call", { spec });
}

export async function wsStartLoad(spec: {
  url: string;
  message: string;
  vus: number;
  duration_secs: number;
  rps_limit: number | null;
  timeout_ms: number;
}): Promise<void> {
  return invoke("ws_start_load", { spec });
}

export async function fetchOAuthToken(cfg: OAuth2Config): Promise<OAuthTokenResponse> {
  return invoke<OAuthTokenResponse>("fetch_oauth_token", {
    spec: {
      grant_type: cfg.grant,
      token_url: cfg.token_url,
      client_id: cfg.client_id,
      client_secret: cfg.client_secret || null,
      scope: cfg.scope || null,
      username: cfg.username || null,
      password: cfg.password || null,
      refresh_token: cfg.refresh_token || null,
      client_auth: cfg.client_auth,
    },
  });
}

export async function oauthAuthorizationCode(
  cfg: OAuth2Config
): Promise<OAuthTokenResponse> {
  return invoke<OAuthTokenResponse>("oauth_authorization_code", {
    spec: {
      auth_url: cfg.auth_url,
      token_url: cfg.token_url,
      client_id: cfg.client_id,
      client_secret: cfg.client_secret || null,
      scope: cfg.scope || null,
      client_auth: cfg.client_auth,
    },
  });
}

export async function loadState(): Promise<PersistedState | null> {
  const raw = await invoke<string>("load_state");
  try {
    return JSON.parse(raw) as PersistedState | null;
  } catch {
    // Main file is corrupt (e.g. interrupted write) — try the backup copy.
    try {
      const bak = await invoke<string>("load_state_backup");
      return JSON.parse(bak) as PersistedState | null;
    } catch {
      return null;
    }
  }
}

export async function saveState(state: PersistedState): Promise<void> {
  return invoke("save_state", { data: JSON.stringify(state) });
}

export async function writeTextFile(path: string, contents: string): Promise<void> {
  return invoke("write_text_file", { path, contents });
}

export async function readTextFile(path: string): Promise<string> {
  return invoke<string>("read_text_file", { path });
}

export async function readLog(): Promise<string> {
  return invoke<string>("read_log");
}
export async function logPath(): Promise<string> {
  return invoke<string>("log_path");
}
export async function clearLog(): Promise<void> {
  return invoke("clear_log");
}
export async function openLogFolder(): Promise<void> {
  return invoke("open_log_folder");
}
export async function logEvent(category: string, message: string): Promise<void> {
  return invoke("log_event", { category, message });
}
export async function appVersion(): Promise<string> {
  return invoke<string>("app_version");
}
export async function openUrl(url: string): Promise<void> {
  return invoke("open_url", { url });
}
