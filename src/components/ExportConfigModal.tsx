import { useMemo, useState } from "react";
import { useT } from "../i18n";

/// All ${ENV_VAR} placeholders referenced by the config, de-duplicated & sorted.
export function requiredEnvVars(json: string): string[] {
  const set = new Set<string>();
  for (const m of json.matchAll(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}/g)) set.add(m[1]);
  return [...set].sort();
}

/// Rewrite ${old} placeholders to ${new} throughout the config text.
export function applyRenames(json: string, renames: Record<string, string>): string {
  let out = json;
  for (const [orig, nw] of Object.entries(renames)) {
    const clean = nw.trim();
    if (clean && clean !== orig) out = out.split("${" + orig + "}").join("${" + clean + "}");
  }
  return out;
}

const sanitize = (s: string) => s.replace(/[^A-Za-z0-9_]/g, "");

// ---- literal-secret detection & auto-redaction -----------------------------
//
// envVars(env, forExport=true) (vars.ts) already turns *Environment* secret
// vars referenced via {{name}} into ${NAME} placeholders — but a value typed
// straight into a credential field (an OAuth client secret, a Basic-auth
// password, a Bearer token, a TLS private key, an AWS key on a dataset
// source) never goes through that indirection, so it gets baked into the
// exported JSON as recoverable plain text while the UI claims the file is
// "self-contained". The CLI expands ${VAR} by substituting the raw config
// *text* before it parses JSON (cli/src/main.rs `expand_env`), so dropping a
// ${NAME} placeholder anywhere inside a string value — including mid-string,
// e.g. "Bearer ${TOKEN}" — is safe and gets resolved at run time. That means
// the redaction below can happen entirely here, without any App.tsx wiring.

/// JSON object keys that hold credential material in the exported scenario
/// shapes (OAuthRefreshSpec, TlsSpec, AwsAuth) — if the value is non-empty
/// and isn't already a ${VAR} reference, it's a literal secret sitting in
/// the file in plain text.
const SECRET_FIELD_ENV_NAME: Record<string, string> = {
  client_secret: "OAUTH_CLIENT_SECRET",
  password: "OAUTH_PASSWORD",
  refresh_token: "OAUTH_REFRESH_TOKEN",
  access_key_id: "AWS_ACCESS_KEY_ID",
  secret_access_key: "AWS_SECRET_ACCESS_KEY",
  session_token: "AWS_SESSION_TOKEN",
  client_key_pem: "TLS_CLIENT_KEY_PEM",
};

/// A value carries no literal secret when it's empty, or when it's made up
/// entirely of ${VAR} reference(s) — i.e. every character sits inside one.
function isPlaceholderOrEmpty(v: string): boolean {
  const t = v.trim();
  if (!t) return true;
  return /^(?:\$\{[A-Za-z_][A-Za-z0-9_]*\})+$/.test(t);
}

export interface SecretFinding {
  /** Dotted JSON path (or header name) that held a literal secret. */
  path: string;
}

/// Scan a built scenario config for literal secrets — known credential
/// fields plus `Authorization: Bearer <token>` / `Basic <base64>` headers —
/// and replace each one with a fresh ${NAME} placeholder. Returns the
/// rewritten JSON and what was found, so the modal can warn instead of
/// falsely claiming the file is self-contained.
///
/// Basic auth is always redacted when present: the header is built as
/// `"Basic " + btoa(user + ":" + password)`, so even a password that came
/// from a secret Environment var is base64-encoded *before* export — the
/// literal ${VAR} text never survives inside the blob — leaving no way to
/// keep it self-contained other than replacing the whole credential.
export function redactLiteralSecrets(json: string): { json: string; findings: SecretFinding[] } {
  let root: unknown;
  try {
    root = JSON.parse(json);
  } catch {
    return { json, findings: [] }; // not JSON (shouldn't happen) — leave untouched
  }

  const findings: SecretFinding[] = [];
  // Seed with every ${VAR} placeholder already present in the config (e.g.
  // an Environment secret var referenced via {{name}} and turned into
  // ${NAME} upstream by envVars(env, forExport=true)) — otherwise a freshly
  // generated name here could collide with one of those and silently merge
  // two unrelated secrets under a single placeholder.
  const used = new Set<string>(requiredEnvVars(json));
  const freshName = (base: string): string => {
    let name = base;
    for (let i = 2; used.has(name); i++) name = `${base}_${i}`;
    used.add(name);
    return name;
  };

  const walk = (node: unknown, path: string): unknown => {
    if (Array.isArray(node)) {
      // A header pair: ["Authorization", "Bearer xxx" | "Basic yyy"].
      if (node.length === 2 && typeof node[0] === "string" && typeof node[1] === "string") {
        const [name, value] = node as [string, string];
        if (name.toLowerCase() === "authorization") {
          const bearer = /^Bearer\s+(.+)$/.exec(value);
          if (bearer && !isPlaceholderOrEmpty(bearer[1])) {
            findings.push({ path: `${path}[Authorization: Bearer]` });
            return [name, `Bearer \${${freshName("BEARER_TOKEN")}}`];
          }
          const basic = /^Basic\s+(.+)$/.exec(value);
          if (basic && !isPlaceholderOrEmpty(basic[1])) {
            findings.push({ path: `${path}[Authorization: Basic]` });
            return [name, `Basic \${${freshName("BASIC_AUTH")}}`];
          }
        }
      }
      return node.map((v, i) => walk(v, `${path}[${i}]`));
    }
    if (node && typeof node === "object") {
      const out: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(node as Record<string, unknown>)) {
        const envBase = SECRET_FIELD_ENV_NAME[k];
        if (envBase && typeof v === "string" && !isPlaceholderOrEmpty(v)) {
          out[k] = `\${${freshName(envBase)}}`;
          findings.push({ path: path ? `${path}.${k}` : k });
        } else {
          out[k] = walk(v, path ? `${path}.${k}` : k);
        }
      }
      return out;
    }
    return node;
  };

  const redacted = walk(root, "");
  return { json: JSON.stringify(redacted, null, 2), findings };
}

