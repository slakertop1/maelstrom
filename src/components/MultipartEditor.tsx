import { open } from "@tauri-apps/plugin-dialog";
import { MultipartField, newMultipartField } from "../types";
import { useT } from "../i18n";

interface Props {
  items: MultipartField[];
  onChange: (items: MultipartField[]) => void;
}

export default function MultipartEditor({ items, onChange }: Props) {
  const t = useT();
  const update = (id: string, patch: Partial<MultipartField>) => {
    let next = items.map((it) => (it.id === id ? { ...it, ...patch } : it));
    const last = next[next.length - 1];
    if (!last || last.name !== "" || last.value !== "") {
      next = [...next, newMultipartField()];
    }
    onChange(next);
  };

  const remove = (id: string) => {
    const next = items.filter((it) => it.id !== id);
    onChange(next.length ? next : [newMultipartField()]);
  };

  const pickFile = async (id: string) => {
    const path = await open({ multiple: false });
    if (typeof path === "string") update(id, { value: path });
  };

  const pickFolder = async (id: string) => {
    const path = await open({ directory: true });
    if (typeof path === "string") update(id, { pool_path: path });
  };

  const addFiles = async (it: MultipartField) => {
    const picked = await open({ multiple: true });
    if (!picked) return;
    const list = Array.isArray(picked) ? picked : [picked];
    const existing = (it.pool_paths ?? "").trim();
    const merged = [existing, ...list].filter(Boolean).join("\n");
    update(it.id, { pool_paths: merged });
  };

  return (
    <div className="mp-table">
      <div className="mp-head">
        <span></span>
        <span>{t("Field name")}</span>
        <span>{t("Type")}</span>
        <span>{t("Value / file")}</span>
        <span></span>
      </div>
      {items.map((it) => {
        const isPool = it.kind === "file" && it.source === "pool";
        return (
          <div className="mp-item" key={it.id}>
            <div className="mp-row">
              <input
                type="checkbox"
                checked={it.enabled}
                onChange={(e) => update(it.id, { enabled: e.target.checked })}
                title={t("Enable/disable")}
              />
              <input
                className="mp-name"
                placeholder="field"
                value={it.name}
                onChange={(e) => update(it.id, { name: e.target.value })}
              />
              <select
                value={it.kind}
                onChange={(e) =>
                  update(it.id, { kind: e.target.value as MultipartField["kind"] })
                }
              >
                <option value="text">{t("text")}</option>
                <option value="file">{t("file")}</option>
              </select>
              {it.kind === "file" ? (
                <div className="mp-file">
                  <select
                    className="mp-src"
                    value={it.source ?? "fixed"}
                    onChange={(e) =>
                      update(it.id, {
                        source: e.target.value as MultipartField["source"],
                      })
                    }
                    title={t("A single file or a random one from a set")}
                  >
                    <option value="fixed">{t("single file")}</option>
                    <option value="pool">{t("from a set")}</option>
                  </select>
                  {!isPool && (
                    <>
                      <input
                        className="mp-value"
                        placeholder={t("path to file (image, .jar …)")}
                        value={it.value}
                        onChange={(e) => update(it.id, { value: e.target.value })}
                      />
                      <button onClick={() => pickFile(it.id)}>{t("Browse…")}</button>
                    </>
                  )}
                  {isPool && (
                    <span className="mp-pool-note">
                      {t("file set — a different one is used for each request")}
                    </span>
                  )}
                </div>
              ) : (
                <input
                  className="mp-value"
                  placeholder={t("value")}
                  value={it.value}
                  onChange={(e) => update(it.id, { value: e.target.value })}
                />
              )}
              <button
                className="ghost del"
                onClick={() => remove(it.id)}
                title={t("Delete")}
              >
                ✕
              </button>
            </div>

            {isPool && (
              <div className="mp-pool">
                <div className="mp-pool-row">
                  <label>{t("Source")}</label>
                  <select
                    value={it.pool_kind ?? "folder"}
                    onChange={(e) =>
                      update(it.id, {
                        pool_kind: e.target.value as MultipartField["pool_kind"],
                      })
                    }
                  >
                    <option value="folder">{t("folder")}</option>
                    <option value="list">{t("file list")}</option>
                    <option value="url">{t("URL list (S3)")}</option>
                  </select>
                  <label>{t("Selection")}</label>
                  <select
                    value={it.pool_mode ?? "random"}
                    onChange={(e) =>
                      update(it.id, {
                        pool_mode: e.target.value as MultipartField["pool_mode"],
                      })
                    }
                  >
                    <option value="random">{t("random")}</option>
                    <option value="sequential">{t("round-robin")}</option>
                  </select>
                </div>

                {(it.pool_kind ?? "folder") === "folder" && (
                  <div className="mp-pool-row">
                    <label>{t("Folder")}</label>
                    <input
                      placeholder={t("path to the folder with files")}
                      value={it.pool_path ?? ""}
                      onChange={(e) => update(it.id, { pool_path: e.target.value })}
                    />
                    <button onClick={() => pickFolder(it.id)}>{t("Browse…")}</button>
                    <input
                      className="mp-mask"
                      placeholder={t("mask: *.jpg,*.png (empty = all)")}
                      value={it.pool_mask ?? ""}
                      onChange={(e) => update(it.id, { pool_mask: e.target.value })}
                    />
                  </div>
                )}

                {it.pool_kind === "list" && (
                  <div className="mp-pool-col">
                    <div className="mp-pool-actions">
                      <button onClick={() => addFiles(it)}>{t("Add files…")}</button>
                      <span className="lt-hint">{t("one path per line")}</span>
                    </div>
                    <textarea
                      placeholder={"C:\\images\\a.png\nC:\\images\\b.jpg"}
                      value={it.pool_paths ?? ""}
                      onChange={(e) => update(it.id, { pool_paths: e.target.value })}
                      spellCheck={false}
                    />
                  </div>
                )}

                {it.pool_kind === "url" && (
                  <div className="mp-pool-col">
                    <span className="lt-hint">
                      {t("one link per line — S3 presigned links or public objects")}
                    </span>
                    <textarea
                      placeholder={
                        "https://bucket.s3.amazonaws.com/img1.png?...\nhttps://bucket.s3.amazonaws.com/img2.jpg?..."
                      }
                      value={it.pool_urls ?? ""}
                      onChange={(e) => update(it.id, { pool_urls: e.target.value })}
                      spellCheck={false}
                    />
                  </div>
                )}
              </div>
            )}
          </div>
        );
      })}
      <div className="lt-hint" style={{ marginTop: 8 }}>
        {t("Files are sent as")} <code>multipart/form-data</code>{t("; the content type is detected from the extension (png/jpg/pdf/jar…). \"From a set\" — under load a different file is used for each request (random or round-robin); files are read once.")}
      </div>
    </div>
  );
}
