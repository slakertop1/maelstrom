import { useEffect, useState } from "react";
import { readLog, clearLog, openLogFolder, logPath, appVersion, openUrl } from "../api";
import { newIssueUrl } from "../config";
import { useT } from "../i18n";

interface Props {
  onClose: () => void;
}

export default function LogModal({ onClose }: Props) {
  const t = useT();
  const [text, setText] = useState("");
  const [path, setPath] = useState("");
  const [copied, setCopied] = useState(false);
  const [reported, setReported] = useState(false);

  const refresh = () => {
    readLog().then(setText).catch(() => setText(""));
  };
  useEffect(() => {
    refresh();
    logPath().then(setPath).catch(() => {});
  }, []);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* ignore */
    }
  };

  // Assemble a ready-to-paste bug report (version + OS + recent log tail),
  // copy it to the clipboard and open the issue tracker.
  const reportBug = async () => {
    const version = await appVersion().catch(() => "Maelstrom");
    const tail = (text || "").split("\n").slice(-200).join("\n");
    const report =
      `## ${t("What happened")}\n${t("<describe what you did and what went wrong; if possible, steps to reproduce>")}\n\n` +
      `## ${t("Expected")}\n${t("<what should have happened>")}\n\n` +
      `## ${t("Environment")}\n${version}\n\n` +
      `## ${t("Log (secrets masked)")}\n\`\`\`\n${tail || t("(log is empty)")}\n\`\`\`\n`;
    try {
      await navigator.clipboard.writeText(report);
    } catch {
      /* ignore */
    }
    await openUrl(newIssueUrl()).catch(() => {});
    setReported(true);
    setTimeout(() => setReported(false), 4000);
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal log-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <span>{t("Logs (for debugging)")}</span>
          <button className="ghost" onClick={onClose}>
            ✕
          </button>
        </div>
        <div className="modal-body">
          <div className="log-toolbar">
            <button className="primary" onClick={reportBug}>
              {reported ? t("Report copied — paste it into the issue ✓") : t("🐞 Report a bug")}
            </button>
            <button onClick={refresh}>{t("Refresh")}</button>
            <button onClick={copy}>{copied ? t("Copied ✓") : t("Copy log")}</button>
            <button onClick={() => openLogFolder().catch(() => {})}>{t("Open folder")}</button>
            <span className="spacer" />
            <button
              className="danger"
              onClick={() => clearLog().then(() => setText(""))}
            >
              {t("Clear")}
            </button>
          </div>
          <div className="log-hint">
            {t("“Report a bug” gathers the version, OS and log into a ready-made report, copies it and opens the issue page — just paste (Ctrl/⌘+V) and describe the problem. Tokens, passwords and")}{" "}
            <code>Authorization</code> {t("are never written to the log (they are masked as")}{" "}
            <code>***</code>{t(").")}
          </div>
          <textarea className="log-view" value={text || t("(log is empty)")} readOnly spellCheck={false} />
          {path && <div className="log-path">{path}</div>}
        </div>
      </div>
    </div>
  );
}
