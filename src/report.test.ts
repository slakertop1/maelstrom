import { describe, it, expect } from "vitest";
import { buildReportHtml, buildScenarioReportHtml } from "./report";
import type { LoadTestResult, ScenarioResult } from "./types";

function baseResult(overrides: Partial<LoadTestResult> = {}): LoadTestResult {
  return {
    url: "https://api.example.com/data",
    method: "GET",
    vus: 10,
    duration_secs: 5,
    rps_limit: null,
    started_at: "2026-01-01T00:00:00Z",
    actual_duration_ms: 5000,
    total_requests: 100,
    errors: 0,
    error_rate: 0,
    rps_avg: 20,
    latency_min_ms: 5,
    latency_max_ms: 50,
    latency_avg_ms: 20,
    p50_ms: 18,
    p75_ms: 25,
    p90_ms: 30,
    p95_ms: 35,
    p99_ms: 45,
    status_counts: [["200", 100]],
    timeline: [{ sec: 0, requests: 20, errors: 0, avg_ms: 20, p50_ms: 18, p95_ms: 35, p99_ms: 45 }],
    histogram: [{ from_ms: 0, to_ms: 50, count: 100 }],
    stopped_early: false,
    ...overrides,
  };
}

function baseScenario(overrides: Partial<ScenarioResult> = {}): ScenarioResult {
  return {
    started_at: "2026-01-01T00:00:00Z",
    duration_secs: 5,
    actual_duration_ms: 5000,
    overall: baseResult(),
    targets: [baseResult()],
    stopped_early: false,
    ...overrides,
  };
}

describe("buildReportHtml URL secret masking (e2)", () => {
  it("masks a secret query param while leaving ordinary params intact", () => {
    const html = buildReportHtml(
      baseResult({ url: "https://api.example.com/data?token=SUPERSECRET&page=2" })
    );
    expect(html).not.toContain("SUPERSECRET");
    expect(html).toContain("token=***");
    expect(html).toContain("page=2");
  });

  it("masks known secret key variants case-insensitively and with -/_ normalized", () => {
    const html = buildReportHtml(
      baseResult({
        url: "https://api.example.com/data?Api-Key=abc123&X-Amz-Signature=def456&normal=ok",
      })
    );
    expect(html).not.toContain("abc123");
    expect(html).not.toContain("def456");
    expect(html).toContain("normal=ok");
  });

  it("leaves URLs without secret params unchanged", () => {
    const html = buildReportHtml(baseResult({ url: "https://api.example.com/data?page=2" }));
    expect(html).toContain("https://api.example.com/data?page=2");
  });

  it("masks composite key names via substring match (e2), not just exact ones", () => {
    const html = buildReportHtml(
      baseResult({
        url:
          "https://api.example.com/data?access_token=AAA&refresh_token=BBB" +
          "&client_secret=CCC&id_token=DDD&page=2&id=7",
      })
    );
    expect(html).not.toContain("AAA");
    expect(html).not.toContain("BBB");
    expect(html).not.toContain("CCC");
    expect(html).not.toContain("DDD");
    expect(html).toContain("page=2");
    expect(html).toContain("id=7");
  });
});

describe("buildScenarioReportHtml URL secret masking (e2)", () => {
  it("masks secret query params in per-endpoint URLs shown via shortUrl", () => {
    const html = buildScenarioReportHtml(
      baseScenario({ targets: [baseResult({ url: "https://api.example.com/data?token=LEAKME" })] })
    );
    expect(html).not.toContain("LEAKME");
  });
});

describe("attribute escaping for untrusted method strings (e1)", () => {
  it("escapes double quotes so the method cannot break out of class=\"...\"", () => {
    const html = buildScenarioReportHtml(
      baseScenario({ targets: [baseResult({ method: 'GET" onmouseover="alert(1)' })] })
    );
    expect(html).not.toMatch(/class="m-tag m-GET" onmouseover="alert\(1\)"/);
    expect(html).toContain("&quot;");
  });

  it("escapes single quotes as well", () => {
    const html = buildScenarioReportHtml(
      baseScenario({ targets: [baseResult({ method: "GET' onmouseover='alert(1)" })] })
    );
    expect(html).not.toContain("GET' onmouseover='alert(1)");
    expect(html).toContain("&#39;");
  });
});