interface Props {
  json: string; // config with default ${KEY} placeholders (from secret var names)
  defaultName: string;
  onSave: (finalJson: string, defaultName: string) => Promise<void>;
  onClose: () => void;
}

export default function ExportConfigModal({ json, defaultName, onSave, onClose }: Props) {
  const t = useT();
  // Redact any literal secret we can find BEFORE anything else runs off of
  // `json` — the "self-contained" check and the ${VAR} list both need to see
  // the redacted text, not the raw config that may still hold plain-text
  // credentials (see redactLiteralSecrets above for why this can't just be a
  // warning: Basic auth in particular has no other self-contained fix).
  const { json: safeJson, findings } = useMemo(() => redactLiteralSecrets(json), [json]);
  const hasLiteralSecrets = findings.length > 0;
  const detected = useMemo(() => requiredEnvVars(safeJson), [safeJson]);
  const [renames, setRenames] = useState<Record<string, string>>({});
  const [saved, setSaved] = useState(false);
  const [copied, setCopied] = useState(false);

  const finalJson = useMemo(() => applyRenames(safeJson, renames), [safeJson, renames]);
  const vars = useMemo(() => requiredEnvVars(finalJson), [finalJson]);

  const secretYaml =
    "stringData:\n" + (vars.length ? vars.map((v) => `  ${v}: "..."`).join("\n") : "  # none");
  const shellLine = vars.length ? vars.map((v) => `${v}=...`).join(" ") + " maelstrom scenario.json" : "";

  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard blocked — the user can still select the text */
    }
  };

  const doSave = async () => {
    await onSave(finalJson, defaultName);
    setSaved(true);
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ width: 640 }}>
        <div className="modal-head">
          <span>{t("Export for CI")}</span>
          <button className="ghost" onClick={onClose}>
            ✕
          </button>
        </div>
        <div className="modal-body">
          {detected.length === 0 ? (
            <div className="lt-hint">
              {t("No secret variables — the config is self-contained. Nothing to set in the cluster.")}
            </div>
          ) : (
            <>
              {hasLiteralSecrets && (
                <div className="lt-hint" style={{ marginBottom: 10, color: "var(--yellow)" }}>
                  {t(
                    "This config held secrets in plain text (a literal password, token, key or credential — not a ${VAR} placeholder). They were rewritten to ${VAR} below; set those environment variables before the run and never commit the original values."
                  )}
                </div>
              )}
              <div className="lt-hint" style={{ marginBottom: 10 }}>
                {t("Secrets are written as ${NAME} placeholders — set these environment variables in your cluster before the run. Rename them here if you like; the file updates to match.")}
              </div>
              <div className="form-grid">
                {detected.map((orig) => (
                  <span key={orig} style={{ display: "contents" }}>
                    <label style={{ fontFamily: "monospace" }}>{orig}</label>
                    <input
                      value={renames[orig] ?? orig}
                      spellCheck={false}
                      onChange={(e) =>
                        setRenames((m) => ({ ...m, [orig]: sanitize(e.target.value) }))
                      }
                    />
                  </span>
                ))}
              </div>

              <div style={{ marginTop: 16 }}>
                <div className="export-vars-head">
                  <b style={{ color: "var(--text)" }}>
                    {t("Environment variables to set")} ({vars.length})
                  </b>
                  <button className="ghost" onClick={() => copy(vars.join("\n"))}>
                    {copied ? t("Copied") : t("Copy")}
                  </button>
                </div>
                <div className="export-vars-list">
                  {vars.map((v) => (
                    <code key={v}>{v}</code>
                  ))}
                </div>
              </div>

              <div style={{ marginTop: 14 }}>
                <div className="export-vars-head">
                  <span className="lt-hint" style={{ margin: 0 }}>{t("Kubernetes Secret")}</span>
                  <button className="ghost" onClick={() => copy(secretYaml)}>
                    {t("Copy")}
                  </button>
                </div>
                <textarea className="export-snippet" readOnly rows={Math.min(vars.length + 1, 8)} value={secretYaml} />
                {shellLine && (
                  <>
                    <div className="export-vars-head" style={{ marginTop: 8 }}>
                      <span className="lt-hint" style={{ margin: 0 }}>{t("…or run locally")}</span>
                      <button className="ghost" onClick={() => copy(shellLine)}>
                        {t("Copy")}
                      </button>
                    </div>
                    <textarea className="export-snippet" readOnly rows={2} value={shellLine} />
                  </>
                )}
              </div>
            </>
          )}

          <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 18 }}>
            {saved && <span className="export-saved">✓ {t("Saved")}</span>}
            <button className="ghost" onClick={onClose}>
              {t("Close")}
            </button>
            <button className="primary" onClick={doSave}>
              {t("Save config…")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
