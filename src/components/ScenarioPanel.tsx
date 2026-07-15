import { useMemo, useState } from "react";
import { useT, tr2 } from "../i18n";
import { fmtMs, fmtNum, lineChart } from "../charts";
import {
  Collection,
  RequestConfig,
  ScenarioProgress,
  ScenarioResult,
} from "../types";

export interface ScenarioItemConfig {
  requestId: string;
  enabled: boolean;
  rps: number;
}

export interface ScenarioRunConfig {
  durationSecs: number;
  timeoutMs: number;
  items: ScenarioItemConfig[];
}

const PALETTE = [
  "#4c9df3",
  "#3fb950",
  "#d29922",
  "#a371f7",
  "#f0649a",
  "#39c5cf",
  "#ff9d5c",
  "#f85149",
  "#7ee0ff",
  "#b0bec5",
];

interface Props {
  collection: Collection;
  running: boolean;
  progress: ScenarioProgress | null;
  progressLog: ScenarioProgress[];
  result: ScenarioResult | null;
  error: string | null;
  onStart: (config: ScenarioRunConfig) => void;
  onStop: () => void;
  onExportHtml: () => void;
  onExportConfig: (config: ScenarioRunConfig) => void;
  onClose: () => void;
  tokenRefreshes: number;
  /** Unresolved {{vars}} for a request in the active environment — shown inline
   *  the moment the endpoint is checked, not only at Run time. */
  missingVars: (r: RequestConfig) => string[];
}

/// A missing var that looks like a dataset/file-pool reference typed without the
/// leading `$` ({{data.users.id}} instead of {{$data.users.id}}).
export function looksLikeDatasetTypo(name: string): boolean {
  return name.startsWith("data.") || name.startsWith("file.");
}

