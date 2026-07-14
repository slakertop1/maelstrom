import { useMemo, useState } from "react";
import { HttpResponseData } from "../types";
import { fmtMs } from "../charts";
import { Assertion, evaluateAssertions } from "../assertions";
import { useT, tr, tr2 } from "../i18n";

interface Props {
  response: HttpResponseData | null;
  error: string | null;
  sending: boolean;
  assertions?: Assertion[];
}

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function highlightJson(text: string): { html: string; pretty: boolean } {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return { html: escapeHtml(text), pretty: false };
  }
  const json = JSON.stringify(parsed, null, 2);
  if (json.length > 1_500_000) return { html: escapeHtml(json), pretty: true };
  const html = escapeHtml(json).replace(
    /("(?:\\.|[^"\\])*")(\s*:)?|\b(true|false)\b|\bnull\b|-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/g,
    (match, str, colon, bool) => {
      if (str !== undefined) {
        return colon
          ? `<span class="json-key">${str}</span>${colon}`
          : `<span class="json-str">${str}</span>`;
      }
      if (bool !== undefined) return `<span class="json-bool">${match}</span>`;
      if (match === "null") return `<span class="json-null">null</span>`;
      return `<span class="json-num">${match}</span>`;
    }
  );
  return { html, pretty: true };
}

function fmtSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return (bytes / 1024 / 1024).toFixed(2) + " " + tr("MB");
  if (bytes >= 1024) return (bytes / 1024).toFixed(1) + " " + tr("KB");
  return bytes + " " + tr("B");
}

export default function ResponseView({ response, error, sending, assertions }: Props) {
  const t = useT();
  const [tab, setTab] = useState<"body" | "headers">("body");

  const assertResults = useMemo(() => {
    if (!response || !assertions || assertions.length === 0) return [];
    return evaluateAssertions(assertions, {
      status: response.status,
      headers: response.headers,
      body: response.body_base64 ? "" : response.body,
      durationMs: response.duration_ms,
    });
  }, [response, assertions]);

  const bodyHtml = useMemo(() => {
    if (!response) return { html: "", pretty: false };
    if (response.body_base64)
      return {
        html: `<em>${tr2("Binary response ({size}) — preview not available", { size: fmtSize(response.size_bytes) })}</em>`,
        pretty: false,
      };
    return highlightJson(response.body);
  }, [response]);

  if (sending) {
    return (
      <div className="response-pane">
        <div className="resp-empty">
          <div className="spinner" style={{ borderTopColor: "var(--accent)" }} />
          <div>{t("Sending request…")}</div>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="response-pane">
        <div className="resp-head">
          <span className="title">{t("Response")}</span>
          <span className="status-pill status-5xx">{t("Error")}</span>
        </div>
        <div className="resp-error">{error}</div>
      </div>
    );
  }

  if (!response) {
    return (
      <div className="response-pane">
        <div className="resp-empty">
          <div style={{ fontSize: 28, opacity: 0.4 }}>⚡</div>
          <div>{t("Send a request to see the response")}</div>
        </div>
      </div>
    );
  }

  const statusClass =
    response.status < 300
      ? "status-2xx"
      : response.status < 400
        ? "status-3xx"
        : response.status < 500
          ? "status-4xx"
          : "status-5xx";

  return (
    <div className="response-pane">
      <div className="resp-head">
        <span className="title">{t("Response")}</span>
        <span className={`status-pill ${statusClass}`}>
          {response.status} {response.status_text}
        </span>
        <span className="resp-meta">{fmtMs(response.duration_ms)}</span>
        <span className="resp-meta">{fmtSize(response.size_bytes)}</span>
        <div className="resp-tabs">
          <span
            className={`tab ${tab === "body" ? "active" : ""}`}
            onClick={() => setTab("body")}
          >
            {t("Body")}
          </span>
          <span
            className={`tab ${tab === "headers" ? "active" : ""}`}
            onClick={() => setTab("headers")}
          >
            {t("Headers")} <span className="badge">{response.headers.length}</span>
          </span>
        </div>
      </div>
      {assertResults.length > 0 && (
        <div className="assert-results">
          <div className={`assert-summary ${assertResults.every((r) => r.passed) ? "ok" : "bad"}`}>
            {tr2("Assertions: {passed}/{total} passed", {
              passed: assertResults.filter((r) => r.passed).length,
              total: assertResults.length,
            })}
          </div>
          {assertResults.map((r, i) => (
            <div className={`assert-result ${r.passed ? "ok" : "bad"}`} key={i}>
              <span className="assert-mark">{r.passed ? "✅" : "❌"}</span>
              <span className="assert-detail">{r.detail}</span>
            </div>
          ))}
        </div>
      )}
      {tab === "body" ? (
        <div
          className="resp-body"
          dangerouslySetInnerHTML={{ __html: bodyHtml.html }}
        />
      ) : (
        <div className="resp-body">
          <table className="resp-headers-table">
            <tbody>
              {response.headers.map(([k, v], i) => (
                <tr key={i}>
                  <td>{k}</td>
                  <td>{v}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
