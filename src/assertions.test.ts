import { describe, it, expect } from "vitest";
import { evaluateAssertions, getByPath, newAssertion, Assertion, ResponseFacts } from "./assertions";

const facts: ResponseFacts = {
  status: 200,
  headers: [
    ["Content-Type", "application/json"],
    ["X-Trace", "abc-123"],
  ],
  body: '{"id":7,"user":{"name":"Мир"},"items":[{"sku":"A"},{"sku":"B"}]}',
  durationMs: 42,
};

function a(patch: Partial<Assertion>): Assertion {
  return { ...newAssertion(patch.type), ...patch, id: "x", enabled: true } as Assertion;
}

describe("getByPath", () => {
  it("reads dot and bracket paths", () => {
    const root = JSON.parse(facts.body);
    expect(getByPath(root, "id")).toBe(7);
    expect(getByPath(root, "user.name")).toBe("Мир");
    expect(getByPath(root, "items.1.sku")).toBe("B");
    expect(getByPath(root, "items[0].sku")).toBe("A");
    expect(getByPath(root, "missing.x")).toBeUndefined();
  });

  it("does not walk the prototype chain", () => {
    const root = JSON.parse(facts.body);
    // These exist on Object/Function prototypes, never in the data — must be undefined.
    expect(getByPath(root, "__proto__")).toBeUndefined();
    expect(getByPath(root, "constructor")).toBeUndefined();
    expect(getByPath(root, "user.__proto__.polluted")).toBeUndefined();
    // A genuine own array index still resolves.
    expect(getByPath(root, "items.0.sku")).toBe("A");
  });
});

describe("evaluateAssertions", () => {
  it("status eq and class match", () => {
    expect(evaluateAssertions([a({ type: "status", op: "eq", value: "200" })], facts)[0].passed).toBe(true);
    expect(evaluateAssertions([a({ type: "status", op: "eq", value: "404" })], facts)[0].passed).toBe(false);
    expect(evaluateAssertions([a({ type: "status", value: "2xx" })], facts)[0].passed).toBe(true);
    expect(evaluateAssertions([a({ type: "status", value: "5xx" })], facts)[0].passed).toBe(false);
  });

  it("response time bound", () => {
    expect(evaluateAssertions([a({ type: "time", op: "lt", value: "500" })], facts)[0].passed).toBe(true);
    expect(evaluateAssertions([a({ type: "time", op: "lt", value: "10" })], facts)[0].passed).toBe(false);
  });

  it("header exists / contains", () => {
    expect(evaluateAssertions([a({ type: "header", target: "content-type", op: "exists" })], facts)[0].passed).toBe(true);
    expect(evaluateAssertions([a({ type: "header", target: "X-Trace", op: "contains", value: "abc" })], facts)[0].passed).toBe(true);
    expect(evaluateAssertions([a({ type: "header", target: "X-None", op: "exists" })], facts)[0].passed).toBe(false);
  });

  it("body contains", () => {
    expect(evaluateAssertions([a({ type: "body_contains", value: "Мир" })], facts)[0].passed).toBe(true);
    expect(evaluateAssertions([a({ type: "body_contains", value: "nope" })], facts)[0].passed).toBe(false);
  });

  it("json_path exists / eq / is_number / matches", () => {
    expect(evaluateAssertions([a({ type: "json_path", target: "user.name", op: "exists" })], facts)[0].passed).toBe(true);
    expect(evaluateAssertions([a({ type: "json_path", target: "user.name", op: "eq", value: "Мир" })], facts)[0].passed).toBe(true);
    expect(evaluateAssertions([a({ type: "json_path", target: "id", op: "is_number" })], facts)[0].passed).toBe(true);
    expect(evaluateAssertions([a({ type: "json_path", target: "items.0.sku", op: "matches", value: "^[AB]$" })], facts)[0].passed).toBe(true);
    expect(evaluateAssertions([a({ type: "json_path", target: "missing", op: "exists" })], facts)[0].passed).toBe(false);
  });

  it("json_path on non-JSON body fails gracefully", () => {
    const r = evaluateAssertions([a({ type: "json_path", target: "x", op: "exists" })], { ...facts, body: "not json" });
    expect(r[0].passed).toBe(false);
    expect(r[0].detail).toMatch(/not JSON/);
  });

  it("skips disabled assertions", () => {
    const dis = { ...a({ type: "status", value: "404" }), enabled: false };
    expect(evaluateAssertions([dis], facts)).toHaveLength(0);
  });
});
