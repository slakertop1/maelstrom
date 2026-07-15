import { useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { GrpcConfig, GrpcMethodInfo, newTls } from "../types";
import { grpcListMethods, grpcRequestTemplate } from "../api";
import { useT } from "../i18n";
import TlsEditor from "./TlsEditor";

interface Props {
  config: GrpcConfig;
  onChange: (c: GrpcConfig) => void;
}

/// Editor for a gRPC request: pick a .proto, choose a method, fill the JSON body.
/// The endpoint + «Вызвать» live in the top bar (RequestEditor), like DB/HTTP.
export default function GrpcEditor({ config, onChange }: Props) {
  const t = useT();
  const [methods, setMethods] = useState<GrpcMethodInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Bumped on every selectMethod() call so a slow grpcRequestTemplate() from a
  // previous selection can detect it's stale and skip applying its result — ed5.
  const selectionRef = useRef(0);

  const set = (patch: Partial<GrpcConfig>) => onChange({ ...config, ...patch });

  const importDirs = (config.import_paths ?? "")
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter(Boolean);

  const addImportDir = async () => {
    const dir = await open({ directory: true, multiple: false });
    if (typeof dir === "string") {
      const cur = (config.import_paths ?? "").trim();
      set({ import_paths: [cur, dir].filter(Boolean).join("\n") });
    }
  };

  const pickProto = async () => {
    const path = await open({
      multiple: false,
      // "Все файлы" as a fallback — some .proto files (odd extensions, macOS
      // type quirks) aren't selectable under a strict *.proto filter.
      filters: [
        { name: "Proto", extensions: ["proto"] },
        { name: t("All files"), extensions: ["*"] },
      ],
    });
    if (typeof path === "string") {
      set({ proto_path: path });
      await loadMethods(path);
    }
  };

  const loadMethods = async (path?: string) => {
    const p = path ?? config.proto_path;
    if (!p.trim()) {
      setError(t("Select a .proto file first"));
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const list = await grpcListMethods(p, importDirs);
      setMethods(list);
      if (list.length === 0) setError(t("The .proto has no services with methods"));
    } catch (e) {
      setError(String(e));
      setMethods([]);
    } finally {
      setLoading(false);
    }
  };

  const selectMethod = async (value: string) => {
    const [service, method] = value.split("::");
    const mySelection = ++selectionRef.current;
    set({ service, method });
    // Prefill the request body with a JSON skeleton of the method's input type.
    if (service && method && !config.body.trim()) {
      try {
        const tmpl = await grpcRequestTemplate(config.proto_path, service, method, importDirs);
        // If the user switched to another method while this was in flight,
        // selectionRef has moved on — drop this stale template (ed5).
        if (selectionRef.current !== mySelection) return;
        set({ service, method, body: tmpl });
      } catch {
        /* keep body empty on failure */
      }
    }
  };

  const selected = methods.find(
    (m) => m.service === config.service && m.method === config.method
  );

  return (
    <div className="grpc-editor">
      <div className="grpc-row">
        <label title={t("A .proto file describing the service.")}>
          {t(".proto file")}
        </label>
        <input
          className="grpc-proto"
          placeholder={t("path to service.proto")}
          value={config.proto_path}
          onChange={(e) => set({ proto_path: e.target.value })}
        />
        <button onClick={pickProto}>{t("Browse…")}</button>
        <button onClick={() => loadMethods()} disabled={loading}>
          {loading ? <span className="spinner" /> : t("Load methods")}
        </button>
      </div>

      <div className="grpc-row" style={{ alignItems: "flex-start" }}>
        <label title={t("Folders that imports are resolved against (like protoc -I). One per line.")}>
          {t("Import folders")}
        </label>
        <textarea
          className="grpc-imports"
          placeholder={t("proto root in the repository\ne.g. …/schema-registry\n…/schema-registry/external")}
          value={config.import_paths ?? ""}
          onChange={(e) => set({ import_paths: e.target.value })}
          spellCheck={false}
          rows={2}
        />
        <button onClick={addImportDir}>{t("+ Folder…")}</button>
      </div>

      <div title={t("Custom TLS for the server address above: trust a self-signed / internal CA certificate, or present a client certificate for mTLS. Leave off for plain http:// or a publicly trusted https:// certificate.")}>
        <div className="tls-section-title">{t("TLS / mTLS")}</div>
        <TlsEditor tls={config.tls ?? newTls()} onChange={(tls) => set({ tls })} />
      </div>

      <div className="grpc-row">
        <label title={t("Pick a gRPC method from the loaded .proto")}>{t("Method")}</label>
        <select
          className="grpc-method"
          value={config.service && config.method ? `${config.service}::${config.method}` : ""}
          onChange={(e) => selectMethod(e.target.value)}
          disabled={methods.length === 0}
        >
          <option value="">
            {methods.length ? t("— select a method —") : t("load methods first")}
          </option>
          {methods.map((m) => (
            <option key={m.path} value={`${m.service}::${m.method}`}>
              {m.service} / {m.method}
              {m.server_streaming ? " (server stream)" : ""}
              {m.client_streaming ? " (client stream)" : ""}
            </option>
          ))}
        </select>
      </div>

      {selected && (
        <div className="grpc-meta lt-hint">
          {t("Request:")} <code>{selected.input_type}</code> → {t("Response:")} <code>{selected.output_type}</code>
          {selected.server_streaming && t(" · the server returns a stream of messages")}
          {selected.client_streaming &&
            t(" · client streaming: the body is a JSON array of messages [{…},{…}]")}
        </div>
      )}

      <label className="grpc-body-label" title={t("Request body in JSON format (proto3 JSON). Field names as in the .proto.")}>
        {t("Request body (JSON)")}
      </label>
      <textarea
        className="grpc-body"
        placeholder='{\n  "name": "value"\n}'
        value={config.body}
        onChange={(e) => set({ body: e.target.value })}
        spellCheck={false}
      />

      {error && <div className="lt-error" style={{ marginTop: 8 }}>{error}</div>}

      <div className="lt-hint" style={{ marginTop: 8 }}>
        {t("The server address (for example")} <code>http://localhost:50051</code> {t("or")}{" "}
        <code>https://grpc.example.com</code>{t(") is entered in the bar above. Imports inside the .proto are resolved automatically in the tree next to the file — \"Import folders\" are only needed if the dependencies live elsewhere.")}{" "}
        <code>google/protobuf/*</code> {t("are built in;")}{" "}
        <code>google/api/*</code> {t("(annotations, http) must be in the repository. The same call can be run under load on the \"⚡ Load\" tab.")}
      </div>
    </div>
  );
}
