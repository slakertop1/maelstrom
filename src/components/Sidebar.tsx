import { useState } from "react";
import { Collection, RequestConfig } from "../types";
import { useT, tr2 } from "../i18n";

interface Props {
  collections: Collection[];
  activeRequestId: string | null;
  onOpen: (collectionId: string, request: RequestConfig) => void;
  onAddCollection: () => void;
  onImport: () => void;
  onAddRequest: (collectionId: string) => void;
  onDeleteRequest: (collectionId: string, requestId: string) => void;
  onDeleteCollection: (collectionId: string) => void;
  onRenameCollection: (collectionId: string, name: string) => void;
  onLoadService: (collectionId: string) => void;
  onStreams: (collectionId: string) => void;
}

export default function Sidebar(p: Props) {
  const t = useT();
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({});
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");

  const toggle = (id: string) =>
    setCollapsed((c) => ({ ...c, [id]: !c[id] }));

  const commitRename = (id: string) => {
    if (renameValue.trim()) p.onRenameCollection(id, renameValue.trim());
    setRenaming(null);
  };

  return (
    <div className="sidebar">
      <div className="sidebar-head">
        <span>{t("Collections")}</span>
        <span style={{ display: "flex", gap: 2 }}>
          <button
            className="ghost"
            onClick={p.onImport}
            title={t("Import OpenAPI / Swagger")}
          >
            ↥
          </button>
          <button className="ghost" onClick={p.onAddCollection} title={t("New collection")}>
            ＋
          </button>
        </span>
      </div>
      <div className="sidebar-body">
        {p.collections.length === 0 && (
          <div className="sidebar-empty">
            {t("No collections.")}
            <br />
            {t("Click ＋ to create one, or ↥ to import OpenAPI/Swagger.")}
          </div>
        )}
        {p.collections.map((col) => (
          <div className="col-item" key={col.id}>
            <div className="col-head" onClick={() => toggle(col.id)}>
              <span className="chev">{collapsed[col.id] ? "▶" : "▼"}</span>
              {renaming === col.id ? (
                <input
                  autoFocus
                  value={renameValue}
                  onClick={(e) => e.stopPropagation()}
                  onChange={(e) => setRenameValue(e.target.value)}
                  onBlur={() => commitRename(col.id)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") commitRename(col.id);
                    if (e.key === "Escape") setRenaming(null);
                  }}
                  style={{ flex: 1, padding: "2px 6px" }}
                />
              ) : (
                <span
                  className="name"
                  onDoubleClick={(e) => {
                    e.stopPropagation();
                    setRenaming(col.id);
                    setRenameValue(col.name);
                  }}
                >
                  {col.name}
                </span>
              )}
              <span className="actions">
                <button
                  className="ghost"
                  title={t("Load-test service (multiple endpoints)")}
                  onClick={(e) => {
                    e.stopPropagation();
                    p.onLoadService(col.id);
                  }}
                >
                  ⚡
                </button>
                <button
                  className="ghost"
                  title={t("Chained load (streams): multi-step scenarios with value passing")}
                  onClick={(e) => {
                    e.stopPropagation();
                    p.onStreams(col.id);
                  }}
                >
                  🔗
                </button>
                <button
                  className="ghost"
                  title={t("Add request")}
                  onClick={(e) => {
                    e.stopPropagation();
                    p.onAddRequest(col.id);
                  }}
                >
                  ＋
                </button>
                <button
                  className="ghost"
                  title={t("Delete collection")}
                  onClick={(e) => {
                    e.stopPropagation();
                    if (confirm(tr2("Delete collection “{name}” with all its requests?", { name: col.name })))
                      p.onDeleteCollection(col.id);
                  }}
                >
                  🗑
                </button>
              </span>
            </div>
            {!collapsed[col.id] &&
              col.requests.map((req) => (
                <div
                  key={req.id}
                  className={`req-item ${p.activeRequestId === req.id ? "active" : ""}`}
                  onClick={() => p.onOpen(col.id, req)}
                >
                  <span className={`method-tag m-${req.method}`}>{req.method}</span>
                  <span className="name">{req.name}</span>
                  <span className="actions">
                    <button
                      className="ghost"
                      title={t("Delete request")}
                      onClick={(e) => {
                        e.stopPropagation();
                        p.onDeleteRequest(col.id, req.id);
                      }}
                    >
                      ✕
                    </button>
                  </span>
                </div>
              ))}
          </div>
        ))}
      </div>
    </div>
  );
}
