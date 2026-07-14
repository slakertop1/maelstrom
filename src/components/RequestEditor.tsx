import { ReactNode, useState } from "react";
import { BodyType, RequestConfig } from "../types";
import KeyValueEditor from "./KeyValueEditor";
import MultipartEditor from "./MultipartEditor";
import AuthEditor from "./AuthEditor";
import TlsEditor from "./TlsEditor";
import GrpcEditor from "./GrpcEditor";
import WsEditor from "./WsEditor";
import AssertionsEditor from "./AssertionsEditor";
import { useT } from "../i18n";

const METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

interface Props {
  request: RequestConfig;
  dirty: boolean;
  sending: boolean;
  onChange: (req: RequestConfig) => void;
  onSend: () => void;
  onSave: () => void;
  onFetchToken: () => Promise<string>;
  tokenUrls?: string[];
  authProfiles?: import("../types").AuthProfile[];
  onSaveAuthProfile?: () => void;
  onDeleteAuthProfile?: (id: string) => void;
  loadTestPanel: ReactNode;
  loadTestRunning: boolean;
  onTabChange?: (tab: string) => void;
}

type Tab = "params" | "headers" | "body" | "auth" | "tls" | "assert" | "load";

export default function RequestEditor(p: Props) {
  const t = useT();
  const [tab, setTabRaw] = useState<Tab>("params");
  const setTab = (t: Tab) => {
    setTabRaw(t);
    p.onTabChange?.(t);
  };
  const req = p.request;
  const set = (patch: Partial<RequestConfig>) => p.onChange({ ...req, ...patch });
  const isDb = req.kind === "db";
  const isGrpc = req.kind === "grpc";
  const isWs = req.kind === "ws";

  const countEnabled = (items: { key: string; enabled: boolean }[]) =>
    items.filter((i) => i.enabled && i.key.trim()).length;

  const authBadge = req.auth.type !== "none";
  const tlsBadge = req.tls.enabled;

  const tabs: { id: Tab; label: string; badge?: number; dot?: boolean }[] = [
    { id: "params", label: t("Params"), badge: countEnabled(req.params) },
    { id: "headers", label: t("Headers"), badge: countEnabled(req.headers) },
    { id: "body", label: t("Body") },
    { id: "auth", label: t("Auth"), dot: authBadge },
    { id: "tls", label: t("Certificates"), dot: tlsBadge },
    { id: "assert", label: t("✔ Assertions"), badge: req.assertions.filter((a) => a.enabled).length },
    { id: "load", label: t("⚡ Load") },
  ];

  const canSend = isDb
    ? !!req.db.url.trim() && !!req.db.query.trim()
    : isGrpc
      ? !!req.grpc.endpoint.trim() && !!req.grpc.service && !!req.grpc.method
      : isWs
        ? !!req.ws.url.trim()
        : !!req.url.trim();

  return (
    <>
      <div className="req-name-row">
        <input
          value={req.name}
          onChange={(e) => set({ name: e.target.value })}
          title={t("Request name")}
        />
        <div className="kind-switch">
          <button
            className={req.kind === "http" ? "active" : ""}
            onClick={() => set({ kind: "http" })}
          >
            HTTP
          </button>
          <button className={isDb ? "active" : ""} onClick={() => set({ kind: "db" })}>
            {t("Database")}
          </button>
          <button className={isGrpc ? "active" : ""} onClick={() => set({ kind: "grpc" })}>
            gRPC
          </button>
          <button className={isWs ? "active" : ""} onClick={() => set({ kind: "ws" })}>
            WebSocket
          </button>
        </div>
        {p.dirty && <span className="dirty-dot" title={t("Unsaved changes")} />}
      </div>

      {isDb ? (
        <DbBar request={req} set={set} sending={p.sending} canSend={canSend} onSend={p.onSend} onSave={p.onSave} />
      ) : isGrpc ? (
        <GrpcBar request={req} set={set} sending={p.sending} canSend={canSend} onSend={p.onSend} onSave={p.onSave} />
      ) : isWs ? (
        <WsBar request={req} set={set} sending={p.sending} canSend={canSend} onSend={p.onSend} onSave={p.onSave} />
      ) : (
        <div className="request-bar">
          <select
            className="method"
            value={req.method}
            onChange={(e) => set({ method: e.target.value })}
          >
            {METHODS.map((m) => (
              <option key={m}>{m}</option>
            ))}
          </select>
          <input
            className="url"
            placeholder="https://api.example.com/users?id={{user_id}}"
            value={req.url}
            onChange={(e) => set({ url: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !p.sending) p.onSend();
            }}
          />
          <button className="primary" onClick={p.onSend} disabled={p.sending || !canSend}>
            {p.sending ? <span className="spinner" /> : t("Send")}
          </button>
          <button onClick={p.onSave}>{t("Save")}</button>
        </div>
      )}

      {isDb || isGrpc || isWs ? (
        <div className="tabs">
          <span
            className={`tab ${tab !== "load" ? "active" : ""}`}
            onClick={() => setTab("params")}
          >
            {isGrpc ? t("Request") : isWs ? t("Message") : t("SQL query")}
          </span>
          <span
            className={`tab ${tab === "load" ? "active" : ""}`}
            onClick={() => setTab("load")}
            style={{ marginLeft: "auto" }}
          >
            {t("⚡ Load")}
          </span>
        </div>
      ) : (
        <div className="tabs">
          {tabs.map((t) => (
            <span
              key={t.id}
              className={`tab ${tab === t.id ? "active" : ""}`}
              onClick={() => setTab(t.id)}
            >
              {t.label}
              {t.badge ? <span className="badge">{t.badge}</span> : null}
              {t.dot ? <span className="tab-dot" /> : null}
            </span>
          ))}
        </div>
      )}

      <div className="tab-body">
        {isDb ? (
          tab === "load" ? (
            p.loadTestPanel
          ) : (
            <DbQueryEditor request={req} set={set} />
          )
        ) : isGrpc ? (
          tab === "load" ? (
            p.loadTestPanel
          ) : (
            <GrpcEditor config={req.grpc} onChange={(grpc) => set({ grpc })} />
          )
        ) : isWs ? (
          tab === "load" ? (
            p.loadTestPanel
          ) : (
            <WsEditor config={req.ws} onChange={(ws) => set({ ws })} />
          )
        ) : (
          <>
            {tab === "params" && (
              <KeyValueEditor
                items={req.params}
                onChange={(params) => set({ params })}
                keyPlaceholder={t("parameter")}
              />
            )}
            {tab === "headers" && (
              <KeyValueEditor
                items={req.headers}
                onChange={(headers) => set({ headers })}
                keyPlaceholder="Header-Name"
              />
            )}
            {tab === "body" && (
              <div className="body-editor">
                <div className="body-type-row">
                  {(
                    [
                      ["none", t("none")],
                      ["json", "JSON"],
                      ["text", t("text")],
                      ["form", "form-urlencoded"],
                      ["multipart", t("multipart / files")],
                    ] as [BodyType, string][]
                  ).map(([value, label]) => (
                    <label key={value}>
                      <input
                        type="radio"
                        name="bodyType"
                        checked={req.body_type === value}
                        onChange={() => set({ body_type: value })}
                      />
                      {label}
                    </label>
                  ))}
                </div>
                {req.body_type === "form" ? (
                  <KeyValueEditor
                    items={req.form_body}
                    onChange={(form_body) => set({ form_body })}
                    keyPlaceholder={t("field")}
                  />
                ) : req.body_type === "multipart" ? (
                  <MultipartEditor
                    items={req.multipart_body}
                    onChange={(multipart_body) => set({ multipart_body })}
                  />
                ) : req.body_type !== "none" ? (
                  <textarea
                    placeholder={
                      req.body_type === "json" ? '{\n  "key": "value"\n}' : t("Request body…")
                    }
                    value={req.body}
                    onChange={(e) => set({ body: e.target.value })}
                    spellCheck={false}
                  />
                ) : (
                  <div className="lt-hint">{t("This request has no body.")}</div>
                )}
              </div>
            )}
            {tab === "auth" && (
              <AuthEditor
                key={req.id} // reset fetch error/busy/profile selection per request
                auth={req.auth}
                onChange={(auth) => set({ auth })}
                onFetchToken={p.onFetchToken}
                tokenUrls={p.tokenUrls}
                authProfiles={p.authProfiles}
                onSaveAuthProfile={p.onSaveAuthProfile}
                onDeleteAuthProfile={p.onDeleteAuthProfile}
              />
            )}
            {tab === "tls" && <TlsEditor tls={req.tls} onChange={(tls) => set({ tls })} />}
            {tab === "assert" && (
              <AssertionsEditor
                items={req.assertions}
                onChange={(assertions) => set({ assertions })}
              />
            )}
            {tab === "load" && p.loadTestPanel}
          </>
        )}
      </div>
    </>
  );
}

