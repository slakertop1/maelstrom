import { WsConfig } from "../types";
import { useT } from "../i18n";

interface Props {
  config: WsConfig;
  onChange: (c: WsConfig) => void;
}

/// Editor for a WebSocket request: the message to send. The ws:// URL and
/// «Отправить» live in the top bar (RequestEditor).
export default function WsEditor({ config, onChange }: Props) {
  const t = useT();
  return (
    <div className="grpc-editor">
      <label
        className="grpc-body-label"
        title={t("Text sent to the server after connecting. Supports {{variables}}.")}
      >
        {t("Message to send")}
      </label>
      <textarea
        className="grpc-body"
        placeholder='{"action":"subscribe","channel":"ticker"}'
        value={config.message}
        onChange={(e) => onChange({ ...config, message: e.target.value })}
        spellCheck={false}
      />
      <div className="lt-hint" style={{ marginTop: 8 }}>
        {t("The address is entered above:")} <code>ws://localhost:8080/socket</code> {t("or")}{" "}
        <code>wss://…</code> {t("(secure). On \"Send\" the client connects, sends the message, and shows the server's responses. Under load (the \"⚡ Load\" tab) each virtual user holds the connection and measures the message → response time.")}
      </div>
    </div>
  );
}
