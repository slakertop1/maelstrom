import { barChart, donutChart, fmtMs, fmtNum, lineChart } from "./charts";
import type { LoadTestResult } from "./types";
import { tr, tr2 } from "./i18n";

function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

// ---- secret query-param masking (exported HTML reports are shareable files —
// query strings like ?token=... / ?api_key=... must never leak into them) ----

// e2: matched as a SUBSTRING of the normalized (lowercase, -/_ stripped) key,
// not an exact match — otherwise composite names like access_token,
// refresh_token, client_secret or id_token slip through unmasked. Mirrors the
// Rust side (core/src/redact.rs, is_secret_query_key), which also uses
// contains().
const SECRET_QUERY_KEY_SUBSTRINGS = [
  "token",
  "key",
  "secret",
  "password",
  "apikey",
  "sig",
  "signature",
  "accesskey",
];

function isSecretQueryKey(name: string): boolean {
  const normalized = name.toLowerCase().replace(/[-_]/g, "");
  if (normalized.startsWith("xamz")) return true; // x-amz-*, x-amz-signature, etc.
  return SECRET_QUERY_KEY_SUBSTRINGS.some((s) => normalized.includes(s));
}

/// Mutates `params` in place, replacing any secret-looking values with `***`.
/// Returns true if anything was masked.
function maskSecretQueryParams(params: URLSearchParams): boolean {
  let masked = false;
  for (const name of [...params.keys()]) {
    if (isSecretQueryKey(name)) {
      params.set(name, "***");
      masked = true;
    }
  }
  return masked;
}

/// Mask secret-looking query params (token/key/secret/password/apikey/sig/
/// signature/x-amz-*, case-insensitive, -/_ normalized) before a URL is
/// embedded in an exported (shareable) HTML report.
function maskUrl(rawUrl: string): string {
  let u: URL;
  let hasOrigin = true;
  try {
    u = new URL(rawUrl);
  } catch {
    try {
      u = new URL(rawUrl, "http://placeholder.invalid");
      hasOrigin = false;
    } catch {
      return rawUrl;
    }
  }
  if (!maskSecretQueryParams(u.searchParams)) return rawUrl;
  return hasOrigin ? u.toString() : u.pathname + u.search + u.hash;
}

const REPORT_CSS = `
  :root { color-scheme: dark; }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body { background: #141519; color: #e6e8ec; font-family: "Segoe UI", -apple-system, Arial, sans-serif; line-height: 1.5; }
  .wrap { max-width: 1080px; margin: 0 auto; padding: 32px 24px 64px; }
  header { padding: 28px 28px 24px; border-radius: 16px; background: linear-gradient(135deg, #1d1e25 0%, #23242c 60%, #2a2320 100%); border: 1px solid rgba(255,255,255,0.07); margin-bottom: 24px; }
  header h1 { font-size: 22px; font-weight: 650; margin-bottom: 6px; }
  header h1 .accent { color: #ff6c37; }
  .target { display: flex; align-items: center; gap: 10px; margin: 14px 0 4px; flex-wrap: wrap; }
  .method { background: #ff6c37; color: #16110d; font-weight: 700; font-size: 12px; padding: 3px 10px; border-radius: 6px; letter-spacing: 0.5px; }
  .url { font-family: Consolas, monospace; font-size: 14px; color: #cdd2da; word-break: break-all; }
  .chips { display: flex; gap: 8px; margin-top: 14px; flex-wrap: wrap; }
  .chip { font-size: 12px; color: #9aa0ab; background: rgba(255,255,255,0.05); border: 1px solid rgba(255,255,255,0.08); padding: 4px 12px; border-radius: 999px; }
  .chip b { color: #e6e8ec; font-weight: 600; }
  .verdict { display: flex; align-items: center; gap: 12px; padding: 14px 18px; border-radius: 12px; margin-bottom: 24px; font-weight: 600; }
  .verdict .note { font-weight: 400; color: rgba(230,232,236,0.75); font-size: 13px; }
  .verdict.ok { background: rgba(63,185,80,0.12); border: 1px solid rgba(63,185,80,0.4); color: #56d364; }
  .verdict.warn { background: rgba(210,153,34,0.12); border: 1px solid rgba(210,153,34,0.4); color: #e3b341; }
  .verdict.bad { background: rgba(248,81,73,0.12); border: 1px solid rgba(248,81,73,0.4); color: #ff7b72; }
  .cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(190px, 1fr)); gap: 12px; margin-bottom: 24px; }
  .card { background: #1d1e25; border: 1px solid rgba(255,255,255,0.07); border-radius: 12px; padding: 16px 18px; }
  .card .label { font-size: 12px; color: #9aa0ab; margin-bottom: 6px; }
  .card .value { font-size: 22px; font-weight: 650; font-variant-numeric: tabular-nums; }
  .card.ok .value { color: #56d364; }
  .card.bad .value { color: #ff7b72; }
  section { background: #1d1e25; border: 1px solid rgba(255,255,255,0.07); border-radius: 14px; padding: 20px 22px; margin-bottom: 20px; }
  section h2 { font-size: 15px; font-weight: 650; margin-bottom: 14px; color: #cdd2da; }
  .two-col { display: grid; grid-template-columns: 1fr 1fr; gap: 20px; }
  @media (max-width: 800px) { .two-col { grid-template-columns: 1fr; } }
  table { width: 100%; border-collapse: collapse; font-size: 13px; font-variant-numeric: tabular-nums; }
  th { text-align: left; color: #9aa0ab; font-weight: 600; padding: 8px 10px; border-bottom: 1px solid rgba(255,255,255,0.1); }
  td { padding: 7px 10px; border-bottom: 1px solid rgba(255,255,255,0.05); }
  tr:last-child td { border-bottom: none; }
  details summary { cursor: pointer; color: #9aa0ab; font-size: 13px; padding: 4px 0; }
  .table-scroll { overflow-x: auto; }
  .m-tag { font-weight: 700; font-size: 11px; }
  .m-GET { color: #3fb950; } .m-POST { color: #d29922; } .m-PUT { color: #4c9df3; }
  .m-PATCH { color: #a371f7; } .m-DELETE { color: #f85149; }
  td.err { color: #ff7b72; }
  footer { text-align: center; color: #6b7280; font-size: 12px; margin-top: 32px; }
`;