function WsBar({
  request,
  set,
  sending,
  canSend,
  onSend,
  onSave,
}: {
  request: RequestConfig;
  set: (patch: Partial<RequestConfig>) => void;
  sending: boolean;
  canSend: boolean;
  onSend: () => void;
  onSave: () => void;
}) {
  const t = useT();
  return (
    <div className="request-bar">
      <span className="grpc-scheme" title="WebSocket">
        WS
      </span>
      <input
        className="url"
        placeholder={t("ws://localhost:8080/socket  (or wss://…)")}
        value={request.ws.url}
        onChange={(e) => set({ ws: { ...request.ws, url: e.target.value } })}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !sending && canSend) onSend();
        }}
      />
      <button className="primary" onClick={onSend} disabled={sending || !canSend}>
        {sending ? <span className="spinner" /> : t("Send")}
      </button>
      <button onClick={onSave}>{t("Save")}</button>
    </div>
  );
}

function GrpcBar({
  request,
  set,
  sending,
  canSend,
  onSend,
  onSave,
}: {
  request: RequestConfig;
  set: (patch: Partial<RequestConfig>) => void;
  sending: boolean;
  canSend: boolean;
  onSend: () => void;
  onSave: () => void;
}) {
  const t = useT();
  return (
    <div className="request-bar">
      <span className="grpc-scheme" title={t("gRPC over HTTP/2")}>
        gRPC
      </span>
      <input
        className="url"
        placeholder={t("http://localhost:50051  (gRPC server address)")}
        value={request.grpc.endpoint}
        onChange={(e) => set({ grpc: { ...request.grpc, endpoint: e.target.value } })}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !sending && canSend) onSend();
        }}
      />
      <button className="primary" onClick={onSend} disabled={sending || !canSend}>
        {sending ? <span className="spinner" /> : t("Call")}
      </button>
      <button onClick={onSave}>{t("Save")}</button>
    </div>
  );
}

