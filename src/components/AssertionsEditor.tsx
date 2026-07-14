import { useT } from "../i18n";
import {
  Assertion,
  AssertionType,
  AssertionOp,
  newAssertion,
} from "../assertions";

interface Props {
  items: Assertion[];
  onChange: (items: Assertion[]) => void;
}

const TYPES: { id: AssertionType; label: string }[] = [
  { id: "status", label: "Status code" },
  { id: "time", label: "Response time, ms" },
  { id: "header", label: "Header" },
  { id: "body_contains", label: "Body contains" },
  { id: "json_path", label: "JSON field" },
];

const OPS: Record<AssertionType, { id: AssertionOp; label: string }[]> = {
  status: [
    { id: "eq", label: "=" },
    { id: "neq", label: "≠" },
    { id: "lt", label: "<" },
    { id: "gte", label: "≥" },
  ],
  time: [
    { id: "lt", label: "<" },
    { id: "lte", label: "≤" },
    { id: "gt", label: ">" },
  ],
  header: [
    { id: "exists", label: "exists" },
    { id: "eq", label: "equals" },
    { id: "contains", label: "contains" },
    { id: "matches", label: "regex" },
  ],
  body_contains: [{ id: "contains", label: "contains" }],
  json_path: [
    { id: "exists", label: "exists" },
    { id: "eq", label: "=" },
    { id: "neq", label: "≠" },
    { id: "contains", label: "contains" },
    { id: "is_number", label: "number" },
    { id: "matches", label: "regex" },
    { id: "gt", label: ">" },
    { id: "lt", label: "<" },
  ],
};

export default function AssertionsEditor({ items, onChange }: Props) {
  const t = useT();
  const update = (id: string, patch: Partial<Assertion>) => {
    let next = items.map((it) => (it.id === id ? { ...it, ...patch } : it));
    const last = next[next.length - 1];
    if (!last || last.value !== "" || last.target !== "") next = [...next, newAssertion()];
    onChange(next);
  };
  const remove = (id: string) => onChange(items.filter((it) => it.id !== id));

  const list = items.length ? items : [newAssertion()];
  const needsTarget = (t: AssertionType) => t === "header" || t === "json_path";
  const needsValue = (a: Assertion) => a.op !== "exists" && a.op !== "is_number";

  return (
    <div className="assert-editor">
      <div className="lt-hint" style={{ marginBottom: 8 }}>
        {t("Checks run after the response and show ✅/❌. They suit dynamic data: verify the")} <b>{t("shape")}</b> {t("of the response (status, time, field type/presence, regex) rather than an exact value. A JSON field is given as a path:")} <code>user.name</code>,{" "}
        <code>items.0.sku</code>.
      </div>
      {list.map((a) => (
        <div className="assert-row" key={a.id}>
          <input
            type="checkbox"
            checked={a.enabled}
            onChange={(e) => update(a.id, { enabled: e.target.checked })}
            title={t("Enable/disable check")}
          />
          <select
            value={a.type}
            onChange={(e) => {
              const type = e.target.value as AssertionType;
              const def = newAssertion(type);
              update(a.id, { type, op: def.op, target: def.target, value: def.value });
            }}
          >
            {TYPES.map((ty) => (
              <option key={ty.id} value={ty.id}>
                {t(ty.label)}
              </option>
            ))}
          </select>
          {needsTarget(a.type) ? (
            <input
              className="assert-target"
              placeholder={a.type === "header" ? t("Header-Name") : "user.name"}
              value={a.target}
              onChange={(e) => update(a.id, { target: e.target.value })}
            />
          ) : (
            <span className="assert-target-empty" />
          )}
          <select value={a.op} onChange={(e) => update(a.id, { op: e.target.value as AssertionOp })}>
            {OPS[a.type].map((o) => (
              <option key={o.id} value={o.id}>
                {t(o.label)}
              </option>
            ))}
          </select>
          {needsValue(a) ? (
            <input
              className="assert-value"
              placeholder={
                a.type === "status" ? t("200 or 2xx") : a.type === "time" ? "500" : t("value")
              }
              value={a.value}
              onChange={(e) => update(a.id, { value: e.target.value })}
            />
          ) : (
            <span className="assert-value-empty" />
          )}
          <button className="ghost del" onClick={() => remove(a.id)} title={t("Delete")}>
            ✕
          </button>
        </div>
      ))}
    </div>
  );
}
