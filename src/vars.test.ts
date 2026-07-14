import { describe, it, expect } from "vitest";
import { resolveVars, envVars, secretKeys } from "./vars";
import { Environment, KV } from "./types";

function kv(key: string, value: string, extra: Partial<KV> = {}): KV {
  return { id: key, key, value, enabled: true, ...extra };
}

function env(vars: KV[]): Environment {
  return { id: "e", name: "test", variables: vars };
}

describe("resolveVars", () => {
  it("substitutes known {{vars}} and leaves unknown ones", () => {
    const m = new Map([["host", "api.example.com"]]);
    expect(resolveVars("https://{{host}}/x/{{missing}}", m)).toBe(
      "https://api.example.com/x/{{missing}}"
    );
  });

  it("never touches dynamic {{$...}} providers", () => {
    const m = new Map([["id", "7"]]);
    expect(resolveVars("{{id}}-{{$uuid}}-{{$data.u.x}}", m)).toBe("7-{{$uuid}}-{{$data.u.x}}");
  });

  it("resolves a variable whose value references another variable", () => {
    // BaseURL is composed from host — the nested {{host}} must fully expand.
    const m = new Map([
      ["BaseURL", "https://{{host}}/v1"],
      ["host", "api.example.com"],
    ]);
    expect(resolveVars("{{BaseURL}}/orders", m)).toBe("https://api.example.com/v1/orders");
  });

  it("terminates on a cyclic reference instead of looping forever", () => {
    const m = new Map([
      ["a", "{{b}}"],
      ["b", "{{a}}"],
    ]);
    // Bounded passes: it stops without throwing; residual placeholder is fine.
    const out = resolveVars("{{a}}", m);
    expect(out === "{{a}}" || out === "{{b}}").toBe(true);
  });
});

describe("envVars", () => {
  it("bakes plain and secret values at runtime (no export)", () => {
    const e = env([kv("host", "h"), kv("token", "s3cret", { secret: true })]);
    const m = envVars(e, false);
    expect(m.get("host")).toBe("h");
    expect(m.get("token")).toBe("s3cret");
  });

  it("emits ${KEY} for secret vars when exporting for CI", () => {
    const e = env([kv("host", "h"), kv("token", "s3cret", { secret: true })]);
    const m = envVars(e, true);
    expect(m.get("host")).toBe("h"); // non-secret baked
    expect(m.get("token")).toBe("${token}"); // secret becomes a placeholder
  });

  it("skips disabled and unnamed variables", () => {
    const e = env([kv("a", "1", { enabled: false }), kv("", "2")]);
    expect(envVars(e, false).size).toBe(0);
  });

  it("returns empty map for no environment", () => {
    expect(envVars(null).size).toBe(0);
  });
});

describe("secretKeys", () => {
  it("lists only secret variable names", () => {
    const e = env([kv("host", "h"), kv("token", "s", { secret: true })]);
    expect(secretKeys(e)).toEqual(["token"]);
  });
});

describe("end-to-end: export produces a CLI-expandable placeholder", () => {
  it("{{token}} -> ${token} so the pipeline injects the secret", () => {
    const e = env([kv("token", "local-dev-secret", { secret: true })]);
    const exported = resolveVars("Bearer {{token}}", envVars(e, true));
    expect(exported).toBe("Bearer ${token}");
    // ...and the local run still uses the real value:
    const local = resolveVars("Bearer {{token}}", envVars(e, false));
    expect(local).toBe("Bearer local-dev-secret");
  });
});
