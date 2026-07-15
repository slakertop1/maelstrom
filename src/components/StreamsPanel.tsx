import { useMemo, useState } from "react";
import { useT, tr2 } from "../i18n";
import { fmtMs, fmtNum } from "../charts";
import {
  Collection,
  RequestConfig,
  StreamsProgress,
  StreamsResult,
  StreamResult,
  UiStream,
  UiExtract,
  newUiStream,
  newUiStreamStep,
  newUiExtract,
} from "../types";

export interface StreamRunConfig {
  durationSecs: number;
  timeoutMs: number;
  streams: UiStream[];
}

interface Props {
  collection: Collection;
  running: boolean;
  progress: StreamsProgress | null;
  result: StreamsResult | null;
  error: string | null;
  onStart: (config: StreamRunConfig) => void;
  onStop: () => void;
  onClose: () => void;
  /** Unresolved {{env vars}} for the current streams (extracted vars excluded). */
  missingVars: (streams: UiStream[]) => string[];
}

export default function StreamsPanel(p: Props) {
  const t = useT();
  // Only HTTP requests can be steps (a step IS an HTTP request + extract).
  const httpRequests = useMemo(
    () => p.collection.requests.filter((r) => r.kind === "http"),
    [p.collection]
  );
  const reqName = (id: string) => httpRequests.find((r) => r.id === id)?.name ?? t("(deleted)");

  const [duration, setDuration] = useState(30);
  // timeoutMs/setTimeoutMs, not timeout/setTimeout — avoids shadowing the
  // global window.setTimeout. See pa4.
  const [timeoutMs, setTimeoutMs] = useState(10000);
  const [streams, setStreams] = useState<UiStream[]>([]);

  const patchStream = (id: string, patch: Partial<UiStream>) =>
    setStreams((ss) => ss.map((s) => (s.id === id ? { ...s, ...patch } : s)));
  const addStream = () => setStreams((ss) => [...ss, newUiStream(ss.length + 1)]);
  const removeStream = (id: string) => setStreams((ss) => ss.filter((s) => s.id !== id));

  const addStep = (sid: string) => {
    const first = httpRequests[0];
    if (!first) return;
    patchStreamSteps(sid, (steps) => [...steps, newUiStreamStep(first.id)]);
  };
  const patchStreamSteps = (sid: string, fn: (steps: UiStream["steps"]) => UiStream["steps"]) =>
    setStreams((ss) => ss.map((s) => (s.id === sid ? { ...s, steps: fn(s.steps) } : s)));
  const setStepReq = (sid: string, stepId: string, requestId: string) =>
    patchStreamSteps(sid, (steps) => steps.map((st) => (st.id === stepId ? { ...st, requestId } : st)));
  const removeStep = (sid: string, stepId: string) =>
    patchStreamSteps(sid, (steps) => steps.filter((st) => st.id !== stepId));
  const moveStep = (sid: string, i: number, dir: -1 | 1) =>
    patchStreamSteps(sid, (steps) => {
      const j = i + dir;
      if (j < 0 || j >= steps.length) return steps;
      const next = steps.slice();
      [next[i], next[j]] = [next[j], next[i]];
      return next;
    });

  const patchExtract = (
    sid: string,
    stepId: string,
    fn: (ex: UiExtract[]) => UiExtract[]
  ) =>
    patchStreamSteps(sid, (steps) =>
      steps.map((st) => (st.id === stepId ? { ...st, extract: fn(st.extract) } : st))
    );

  const runnable = streams.filter((s) => s.steps.length > 0);
  const missing = p.missingVars(runnable);

  const start = () =>
    p.onStart({ durationSecs: duration, timeoutMs, streams: runnable });

  return (
    <div className="scenario-overlay">
      <div className="scenario-head">
        <div>
          <div className="scenario-title">⚡ {t("Chained load (streams)")}</div>
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
              <button className="ghost" onClick={addStream}>
                ＋ {t("Add stream")}
              </button>
            </div>

            {httpRequests.length === 0 ? (
              <div className="lt-hint">
                {t("The collection has no HTTP requests. Import OpenAPI/Swagger or add requests.")}
              </div>
            ) : streams.length === 0 ? (
              <div className="lt-hint">
                {t("Add a stream. A stream fires its steps in order at its own rate (iterations/sec); one step = single load, several = a chain. Extract a value from a step's response to use as {{var}} in the next.")}
              </div>
            ) : (
              streams.map((s) => (
                <div key={s.id} className="stream-card">
                  <div className="stream-head">
                    <input
                      className="stream-name"
                      value={s.name}
                      onChange={(e) => patchStream(s.id, { name: e.target.value })}
                    />
                    <label
                      className="stream-rps"
                      title={t("Chain iterations started per second (open model). Each step sees ≈ this rate.")}
                    >
                      {t("iters/sec")}
                      <input
                        type="number"
                        min={1}
                        value={s.rps}
                        onChange={(e) => patchStream(s.id, { rps: +e.target.value || 1 })}
                      />
                    </label>
                    <button className="ghost" onClick={() => addStep(s.id)}>
                      ＋ {t("step")}
                    </button>
                    <button
                      className="ghost"
                      title={t("Delete stream")}
                      onClick={() => removeStream(s.id)}
                    >
                      🗑
                    </button>
                  </div>

                  {s.steps.length === 0 ? (
                    <div className="lt-hint" style={{ margin: "4px 0 0" }}>
                      {t("No steps yet — add a request as the first step.")}
                    </div>
                  ) : (
                    <ol className="stream-steps">
                      {s.steps.map((st, i) => (
                        <li key={st.id} className="stream-step">
                          <div className="stream-step-row">
                            <span className="stream-step-n">{i + 1}</span>
                            <select
                              value={st.requestId}
                              onChange={(e) => setStepReq(s.id, st.id, e.target.value)}
                            >
                              {httpRequests.map((r) => (
                                <option key={r.id} value={r.id}>
                                  {r.method} · {r.name}
                                </option>
                              ))}
                            </select>
                            <span className="stream-step-actions">
                              <button
                                className="ghost"
                                disabled={i === 0}
                                title={t("Move up")}
                                onClick={() => moveStep(s.id, i, -1)}
                              >
                                ↑
                              </button>
                              <button
                                className="ghost"
                                disabled={i === s.steps.length - 1}
                                title={t("Move down")}
                                onClick={() => moveStep(s.id, i, 1)}
                              >
                                ↓
                              </button>
                              <button
                                className="ghost"
                                title={t("Delete step")}
                                onClick={() => removeStep(s.id, st.id)}
                              >
                                ✕
                              </button>
                            </span>
                          </div>

                          <div className="stream-extracts">
                            {st.extract.map((e) => (
                              <div key={e.id} className="stream-extract-row">
                                <input
                                  className="ex-name"
                                  placeholder={t("var")}
                                  value={e.name}
                                  onChange={(ev) =>
                                    patchExtract(s.id, st.id, (xs) =>
                                      xs.map((x) => (x.id === e.id ? { ...x, name: ev.target.value } : x))
                                    )
                                  }
                                />
                                <span className="ex-eq">←</span>
                                <select
                                  value={e.from}
                                  onChange={(ev) =>
                                    patchExtract(s.id, st.id, (xs) =>
                                      xs.map((x) =>
                                        x.id === e.id ? { ...x, from: ev.target.value as UiExtract["from"] } : x
                                      )
                                    )
                                  }
                                >
                                  <option value="json">JSON</option>
                                  <option value="header">{t("header")}</option>
                                  <option value="regex">regex</option>
                                </select>
                                <input
                                  className="ex-expr"
                                  placeholder={
                                    e.from === "json"
                                      ? "data.token"
                                      : e.from === "header"
                                        ? "X-Token"
                                        : "token=(\\w+)"
                                  }
                                  value={e.expr}
                                  onChange={(ev) =>
                                    patchExtract(s.id, st.id, (xs) =>
                                      xs.map((x) => (x.id === e.id ? { ...x, expr: ev.target.value } : x))
                                    )
                                  }
                                />
                                <button
                                  className="ghost"
                                  title={t("Remove extract")}
                                  onClick={() =>
                                    patchExtract(s.id, st.id, (xs) => xs.filter((x) => x.id !== e.id))
                                  }
                                >
                                  ✕
                                </button>
                              </div>
                            ))}
                            <button
                              className="ghost stream-add-extract"
                              onClick={() => patchExtract(s.id, st.id, (xs) => [...xs, newUiExtract()])}
                              title={t("Extract a value from this step's response into a variable for later steps")}
                            >
                              ＋ extract
                            </button>
                          </div>
                        </li>
                      ))}
                    </ol>
                  )}
                </div>
              ))
            )}

            <div className="scenario-footer">
              <div className="scenario-summary">
                {tr2("{n} runnable stream(s)", { n: runnable.length })}
                {missing.length > 0 && (
                  <span className="scenario-missing" style={{ marginLeft: 10 }}>
                    ⚠ {tr2("unset variables: {vars}", { vars: missing.map((v) => `{{${v}}}`).join(", ") })}
                  </span>
                )}
              </div>
              <button className="primary" onClick={start} disabled={runnable.length === 0}>
                ▶ {t("Run")}
              </button>
            </div>
          </div>
        )}

        {p.error && <div className="lt-error">{p.error}</div>}

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
              </div>
            )}

            <OverallCards progress={p.progress} result={p.result} running={p.running} />

            {p.result &&
              p.result.streams.map((s, i) => <StreamResultView key={i} s={s} />)}

            {p.running && p.progress && (
              <table className="scenario-table result">
                <thead>
                  <tr>
                    <th>{t("Stream")}</th>
                    <th>{t("iters/sec")}</th>
                    <th>{t("Iterations")}</th>
                    <th>{t("Errors")}</th>
                  </tr>
                </thead>
                <tbody>
                  {p.progress.streams.map((s, i) => (
                    <tr key={i}>
                      <td>{s.name}</td>
                      <td>{fmtNum(s.iters_per_sec)}</td>
                      <td>{fmtNum(s.iterations)}</td>
                      <td className={s.errors > 0 ? "err" : ""}>{fmtNum(s.errors)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </div>
        )}
      </div>
    </div>
  );

  void reqName; // reserved for future funnel labelling
}

function OverallCards({
  progress,
  result,
  running,
}: {
  progress: StreamsProgress | null;
  result: StreamsResult | null;
  running: boolean;
}) {
  const t = useT();
  const cards =
    running && progress
      ? [
          { label: t("Total requests"), value: fmtNum(progress.overall_total) },
          { label: t("RPS (all steps)"), value: fmtNum(progress.overall_rps) },
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
            { label: t("RPS (all steps, avg)"), value: fmtNum(result.overall.rps_avg) },
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
        <div className={`lt-card ${(c as { cls?: string }).cls ?? ""}`} key={c.label}>
          <div className="label">{c.label}</div>
          <div className="value">{c.value}</div>
        </div>
      ))}
    </div>
  );
}

