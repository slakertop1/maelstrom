import { useMemo } from "react";
import { useT, tr2 } from "../i18n";
import { fmtMs, fmtNum, lineChart, barChart, donutChart } from "../charts";
import {
  LoadTestResult,
  ProgressSnapshot,
  TimelinePoint,
} from "../types";

export interface LoadTestConfig {
  vus: number;
  durationSecs: number;
  rpsLimit: number | "";
  timeoutMs: number;
}

interface Props {
  running: boolean;
  progress: ProgressSnapshot | null;
  timeline: TimelinePoint[];
  result: LoadTestResult | null;
  error: string | null;
  config: LoadTestConfig;
  setConfig: (c: LoadTestConfig) => void;
  onStart: () => void;
  onStop: () => void;
  onExportHtml: () => void;
  onExportJson: () => void;
  onExportConfig: () => void;
  tokenRefreshes: number;
}

export default function LoadTestPanel(p: Props) {
  const t = useT();
  const { config, setConfig } = p;

  const stats = useMemo(() => {
    if (p.running && p.progress) {
      const s = p.progress;
      return [
        { label: t("Requests"), value: fmtNum(s.total_requests) },
        { label: t("RPS (current)"), value: fmtNum(s.rps_current) },
        { label: t("Average"), value: fmtMs(s.avg_ms) },
        { label: "p95", value: fmtMs(s.p95_ms) },
        { label: "p99", value: fmtMs(s.p99_ms) },
        {
          label: t("Errors"),
          value: fmtNum(s.errors),
          cls: s.errors > 0 ? "bad" : "ok",
        },
      ];
    }
    if (p.result) {
      const r = p.result;
      return [
        { label: t("Requests"), value: fmtNum(r.total_requests) },
        { label: t("RPS (average)"), value: fmtNum(r.rps_avg) },
        { label: t("Average"), value: fmtMs(r.latency_avg_ms) },
        { label: "p95", value: fmtMs(r.p95_ms) },
        { label: "p99", value: fmtMs(r.p99_ms) },
        {
          label: t("Errors"),
          value: `${fmtNum(r.errors)} · ${r.error_rate.toFixed(1)}%`,
          cls: r.errors > 0 ? "bad" : "ok",
        },
      ];
    }
    return null;
  }, [p.running, p.progress, p.result, t]);

  const timeline = p.running || !p.result ? p.timeline : p.result.timeline;

  const rpsChartHtml = useMemo(() => {
    if (!timeline.length) return "";
    return lineChart({
      series: [
        {
          name: t("Requests/s"),
          color: "#4c9df3",
          fill: true,
          points: timeline.map((t) => ({ x: t.sec, y: t.requests })),
        },
        {
          name: t("Errors/s"),
          color: "#f85149",
          points: timeline.map((t) => ({ x: t.sec, y: t.errors })),
        },
      ],
      height: 220,
      xFormat: (v) => `${Math.round(v)}${t("s")}`,
    });
  }, [timeline, t]);

  const latChartHtml = useMemo(() => {
    if (!timeline.length) return "";
    return lineChart({
      series: [
        { name: "p50", color: "#3fb950", points: timeline.map((t) => ({ x: t.sec, y: t.p50_ms })) },
        { name: "p95", color: "#d29922", points: timeline.map((t) => ({ x: t.sec, y: t.p95_ms })) },
        { name: "p99", color: "#f85149", points: timeline.map((t) => ({ x: t.sec, y: t.p99_ms })) },
      ],
      height: 220,
      yFormat: fmtMs,
      xFormat: (v) => `${Math.round(v)}${t("s")}`,
    });
  }, [timeline, t]);

  const histHtml = useMemo(() => {
    if (!p.result || !p.result.histogram.length) return "";
    return barChart(
      p.result.histogram.map((b) => ({
        label: `${fmtNum(b.from_ms)}–${fmtNum(b.to_ms)} ${t("ms")}`,
        count: b.count,
      })),
      { color: "#a371f7", height: 230 }
    );
  }, [p.result, t]);

  const donutHtml = useMemo(() => {
    if (!p.result || !p.result.status_counts.length) return "";
    return donutChart(
      p.result.status_counts.map(([label, value]) => ({ label, value })),
      { height: 230 }
    );
  }, [p.result]);

  const progressPct = p.running
    ? Math.min(100, ((p.progress?.elapsed_secs ?? 0) / config.durationSecs) * 100)
    : 0;

  return (
    <div>
      <div className="lt-config">
        <div
          className="lt-field"
          title={t("Without a target RPS: how many parallel “users” hammer back-to-back (throughput = VUs × response time). With a target RPS set, the rate is fixed and VUs don't bound it.")}
        >
          <label>{t("Virtual users")}</label>
          <input
            type="number"
            min={1}
            max={10000}
            value={config.vus}
            disabled={p.running}
            onChange={(e) => setConfig({ ...config, vus: +e.target.value || 1 })}
          />
        </div>
        <div className="lt-field" title={t("How many seconds to run the load.")}>
          <label>{t("Duration, sec")}</label>
          <input
            type="number"
            min={1}
            max={3600}
            value={config.durationSecs}
            disabled={p.running}
            onChange={(e) =>
              setConfig({ ...config, durationSecs: +e.target.value || 1 })
            }
          />
        </div>
        <div
          className="lt-field"
          title={t("Fixed request rate: exactly this many requests per second are fired, regardless of how slowly the target responds (concurrency grows as needed). Empty — no schedule: VUs send as fast as responses allow.")}
        >
          <label>{t("Target RPS (empty = VU-driven)")}</label>
          <input
            type="number"
            min={1}
            value={config.rpsLimit}
            disabled={p.running}
            placeholder="∞"
            onChange={(e) =>
              setConfig({
                ...config,
                rpsLimit: e.target.value === "" ? "" : +e.target.value,
              })
            }
          />
        </div>
        <div
          className="lt-field"
          title={t("Maximum time per request. Exceeding it counts as an error.")}
        >
          <label>{t("Timeout, ms")}</label>
          <input
            type="number"
            min={100}
            value={config.timeoutMs}
            disabled={p.running}
            onChange={(e) =>
              setConfig({ ...config, timeoutMs: +e.target.value || 10000 })
            }
          />
        </div>
        {p.running ? (
          <button className="danger" onClick={p.onStop}>
            ■ {t("Stop")}
          </button>
        ) : (
          <button className="primary" onClick={p.onStart}>
            ▶ {t("Run test")}
          </button>
        )}
        <button
          onClick={p.onExportConfig}
          disabled={p.running}
          title={t("Export this request as a scenario.json for the headless CLI (CI / Kubernetes)")}
        >
          ↓ {t("Export for CI")}
        </button>
      </div>

      {p.error && <div className="lt-error">{p.error}</div>}

      {p.tokenRefreshes > 0 && (
        <div className="token-refresh-badge">
          🔄 {tr2("Token refreshed automatically: {n}", { n: p.tokenRefreshes })}
        </div>
      )}

      {p.running && (
        <div className="lt-progressbar">
          <div style={{ width: `${progressPct}%` }} />
        </div>
      )}

      {p.result && !p.running && (
        <div className={`lt-done-banner ${p.result.error_rate < 1 ? "ok" : "bad"}`}>
          {p.result.error_rate < 1 ? t("✔ Test finished") : t("✖ Test finished with errors")}
          {p.result.stopped_early ? t(" (stopped manually)") : ""}
          <span className="export-btns">
            <button className="primary" onClick={p.onExportHtml}>
              📄 {t("Export report (HTML)")}
            </button>
            <button onClick={p.onExportJson}>{t("Export JSON")}</button>
          </span>
        </div>
      )}

      {p.result && !p.running && (p.result.dropped ?? 0) > 0 && (
        <div
          className="lt-warn"
          style={{
            marginTop: 8,
            padding: "8px 12px",
            borderRadius: 6,
            background: "rgba(240, 173, 78, 0.15)",
            border: "1px solid rgba(240, 173, 78, 0.5)",
          }}
          title={t("The target responds too slowly to sustain the requested rate (in-flight requests hit the 2×RPS safety cap), so part of the schedule was skipped. Lower the RPS target or speed up the endpoint.")}
        >
          ⚠ {tr2("{n} requests weren't sent — the requested RPS wasn't sustained (achieved RPS is below target).", { n: p.result.dropped ?? 0 })}
        </div>
      )}

      {!stats && (
        <div className="lt-hint">
          {t("The test replays the current request as-is — with the same environment variables, params, headers, body, OAuth2 token and TLS certificates (for databases, the same connection string and SQL) — in parallel on behalf of the given number of virtual users. Stats and charts update every second, and an HTML report with diagrams is exported on completion.")}
        </div>
      )}

      {stats && (
        <div className="lt-cards">
          {stats.map((s) => (
            <div className={`lt-card ${"cls" in s ? (s as any).cls ?? "" : ""}`} key={s.label}>
              <div className="label">{s.label}</div>
              <div className="value">{s.value}</div>
            </div>
          ))}
        </div>
      )}

      {timeline.length > 0 && (
        <>
          <div className="lt-chart">
            <h3>{t("Throughput")}</h3>
            <div dangerouslySetInnerHTML={{ __html: rpsChartHtml }} />
          </div>
          <div className="lt-chart">
            <h3>{t("Latency (percentiles)")}</h3>
            <div dangerouslySetInnerHTML={{ __html: latChartHtml }} />
          </div>
        </>
      )}

      {p.result && !p.running && (
        <div className="lt-grid-2">
          {histHtml && (
            <div className="lt-chart">
              <h3>{t("Response time distribution")}</h3>
              <div dangerouslySetInnerHTML={{ __html: histHtml }} />
            </div>
          )}
          {donutHtml && (
            <div className="lt-chart">
              <h3>{t("Response codes")}</h3>
              <div dangerouslySetInnerHTML={{ __html: donutHtml }} />
            </div>
          )}
        </div>
      )}
    </div>
  );
}
