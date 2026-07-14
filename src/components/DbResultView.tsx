import { DbResponse } from "../types";
import { fmtMs } from "../charts";
import { useT, tr2 } from "../i18n";

interface Props {
  result: DbResponse | null;
  error: string | null;
  sending: boolean;
}

export default function DbResultView({ result, error, sending }: Props) {
  const t = useT();
  if (sending) {
    return (
      <div className="response-pane">
        <div className="resp-empty">
          <div className="spinner" style={{ borderTopColor: "var(--accent)" }} />
          <div>{t("Running query…")}</div>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="response-pane">
        <div className="resp-head">
          <span className="title">{t("Result")}</span>
          <span className="status-pill status-5xx">{t("Error")}</span>
        </div>
        <div className="resp-error">{error}</div>
      </div>
    );
  }

  if (!result) {
    return (
      <div className="response-pane">
        <div className="resp-empty">
          <div style={{ fontSize: 28, opacity: 0.4 }}>🗄</div>
          <div>{t("Run a SQL query to see the result")}</div>
        </div>
      </div>
    );
  }

  const isSelect = result.columns.length > 0 || result.rows_affected === null;

  return (
    <div className="response-pane">
      <div className="resp-head">
        <span className="title">{t("Result")}</span>
        <span className="status-pill status-2xx">OK</span>
        {isSelect ? (
          <span className="resp-meta">
            {tr2("rows: {n}", { n: result.row_count })}
            {result.truncated ? " " + tr2("(first {n} shown)", { n: result.rows.length }) : ""}
          </span>
        ) : (
          <span className="resp-meta">{tr2("rows affected: {n}", { n: result.rows_affected ?? 0 })}</span>
        )}
        <span className="resp-meta">{fmtMs(result.duration_ms)}</span>
      </div>
      <div className="resp-body" style={{ padding: 0 }}>
        {result.columns.length > 0 ? (
          <div className="db-table-scroll">
            <table className="db-table">
              <thead>
                <tr>
                  <th className="rownum">#</th>
                  {result.columns.map((c, i) => (
                    <th key={i}>{c}</th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {result.rows.map((row, ri) => (
                  <tr key={ri}>
                    <td className="rownum">{ri + 1}</td>
                    {row.map((cell, ci) => (
                      <td key={ci} className={cell === "NULL" ? "null-cell" : ""}>
                        {cell}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <div style={{ padding: 16 }}>
            {tr2("Query executed successfully. Rows affected: {n}.", { n: result.rows_affected ?? 0 })}
          </div>
        )}
      </div>
    </div>
  );
}
