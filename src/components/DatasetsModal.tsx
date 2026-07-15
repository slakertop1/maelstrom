import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { useT, tr2 } from "../i18n";
import { Dataset, DatasetSourceKind, newDataset } from "../types";
import { dbExecute } from "../api";

interface Props {
  datasets: Dataset[];
  onChange: (d: Dataset[]) => void;
  onClose: () => void;
}

export default function DatasetsModal({ datasets, onChange, onClose }: Props) {
  const t = useT();
  const [selectedId, setSelectedId] = useState<string | null>(datasets[0]?.id ?? null);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState<{ ok: boolean; text: string } | null>(null);
  const [showDbUrl, setShowDbUrl] = useState(false);
  const selected = datasets.find((d) => d.id === selectedId) ?? null;

  const select = (id: string | null) => {
    setSelectedId(id);
    setTestResult(null);
  };

  const add = () => {
    const d = newDataset();
    d.name = `dataset${datasets.length + 1}`;
    onChange([...datasets, d]);
    select(d.id);
  };

  const update = (patch: Partial<Dataset>) => {
    if (!selected) return;
    // Editing the connection/query invalidates a previous test result.
    if ("db_url" in patch || "query" in patch) setTestResult(null);
    onChange(datasets.map((d) => (d.id === selected.id ? { ...d, ...patch } : d)));
  };

  const testDb = async () => {
    if (!selected) return;
    if (!selected.db_url.trim()) {
      setTestResult({ ok: false, text: t("✗ Enter a connection string") });
      return;
    }
    if (!selected.query.trim()) {
      setTestResult({ ok: false, text: t("✗ Enter a SQL query") });
      return;
    }
    setTesting(true);
    setTestResult(null);
    try {
      const res = await dbExecute({
        url: selected.db_url,
        query: selected.query,
        timeout_ms: 15000,
      });
      const cols = res.columns.join(", ");
      const sample = res.rows[0]?.join(" | ") ?? "";
      setTestResult({
        ok: true,
        text:
          tr2("✓ Connection ok · rows: {rows} · columns: {cols}", {
            rows: res.row_count,
            cols: cols || "—",
          }) + (sample ? tr2(" · sample: {sample}", { sample }) : ""),
      });
    } catch (e) {
      setTestResult({ ok: false, text: `✗ ${e}` });
    } finally {
      setTesting(false);
    }
  };

  const remove = () => {
    if (!selected) return;
    const next = datasets.filter((d) => d.id !== selected.id);
    onChange(next);
    select(next[0]?.id ?? null);
  };

  const pickFile = async () => {
    const path = await open({
      multiple: false,
      filters: [{ name: t("Data"), extensions: ["csv", "json"] }],
    });
    if (typeof path === "string") update({ path });
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ width: 680 }}>
        <div className="modal-head">
          <span>{t("Data for load testing (collections)")}</span>
          <button className="ghost" onClick={onClose}>
            ✕
          </button>
        </div>
        <div className="modal-body">
          <div className="env-list">
            {datasets.map((d) => (
              <span
                key={d.id}
                className={`env-pill ${d.id === selectedId ? "active" : ""}`}
                onClick={() => select(d.id)}
              >
                {d.name || t("(unnamed)")}
              </span>
            ))}
            <span className="env-pill" onClick={add}>
              ＋ {t("New")}
            </span>
          </div>

          {selected ? (
            <>
              <div className="form-grid">
                <label>{t("Name (for")} {"{{$data.name.column}}"})</label>
                <input value={selected.name} onChange={(e) => update({ name: e.target.value })} />
                <label>{t("Row selection")}</label>
                <select
                  value={selected.mode}
                  onChange={(e) => update({ mode: e.target.value as Dataset["mode"] })}
                >
                  <option value="sequential">{t("in order (round-robin)")}</option>
                  <option value="random">{t("random")}</option>
                </select>
                <label>{t("Source")}</label>
                <select
                  value={selected.source_kind}
                  onChange={(e) =>
                    update({ source_kind: e.target.value as DatasetSourceKind })
                  }
                >
                  <option value="file">{t("local file (CSV / JSON)")}</option>
                  <option value="url">{t("URL / S3 (CSV / JSON)")}</option>
                  <option value="db">{t("database (SQL)")}</option>
                </select>

                {selected.source_kind === "file" && (
                  <>
                    <label>{t("File")}</label>
                    <div className="tls-file-input">
                      <input
                        value={selected.path}
                        placeholder={t("path to .csv / .json")}
                        onChange={(e) => update({ path: e.target.value })}
                      />
                      <button onClick={pickFile}>{t("Browse…")}</button>
                    </div>
                  </>
                )}
                {selected.source_kind === "url" && (
                  <>
                    <label>URL</label>
                    <input
                      value={selected.url}
                      placeholder="https://bucket.s3.amazonaws.com/users.csv?…"
                      onChange={(e) => update({ url: e.target.value })}
                    />
                    <label title={t("For a private S3 bucket — sign the request with AWS credentials (SigV4). Leave off for a public object or a presigned URL.")}>
                      {t("Private S3 (AWS keys)")}
                    </label>
                    <label className="inline-check">
                      <input
                        type="checkbox"
                        checked={!!selected.aws_enabled}
                        onChange={(e) => update({ aws_enabled: e.target.checked })}
                      />{" "}
                      {t("sign the request with AWS credentials")}
                    </label>
                    {selected.aws_enabled && (
                      <>
                        <label>{t("AWS region")}</label>
                        <input
                          value={selected.aws_region ?? ""}
                          placeholder="us-east-1"
                          onChange={(e) => update({ aws_region: e.target.value })}
                        />
                        <label>{t("Access key ID")}</label>
                        <input
                          value={selected.aws_access_key_id ?? ""}
                          placeholder="AKIA…"
                          autoComplete="off"
                          spellCheck={false}
                          onChange={(e) => update({ aws_access_key_id: e.target.value })}
                        />
                        <label>{t("Secret access key")}</label>
                        <input
                          type="password"
                          value={selected.aws_secret_access_key ?? ""}
                          autoComplete="off"
                          onChange={(e) => update({ aws_secret_access_key: e.target.value })}
                        />
                        <label title={t("Only for temporary credentials (STS / assumed role)")}>
                          {t("Session token (optional)")}
                        </label>
                        <input
                          type="password"
                          value={selected.aws_session_token ?? ""}
                          autoComplete="off"
                          onChange={(e) => update({ aws_session_token: e.target.value })}
                        />
                      </>
                    )}
                  </>
                )}
                {selected.source_kind === "db" && (
                  <>
                    <label title={t("postgres:// mysql:// (mariadb://) sqlite: — you can paste a JDBC URL")}>
                      {t("Connection string")}
                    </label>
                    <div className="tls-file-input">
                      <input
                        type={showDbUrl ? "text" : "password"}
                        value={selected.db_url}
                        placeholder="postgres://user:pass@host:5432/db"
                        onChange={(e) => update({ db_url: e.target.value })}
                      />
                      <button className="ghost" onClick={() => setShowDbUrl((s) => !s)} title={t("Show/hide the connection string")}>
                        {showDbUrl ? t("Hide") : t("Show")}
                      </button>
                    </div>
                    <label title={t("The SELECT runs once before the load; its rows are used as data")}>
                      {t("SQL query")}
                    </label>
                    <input
                      value={selected.query}
                      placeholder="SELECT id, email FROM users LIMIT 1000"
                      onChange={(e) => update({ query: e.target.value })}
                    />
                    <div className="db-test-row">
                      <button onClick={testDb} disabled={testing}>
                        {testing ? <span className="spinner" /> : t("Test connection")}
                      </button>
                      <span className="lt-hint" style={{ margin: 0 }}>
                        {t("runs your SQL now — add")} <code>LIMIT 1</code> {t("for a quick check")}
                      </span>
                    </div>
                    {testResult && (
                      <div className={`db-test-result ${testResult.ok ? "ok" : "bad"}`}>
                        {testResult.text}
                      </div>
                    )}
                    <div className="lt-hint" style={{ marginTop: 6 }}>
                      {t("The query runs once (both in the app and in the CLI), and its rows become data: reference a column as")}{" "}
                      <code>{"{{$data.name.column}}"}</code>. {t("Works even for a million rows (streamed with a limit). You can embed login/password in the URL.")}
                    </div>
                  </>
                )}
                {selected.source_kind !== "db" && (
                  <>
                    <label>{t("Format")}</label>
                    <select
                      value={selected.format}
                      onChange={(e) =>
                        update({ format: e.target.value as Dataset["format"] })
                      }
                    >
                      <option value="">{t("auto (by extension)")}</option>
                      <option value="csv">CSV</option>
                      <option value="json">{t("JSON (array of objects)")}</option>
                    </select>
                  </>
                )}
              </div>

              <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 12 }}>
                <button className="ghost" onClick={remove} title={t("Delete dataset")}>
                  🗑 {t("Delete")}
                </button>
              </div>

              <div className="lt-hint" style={{ marginTop: 12 }}>
                {t("Dataset columns are available in any request field as")}{" "}
                <code>{"{{$data." + (selected.name || "name") + ".column}}"}</code>. {t("All references to one dataset within a single request take the same row. Data is read once before the run.")}
              </div>
            </>
          ) : (
            <div className="lt-hint">{t("Create a dataset to substitute values from a collection.")}</div>
          )}

          <div className="lt-hint" style={{ marginTop: 16, borderTop: "1px solid var(--border)", paddingTop: 12 }}>
            <b style={{ color: "var(--text)" }}>{t("Value generators")}</b> {t("(work without a dataset, in any field):")}<br />
            <code>{"{{$randomInt(1,1000)}}"}</code> · <code>{"{{$randomFrom(a|b|c)}}"}</code> ·{" "}
            <code>{"{{$randomString(12)}}"}</code> · <code>{"{{$uuid}}"}</code> ·{" "}
            <code>{"{{$timestamp}}"}</code> · <code>{"{{$counter}}"}</code>
          </div>
        </div>
      </div>
    </div>
  );
}