function StreamResultView({ s }: { s: StreamResult }) {
  const t = useT();
  const ok = s.success_rate >= 99;
  const last = s.steps.length - 1;
  const targetRps = last >= 0 ? s.steps[last].rps_avg : 0;
  return (
    <div className="lt-chart">
      <h3>
        {t("Stream")} «{s.name}» ·{" "}
        <b>{tr2("{rps} rps on the target endpoint", { rps: fmtNum(targetRps) })}</b> ·{" "}
        <span className={ok ? "sr-ok" : "sr-bad"}>
          {tr2("{done}/{started} chains completed ({rate}%)", {
            done: fmtNum(s.iterations_completed),
            started: fmtNum(s.iterations_started),
            rate: s.success_rate.toFixed(1),
          })}
        </span>{" "}
        · e2e p95 {fmtMs(s.e2e_p95_ms)}
        {s.dropped > 0 ? ` · ${tr2("{n} not sent", { n: s.dropped })}` : ""}
      </h3>
      <table className="scenario-table result">
        <thead>
          <tr>
            <th>#</th>
            <th>{t("Method")}</th>
            <th>{t("Endpoint")}</th>
            <th>{t("Requests")}</th>
            <th>RPS</th>
            <th>{t("Errors")}</th>
            <th>p50</th>
            <th>p95</th>
            <th>p99</th>
          </tr>
        </thead>
        <tbody>
          {s.steps.map((st, i) => (
            <tr key={i}>
              <td>{i + 1}</td>
              <td>
                <span className={`method-tag m-${st.method}`}>{st.method}</span>
              </td>
              <td className="scenario-url" style={{ maxWidth: 360 }}>
                {st.url}
                {i === last ? (
                  <span style={{ opacity: 0.7, fontSize: "0.85em" }}> · {t("target")}</span>
                ) : null}
              </td>
              <td>{fmtNum(st.total_requests)}</td>
              <td>{fmtNum(st.rps_avg)}</td>
              <td className={st.errors > 0 ? "err" : ""}>
                {fmtNum(st.errors)} · {st.error_rate.toFixed(1)}%
              </td>
              <td>{fmtMs(st.p50_ms)}</td>
              <td>{fmtMs(st.p95_ms)}</td>
              <td>{fmtMs(st.p99_ms)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
