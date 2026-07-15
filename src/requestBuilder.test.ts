import { describe, it, expect } from "vitest";
import { buildRequest, buildAuthRefresh, unresolvedVars, builtStrings } from "./requestBuilder";
import { newRequest, RequestConfig, Environment } from "./types";

function req(patch: Partial<RequestConfig> = {}): RequestConfig {
  return { ...newRequest(), ...patch };
}

const authBase = () => newRequest().auth;

const env = (vars: [string, string][]): Environment => ({
  id: "e",
  name: "t",
  variables: vars.map(([key, value], i) => ({ id: String(i), key, value, enabled: true })),
});

describe("buildRequest — headers & auth", () => {
  it("injects a Bearer token", () => {
    const b = buildRequest(
      req({ url: "https://api/x", auth: { ...authBase(), type: "bearer", token: "T" } }),
      null
    );
    expect(b.headers).toContainEqual(["Authorization", "Bearer T"]);
  });

  it("injects Basic auth", () => {
    const b = buildRequest(
      req({ url: "https://api/x", auth: { ...authBase(), type: "basic", username: "u", password: "p" } }),
      null
    );
    expect(b.headers).toContainEqual(["Authorization", "Basic " + btoa("u:p")]);
  });

  it("does NOT override an Authorization header the user set explicitly", () => {
    const b = buildRequest(
      req({
        url: "https://api/x",
        auth: { ...authBase(), type: "bearer", token: "T" },
        headers: [{ id: "h", key: "Authorization", value: "Bearer EXPLICIT", enabled: true }],
      }),
      null
    );
    expect(b.headers.filter(([k]) => k.toLowerCase() === "authorization")).toEqual([
      ["Authorization", "Bearer EXPLICIT"],
    ]);
  });

  it("resolves {{vars}} and appends query params onto a URL that already has ?", () => {
    const b = buildRequest(
      req({
        url: "https://{{host}}/x?a=1",
        params: [{ id: "p", key: "b", value: "2", enabled: true }],
      }),
      env([["host", "api.example.com"]])
    );
    expect(b.url).toBe("https://api.example.com/x?a=1&b=2");
  });

  it("defaults Content-Type for JSON, but leaves an explicit one alone", () => {
    const j = buildRequest(req({ body_type: "json", body: '{"a":1}' }), null);
    expect(j.headers).toContainEqual(["Content-Type", "application/json"]);

    const withCt = buildRequest(
      req({
        body_type: "json",
        body: '{"a":1}',
        headers: [{ id: "h", key: "Content-Type", value: "application/vnd.api+json", enabled: true }],
      }),
      null
    );
    expect(withCt.headers.filter(([k]) => k.toLowerCase() === "content-type")).toEqual([
      ["Content-Type", "application/vnd.api+json"],
    ]);
  });
});

describe("buildRequest — multipart pool", () => {
  it("rewrites a pool file part to {{$file.NAME}} and registers the pool", () => {
    const b = buildRequest(
      req({
        body_type: "multipart",
        multipart_body: [
          {
            id: "abc",
            name: "photo",
            kind: "file",
            value: "",
            filename: "",
            content_type: "",
            enabled: true,
            source: "pool",
            pool_mode: "random",
            pool_kind: "url",
            pool_urls: "https://b/a.png\nhttps://b/c.png",
          },
        ],
      }),
      null
    );
    expect(b.multipart?.[0].value).toBe("{{$file.mpf_abc}}");
    expect(b.file_pools).toHaveLength(1);
    expect(b.file_pools[0].name).toBe("mpf_abc");
    expect(b.file_pools[0].source.urls).toEqual(["https://b/a.png", "https://b/c.png"]);
  });
});