function DbBar({
  request,
  set,
  sending,
  canSend,
  onSend,
  onSave,
}: {
  request: RequestConfig;
  set: (patch: Partial<RequestConfig>) => void;
  sending: boolean;
  canSend: boolean;
  onSend: () => void;
  onSave: () => void;
}) {
  const t = useT();
  return (
    <div className="request-bar">
      <select
        className="method"
        value={request.db.driver}
        onChange={(e) => set({ db: { ...request.db, driver: e.target.value as any } })}
        title={t("Database engine")}
      >
        <option value="postgres">Postgres</option>
        <option value="mysql">MySQL</option>
        <option value="sqlite">SQLite</option>
      </select>
      <input
        className="url"
        placeholder={
          request.db.driver === "postgres"
            ? "postgres://user:pass@host:5432/db"
            : request.db.driver === "mysql"
              ? "mysql://user:pass@host:3306/db"
              : "sqlite:///path/to/file.db"
        }
        value={request.db.url}
        onChange={(e) => set({ db: { ...request.db, url: e.target.value } })}
      />
      <button className="primary" onClick={onSend} disabled={sending || !canSend}>
        {sending ? <span className="spinner" /> : t("Run")}
      </button>
      <button onClick={onSave}>{t("Save")}</button>
    </div>
  );
}

function DbQueryEditor({
  request,
  set,
}: {
  request: RequestConfig;
  set: (patch: Partial<RequestConfig>) => void;
}) {
  const t = useT();
  const isSqlite = request.db.driver === "sqlite";
  return (
    <div className="body-editor">
      {!isSqlite && (
        <div className="db-creds">
          <input
            className="db-cred"
            placeholder={t("username")}
            value={request.db.username}
            onChange={(e) => set({ db: { ...request.db, username: e.target.value } })}
          />
          <input
            className="db-cred"
            type="password"
            placeholder={t("password")}
            value={request.db.password}
            onChange={(e) => set({ db: { ...request.db, password: e.target.value } })}
          />
          <span className="db-creds-hint">
            {t("username/password separately (like in DBeaver) — inserted into the connection string")}
          </span>
        </div>
      )}
      <textarea
        placeholder="SELECT * FROM users WHERE created_at > now() - interval '1 day' LIMIT 100;"
        value={request.db.query}
        onChange={(e) => set({ db: { ...request.db, query: e.target.value } })}
        spellCheck={false}
        style={{ minHeight: 180 }}
      />
      <div className="lt-hint">
        {t("SELECT / WITH / SHOW return a table; INSERT / UPDATE / DELETE return the number of affected rows. The same query can be run under load on the \"⚡ Load\" tab. The connection string can be moved to an environment variable.")}
      </div>
    </div>
  );
}
