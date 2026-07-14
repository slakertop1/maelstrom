import { open } from "@tauri-apps/plugin-dialog";
import { TlsConfig } from "../types";
import { useT } from "../i18n";

interface Props {
  tls: TlsConfig;
  onChange: (t: TlsConfig) => void;
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

  const FileRow = ({
    label,
    field,
    hint,
  }: {
    label: string;
    field: "client_cert_pem" | "client_key_pem" | "ca_cert_pem";
    hint: string;
  }) => {
    const t = useT();
    return (
    <div className="tls-file-row">
      <label>{label}</label>
      <div className="tls-file-input">
        <input
          value={tls[field]}
          placeholder={hint}
          onChange={(e) => set({ [field]: e.target.value } as Partial<TlsConfig>)}
        />
        <button onClick={() => pick(field)}>{t("Browse…")}</button>
        {tls[field] && (
          <button className="ghost" onClick={() => set({ [field]: "" } as Partial<TlsConfig>)}>
            ✕
          </button>
        )}
      </div>
    </div>
    );
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
            field="client_cert_pem"
            hint={t("path to client.pem / client.crt")}
          />
          <FileRow
            label={t("Private key (PEM)")}
            field="client_key_pem"
            hint={t("path to client.key")}
          />

          <div className="tls-section-title">{t("Trusted root CA")}</div>
          <FileRow label={t("CA certificate (PEM)")} field="ca_cert_pem" hint={t("path to ca.pem")} />

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
