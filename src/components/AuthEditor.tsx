import { useState } from "react";
import { AuthConfig, AuthProfile, OAuth2Config, OAuthGrant } from "../types";
import { useT, tr2 } from "../i18n";

interface Props {
  auth: AuthConfig;
  onChange: (a: AuthConfig) => void;
  onFetchToken: () => Promise<string>;
  /** Недавно использованные Token URL — общая подсказка для всех запросов. */
  tokenUrls?: string[];
  /** Reusable auth setups: apply a saved profile here, save the current one. */
  authProfiles?: AuthProfile[];
  onSaveAuthProfile?: () => void;
  onDeleteAuthProfile?: (id: string) => void;
}

const GRANTS: { id: OAuthGrant; label: string }[] = [
  { id: "client_credentials", label: "Client Credentials (service-to-service)" },
  { id: "authorization_code", label: "Authorization Code + PKCE (SSO via browser)" },
  { id: "password", label: "Password (ROPC)" },
  { id: "refresh_token", label: "Refresh Token" },
];

export default function AuthEditor(p: Props) {
  const t = useT();
  const { auth } = p;
  const [profileId, setProfileId] = useState("");
  const set = (patch: Partial<AuthConfig>) => p.onChange({ ...auth, ...patch });
  const setOAuth = (patch: Partial<OAuth2Config>) =>
    p.onChange({ ...auth, oauth2: { ...auth.oauth2, ...patch } });

  const profiles = p.authProfiles ?? [];
  const applyProfile = () => {
    const prof = profiles.find((x) => x.id === profileId);
    // Deep copy: editing the request afterwards must not mutate the profile.
    if (prof) p.onChange(structuredClone(prof.auth));
  };

  return (
    <div className="auth-editor">
      {(profiles.length > 0 || auth.type !== "none") && (
        <div
          className="auth-profiles-row"
          title={t("Fill the credentials once, save them as a profile, then apply it in any other request instead of retyping.")}
        >
          {profiles.length > 0 && (
            <>
              <select value={profileId} onChange={(e) => setProfileId(e.target.value)}>
                <option value="">{t("Auth profiles…")}</option>
                {profiles.map((x) => (
                  <option key={x.id} value={x.id}>
                    {x.name}
                  </option>
                ))}
              </select>
              <button disabled={!profileId} onClick={applyProfile}>
                {t("Apply")}
              </button>
              <button
                className="ghost"
                disabled={!profileId}
                title={t("Delete the selected profile")}
                onClick={() => {
                  p.onDeleteAuthProfile?.(profileId);
                  setProfileId("");
                }}
              >
                🗑
              </button>
            </>
          )}
          {auth.type !== "none" && p.onSaveAuthProfile && (
            <button
              className="ghost"
              style={{ marginLeft: "auto" }}
              title={t("Save the current auth settings as a reusable profile")}
              onClick={p.onSaveAuthProfile}
            >
              💾 {t("Save as profile")}
            </button>
          )}
        </div>
      )}
      <div className="form-grid">
        <label>{t("Authorization type")}</label>
        <select
          value={auth.type}
          onChange={(e) => set({ type: e.target.value as AuthConfig["type"] })}
        >
          <option value="none">{t("No authorization")}</option>
          <option value="bearer">Bearer Token</option>
          <option value="basic">Basic Auth</option>
          <option value="oauth2">OAuth 2.0 / SSO</option>
        </select>
        {auth.type === "bearer" && (
          <>
            <label>{t("Token")}</label>
            <input
              value={auth.token}
              placeholder={t("{{token}} or a value")}
              onChange={(e) => set({ token: e.target.value })}
            />
          </>
        )}
        {auth.type === "basic" && (
          <>
            <label>{t("Username")}</label>
            <input
              value={auth.username}
              onChange={(e) => set({ username: e.target.value })}
            />
            <label>{t("Password")}</label>
            <input
              type="password"
              value={auth.password}
              onChange={(e) => set({ password: e.target.value })}
            />
          </>
        )}
      </div>

      {auth.type === "oauth2" && (
        <OAuth2Editor
          cfg={auth.oauth2}
          setOAuth={setOAuth}
          onFetchToken={p.onFetchToken}
          tokenUrls={p.tokenUrls}
        />
      )}
    </div>
  );
}

