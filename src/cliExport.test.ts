import { describe, it, expect } from "vitest";
import {
  buildHttpScenario,
  buildGrpcScenario,
  buildWsScenario,
  HTTP_TARGET_DEFAULT_RPS,
} from "./cliExport";

const lt = (rpsLimit: number | "") => ({
  vus: 20,
  durationSecs: 30,
  rpsLimit,
  timeoutMs: 10000,
});

const emptyHttp = {
  method: "GET",
  url: "u",
  headers: [] as [string, string][],
  body: null,
  tls: null,
  multipart: null,
  authRefresh: null,
  datasets: [],
  filePools: [],
};

describe("CLI scenario builders", () => {
  it("HTTP: one target with the given RPS, thresholds, and secret placeholders preserved", () => {
    const cfg: any = buildHttpScenario("svc", lt(250), {
      ...emptyHttp,
      url: "https://api/x?token=${TOKEN}",
      headers: [["Content-Type", "application/json"]],
    });
    expect(cfg.duration_secs).toBe(30);
    expect(cfg.timeout_ms).toBe(10000);
    expect(cfg.targets).toHaveLength(1);
    expect(cfg.targets[0].method).toBe("GET");
    expect(cfg.targets[0].rps).toBe(250);
    expect(cfg.targets[0].url).toContain("${TOKEN}"); // left for the CLI to expand
    expect(cfg.thresholds).toEqual({ max_error_rate: 1.0, max_p95_ms: 500 });
    expect(cfg.grpc).toBeUndefined();
    expect(cfg.websocket).toBeUndefined();
  });

  it("HTTP: an unset RPS limit falls back to a positive default (CLI needs rps > 0)", () => {
    const cfg: any = buildHttpScenario("svc", lt(""), emptyHttp);
    expect(cfg.targets[0].rps).toBe(HTTP_TARGET_DEFAULT_RPS);
  });

  it("gRPC: a grpc block, VU-driven, rps_limit null when unset and carried when set", () => {
    const unset: any = buildGrpcScenario("svc", lt(""), {
      endpoint: "http://h:50051",
      proto_path: "s.proto",
      includes: [],
      service: "demo.Greeter",
      method: "SayHello",
      body: '{"name":"x"}',
    });
    expect(unset.targets).toBeUndefined();
    expect(unset.grpc.service).toBe("demo.Greeter");
    expect(unset.grpc.vus).toBe(20);
    expect(unset.grpc.rps_limit).toBeNull();

    const set: any = buildGrpcScenario("svc", lt(120), {
      endpoint: "e",
      proto_path: "p",
      includes: [],
      service: "s",
      method: "m",
      body: "",
    });
    expect(set.grpc.rps_limit).toBe(120);
  });

  it("WebSocket: a websocket block, rps_limit null when unset", () => {
    const cfg: any = buildWsScenario("svc", lt(""), { url: "ws://h", message: "{}" });
    expect(cfg.websocket.url).toBe("ws://h");
    expect(cfg.websocket.vus).toBe(20);
    expect(cfg.websocket.rps_limit).toBeNull();
    expect(cfg.targets).toBeUndefined();
  });
});
