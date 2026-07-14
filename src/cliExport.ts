// Pure builders that turn a single request + the Load-tab config into a CLI
// scenario JSON (the format `maelstrom` runs). Kept out of App.tsx so it's unit-testable.
import type {
  DatasetSpec,
  FilePoolSpec,
  MultipartPartSpec,
  OAuthRefreshSpec,
  TlsSpec,
} from "./types";

export interface LoadCfg {
  vus: number;
  durationSecs: number;
  rpsLimit: number | "";
  timeoutMs: number;
}

export interface HttpTarget {
  method: string;
  url: string;
  headers: [string, string][];
  body: string | null;
  tls: TlsSpec | null;
  multipart: MultipartPartSpec[] | null;
  authRefresh: OAuthRefreshSpec | null;
  datasets: DatasetSpec[];
  filePools: FilePoolSpec[];
}

export interface GrpcBlock {
  endpoint: string;
  proto_path: string;
  includes: string[];
  service: string;
  method: string;
  body: string;
}

export interface WsBlock {
  url: string;
  message: string;
}

/// The scenario engine drops HTTP targets with rps <= 0, so an unset RPS limit
/// has to become a concrete positive default in the exported config.
export const HTTP_TARGET_DEFAULT_RPS = 100;

const rpsLimitOrNull = (lt: LoadCfg): number | null =>
  lt.rpsLimit === "" ? null : lt.rpsLimit;

const head = (name: string, lt: LoadCfg) => ({
  name,
  duration_secs: lt.durationSecs,
  timeout_ms: lt.timeoutMs,
});

export function buildHttpScenario(name: string, lt: LoadCfg, t: HttpTarget) {
  return {
    ...head(name, lt),
    targets: [
      {
        name,
        method: t.method,
        url: t.url,
        headers: t.headers,
        body: t.body,
        rps: lt.rpsLimit === "" ? HTTP_TARGET_DEFAULT_RPS : lt.rpsLimit,
        tls: t.tls,
        auth_refresh: t.authRefresh,
        multipart: t.multipart,
      },
    ],
    datasets: t.datasets,
    file_pools: t.filePools,
    thresholds: { max_error_rate: 1.0, max_p95_ms: 500 },
  };
}

export function buildGrpcScenario(name: string, lt: LoadCfg, g: GrpcBlock) {
  return {
    ...head(name, lt),
    grpc: {
      endpoint: g.endpoint,
      proto_path: g.proto_path,
      includes: g.includes,
      service: g.service,
      method: g.method,
      body: g.body,
      vus: lt.vus,
      rps_limit: rpsLimitOrNull(lt),
      timeout_ms: lt.timeoutMs,
    },
  };
}

export function buildWsScenario(name: string, lt: LoadCfg, w: WsBlock) {
  return {
    ...head(name, lt),
    websocket: {
      url: w.url,
      message: w.message,
      vus: lt.vus,
      rps_limit: rpsLimitOrNull(lt),
      timeout_ms: lt.timeoutMs,
    },
  };
}