describe("buildRequest — TLS collapse", () => {
  it("is null when nothing meaningful is set", () => {
    expect(buildRequest(req(), null).tls).toBeNull();
  });
  it("emits a spec when insecure is on", () => {
    const r = req({
      tls: { enabled: true, client_cert_pem: "", client_key_pem: "", ca_cert_pem: "", insecure: true },
    });
    expect(buildRequest(r, null).tls).toMatchObject({ insecure: true });
  });
});

describe("buildAuthRefresh", () => {
  const oauth = (patch: Record<string, unknown>) => ({
    ...authBase(),
    type: "oauth2" as const,
    oauth2: { ...authBase().oauth2, auto_refresh: true, ...patch },
  });

  it("builds a client_credentials refresh spec", () => {
    const spec = buildAuthRefresh(
      req({ auth: oauth({ grant: "client_credentials", token_url: "https://idp/token", client_id: "cid" }) }),
      null
    );
    expect(spec?.grant_type).toBe("client_credentials");
    expect(spec?.token_url).toBe("https://idp/token");
    expect(spec?.client_id).toBe("cid");
  });

  it("maps authorization_code WITH a refresh token to a refresh_token grant", () => {
    const spec = buildAuthRefresh(
      req({ auth: oauth({ grant: "authorization_code", token_url: "https://idp/token", refresh_token: "rt" }) }),
      null
    );
    expect(spec?.grant_type).toBe("refresh_token");
    expect(spec?.refresh_token).toBe("rt");
  });

  it("returns null for authorization_code WITHOUT a refresh token", () => {
    expect(
      buildAuthRefresh(
        req({ auth: oauth({ grant: "authorization_code", token_url: "https://idp/token", refresh_token: "" }) }),
        null
      )
    ).toBeNull();
  });

  it("returns null when auth isn't oauth2 or auto-refresh is off", () => {
    expect(buildAuthRefresh(req({ auth: { ...authBase(), type: "bearer" } }), null)).toBeNull();
    expect(buildAuthRefresh(req({ auth: oauth({ token_url: "https://idp/token", auto_refresh: false }) }), null)).toBeNull();
  });
});

describe("buildRequest — excludeVarNames (chain-var collision)", () => {
  it("bakes an Environment value by default, even when its name collides with a chain-extract name", () => {
    const b = buildRequest(
      req({ url: "https://api/x", headers: [{ id: "h", key: "X-Token", value: "{{token}}", enabled: true }] }),
      env([["token", "FROM_ENV"]])
    );
    expect(b.headers).toContainEqual(["X-Token", "FROM_ENV"]);
  });

  it("leaves {{name}} as a placeholder when the name is passed in excludeVarNames", () => {
    const b = buildRequest(
      req({ url: "https://api/x", headers: [{ id: "h", key: "X-Token", value: "{{token}}", enabled: true }] }),
      env([["token", "FROM_ENV"]]),
      false,
      new Set(["token"])
    );
    expect(b.headers).toContainEqual(["X-Token", "{{token}}"]);
  });

  it("accepts a plain array too, and only excludes the named vars", () => {
    const b = buildRequest(
      req({ url: "https://{{host}}/{{token}}" }),
      env([
        ["host", "api.example.com"],
        ["token", "FROM_ENV"],
      ]),
      false,
      ["token"]
    );
    expect(b.url).toBe("https://api.example.com/{{token}}");
  });
});

describe("unresolvedVars & builtStrings", () => {
  it("flags {{env}} vars, ignores {{$generators}}, and dedups", () => {
    expect(
      unresolvedVars(["https://{{host}}/{{path}}", "{{host}}-{{$uuid}}-{{$data.u.x}}"]).sort()
    ).toEqual(["host", "path"]);
  });

  it("collects url, body and header strings from a built request", () => {
    const b = buildRequest(req({ url: "https://api/x", body_type: "json", body: '{"v":"{{tok}}"}' }), null);
    const strings = builtStrings(b);
    expect(strings).toContain("https://api/x");
    expect(strings.some((s) => s?.includes("{{tok}}"))).toBe(true);
  });
});