export function buildReportHtml(r: LoadTestResult): string {
  const maskedUrl = maskUrl(r.url);
  const ok = r.error_rate < 1;
  const warn = r.error_rate >= 1 && r.error_rate < 5;
  const verdict = ok
    ? { text: tr("Test passed"), cls: "ok", note: tr("Error rate below 1%") }
    : warn
      ? { text: tr("Test passed with warnings"), cls: "warn", note: tr2("Error rate {rate}%", { rate: r.error_rate.toFixed(2) }) }
      : { text: tr("Problems detected"), cls: "bad", note: tr2("Error rate {rate}%", { rate: r.error_rate.toFixed(2) }) };

  const rpsChart = lineChart({
    series: [
      {
        name: tr("Requests/s"),
        color: "#4c9df3",
        fill: true,
        points: r.timeline.map((p) => ({ x: p.sec, y: p.requests })),
      },
      {
        name: tr("Errors/s"),
        color: "#f85149",
        points: r.timeline.map((p) => ({ x: p.sec, y: p.errors })),
      },
    ],
    xFormat: (v) => tr2("{n}s", { n: Math.round(v) }),
  });

  const latChart = lineChart({
    series: [
      { name: "p50", color: "#3fb950", points: r.timeline.map((p) => ({ x: p.sec, y: p.p50_ms })) },
      { name: "p95", color: "#d29922", points: r.timeline.map((p) => ({ x: p.sec, y: p.p95_ms })) },
      { name: "p99", color: "#f85149", points: r.timeline.map((p) => ({ x: p.sec, y: p.p99_ms })) },
    ],
    yFormat: (v) => fmtMs(v),
    xFormat: (v) => tr2("{n}s", { n: Math.round(v) }),
  });

  const histChart = barChart(
    r.histogram.map((b) => ({
      label: tr2("{from}–{to} ms", { from: fmtNum(b.from_ms), to: fmtNum(b.to_ms) }),
      count: b.count,
    })),
    { color: "#a371f7" }
  );

  const donut = donutChart(
    r.status_counts.map(([label, value]) => ({ label, value }))
  );

  const cards: [string, string, string?][] = [
    [tr("Total requests"), fmtNum(r.total_requests)],
    [tr("RPS (average)"), fmtNum(r.rps_avg)],
    [tr("Errors"), `${fmtNum(r.errors)} · ${r.error_rate.toFixed(2)}%`, r.errors > 0 ? "bad" : "ok"],
    [tr("Average latency"), fmtMs(r.latency_avg_ms)],
    ["p50", fmtMs(r.p50_ms)],
    ["p95", fmtMs(r.p95_ms)],
    ["p99", fmtMs(r.p99_ms)],
    [tr("Maximum"), fmtMs(r.latency_max_ms)],
  ];

  const statusRows = r.status_counts
    .map(
      ([label, count]) =>
        `<tr><td>${esc(label)}</td><td>${fmtNum(count)}</td><td>${((count / Math.max(1, r.total_requests)) * 100).toFixed(2)}%</td></tr>`
    )
    .join("");

  const timelineRows = r.timeline
    .map(
      (p) =>
        `<tr><td>${p.sec}</td><td>${p.requests}</td><td>${p.errors}</td><td>${fmtMs(p.avg_ms)}</td><td>${fmtMs(p.p50_ms)}</td><td>${fmtMs(p.p95_ms)}</td><td>${fmtMs(p.p99_ms)}</td></tr>`
    )
    .join("");

  const percRows: [string, number][] = [
    ["min", r.latency_min_ms],
    ["p50", r.p50_ms],
    ["p75", r.p75_ms],
    ["p90", r.p90_ms],
    ["p95", r.p95_ms],
    ["p99", r.p99_ms],
    ["max", r.latency_max_ms],
  ];
  const percTable = percRows
    .map(([n, v]) => `<tr><td>${n}</td><td>${fmtMs(v)}</td></tr>`)
    .join("");

  return `<!doctype html>
<html lang="ru">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${tr("Load test report")} — ${esc(maskedUrl)}</title>
<style>${REPORT_CSS}</style>
</head>
<body>
<div class="wrap">
  <header>
    <h1><span class="accent">⚡ Maelstrom</span> — ${tr("load test report")}</h1>
    <div class="target"><span class="method">${esc(r.method)}</span><span class="url">${esc(maskedUrl)}</span></div>
    <div class="chips">
      <span class="chip">${tr("Started")}: <b>${esc(r.started_at)}</b></span>
      <span class="chip">${tr("Duration")}: <b>${(r.actual_duration_ms / 1000).toFixed(1)} ${tr("s")}</b>${r.stopped_early ? " " + tr("(stopped manually)") : ""}</span>
      <span class="chip">${tr("Virtual users")}: <b>${r.vus}</b></span>
      ${r.rps_limit ? `<span class="chip">${tr("RPS limit")}: <b>${r.rps_limit}</b></span>` : ""}
    </div>
  </header>

  <div class="verdict ${verdict.cls}">
    ${verdict.cls === "ok" ? "✔" : verdict.cls === "warn" ? "⚠" : "✖"} ${verdict.text}
    <span class="note">${verdict.note}</span>
  </div>

  <div class="cards">
    ${cards.map(([label, value, cls]) => `<div class="card ${cls ?? ""}"><div class="label">${label}</div><div class="value">${value}</div></div>`).join("")}
  </div>

  <section>
    <h2>${tr("Throughput (requests per second)")}</h2>
    ${rpsChart}
  </section>

  <section>
    <h2>${tr("Latency over time (percentiles)")}</h2>
    ${latChart}
  </section>

  <div class="two-col">
    <section>
      <h2>${tr("Response time distribution")}</h2>
      ${histChart}
    </section>
    <section>
      <h2>${tr("Response codes")}</h2>
      ${donut}
    </section>
  </div>

  <div class="two-col">
    <section>
      <h2>${tr("Latency percentiles")}</h2>
      <table><thead><tr><th>${tr("Percentile")}</th><th>${tr("Time")}</th></tr></thead><tbody>${percTable}</tbody></table>
    </section>
    <section>
      <h2>${tr("Response statuses")}</h2>
      <table><thead><tr><th>${tr("Status")}</th><th>${tr("Count")}</th><th>${tr("Share")}</th></tr></thead><tbody>${statusRows}</tbody></table>
    </section>
  </div>

  <section>
    <details>
      <summary>${tr2("Per-second breakdown ({n} s)", { n: r.timeline.length })}</summary>
      <div class="table-scroll">
      <table style="margin-top:12px">
        <thead><tr><th>${tr("Second")}</th><th>${tr("Requests")}</th><th>${tr("Errors")}</th><th>${tr("Average")}</th><th>p50</th><th>p95</th><th>p99</th></tr></thead>
        <tbody>${timelineRows}</tbody>
      </table>
      </div>
    </details>
  </section>

  <footer>${tr("Generated by Maelstrom")} · ${esc(new Date().toLocaleString("ru-RU"))}</footer>
</div>
</body>
</html>`;
}

