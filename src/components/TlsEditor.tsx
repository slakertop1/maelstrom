import { open } from "@tauri-apps/plugin-dialog";
import { TlsConfig } from "../types";
import { useT } from "../i18n";

interface Props {
  tls: TlsConfig;
  onChange: (t: TlsConfig) => void;
}

// Module-level (not defined inside TlsEditor's render): a component declared
// inside another component's body gets a new identity every render, which
// makes React remount the <input> on every keystroke and drop focus. See ed1.
function FileRow({
  label,
  hint,
  value,
  onValueChange,
  onPick,
  onClear,
}: {
  label: string;
  hint: string;
  value: string;
  onValueChange: (v: string) => void;
  onPick: () => void;
  onClear: () => void;
}) {
  const t = useT();
  return (
    <div className="tls-file-row">
      <label>{label}</label>
      <div className="tls-file-input">
        <input value={value} placeholder={hint} onChange={(e) => onValueChange(e.target.value)} />
        <button onClick={onPick}>{t("Browse…")}</button>
        {value && (
          <button className="ghost" onClick={onClear}>
            ✕
          </button>
        )}
      </div>
    </div>
  );
}

export default function TlsEditor({ tls, onChange }: Props) {
  const t = useT();
  const set = (patch: Partial<TlsConfig>) => onChange({ ...tls, ...patch });

  const pick = async (field: keyof TlsConfig) => {
    const path = await open({
      multiple: false,
      filters: [
        { name: t("Certificates/keys"), extensions: ["pem", "crt", "cer", "key"] },
        { name: t("All files"), extensions: ["*"] },
      ],
    });
    if (typeof path === "string") set({ [field]: path } as Partial<TlsConfig>);
  };

  return (
    <div className="tls-editor">
      <label className="tls-toggle">
        <input
          type="checkbox"
          checked={tls.enabled}
          onChange={(e) => set({ enabled: e.target.checked })}
        />
        {t("Use a custom TLS configuration for this request")}
      </label>

      {tls.enabled && (
        <>
          <div className="tls-section-title">{t("Client certificate (mTLS)")}</div>
          <FileRow
            label={t("Certificate (PEM)")}
            hint={t("path to client.pem / client.crt")}
            value={tls.client_cert_pem}
            onValueChange={(v) => set({ client_cert_pem: v })}
            onPick={() => pick("client_cert_pem")}
            onClear={() => set({ client_cert_pem: "" })}
          />
          <FileRow
            label={t("Private key (PEM)")}
            hint={t("path to client.key")}
            value={tls.client_key_pem}
            onValueChange={(v) => set({ client_key_pem: v })}
            onPick={() => pick("client_key_pem")}
            onClear={() => set({ client_key_pem: "" })}
          />

          <div className="tls-section-title">{t("Trusted root CA")}</div>
          <FileRow
            label={t("CA certificate (PEM)")}
            hint={t("path to ca.pem")}
            value={tls.ca_cert_pem}
            onValueChange={(v) => set({ ca_cert_pem: v })}
            onPick={() => pick("ca_cert_pem")}
            onClear={() => set({ ca_cert_pem: "" })}
          />

          <label className="tls-toggle" style={{ marginTop: 14 }}>
            <input
              type="checkbox"
              checked={tls.insecure}
              onChange={(e) => set({ insecure: e.target.checked })}
            />
            {t("Skip server certificate verification (insecure, dev only)")}
          </label>

          <div className="lt-hint" style={{ marginTop: 12 }}>
            {t("TLS settings apply to both single requests and load tests — all virtual users use the same client certificate. The key must have no passphrase (PKCS#8 or RSA in PEM).")}
          </div>
        </>
      )}
    </div>
  );
}
