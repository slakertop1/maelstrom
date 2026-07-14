// Pure builders that turn a UI RequestConfig + the active environment into the
// wire specs the backend runs (an HTTP request, its OAuth auto-refresh config)
// plus the preflight "unresolved {{var}}" detection. Kept out of App.tsx so the
// auth-injection / header-dedup / multipart-pool logic is unit-testable.
import { toFilePoolSpec } from "./types";
import type {
  RequestConfig,
  Environment,
  KV,
  OAuthRefreshSpec,
  TlsConfig,
  TlsSpec,
  MultipartPartSpec,
  FilePoolSpec,
} from "./types";
import { resolveVars, envVars } from "./vars";

export interface BuiltRequest {
  method: string;
  url: string;
  headers: [string, string][];
  body: string | null;
  tls: TlsSpec | null;
  multipart: MultipartPartSpec[] | null;
  file_pools: FilePoolSpec[];
}

function toTlsSpec(tls: TlsConfig): TlsSpec | null {
  if (!tls.enabled) return null;
  const spec: TlsSpec = {
    client_cert_pem: tls.client_cert_pem.trim() || null,
    client_key_pem: tls.client_key_pem.trim() || null,
    ca_cert_pem: tls.ca_cert_pem.trim() || null,
    insecure: tls.insecure,
  };
  if (!spec.client_cert_pem && !spec.ca_cert_pem && !spec.insecure) return null;
  return spec;
}

/// Build the auto-refresh config for a load test, or null if the request's auth
/// can't be refreshed non-interactively (no OAuth2, refresh disabled, or an
/// authorization_code flow without a refresh token).
export function buildAuthRefresh(
  req: RequestConfig,
  env: Environment | null,
  forExport = false
): OAuthRefreshSpec | null {
  if (req.auth.type !== "oauth2") return null;
  const cfg = req.auth.oauth2;
  if (!cfg.auto_refresh) return null;
  const vars = envVars(env, forExport);
  const r = (s: string) => resolveVars(s, vars);
  const tokenUrl = r(cfg.token_url).trim();
  if (!tokenUrl) return null;

  let grant = cfg.grant;
  let refreshToken = r(cfg.refresh_token);
  if (grant === "authorization_code") {
    // can only be renewed if the IdP gave us a refresh token
    if (!refreshToken) return null;
    grant = "refresh_token";
  }

  return {
    grant_type: grant,
    token_url: tokenUrl,
    client_id: r(cfg.client_id),
    client_secret: cfg.client_secret ? r(cfg.client_secret) : null,
    scope: cfg.scope ? r(cfg.scope) : null,
    username: cfg.username ? r(cfg.username) : null,
    password: cfg.password ? r(cfg.password) : null,
    refresh_token: refreshToken || null,
    client_auth: cfg.client_auth,
  };
}

export function buildRequest(
  req: RequestConfig,
  env: Environment | null,
  forExport = false
): BuiltRequest {
  const vars = envVars(env, forExport);
  const r = (s: string) => resolveVars(s, vars);

  let url = r(req.url.trim());
  const enabled = (items: KV[]) => items.filter((i) => i.enabled && i.key.trim());

  const params = enabled(req.params);
  if (params.length) {
    const qs = params
      .map((p) => `${encodeURIComponent(r(p.key.trim()))}=${encodeURIComponent(r(p.value))}`)
      .join("&");
    url += (url.includes("?") ? "&" : "?") + qs;
  }

  const headers: [string, string][] = enabled(req.headers).map((h) => [
    r(h.key.trim()),
    r(h.value),
  ]);
  const hasHeader = (name: string) =>
    headers.some(([k]) => k.toLowerCase() === name.toLowerCase());

  if (req.auth.type === "bearer" && req.auth.token && !hasHeader("authorization")) {
    headers.push(["Authorization", `Bearer ${r(req.auth.token)}`]);
  } else if (req.auth.type === "basic" && !hasHeader("authorization")) {
    headers.push([
      "Authorization",
      "Basic " + btoa(`${r(req.auth.username)}:${r(req.auth.password)}`),
    ]);
  } else if (
    req.auth.type === "oauth2" &&
    req.auth.oauth2.access_token &&
    !hasHeader("authorization")
  ) {
    // The token is fetched once (OAuth2/SSO tab) and reused for every request,
    // including every virtual user in a load test — that's what makes an
    // authenticated request reproducible under load.
    headers.push(["Authorization", `Bearer ${req.auth.oauth2.access_token}`]);
  }

  let body: string | null = null;
  let multipart: MultipartPartSpec[] | null = null;
  const file_pools: FilePoolSpec[] = [];
  if (req.body_type === "json" && req.body.trim()) {
    body = r(req.body);
    if (!hasHeader("content-type")) headers.push(["Content-Type", "application/json"]);
  } else if (req.body_type === "text" && req.body) {
    body = r(req.body);
    if (!hasHeader("content-type")) headers.push(["Content-Type", "text/plain"]);
  } else if (req.body_type === "form") {
    const fields = enabled(req.form_body);
    if (fields.length) {
      body = fields
        .map(
          (f) => `${encodeURIComponent(r(f.key.trim()))}=${encodeURIComponent(r(f.value))}`
        )
        .join("&");
      if (!hasHeader("content-type"))
        headers.push(["Content-Type", "application/x-www-form-urlencoded"]);
    }
  } else if (req.body_type === "multipart") {
    const parts = req.multipart_body
      .filter((p) => p.enabled && p.name.trim())
      .map<MultipartPartSpec>((p) => {
        // A file part in "pool" mode draws a fresh file from a set per request:
        // register the pool and point the part at it via {{$file.NAME}}.
        if (p.kind === "file" && p.source === "pool") {
          const poolName = `mpf_${p.id}`;
          file_pools.push(toFilePoolSpec(poolName, p));
          return {
            name: r(p.name.trim()),
            kind: "file",
            value: `{{$file.${poolName}}}`,
            filename: p.filename.trim() ? r(p.filename) : null,
            content_type: p.content_type.trim() ? r(p.content_type) : null,
            enabled: true,
          };
        }
        return {
          name: r(p.name.trim()),
          kind: p.kind,
          value: r(p.value), // text value or fixed file path (vars resolved)
          filename: p.filename.trim() ? r(p.filename) : null,
          content_type: p.content_type.trim() ? r(p.content_type) : null,
          enabled: true,
        };
      });
    if (parts.length) multipart = parts;
    // Content-Type (with boundary) is set by the backend/reqwest.
  }

  return {
    method: req.method,
    url,
    headers,
    body,
    tls: toTlsSpec(req.tls),
    multipart,
    file_pools,
  };
}

// Unresolved environment-style placeholders ({{name}}), excluding the dynamic
// generators ({{$...}}) which the load engine fills in per request.
const VAR_RE = /\{\{\s*(?!\$)([\w.-]+)\s*\}\}/g;

export function unresolvedVars(strings: (string | null | undefined)[]): string[] {
  const set = new Set<string>();
  for (const s of strings) {
    if (!s) continue;
    for (const m of s.matchAll(VAR_RE)) set.add(m[1]);
  }
  return [...set];
}

export function builtStrings(b: BuiltRequest): (string | null)[] {
  const out: (string | null)[] = [b.url, b.body];
  for (const [k, v] of b.headers) out.push(k, v);
  if (b.multipart) {
    for (const p of b.multipart) out.push(p.value, p.name, p.filename, p.content_type);
  }
  return out;
}
