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
  tls?: TlsSpec | null;
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

/// The HTTP target's rps must be a finite positive number or the engine drops
/// the target outright (dead scenario). `""` (unset) already falls back to
/// the default; an explicit 0/negative/NaN limit — e.g. a stray value left in
/// the Load tab, or a manually edited config — has to fall back too, not be
/// passed through verbatim.
function httpTargetRps(lt: LoadCfg): number {
  const n = lt.rpsLimit;
  return typeof n === "number" && Number.isFinite(n) && n > 0 ? n : HTTP_TARGET_DEFAULT_RPS;
}

/// Default error-rate gate (percent, matching LoadTestResult.error_rate's
/// 0–100 scale): tight enough to actually catch a broken run, loose enough
/// not to fail on ordinary noise. Full UI configurability is a follow-up.
const DEFAULT_MAX_ERROR_RATE_PCT = 5.0;

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
        rps: httpTargetRps(lt),
        tls: t.tls,
        auth_refresh: t.authRefresh,
        multipart: t.multipart,
      },
    ],
    datasets: t.datasets,
    file_pools: t.filePools,
    thresholds: { max_error_rate: DEFAULT_MAX_ERROR_RATE_PCT, max_p95_ms: 500 },
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
      tls: g.tls ?? null,
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
