import { KV, newKV } from "../types";
import { useT } from "../i18n";

interface Props {
  items: KV[];
  onChange: (items: KV[]) => void;
  keyPlaceholder?: string;
  valuePlaceholder?: string;
}

export default function KeyValueEditor({
  items,
  onChange,
  keyPlaceholder,
  valuePlaceholder,
}: Props) {
  const t = useT();
  const keyPh = keyPlaceholder ?? t("Key");
  const valuePh = valuePlaceholder ?? t("Value");
  const update = (id: string, patch: Partial<KV>) => {
    let next = items.map((it) => (it.id === id ? { ...it, ...patch } : it));
    // Keep one trailing empty row for quick entry.
    const last = next[next.length - 1];
    if (!last || last.key !== "" || last.value !== "") {
      next = [...next, newKV()];
    }
    onChange(next);
  };

  const remove = (id: string) => {
    const next = items.filter((it) => it.id !== id);
    onChange(next.length ? next : [newKV()]);
  };

  return (
    <div className="kv-table">
      {items.map((it) => (
        <div className="kv-row" key={it.id}>
          <input
            type="checkbox"
            checked={it.enabled}
            onChange={(e) => update(it.id, { enabled: e.target.checked })}
            title={t("Enable/disable")}
          />
          <input
            className="k"
            placeholder={keyPh}
            value={it.key}
            onChange={(e) => update(it.id, { key: e.target.value })}
          />
          <input
            className="v"
            placeholder={valuePh}
            value={it.value}
            onChange={(e) => update(it.id, { value: e.target.value })}
          />
          <button className="ghost del" onClick={() => remove(it.id)} title={t("Delete")}>
            ✕
          </button>
        </div>
      ))}
    </div>
  );
}
