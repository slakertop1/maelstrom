import { describe, it, expect } from "vitest";
import { buildStreamsScenario, streamsMissingVars } from "./streamsBuilder";
import { newRequest, RequestConfig, UiStream, DatasetSpec } from "./types";

function req(name: string, patch: Partial<RequestConfig> = {}): RequestConfig {
  return { ...newRequest(name), ...patch };
}
function byIdOf(rs: RequestConfig[]) {
  return new Map(rs.map((r) => [r.id, r]));
}
function ui(over: Partial<UiStream> & { steps: UiStream["steps"] }): UiStream {
  return { id: "s1", name: "chain", rps: 40, ...over };
}

describe("buildStreamsScenario", () => {
  it("maps a chain: extract on step 1, {{token}} carried into step 2", () => {
    const login = req("login", { method: "POST", url: "https://api/login" });
    const order = req("order", {
      method: "GET",
      url: "https://api/order",
      headers: [{ id: "h", key: "Authorization", value: "Bearer {{token}}", enabled: true }],
    });
    const streams: UiStream[] = [
      ui({
        rps: 100,
        steps: [
          {
            id: "st1",
            requestId: login.id,
            extract: [{ id: "e", name: "token", from: "json", expr: "data.token" }],
          },
          { id: "st2", requestId: order.id, extract: [] },
        ],
      }),
    ];
    const spec = buildStreamsScenario(streams, byIdOf([login, order]), null, 30, 10000, []);
    expect(spec.duration_secs).toBe(30);
    expect(spec.streams).toHaveLength(1);
    const s = spec.streams[0];
    expect(s.rps).toBe(100);
    expect(s.steps.map((x) => x.method)).toEqual(["POST", "GET"]);
    expect(s.steps[0].extract).toEqual([{ name: "token", from: "json", expr: "data.token" }]);
    // {{token}} left in the header for the engine to substitute at runtime.
    expect(s.steps[1].headers.find(([k]) => k === "Authorization")?.[1]).toBe("Bearer {{token}}");
  });

  it("drops empty extract rules and steps whose request was deleted", () => {
    const a = req("a", { url: "https://api/a" });
    const streams: UiStream[] = [
      ui({
        steps: [
          {
            id: "st1",
            requestId: a.id,
            extract: [
              { id: "e1", name: "", from: "json", expr: "x" }, // no name → dropped
              { id: "e2", name: "y", from: "json", expr: "" }, // no expr → dropped
              { id: "e3", name: "keep", from: "header", expr: "X-Id" },
            ],
          },
          { id: "st2", requestId: "GONE", extract: [] }, // request not in collection → dropped
        ],
      }),
    ];
    const spec = buildStreamsScenario(streams, byIdOf([a]), null, 10, 5000, []);
    expect(spec.streams[0].steps).toHaveLength(1);
    expect(spec.streams[0].steps[0].extract).toEqual([{ name: "keep", from: "header", expr: "X-Id" }]);
  });

  it("drops streams that end up with no runnable steps", () => {
    const streams: UiStream[] = [
      ui({ id: "s1", steps: [{ id: "st", requestId: "GONE", extract: [] }] }),
    ];
    const spec = buildStreamsScenario(streams, new Map(), null, 10, 5000, []);
    expect(spec.streams).toHaveLength(0);
  });

  it("attaches datasets only when a step references {{$data.*}}", () => {
    const ds: DatasetSpec[] = [{ name: "clients", mode: "sequential", source: { kind: "url", url: "u" } }];
    const plain = req("plain", { url: "https://api/x" });
    const withData = req("wd", { url: "https://api/x?c={{$data.clients.id}}" });

    const specNo = buildStreamsScenario(
      [ui({ steps: [{ id: "s", requestId: plain.id, extract: [] }] })],
      byIdOf([plain]),
      null,
      10,
      5000,
      ds
    );
    expect(specNo.datasets).toHaveLength(0);

    const specYes = buildStreamsScenario(
      [ui({ steps: [{ id: "s", requestId: withData.id, extract: [] }] })],
      byIdOf([withData]),
      null,
      10,
      5000,
      ds
    );
    expect(specYes.datasets).toHaveLength(1);
  });
});

describe("streamsMissingVars", () => {
  it("flags an unresolved env var but NOT an extracted chain var", () => {
    const login = req("login", { url: "https://api/login" });
    const order = req("order", {
      url: "https://api/order/{{orderId}}", // env var, unset → flagged
      headers: [{ id: "h", key: "Authorization", value: "Bearer {{token}}", enabled: true }], // extracted → not flagged
    });
    const streams: UiStream[] = [
      ui({
        steps: [
          {
            id: "st1",
            requestId: login.id,
            extract: [{ id: "e", name: "token", from: "json", expr: "data.token" }],
          },
          { id: "st2", requestId: order.id, extract: [] },
        ],
      }),
    ];
    const missing = streamsMissingVars(streams, byIdOf([login, order]), null);
    expect(missing).toContain("orderId");
    expect(missing).not.toContain("token"); // extracted at runtime, not an env var
  });
});