function OAuth2Editor({
  cfg,
  setOAuth,
  onFetchToken,
  tokenUrls,
}: {
  cfg: OAuth2Config;
  setOAuth: (patch: Partial<OAuth2Config>) => void;
  onFetchToken: () => Promise<string>;
  tokenUrls?: string[];
}) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const grant = cfg.grant;
  const needsAuthUrl = grant === "authorization_code";
  const needsUserPass = grant === "password";
  const needsRefresh = grant === "refresh_token";

  const doFetch = async () => {
    // Keep the scroll position — fetching the token re-renders the editor and
    // would otherwise jump the panel back to the top.
    const top = document.querySelector<HTMLElement>(".tab-body")?.scrollTop ?? 0;
    setBusy(true);
    setError(null);
    try {
      await onFetchToken();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
      requestAnimationFrame(() => {
        const s = document.querySelector<HTMLElement>(".tab-body");
        if (s) s.scrollTop = top;
      });
    }
  };

  const tokenState = (() => {
    if (!cfg.access_token) return null;
    if (!cfg.expires_at) return { text: t("Token obtained"), cls: "ok" };
    const left = Math.round((cfg.expires_at - Date.now()) / 1000);
    if (left <= 0) return { text: t("Token expired — refresh it"), cls: "bad" };
    const mins = Math.floor(left / 60);
    return {
      text:
        mins > 0
          ? tr2("Token active, expires in {mins} min", { mins })
          : tr2("Token active, expires in {secs} s", { secs: left }),
      cls: "ok",
    };
  })();

  return (
    <div className="oauth-box">
      <div className="form-grid">
        <label>Grant type</label>
        <select
          value={grant}
          onChange={(e) => setOAuth({ grant: e.target.value as OAuthGrant })}
        >
          {GRANTS.map((g) => (
            <option key={g.id} value={g.id}>
              {t(g.label)}
            </option>
          ))}
        </select>

        {needsAuthUrl && (
          <>
            <label>Authorization URL</label>
            <input
              value={cfg.auth_url}
              placeholder="https://idp.example.com/authorize"
              onChange={(e) => setOAuth({ auth_url: e.target.value })}
            />
          </>
        )}

        <label>Token URL</label>
        <input
          value={cfg.token_url}
          placeholder="https://idp.example.com/oauth/token"
          list="token-url-history"
          onChange={(e) => setOAuth({ token_url: e.target.value })}
        />
        <datalist id="token-url-history">
          {(tokenUrls ?? []).map((u) => (
            <option key={u} value={u} />
          ))}
        </datalist>

        <label>Client ID</label>
        <input
          value={cfg.client_id}
          onChange={(e) => setOAuth({ client_id: e.target.value })}
        />

        <label>Client Secret</label>
        <input
          type="password"
          value={cfg.client_secret}
          placeholder={needsAuthUrl ? t("(optional for PKCE)") : ""}
          onChange={(e) => setOAuth({ client_secret: e.target.value })}
        />

        <label>Scope</label>
        <input
          value={cfg.scope}
          placeholder="openid profile api.read"
          onChange={(e) => setOAuth({ scope: e.target.value })}
        />

        {needsUserPass && (
          <>
            <label>Username</label>
            <input
              value={cfg.username}
              onChange={(e) => setOAuth({ username: e.target.value })}
            />
            <label>Password</label>
            <input
              type="password"
              value={cfg.password}
              onChange={(e) => setOAuth({ password: e.target.value })}
            />
          </>
        )}

        {needsRefresh && (
          <>
            <label>Refresh Token</label>
            <input
              value={cfg.refresh_token}
              onChange={(e) => setOAuth({ refresh_token: e.target.value })}
            />
          </>
        )}

        <label>{t("Client authentication")}</label>
        <select
          value={cfg.client_auth}
          onChange={(e) =>
            setOAuth({ client_auth: e.target.value as OAuth2Config["client_auth"] })
          }
        >
          <option value="body">{t("In request body")}</option>
          <option value="basic">{t("Basic header")}</option>
        </select>
      </div>

      <label className="tls-toggle" style={{ marginTop: 14 }}>
        <input
          type="checkbox"
          checked={cfg.auto_refresh}
          onChange={(e) => setOAuth({ auto_refresh: e.target.checked })}
        />
        {t("Automatically refresh the token during load (on TTL expiry)")}
      </label>
      {cfg.auto_refresh && grant === "authorization_code" && (
        <div className="lt-hint" style={{ marginTop: 6 }}>
          {t("For browser login, auto-refresh is only possible if the server issued a refresh_token.")}
        </div>
      )}

      <div className="oauth-actions">
        <button className="primary" onClick={doFetch} disabled={busy}>
          {busy ? (
            <span className="spinner" />
          ) : grant === "authorization_code" ? (
            t("Sign in via browser")
          ) : (
            t("Get token")
          )}
        </button>
        {tokenState && (
          <span className={`token-state ${tokenState.cls}`}>{tokenState.text}</span>
        )}
      </div>

      {error && <div className="lt-error" style={{ marginTop: 10 }}>{error}</div>}

      {cfg.access_token && (
        <div className="token-preview">
          <div className="token-preview-head">
            <span>Access token</span>
            <button
              className="ghost"
              onClick={() => navigator.clipboard?.writeText(cfg.access_token)}
              title={t("Copy")}
            >
              {t("⧉ copy")}
            </button>
          </div>
          <code>{cfg.access_token.slice(0, 48)}…</code>
          <div className="lt-hint" style={{ marginTop: 8 }}>
            {t("The token is added to the")} <code>Authorization: Bearer</code> {t("header when sending and for all virtual users of the load test.")}
          </div>
        </div>
      )}
    </div>
  );
}
