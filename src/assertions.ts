// Response assertions — invariant checks that don't need to know the exact
// response value (status, timing, header presence, body text, JSON field
// shape/value). Evaluated on a single send; the same rules feed the load runner.
import { uid } from "./types";
import { tr, tr2 } from "./i18n";

export type AssertionType =
  | "status"
  | "time"
  | "header"
  | "body_contains"
  | "json_path";

export type AssertionOp =
  | "eq"
  | "neq"
  | "lt"
  | "lte"
  | "gt"
  | "gte"
  | "contains"
  | "exists"
  | "is_number"
  | "matches";

export interface Assertion {
  id: string;
  enabled: boolean;
  type: AssertionType;
  target: string; // header name or JSON path (unused for status/time/body)
  op: AssertionOp;
  value: string;
}

export interface AssertionResult {
  assertion: Assertion;
  passed: boolean;
  detail: string;
}

export interface ResponseFacts {
  status: number;
  headers: [string, string][];
  body: string;
  durationMs: number;
}

export function newAssertion(type: AssertionType = "status"): Assertion {
  const defaults: Record<AssertionType, Partial<Assertion>> = {
    status: { op: "eq", value: "200", target: "" },
    time: { op: "lt", value: "500", target: "" },
    header: { op: "exists", value: "", target: "Content-Type" },
    body_contains: { op: "contains", value: "", target: "" },
    json_path: { op: "exists", value: "", target: "" },
  };
  return { id: uid(), enabled: true, type, target: "", op: "eq", value: "", ...defaults[type] };
}

/// Get a value from parsed JSON by a simple dot/bracket path: `user.name`,
/// `items.0.id`, `items[1].sku`. Returns undefined if the path is missing.
export function getByPath(root: unknown, path: string): unknown {
  const parts = path
    .replace(/\[(\d+)\]/g, ".$1")
    .split(".")
    .map((p) => p.trim())
    .filter(Boolean);
  let cur: any = root;
  for (const p of parts) {
    if (cur == null) return undefined;
    // Only descend into own properties: never walk the prototype chain
    // (`__proto__`, `constructor`, …), which would otherwise make `exists` /
    // `is_number` report bogus truthy results for paths that aren't in the data.
    if (typeof cur !== "object" || !Object.prototype.hasOwnProperty.call(cur, p)) {
      return undefined;
    }
    cur = cur[p];
  }
  return cur;
}

function isFiniteNumericString(s: string): boolean {
  if (s.trim() === "") return false;
  return Number.isFinite(Number(s));
}

/// Returns a finite numeric reading of `actual` when it's a number or a
/// numeric-looking string, otherwise null. Deliberately excludes
/// null/undefined/boolean/object/array — `Number(null) === 0` and
/// `Number(true) === 1` would otherwise produce bogus numeric matches.
function actualAsNumber(actual: unknown): number | null {
  if (typeof actual === "number") return Number.isFinite(actual) ? actual : null;
  if (typeof actual === "string" && actual.trim() !== "") {
    const n = Number(actual);
    return Number.isFinite(n) ? n : null;
  }
  return null;
}

/// eq/neq comparison for json_path: numbers (incl. numeric strings on either
/// side) compare numerically so `19.9` matches `"19.90"`; objects/arrays are
/// an explicit type mismatch against a scalar assertion value rather than
/// being coerced through `String()` (which would collapse to
/// "[object Object]"); everything else falls back to plain string equality.
function jsonPathEquals(actual: unknown, expected: string): boolean {
  if (actual !== null && typeof actual === "object") return false;
  const actualNum = actualAsNumber(actual);
  if (actualNum !== null && isFiniteNumericString(expected)) {
    return actualNum === Number(expected);
  }
  return String(actual) === expected;
}

function numOp(op: AssertionOp, actual: number, expected: number): boolean {
  switch (op) {
    case "eq":
      return actual === expected;
    case "neq":
      return actual !== expected;
    case "lt":
      return actual < expected;
    case "lte":
      return actual <= expected;
    case "gt":
      return actual > expected;
    case "gte":
      return actual >= expected;
    default:
      return false;
  }
}

