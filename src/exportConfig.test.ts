import { describe, it, expect } from "vitest";
import { requiredEnvVars, applyRenames, redactLiteralSecrets } from "./components/ExportConfigModal";

describe("export-for-CI env var handling", () => {
  const json = '{"a":"${TOKEN}","b":"${DB_PASSWORD}","c":"${TOKEN}","d":"literal"}';

  it("detects, de-dupes and sorts required vars", () => {
    expect(requiredEnvVars(json)).toEqual(["DB_PASSWORD", "TOKEN"]);
  });

  it("returns nothing when the config is self-contained", () => {
    expect(requiredEnvVars('{"x":"plain"}')).toEqual([]);
  });

  it("renames a placeholder everywhere it appears", () => {
    const out = applyRenames(json, { TOKEN: "API_TOKEN" });
    expect(out).toContain("${API_TOKEN}");
    expect(out).not.toContain("${TOKEN}");
    expect(requiredEnvVars(out)).toEqual(["API_TOKEN", "DB_PASSWORD"]);
  });

  it("ignores empty and identity renames", () => {
    expect(applyRenames(json, { TOKEN: "", DB_PASSWORD: "DB_PASSWORD" })).toBe(json);
  });
});

// f1 + f4: literal secrets (AWS creds via toDatasetSpec, OAuth client_secret /
// password, Basic auth, Bearer tokens, TLS private keys) must never survive
// into the exported file as plain text — the modal has to catch them itself.
describe("redactLiteralSecrets (f1 + f4)", () => {
  it("finds nothing and leaves a config that only uses ${VAR} untouched", () => {
    const clean = JSON.stringify({
      targets: [{ headers: [["Authorization", "Bearer ${TOKEN}"]] }],
      thresholds: { max_error_rate: 5, max_p95_ms: 500 },
    });
    const { json: out, findings } = redactLiteralSecrets(clean);
    expect(findings).toEqual([]);
    expect(out.replace(/\s/g, "")).toBe(clean.replace(/\s/g, ""));
  });

  it("redacts a literal AWS access_key_id / secret_access_key on a dataset source (toDatasetSpec)", () => {
    const cfg = JSON.stringify({
      datasets: [
        {
          name: "clients",
          mode: "sequential",
          source: {
            kind: "url",
            url: "https://bucket/clients.csv",
            aws: {
              access_key_id: "AKIAABCDEFGHIJKLMNOP",
              secret_access_key: "supersecretliteralvalue",
              region: "us-east-1",
              session_token: null,
            },
          },
        },
      ],
    });
    const { json: out, findings } = redactLiteralSecrets(cfg);
    expect(findings.length).toBe(2);
    expect(out).not.toContain("AKIAABCDEFGHIJKLMNOP");
    expect(out).not.toContain("supersecretliteralvalue");
    const parsed = JSON.parse(out);
    expect(parsed.datasets[0].source.aws.access_key_id).toBe("${AWS_ACCESS_KEY_ID}");
    expect(parsed.datasets[0].source.aws.secret_access_key).toBe("${AWS_SECRET_ACCESS_KEY}");
    expect(requiredEnvVars(out)).toEqual(
      expect.arrayContaining(["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"])
    );
  });

  it("redacts a literal OAuth client_secret / password in auth_refresh", () => {
    const cfg = JSON.stringify({
      targets: [
        {
          auth_refresh: {
            grant_type: "client_credentials",
            token_url: "https://idp/token",
            client_id: "cid",
            client_secret: "literal-secret-value",
            scope: null,
            username: null,
            password: "literal-password",
            refresh_token: null,
            client_auth: "body",
          },
        },
      ],
    });
    const { json: out, findings } = redactLiteralSecrets(cfg);
    expect(out).not.toContain("literal-secret-value");
    expect(out).not.toContain("literal-password");
    expect(findings.length).toBe(2);
    expect(requiredEnvVars(out)).toEqual(
      expect.arrayContaining(["OAUTH_CLIENT_SECRET", "OAUTH_PASSWORD"])
    );
  });

  it("leaves an OAuth client_secret alone when it's already a ${VAR} placeholder", () => {
    const cfg = JSON.stringify({
      targets: [{ auth_refresh: { client_secret: "${MY_CLIENT_SECRET}" } }],
    });
    const { json: out, findings } = redactLiteralSecrets(cfg);
    expect(findings).toEqual([]);
    expect(out).toContain("${MY_CLIENT_SECRET}");
  });

  it("redacts a literal TLS client_key_pem", () => {
    const cfg = JSON.stringify({
      targets: [
        {
          tls: {
            client_cert_pem: "-----BEGIN CERTIFICATE-----\nabc\n-----END CERTIFICATE-----",
            client_key_pem: "-----BEGIN PRIVATE KEY-----\nsecretbytes\n-----END PRIVATE KEY-----",
            ca_cert_pem: null,
            insecure: false,
          },
        },
      ],
    });
    const { json: out, findings } = redactLiteralSecrets(cfg);
    expect(out).not.toContain("secretbytes");
    expect(findings.length).toBe(1);
    expect(JSON.parse(out).targets[0].tls.client_key_pem).toBe("${TLS_CLIENT_KEY_PEM}");
  });

  it("redacts a literal Bearer token in a headers pair, but leaves an already-${VAR} one alone", () => {
    const cfg = JSON.stringify({
      targets: [
        {
          headers: [
            ["Authorization", "Bearer sk_live_abc123"],
            ["X-Other", "unrelated"],
          ],
        },
        { headers: [["Authorization", "Bearer ${TOKEN}"]] },
      ],
    });
    const { json: out, findings } = redactLiteralSecrets(cfg);
    expect(out).not.toContain("sk_live_abc123");
    expect(out).toContain("${TOKEN}"); // untouched — already an indirection
    expect(findings.length).toBe(1);
    const parsed = JSON.parse(out);
    expect(parsed.targets[0].headers[0][1]).toMatch(/^Bearer \$\{[A-Z_]+\}$/);
    expect(parsed.targets[1].headers[0][1]).toBe("Bearer ${TOKEN}");
  });

  it("always redacts a Basic auth header — the base64 blob can never itself be a ${VAR} placeholder", () => {
    const basicValue = "Basic " + btoa("user:literal-password");
    const cfg = JSON.stringify({ targets: [{ headers: [["Authorization", basicValue]] }] });
    const { json: out, findings } = redactLiteralSecrets(cfg);
    expect(out).not.toContain(btoa("user:literal-password"));
    expect(findings.length).toBe(1);
    expect(JSON.parse(out).targets[0].headers[0][1]).toMatch(/^Basic \$\{[A-Z_]+\}$/);
  });

  it("gives each redacted secret of the same kind a unique env var name", () => {
    const cfg = JSON.stringify({
      targets: [
        { auth_refresh: { client_secret: "s1" } },
        { auth_refresh: { client_secret: "s2" } },
      ],
    });
    const { json: out } = redactLiteralSecrets(cfg);
    const parsed = JSON.parse(out);
    const a = parsed.targets[0].auth_refresh.client_secret;
    const b = parsed.targets[1].auth_refresh.client_secret;
    expect(a).not.toBe(b);
    expect(requiredEnvVars(out)).toEqual(
      expect.arrayContaining(["OAUTH_CLIENT_SECRET", "OAUTH_CLIENT_SECRET_2"])
    );
  });

  it("does not merge a literal secret into an existing ${VAR} placeholder that already uses the same generated name", () => {
    // The config already references an Environment secret var named
    // AWS_ACCESS_KEY_ID via ${AWS_ACCESS_KEY_ID} (from envVars(env,
    // forExport=true)/{{name}} indirection elsewhere), but a *different*,
    // literal AWS key also needs redacting on another dataset source. The
    // literal one must NOT be renamed to the same ${AWS_ACCESS_KEY_ID} —
    // that would silently fuse two unrelated secrets into one placeholder.
    const cfg = JSON.stringify({
      targets: [{ headers: [["X-Env-Key", "${AWS_ACCESS_KEY_ID}"]] }],
      datasets: [
        {
          name: "clients",
          source: {
            kind: "url",
            url: "https://bucket/clients.csv",
            aws: {
              access_key_id: "AKIAABCDEFGHIJKLMNOP",
              secret_access_key: null,
              region: "us-east-1",
              session_token: null,
            },
          },
        },
      ],
    });
    const { json: out, findings } = redactLiteralSecrets(cfg);
    expect(findings.length).toBe(1);
    expect(out).not.toContain("AKIAABCDEFGHIJKLMNOP");
    const parsed = JSON.parse(out);
    // The pre-existing env-var placeholder is left exactly as it was.
    expect(parsed.targets[0].headers[0][1]).toBe("${AWS_ACCESS_KEY_ID}");
    // The literal secret gets a distinct name instead of colliding with it.
    expect(parsed.datasets[0].source.aws.access_key_id).toBe("${AWS_ACCESS_KEY_ID_2}");
    expect(requiredEnvVars(out)).toEqual(["AWS_ACCESS_KEY_ID", "AWS_ACCESS_KEY_ID_2"]);
  });

  it("making the config self-contained: requiredEnvVars reflects the auto-redacted names, not the old false 'no secrets' state", () => {
    // Before the fix, a config with only a literal secret (no pre-existing
    // ${VAR}) would report requiredEnvVars() === [] and the modal would show
    // "self-contained" even though it held a plain-text credential.
    const cfg = JSON.stringify({ targets: [{ auth_refresh: { client_secret: "literal" } }] });
    expect(requiredEnvVars(cfg)).toEqual([]); // the raw config really has no ${VAR} yet
    const { json: out } = redactLiteralSecrets(cfg);
    expect(requiredEnvVars(out)).toEqual(["OAUTH_CLIENT_SECRET"]); // now it correctly needs one
  });
});