export default function ScenarioPanel(p: Props) {
  const t = useT();
  // Only HTTP requests can be scenario targets — a scenario target IS an HTTP
  // spec (method + url). gRPC/WS/DB requests have no url here and would leak in
  // as blank-URL targets, so they're excluded (not just db).
  const httpRequests = useMemo(
    () => p.collection.requests.filter((r) => r.kind === "http"),
    [p.collection]
  );

  const [duration, setDuration] = useState(30);
  // Named timeoutMs/setTimeoutMs (not timeout/setTimeout) — the latter shadows
  // the global window.setTimeout, which is easy to reach for by accident
  // elsewhere in this file. See pa4.
  const [timeoutMs, setTimeoutMs] = useState(10000);
  const [items, setItems] = useState<Record<string, { enabled: boolean; rps: number }>>(
    () => {
      const init: Record<string, { enabled: boolean; rps: number }> = {};
      for (const r of httpRequests) init[r.id] = { enabled: false, rps: 50 };
      return init;
    }
  );

  const get = (id: string) => items[id] ?? { enabled: false, rps: 50 };
  const setItem = (id: string, patch: Partial<{ enabled: boolean; rps: number }>) =>
    setItems((s) => ({ ...s, [id]: { ...get(id), ...patch } }));

  const selected = httpRequests.filter((r) => get(r.id).enabled);
  const totalRps = selected.reduce((a, r) => a + (get(r.id).rps || 0), 0);

  // Unresolved vars per request (against the active environment) — so the
  // problem is visible right when the endpoint is checked, not after Run.
  const missing = useMemo(() => {
    const m = new Map<string, string[]>();
    for (const r of httpRequests) m.set(r.id, p.missingVars(r));
    return m;
  }, [httpRequests, p.missingVars]);
  const selectedWithMissing = selected.filter((r) => (missing.get(r.id) ?? []).length > 0);

  const toggleAll = (on: boolean) =>
    setItems((s) => {
      const next = { ...s };
      for (const r of httpRequests) next[r.id] = { ...get(r.id), enabled: on };
      return next;
    });

  const start = () => {
    p.onStart({
      durationSecs: duration,
      timeoutMs,
      items: selected.map((r) => ({
        requestId: r.id,
        enabled: true,
        rps: get(r.id).rps,
      })),
    });
  };

  // live per-endpoint RPS chart (from progress log)
  const liveChart = useMemo(() => {
    if (!p.progressLog.length) return "";
    const names = p.progressLog[p.progressLog.length - 1].targets.map((t) => t.name);
    const series = names.map((name, i) => ({
      name: truncate(name, 16),
      color: PALETTE[i % PALETTE.length],
      points: p.progressLog.map((snap, x) => ({
        x: x + 1,
        y: snap.targets[i]?.rps_current ?? 0,
      })),
    }));
    return lineChart({ series, height: 240, xFormat: (v) => `${Math.round(v)}${t("s")}`, width: 900 });
  }, [p.progressLog, t]);

  const finalChart = useMemo(() => {
    if (!p.result) return "";
    const series = p.result.targets.map((t, i) => ({
      name: truncate(labelFor(t, selected[i]), 16),
      color: PALETTE[i % PALETTE.length],
      points: t.timeline.map((tp) => ({ x: tp.sec, y: tp.requests })),
    }));
    return lineChart({ series, height: 260, yFormat: fmtNum, xFormat: (v) => `${Math.round(v)}${t("s")}`, width: 900 });
  }, [p.result, selected, t]);

  return (
    <div className="scenario-overlay">
      <div className="scenario-head">
        <div>
          <div className="scenario-title">⚡ {t("Service load test")}</div>
          <div className="scenario-sub">{p.collection.name}</div>
        </div>
        <button className="ghost" onClick={p.onClose} title={t("Close")}>
          ✕
        </button>
      </div>

      <div className="scenario-body">
        {!p.running && !p.result && (
          <div className="scenario-config">
            <div className="scenario-toolbar">
              <div className="lt-field">
                <label>{t("Duration, sec")}</label>
                <input
                  type="number"
                  min={1}
                  max={3600}
                  value={duration}
                  onChange={(e) => setDuration(+e.target.value || 1)}
                />
              </div>
              <div className="lt-field">
                <label>{t("Timeout, ms")}</label>
                <input
                  type="number"
                  min={100}
                  value={timeoutMs}
                  onChange={(e) => setTimeoutMs(+e.target.value || 10000)}
                />
              </div>
              <div className="scenario-toolbar-spacer" />
              <button className="ghost" onClick={() => toggleAll(true)}>
                {t("Select all")}
              </button>
              <button className="ghost" onClick={() => toggleAll(false)}>
                {t("Clear all")}
              </button>
            </div>

            {httpRequests.length === 0 ? (
              <div className="lt-hint">
                {t("The collection has no HTTP requests. Import OpenAPI/Swagger or add requests.")}
              </div>
            ) : (
              <table className="scenario-table">
                <thead>
                  <tr>
                    <th style={{ width: 34 }}></th>
                    <th style={{ width: 70 }}>{t("Method")}</th>
                    <th>{t("Endpoint")}</th>
                    <th style={{ width: 150 }}>RPS</th>
                  </tr>
                </thead>
                <tbody>
                  {httpRequests.map((r) => (
                    <tr key={r.id} className={get(r.id).enabled ? "on" : ""}>
                      <td>
                        <input
                          type="checkbox"
                          checked={get(r.id).enabled}
                          onChange={(e) => setItem(r.id, { enabled: e.target.checked })}
                        />
                      </td>
                      <td>
                        <span className={`method-tag m-${r.method}`}>{r.method}</span>
                      </td>
                      <td>
                        <div className="scenario-name">{r.name}</div>
                        <div className="scenario-url">{r.url || "—"}</div>
                        {get(r.id).enabled && (missing.get(r.id) ?? []).length > 0 && (
                          <div className="scenario-missing">
                            ⚠ {t("Unset variables")}:{" "}
                            {(missing.get(r.id) ?? []).map((v) => (
                              <code key={v}>{`{{${v}}}`}</code>
                            ))}
                            {(missing.get(r.id) ?? []).some(looksLikeDatasetTypo) && (
                              <div>
                                {t("Looks like a dataset reference — the syntax is")}{" "}
                                <code>{"{{$" + "data.name.column}}"}</code> ({t("note the $")})
                              </div>
                            )}
                          </div>
                        )}
                      </td>
                      <td>
                        <input
                          type="number"
                          min={1}
                          value={get(r.id).rps}
                          disabled={!get(r.id).enabled}
                          onChange={(e) => setItem(r.id, { rps: +e.target.value || 1 })}
                        />
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}

            <div className="scenario-footer">
              <div className="scenario-summary">
                {t("Endpoints selected:")} <b>{selected.length}</b> · {t("total target RPS:")}{" "}
                <b>{fmtNum(totalRps)}</b>
                {selectedWithMissing.length > 0 && (
                  <span className="scenario-missing" style={{ marginLeft: 10 }}>
                    ⚠{" "}
                    {tr2("{n} selected endpoint(s) have unset variables", {
                      n: selectedWithMissing.length,
                    })}
                  </span>
                )}
              </div>
              <div style={{ display: "flex", gap: 8 }}>
                <button
                  onClick={() =>
                    p.onExportConfig({
                      durationSecs: duration,
                      timeoutMs,
                      items: selected.map((r) => ({
                        requestId: r.id,
                        enabled: true,
                        rps: get(r.id).rps,
                      })),
                    })
                  }
                  disabled={selected.length === 0}
                  title={t("Save a config to run in CI / Kubernetes")}
                >
                  ↓ {t("Export for CI")}
                </button>
                <button className="primary" onClick={start} disabled={selected.length === 0}>
                  ▶ {t("Run load test")}
                </button>
              </div>
            </div>
          </div>
        )}

        {p.error && <div className="lt-error">{p.error}</div>}

        {p.tokenRefreshes > 0 && (
          <div className="token-refresh-badge">
            🔄 {tr2("Token refreshed automatically: {n}", { n: p.tokenRefreshes })}
          </div>
        )}

        {(p.running || p.result) && (
          <div className="scenario-run">
            {p.running && (
              <div className="scenario-live-head">
                <span className="spinner" style={{ borderTopColor: "var(--accent)" }} />
                <span>
                  {tr2("Load running · {elapsed}s of {total}s", {
                    elapsed: Math.round(p.progress?.elapsed_secs ?? 0),
                    total: duration,
                  })}
                </span>
                <button className="danger" onClick={p.onStop} style={{ marginLeft: "auto" }}>
                  ■ {t("Stop")}
                </button>
              </div>
            )}

            {p.result && !p.running && (
              <div className={`lt-done-banner ${p.result.overall.error_rate < 1 ? "ok" : "bad"}`}>
                {p.result.overall.error_rate < 1
                  ? t("✔ Load test finished")
                  : t("✖ Finished with errors")}
                {p.result.stopped_early ? t(" (stopped manually)") : ""}
                <span className="export-btns">
                  <button className="primary" onClick={p.onExportHtml}>
                    📄 {t("Export report (HTML)")}
                  </button>
                </span>
              </div>
            )}

            <OverallCards progress={p.progress} result={p.result} running={p.running} />

            <div className="lt-chart">
              <h3>{t("RPS per endpoint")}</h3>
              <div dangerouslySetInnerHTML={{ __html: p.running ? liveChart : finalChart }} />
            </div>

            <div className="lt-chart">
              <h3>{t("Results per endpoint")}</h3>
              <PerEndpointTable
                progress={p.progress}
                result={p.result}
                running={p.running}
                selected={selected}
              />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function OverallCards({
  progress,
  result,
  running,
}: {
  progress: ScenarioProgress | null;
  result: ScenarioResult | null;
  running: boolean;
}) {
  const t = useT();
  const cards =
    running && progress
      ? [
          { label: t("Total requests"), value: fmtNum(progress.overall_total) },
          { label: t("RPS (total)"), value: fmtNum(progress.overall_rps) },
          { label: t("p95 (overall)"), value: fmtMs(progress.overall_p95_ms) },
          {
            label: t("Errors"),
            value: fmtNum(progress.overall_errors),
            cls: progress.overall_errors > 0 ? "bad" : "ok",
          },
        ]
      : result
        ? [
            { label: t("Total requests"), value: fmtNum(result.overall.total_requests) },
            { label: t("RPS (average)"), value: fmtNum(result.overall.rps_avg) },
            { label: t("p95 (overall)"), value: fmtMs(result.overall.p95_ms) },
            {
              label: t("Errors"),
              value: `${fmtNum(result.overall.errors)} · ${result.overall.error_rate.toFixed(1)}%`,
              cls: result.overall.errors > 0 ? "bad" : "ok",
            },
          ]
        : [];
  return (
    <div className="lt-cards">
      {cards.map((c) => (
        <div className={`lt-card ${(c as any).cls ?? ""}`} key={c.label}>
          <div className="label">{c.label}</div>
          <div className="value">{c.value}</div>
        </div>
      ))}
    </div>
  );
}

function PerEndpointTable({
  progress,
  result,
  running,
  selected,
}: {
  progress: ScenarioProgress | null;
  result: ScenarioResult | null;
  running: boolean;
  selected: RequestConfig[];
}) {
  const t = useT();
  if (running && progress) {
    return (
      <table className="scenario-table result">
        <thead>
          <tr>
            <th>{t("Endpoint")}</th>
            <th>{t("RPS now")}</th>
            <th>{t("Requests")}</th>
            <th>{t("Errors")}</th>
          </tr>
        </thead>
        <tbody>
          {progress.targets.map((t, i) => (
            <tr key={i}>
              <td>{t.name}</td>
              <td>{fmtNum(t.rps_current)}</td>
              <td>{fmtNum(t.total)}</td>
              <td className={t.errors > 0 ? "err" : ""}>{fmtNum(t.errors)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    );
  }
  if (result) {
    return (
      <table className="scenario-table result">
        <thead>
          <tr>
            <th>{t("Endpoint")}</th>
            <th>{t("Method")}</th>
            <th>{t("Requests")}</th>
            <th>RPS</th>
            <th>{t("Errors")}</th>
            <th>p50</th>
            <th>p95</th>
            <th>p99</th>
          </tr>
        </thead>
        <tbody>
          {result.targets.map((t, i) => (
            <tr key={i}>
              <td>{labelFor(t, selected[i])}</td>
              <td>
                <span className={`method-tag m-${t.method}`}>{t.method}</span>
              </td>
              <td>{fmtNum(t.total_requests)}</td>
              <td>{fmtNum(t.rps_avg)}</td>
              <td className={t.errors > 0 ? "err" : ""}>
                {fmtNum(t.errors)} · {t.error_rate.toFixed(1)}%
              </td>
              <td>{fmtMs(t.p50_ms)}</td>
              <td>{fmtMs(t.p95_ms)}</td>
              <td>{fmtMs(t.p99_ms)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    );
  }
  return null;
}

function labelFor(result: { url: string }, req?: RequestConfig): string {
  if (req?.name) return req.name;
  return result.url;
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n - 1) + "…" : s;
}