// ---- multi-endpoint scenario report ----

import type { ScenarioResult } from "./types";

const ENDPOINT_COLORS = [
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

export function buildScenarioReportHtml(s: ScenarioResult): string {
  const o = s.overall;
  const ok = o.error_rate < 1;
  const warn = o.error_rate >= 1 && o.error_rate < 5;
  const verdict = ok
    ? { text: tr("Load test passed"), cls: "ok", note: tr("Error rate below 1%") }
    : warn
      ? { text: tr("Passed with warnings"), cls: "warn", note: tr2("Error rate {rate}%", { rate: o.error_rate.toFixed(2) }) }
      : { text: tr("Problems detected"), cls: "bad", note: tr2("Error rate {rate}%", { rate: o.error_rate.toFixed(2) }) };

  const rpsChart = lineChart({
    series: s.targets.map((t, i) => ({
      name: shortUrl(t.url),
      color: ENDPOINT_COLORS[i % ENDPOINT_COLORS.length],
      points: t.timeline.map((p) => ({ x: p.sec, y: p.requests })),
    })),
    xFormat: (v) => tr2("{n}s", { n: Math.round(v) }),
  });

  const overallRps = lineChart({
    series: [
      {
        name: tr("Requests/s (total)"),
        color: "#4c9df3",
        fill: true,
        points: o.timeline.map((p) => ({ x: p.sec, y: p.requests })),
      },
      {
        name: tr("Errors/s"),
        color: "#f85149",
        points: o.timeline.map((p) => ({ x: p.sec, y: p.errors })),
      },
    ],
    xFormat: (v) => tr2("{n}s", { n: Math.round(v) }),
  });

  const cards: [string, string, string?][] = [
    [tr("Endpoints"), String(s.targets.length)],
    [tr("Total requests"), fmtNum(o.total_requests)],
    [tr("RPS (average, total)"), fmtNum(o.rps_avg)],
    [tr("Errors"), `${fmtNum(o.errors)} · ${o.error_rate.toFixed(2)}%`, o.errors > 0 ? "bad" : "ok"],
    [tr("p95 (overall)"), fmtMs(o.p95_ms)],
    [tr("p99 (overall)"), fmtMs(o.p99_ms)],
  ];

  const rows = s.targets
    .map((t, i) => {
      const color = ENDPOINT_COLORS[i % ENDPOINT_COLORS.length];
      return `<tr>
        <td><span style="display:inline-block;width:9px;height:9px;border-radius:2px;background:${color};margin-right:8px"></span><span class="m-tag m-${esc(t.method)}">${esc(t.method)}</span> <span style="font-family:Consolas,monospace">${esc(shortUrl(t.url))}</span></td>
        <td>${fmtNum(t.total_requests)}</td>
        <td>${fmtNum(t.rps_avg)}</td>
        <td class="${t.errors > 0 ? "err" : ""}">${fmtNum(t.errors)} · ${t.error_rate.toFixed(2)}%</td>
        <td>${fmtMs(t.p50_ms)}</td>
        <td>${fmtMs(t.p95_ms)}</td>
        <td>${fmtMs(t.p99_ms)}</td>
      </tr>`;
    })
    .join("");

  return `<!doctype html>
<html lang="ru">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${tr("Service load test report")} — ${tr2("{n} endpoints", { n: s.targets.length })}</title>
<style>${REPORT_CSS}</style>
</head>
<body>
<div class="wrap">
  <header>
    <h1><span class="accent">⚡ Maelstrom</span> — ${tr("service load test report")}</h1>
    <div class="chips">
      <span class="chip">${tr("Started")}: <b>${esc(s.started_at)}</b></span>
      <span class="chip">${tr("Duration")}: <b>${(s.actual_duration_ms / 1000).toFixed(1)} ${tr("s")}</b>${s.stopped_early ? " " + tr("(stopped manually)") : ""}</span>
      <span class="chip">${tr("Endpoints under load")}: <b>${s.targets.length}</b></span>
    </div>
  </header>

  <div class="verdict ${verdict.cls}">
    ${verdict.cls === "ok" ? "✔" : verdict.cls === "warn" ? "⚠" : "✖"} ${verdict.text}
    <span class="note">${verdict.note}</span>
  </div>

  <div class="cards">
    ${cards.map(([label, value, cls]) => `<div class="card ${cls ?? ""}"><div class="label">${label}</div><div class="value">${value}</div></div>`).join("")}
  </div>

  <section>
    <h2>${tr("Results by endpoint")}</h2>
    <div class="table-scroll">
    <table>
      <thead><tr><th>${tr("Endpoint")}</th><th>${tr("Requests")}</th><th>RPS</th><th>${tr("Errors")}</th><th>p50</th><th>p95</th><th>p99</th></tr></thead>
      <tbody>${rows}</tbody>
    </table>
    </div>
  </section>

  <section>
    <h2>${tr("Throughput by endpoint (requests/s)")}</h2>
    ${rpsChart}
  </section>

  <section>
    <h2>${tr("Total throughput and errors")}</h2>
    ${overallRps}
  </section>

  <footer>${tr("Generated by Maelstrom")} · ${esc(new Date().toLocaleString("ru-RU"))}</footer>
</div>
</body>
</html>`;
}

function shortUrl(url: string): string {
  try {
    const u = new URL(url);
    maskSecretQueryParams(u.searchParams);
    return u.pathname + (u.search ? u.search : "");
  } catch {
    return maskUrl(url);
  }
}
