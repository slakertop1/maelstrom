import { GrpcCallResult } from "../types";
import { useT, tr2 } from "../i18n";

interface Props {
  result: GrpcCallResult | null;
  error: string | null;
  sending: boolean;
}

/// Shows gRPC call responses (one JSON block per message; server-streaming can
/// return several).
export default function GrpcResultView({ result, error, sending }: Props) {
  const t = useT();
  if (sending) {
    return (
      <div className="response-view">
        <div className="resp-empty">
          <span className="spinner" /> {t("Calling…")}
        </div>
      </div>
    );
  }
  if (error) {
    return (
      <div className="response-view">
        <div className="resp-error">
          <div className="resp-error-title">{t("gRPC error")}</div>
          <pre>{error}</pre>
        </div>
      </div>
    );
  }
  if (!result) {
    return (
      <div className="response-view">
        <div className="resp-empty">
          {t("Select a method and click “Call”. The response will appear here.")}
        </div>
      </div>
    );
  }
  return (
    <div className="response-view">
      <div className="resp-status-row">
        <span className="resp-status ok">OK</span>
        <span className="resp-meta">
          {result.server_streaming
            ? tr2("stream: {n} msg", { n: result.responses.length })
            : "unary"}{" "}
          · {tr2("{n} ms", { n: result.duration_ms.toFixed(0) })}
        </span>
      </div>
      <div className="grpc-responses">
        {result.responses.map((r, i) => (
          <div className="grpc-response" key={i}>
            {result.server_streaming && (
              <div className="grpc-response-idx">{tr2("message #{n}", { n: i + 1 })}</div>
            )}
            <pre>{r}</pre>
          </div>
        ))}
        {result.responses.length === 0 && (
          <div className="resp-empty">{t("The server returned no messages.")}</div>
        )}
      </div>
    </div>
  );
}
