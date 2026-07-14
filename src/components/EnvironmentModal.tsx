import { useState } from "react";
import { useT, tr2 } from "../i18n";
import { Environment, KV, newKV, uid } from "../types";

interface Props {
  environments: Environment[];
  onChange: (envs: Environment[]) => void;
  onClose: () => void;
}

/// Variables editor with a per-row "secret" flag: secret values are masked and,
/// when exporting a config for CI, become ${KEY} placeholders (injected per
/// system via OS env). Auto-appends a blank trailing row.
function VariablesEditor({
  items,
  onChange,
}: {
  items: KV[];
  onChange: (items: KV[]) => void;
}) {
  const t = useT();
  const update = (id: string, patch: Partial<KV>) => {
    let next = items.map((it) => (it.id === id ? { ...it, ...patch } : it));
    const last = next[next.length - 1];
    if (!last || last.key !== "" || last.value !== "") next = [...next, newKV()];
    onChange(next);
  };
  const remove = (id: string) => {
    const next = items.filter((it) => it.id !== id);
    onChange(next.length ? next : [newKV()]);
  };
  return (
    <div className="vars-table">
      <div className="vars-head">
        <span></span>
        <span>{t("Name")}</span>
        <span>{t("Value")}</span>
        <span>{t("Secret")}</span>
        <span></span>
      </div>
      {items.map((it) => (
        <div className="vars-row" key={it.id}>
          <input
            type="checkbox"
            checked={it.enabled}
            onChange={(e) => update(it.id, { enabled: e.target.checked })}
            title={t("Enable/disable")}
          />
          <input
            className="vars-key"
            placeholder={t("variable_name")}
            value={it.key}
            onChange={(e) => update(it.id, { key: e.target.value })}
          />
          <input
            className="vars-val"
            type={it.secret ? "password" : "text"}
            placeholder={it.secret ? t("value (won’t go to the CI config)") : t("value")}
            value={it.value}
            onChange={(e) => update(it.id, { value: e.target.value })}
          />
          <label className="vars-secret" title={t("Secret: exported to CI as ${NAME}")}>
            <input
              type="checkbox"
              checked={!!it.secret}
              onChange={(e) => update(it.id, { secret: e.target.checked })}
            />
            🔒
          </label>
          <button className="ghost del" onClick={() => remove(it.id)} title={t("Delete")}>
            ✕
          </button>
        </div>
      ))}
    </div>
  );
}

export default function EnvironmentModal({ environments, onChange, onClose }: Props) {
  const t = useT();
  const [selectedId, setSelectedId] = useState<string | null>(
    environments[0]?.id ?? null
  );
  const selected = environments.find((e) => e.id === selectedId) ?? null;

  const addEnv = () => {
    const env: Environment = {
      id: uid(),
      name: tr2("Environment {n}", { n: environments.length + 1 }),
      variables: [newKV()],
    };
    onChange([...environments, env]);
    setSelectedId(env.id);
  };

  const updateSelected = (patch: Partial<Environment>) => {
    if (!selected) return;
    onChange(
      environments.map((e) => (e.id === selected.id ? { ...e, ...patch } : e))
    );
  };

  const deleteSelected = () => {
    if (!selected) return;
    const next = environments.filter((e) => e.id !== selected.id);
    onChange(next);
    setSelectedId(next[0]?.id ?? null);
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <span>{t("Environments and variables")}</span>
          <button className="ghost" onClick={onClose}>
            ✕
          </button>
        </div>
        <div className="modal-body">
          <div className="env-list">
            {environments.map((e) => (
              <span
                key={e.id}
                className={`env-pill ${e.id === selectedId ? "active" : ""}`}
                onClick={() => setSelectedId(e.id)}
              >
                {e.name}
              </span>
            ))}
            <span className="env-pill" onClick={addEnv}>
              ＋ {t("New")}
            </span>
          </div>
          {selected ? (
            <>
              <div style={{ display: "flex", gap: 8, marginBottom: 12 }}>
                <input
                  value={selected.name}
                  onChange={(e) => updateSelected({ name: e.target.value })}
                  style={{ flex: 1 }}
                />
                <button className="ghost" onClick={deleteSelected} title={t("Delete environment")}>
                  🗑
                </button>
              </div>
              <VariablesEditor
                items={selected.variables}
                onChange={(variables) => updateSelected({ variables })}
              />
              <div className="lt-hint" style={{ marginTop: 12 }}>
                {t("Use variables as")} {"{{variable_name}}"} {t("in the URL, headers, params and body. Mark")} 🔒 <b>{t("Secret")}</b> {t("for tokens/passwords — on")} {t("“↓ Export for CI”")} {t("they are exported as")} <code>{"${NAME}"}</code>{t(", and each pipeline (dev/stage/prod) substitutes its own value from an environment variable.")}
              </div>
              <div className="lt-hint" style={{ marginTop: 8 }}>
                <b>{t("Example: database password.")}</b>{" "}
                {t("Add a variable named")} <code>DB_PASSWORD</code>{t(", put your local password in the value and mark it")} 🔒.{" "}
                {t("Reference it in the request:")}{" "}
                <code>{"postgres://app:{{DB_PASSWORD}}@db-host:5432/shop"}</code>.{" "}
                {t("The exported CI config will say")} <code>{"${DB_PASSWORD}"}</code>{" "}
                {t("— create an environment variable with the SAME name where the CLI runs: in Kubernetes — a Secret exposed via")}{" "}
                <code>secretKeyRef</code>{t(", in GitLab/GitHub CI — a masked variable")} <code>DB_PASSWORD</code>.
              </div>
            </>
          ) : (
            <div className="lt-hint">{t("Create an environment to define variables.")}</div>
          )}
        </div>
      </div>
    </div>
  );
}
