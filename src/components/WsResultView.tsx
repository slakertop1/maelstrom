import { WsCallResult } from "../types";
import { useT, tr2 } from "../i18n";

interface Props {
  result: WsCallResult | null;
  error: string | null;
  sending: boolean;
}

/// Shows the messages received after a WebSocket send.
export default function WsResultView({ result, error, sending }: Props) {
  const t = useT();
  if (sending) {
    return (
      <div className="response-view">
        <div className="resp-empty">
          <span className="spinner" /> {t("Connecting and exchanging…")}
        </div>
      </div>
    );
  }
  if (error) {
    return (
      <div className="response-view">
        <div className="resp-error">
          <div className="resp-error-title">{t("WebSocket error")}</div>
          <pre>{error}</pre>
        </div>
      </div>
    );
  }
  if (!result) {
    return (
      <div className="response-view">
        <div className="resp-empty">
          {t("Enter a ws:// address and a message, then click “Send”. Responses will appear here.")}
        </div>
      </div>
    );
  }
  return (
    <div className="response-view">
      <div className="resp-status-row">
        <span className="resp-status ok">OK</span>
        <span className="resp-meta">
          {tr2("messages received: {n}", { n: result.messages.length })} · {tr2("{n} ms", { n: result.duration_ms.toFixed(0) })}
        </span>
      </div>
      <div className="grpc-responses">
        {result.messages.map((m, i) => (
          <div className="grpc-response" key={i}>
            <div className="grpc-response-idx">{tr2("message #{n}", { n: i + 1 })}</div>
            <pre>{m}</pre>
          </div>
        ))}
        {result.messages.length === 0 && (
          <div className="resp-empty">{t("The server sent no messages (or closed the connection).")}</div>
        )}
      </div>
    </div>
  );
}