function evalOne(a: Assertion, facts: ResponseFacts): AssertionResult {
  const ok = (passed: boolean, detail: string): AssertionResult => ({ assertion: a, passed, detail });
  try {
    switch (a.type) {
      case "status": {
        const want = a.value.trim();
        // Support "2xx"/"4xx" class matches.
        if (/^[1-5]xx$/i.test(want)) {
          const cls = Number(want[0]);
          const passed = Math.floor(facts.status / 100) === cls;
          return ok(passed, tr2("status {actual}, expected {expected}", { actual: facts.status, expected: want }));
        }
        const expected = Number(want);
        const passed = numOp(a.op, facts.status, expected);
        return ok(passed, tr2("status {actual} {op} {expected}", { actual: facts.status, op: a.op, expected }));
      }
      case "time": {
        const expected = Number(a.value);
        const passed = numOp(a.op, facts.durationMs, expected);
        return ok(passed, tr2("time {actual}ms {op} {expected}ms", { actual: facts.durationMs.toFixed(0), op: a.op, expected }));
      }
      case "header": {
        const name = a.target.trim().toLowerCase();
        const found = facts.headers.find(([k]) => k.toLowerCase() === name);
        if (a.op === "exists") return ok(!!found, found ? tr2("«{header}» is present", { header: a.target }) : tr2("header «{header}» is missing", { header: a.target }));
        if (!found) return ok(false, tr2("header «{header}» is missing", { header: a.target }));
        if (a.op === "contains") return ok(found[1].includes(a.value), tr2("«{actual}» contains «{value}»", { actual: found[1], value: a.value }));
        if (a.op === "eq") return ok(found[1] === a.value, tr2("«{actual}» = «{value}»", { actual: found[1], value: a.value }));
        if (a.op === "matches") return ok(new RegExp(a.value).test(found[1]), tr2("«{actual}» ~ /{pattern}/", { actual: found[1], pattern: a.value }));
        return ok(false, tr("unknown operation"));
      }
      case "body_contains": {
        const passed = facts.body.includes(a.value);
        return ok(passed, passed ? tr2("body contains «{value}»", { value: a.value }) : tr2("body does not contain «{value}»", { value: a.value }));
      }
      case "json_path": {
        let root: unknown;
        try {
          root = JSON.parse(facts.body);
        } catch {
          return ok(false, tr("body is not JSON"));
        }
        const actual = getByPath(root, a.target);
        switch (a.op) {
          case "exists":
            return ok(actual !== undefined, actual !== undefined ? tr2("{path} is present", { path: a.target }) : tr2("{path} is missing", { path: a.target }));
          case "is_number":
            return ok(typeof actual === "number", tr2("{path} = {value}", { path: a.target, value: JSON.stringify(actual) }));
          case "matches":
            return ok(new RegExp(a.value).test(String(actual)), tr2("{path}=«{actual}» ~ /{pattern}/", { path: a.target, actual: String(actual), pattern: a.value }));
          case "contains":
            return ok(String(actual).includes(a.value), tr2("{path}=«{actual}» contains «{value}»", { path: a.target, actual: String(actual), value: a.value }));
          case "eq":
            return ok(jsonPathEquals(actual, a.value), tr2("{path}=«{actual}» = «{value}»", { path: a.target, actual: String(actual), value: a.value }));
          case "neq":
            return ok(!jsonPathEquals(actual, a.value), tr2("{path}=«{actual}» ≠ «{value}»", { path: a.target, actual: String(actual), value: a.value }));
          default: {
            const n = Number(actual);
            return ok(numOp(a.op, n, Number(a.value)), tr2("{path}={actual} {op} {value}", { path: a.target, actual: n, op: a.op, value: a.value }));
          }
        }
      }
      default:
        return ok(false, tr("unknown assertion type"));
    }
  } catch (e) {
    return ok(false, tr2("assertion error: {error}", { error: String(e) }));
  }
}

export function evaluateAssertions(
  assertions: Assertion[],
  facts: ResponseFacts
): AssertionResult[] {
  return assertions.filter((a) => a.enabled).map((a) => evalOne(a, facts));
}
