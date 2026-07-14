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

interface Props {
  json: string; // config with default ${KEY} placeholders (from secret var names)
  defaultName: string;
  onSave: (finalJson: string, defaultName: string) => Promise<void>;
  onClose: () => void;
}

export default function ExportConfigModal({ json, defaultName, onSave, onClose }: Props) {
  const t = useT();
  const detected = useMemo(() => requiredEnvVars(json), [json]);
  const [renames, setRenames] = useState<Record<string, string>>({});
  const [saved, setSaved] = useState(false);
  const [copied, setCopied] = useState(false);

  const finalJson = useMemo(() => applyRenames(json, renames), [json, renames]);
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
