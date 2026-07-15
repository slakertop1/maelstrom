import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import {
  dbExecute,
  fetchOAuthToken,
  grpcCall,
  grpcStartLoad,
  loadState,
  oauthAuthorizationCode,
  readTextFile,
  saveState,
  sendRequest,
  startDbLoadTest,
  startLoadTest,
  startScenarioLoadTest,
  startStreamsLoadTest,
  stopLoadTest,
  wsCall,
  wsStartLoad,
  writeTextFile,
  logEvent,
} from "./api";
import { buildHttpScenario, buildGrpcScenario, buildWsScenario } from "./cliExport";
import { resolveVars, envVars } from "./vars";
import { buildRequest, buildAuthRefresh, unresolvedVars, builtStrings, toTlsSpec } from "./requestBuilder";
import { useT, tr, tr2, useLang } from "./i18n";
import { importOpenApi } from "./openapi";
import { buildReportHtml, buildScenarioReportHtml } from "./report";
import Sidebar from "./components/Sidebar";
import RequestEditor from "./components/RequestEditor";
import ResponseView from "./components/ResponseView";
import DbResultView from "./components/DbResultView";
import GrpcResultView from "./components/GrpcResultView";
import WsResultView from "./components/WsResultView";
import LoadTestPanel, { LoadTestConfig } from "./components/LoadTestPanel";
import ScenarioPanel, { ScenarioRunConfig, looksLikeDatasetTypo } from "./components/ScenarioPanel";
import StreamsPanel, { StreamRunConfig } from "./components/StreamsPanel";
import { buildStreamsScenario, streamsMissingVars } from "./streamsBuilder";
import EnvironmentModal from "./components/EnvironmentModal";
import DatasetsModal from "./components/DatasetsModal";
import LogModal from "./components/LogModal";
import ExportConfigModal from "./components/ExportConfigModal";
import {
  AuthProfile,
  authProfileName,
  migrateAuth,
  Collection,
  Dataset,
  DbResponse,
  Environment,
  toDatasetSpec,
  toFilePoolSpec,
  splitLines,
  FilePoolSpec,
  GrpcCallResult,
  WsCallResult,
  HttpResponseData,
  KV,
  LoadTestResult,
  MultipartPartSpec,
  PersistedState,
  ProgressSnapshot,
  OAuthRefreshSpec,
  RequestConfig,
  ScenarioProgress,
  ScenarioResult,
  StreamsProgress,
  StreamsResult,
  UiStream,
  ScenarioTargetSpec,
  TimelinePoint,
  TlsConfig,
  TlsSpec,
  migrateRequest,
  newRequest,
  uid,
} from "./types";

// `resolveVars` / `envVars` live in ./vars for unit testing. `activeVars` keeps
// the old call sites terse (runtime resolution uses local secret values).
const activeVars = (env: Environment | null) => envVars(env, false);

// buildRequest / buildAuthRefresh / unresolvedVars / builtStrings live in
// ./requestBuilder (pure + unit-tested).

// Slack absorbing the backend's second-only `started_at` precision (Rust's
// "%Y-%m-%d %H:%M:%S") against our millisecond `Date.now()` tracking marker —
// without it, a run that legitimately starts and finishes within the same
// wall-clock second would look "stale" to itself (see predatesTracking).
const STALE_TOLERANCE_MS = 2000;

/// A terminal `load_finished`/`scenario_finished`/`streams_finished` payload
/// whose own `started_at` clearly predates `sinceMs` (frontend `Date.now()`
/// captured when we started tracking the CURRENT run, minus the tolerance
/// above) belongs to an earlier, already-superseded run — see the a1 fix in
/// the events effect below. Events carry no run id, so this can't be exact;
/// it's a best-effort filter for the case a run is abandoned (Close) and a
/// new one starts before the old one's stop is confirmed.
function predatesTracking(sinceMs: number, startedAt: string): boolean {
  const t = Date.parse(startedAt.replace(" ", "T"));
  return !Number.isNaN(t) && t < sinceMs - STALE_TOLERANCE_MS;
}

function defaultCollections(): Collection[] {
  const example = newRequest(tr("Example: JSON API"));
  example.url = "https://jsonplaceholder.typicode.com/posts/1";
  return [{ id: uid(), name: tr("My collection"), requests: [example] }];
}

/// Cap for the LIVE progress series. Beyond this the series is downsampled
/// (every other point) so a long run doesn't rebuild an ever-growing SVG each
/// second. The finished report uses the backend's full-resolution timeline.
const MAX_LIVE_POINTS = 720;

export default function App() {
  const t = useT();
  const { lang, setLang } = useLang();
  const [collections, setCollections] = useState<Collection[]>([]);
  const [environments, setEnvironments] = useState<Environment[]>([]);
  const [activeEnvId, setActiveEnvId] = useState<string | null>(null);
  const [datasets, setDatasets] = useState<Dataset[]>([]);
  const [datasetsModalOpen, setDatasetsModalOpen] = useState(false);
  const [logModalOpen, setLogModalOpen] = useState(false);
  const [loaded, setLoaded] = useState(false);
  // Set when loading persisted state failed: we show defaults but must NOT
  // auto-save over the user's (possibly recoverable) file until they acknowledge.
  const [stateLoadError, setStateLoadError] = useState<string | null>(null);
  // Set when a debounced/flush save fails (a5) — the in-memory edits are fine,
  // but nothing actually reached disk, which is worth surfacing to the user.
  const [stateSaveError, setStateSaveError] = useState<string | null>(null);
  const [exportCfg, setExportCfg] = useState<{ json: string; defaultName: string } | null>(null);

  const [current, setCurrent] = useState<RequestConfig>(newRequest());
  // Mirrors `current.id` for async handlers (a2) that must ignore a result
  // meant for a request the user has since navigated away from.
  const currentIdRef = useRef(current.id);
  currentIdRef.current = current.id;
  const [sourceCollectionId, setSourceCollectionId] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);

  const [response, setResponse] = useState<HttpResponseData | null>(null);
  const [respError, setRespError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);

  const [envModalOpen, setEnvModalOpen] = useState(false);
  const [editorTab, setEditorTab] = useState("params");

  const [ltConfig, setLtConfig] = useState<LoadTestConfig>({
    vus: 20,
    durationSecs: 30,
    rpsLimit: "",
    timeoutMs: 10000,
  });
  const [ltRunning, setLtRunning] = useState(false);
  const [ltProgress, setLtProgress] = useState<ProgressSnapshot | null>(null);
  const [ltTimeline, setLtTimeline] = useState<TimelinePoint[]>([]);
  const [ltResult, setLtResult] = useState<LoadTestResult | null>(null);
  const [ltError, setLtError] = useState<string | null>(null);

  const [dbResult, setDbResult] = useState<DbResponse | null>(null);
  const [grpcResult, setGrpcResult] = useState<GrpcCallResult | null>(null);
  const [wsResult, setWsResult] = useState<WsCallResult | null>(null);

  const [tokenRefreshes, setTokenRefreshes] = useState(0);

  const [preflight, setPreflight] = useState<{
    missing: string[];
    action: string;
    proceed: () => void;
  } | null>(null);

  const [scenarioColId, setScenarioColId] = useState<string | null>(null);
  const [scRunning, setScRunning] = useState(false);
  const [scProgress, setScProgress] = useState<ScenarioProgress | null>(null);
  const [scProgressLog, setScProgressLog] = useState<ScenarioProgress[]>([]);
  const [scResult, setScResult] = useState<ScenarioResult | null>(null);
  const [scError, setScError] = useState<string | null>(null);
  // Streams (request chaining) panel.
  const [streamsColId, setStreamsColId] = useState<string | null>(null);
  const [stRunning, setStRunning] = useState(false);
  const [stProgress, setStProgress] = useState<StreamsProgress | null>(null);
  const [stResult, setStResult] = useState<StreamsResult | null>(null);
  const [stError, setStError] = useState<string | null>(null);

  // Недавние Token URL — общая коллекция подсказок для всех запросов.
  const [tokenUrls, setTokenUrls] = useState<string[]>([]);
  const [authProfiles, setAuthProfiles] = useState<AuthProfile[]>([]);

  // Memoized so its reference is stable across renders — otherwise every render
  // produces a new object and the useCallbacks that depend on it (send, load)
  // are rebuilt each time, defeating their memoization.
  const activeEnv = useMemo(
    () => environments.find((e) => e.id === activeEnvId) ?? null,
    [environments, activeEnvId]
  );

  // ---- initial load ----
  useEffect(() => {
    loadState()
      .then((state) => {
        if (state && Array.isArray(state.collections)) {
          const cols = state.collections.map((c) => ({
            ...c,
            requests: (c.requests ?? []).map(migrateRequest),
          }));
          setCollections(cols);
          setEnvironments(state.environments ?? []);
          setActiveEnvId(state.active_env_id ?? null);
          setDatasets(state.datasets ?? []);
          setTokenUrls(state.token_urls ?? []);
          // Profiles get the same backfill as requests: applying one written by
          // an older build must not inject missing oauth2 fields into the editor.
          setAuthProfiles(
            (state.auth_profiles ?? []).map((p) => ({ ...p, auth: migrateAuth(p.auth) }))
          );
          const first = cols[0]?.requests[0];
          if (first) {
            setCurrent(structuredClone(first));
            setSourceCollectionId(cols[0].id);
          }
        } else {
          const cols = defaultCollections();
          setCollections(cols);
          setCurrent(structuredClone(cols[0].requests[0]));
          setSourceCollectionId(cols[0].id);
        }
      })
      .catch((e) => {
        // Don't silently swallow: the user's collections could otherwise appear
        // to vanish. Show defaults but flag it so we don't overwrite their file.
        setCollections(defaultCollections());
        setStateLoadError(String(e));
      })
      .finally(() => setLoaded(true));
  }, []);

  // ---- persistence (debounced, flushed on close) ----
  const persistTimer = useRef<number | null>(null);
  const pendingSave = useRef<PersistedState | null>(null);
  useEffect(() => {
    // While a load error is unacknowledged, do NOT persist — writing the default
    // state now would overwrite the file we failed to read.
    if (!loaded || stateLoadError) return;
    const state: PersistedState = {
      collections,
      environments,
      active_env_id: activeEnvId,
      datasets,
      token_urls: tokenUrls,
      auth_profiles: authProfiles,
    };
    pendingSave.current = state;
    if (persistTimer.current) window.clearTimeout(persistTimer.current);
    persistTimer.current = window.setTimeout(() => {
      pendingSave.current = null;
      saveState(state)
        .then(() => setStateSaveError(null))
        .catch((e) => setStateSaveError(String(e)));
    }, 400);
  }, [collections, environments, activeEnvId, datasets, tokenUrls, authProfiles, loaded, stateLoadError]);

  // Closing the window inside the 400 ms debounce window must not lose the last
  // edit (e.g. "save profile → close app" is a natural one-shot flow).
  useEffect(() => {
    const flush = () => {
      const s = pendingSave.current;
      if (s) {
        pendingSave.current = null;
        saveState(s).catch((e) => setStateSaveError(String(e)));
      }
    };
    window.addEventListener("beforeunload", flush);
    return () => {
      window.removeEventListener("beforeunload", flush);
      flush();
    };
  }, []);

  // ---- load/scenario/streams run tracking (a1, pa1) ----
  // The backend serializes load test / scenario / streams runs on ONE global
  // slot (LoadTestState::try_start in loadtest.rs): a new run cannot start
  // until the previous one has released it, which only happens AFTER its own
  // *_finished/*_error was emitted. execute*() below refuses to start a new
  // run while `xxGenRef` is already non-zero (closing pa1's double-click
  // race). `xxGenRef` is 0 when nothing is tracked and non-zero while a run
  // is in flight; the listeners only accept events while it's set, and
  // open*()/close*() invalidate it so a run abandoned via Close doesn't land
  // on a reopened panel. Events carry no run id, so if Close lets a new run
  // start before the old one's stop is confirmed, both would look "tracked"
  // at once — `xxSinceRef` (wall-clock moment tracking began) plus
  // predatesTracking() catches that residual case for the terminal events,
  // where getting it wrong is worst (a1).
  const ltGenRef = useRef(0);
  const ltSinceRef = useRef(0);
  const scGenRef = useRef(0);
  const scSinceRef = useRef(0);
  const stGenRef = useRef(0);
  const stSinceRef = useRef(0);
  const nextRunGen = useRef(0);

  // ---- load test events ----
  useEffect(() => {
    const unsubs: (() => void)[] = [];
    // StrictMode/remount can run this effect's cleanup before a listen()
    // promise resolves; pushing the late unlisten into `unsubs` after cleanup
    // already ran loses it — the handler stays registered and, once the
    // effect re-runs, fires twice for every event (a3). `cancelled` catches
    // that: if cleanup already happened, unlisten immediately instead.
    let cancelled = false;
    const sub = <T,>(name: string, handler: (e: { payload: T }) => void) => {
      listen<T>(name, handler).then((u) => {
        if (cancelled) u();
        else unsubs.push(u);
      });
    };
    sub<ProgressSnapshot>("load_progress", (e) => {
      if (!ltGenRef.current) return; // no run tracked — stale (a1)
      setLtProgress(e.payload);
      // Bound the LIVE series so a long run stays O(cap) per tick instead of
      // O(n) (the whole SVG is rebuilt each second). The final report uses the
      // backend's full-resolution timeline, so nothing is lost there.
      setLtTimeline((t) => {
        const next = [...t, e.payload.point];
        return next.length > MAX_LIVE_POINTS ? next.filter((_, i) => i % 2 === 0) : next;
      });
    });
    sub<LoadTestResult>("load_finished", (e) => {
      if (!ltGenRef.current || predatesTracking(ltSinceRef.current, e.payload.started_at)) return;
      ltGenRef.current = 0;
      setLtResult(e.payload);
      setLtRunning(false);
    });
    sub<string>("load_error", (e) => {
      if (!ltGenRef.current) return;
      ltGenRef.current = 0;
      setLtError(e.payload);
      setLtRunning(false);
    });
    sub<ScenarioProgress>("scenario_progress", (e) => {
      if (!scGenRef.current) return;
      setScProgress(e.payload);
      setScProgressLog((log) => {
        const next = [...log, e.payload];
        return next.length > MAX_LIVE_POINTS ? next.filter((_, i) => i % 2 === 0) : next;
      });
    });
    sub<ScenarioResult>("scenario_finished", (e) => {
      if (!scGenRef.current || predatesTracking(scSinceRef.current, e.payload.started_at)) return;
      scGenRef.current = 0;
      setScResult(e.payload);
      setScRunning(false);
    });
    sub<string>("scenario_error", (e) => {
      if (!scGenRef.current) return;
      scGenRef.current = 0;
      setScError(e.payload);
      setScRunning(false);
    });
    sub<StreamsProgress>("streams_progress", (e) => {
      if (!stGenRef.current) return;
      setStProgress(e.payload);
    });
    sub<StreamsResult>("streams_finished", (e) => {
      if (!stGenRef.current || predatesTracking(stSinceRef.current, e.payload.started_at)) return;
      stGenRef.current = 0;
      setStResult(e.payload);
      setStRunning(false);
    });
    sub<string>("streams_error", (e) => {
      if (!stGenRef.current) return;
      stGenRef.current = 0;
      setStError(e.payload);
      setStRunning(false);
    });
    sub<number>("token_refreshed", (e) => {
      setTokenRefreshes(e.payload);
    });
    return () => {
      cancelled = true;
      unsubs.forEach((u) => u());
    };
  }, []);

  // ---- auth profiles (reusable auth setups) ----
  // a7 perf: these are useCallback-wrapped (stable references, deps trimmed to
  // what they actually read) so Sidebar/RequestEditor — wrapped in React.memo
  // below — don't re-render on every load-test progress tick just because App
  // re-rendered and would otherwise hand them a fresh function each time.
  const saveAuthProfile = useCallback(() => {
    if (current.auth.type === "none") return;
    const auth = structuredClone(current.auth);
    // A profile stores CREDENTIALS, not the volatile token: an access token
    // snapshotted now would be stale by the time it's applied and would
    // silently overwrite a freshly fetched one. refresh_token stays — it IS
    // a credential (the refresh_token grant runs on it).
    auth.oauth2.access_token = "";
    auth.oauth2.expires_at = null;
    const base = authProfileName(auth);
    // Name dedup inside the updater: a double-click must not create two
    // profiles with the same name off a stale closure array.
    setAuthProfiles((ps) => {
      let name = base;
      for (let i = 2; ps.some((p) => p.name === name); i++) name = `${base} (${i})`;
      return [...ps, { id: uid(), name, auth }];
    });
  }, [current]);
  const deleteAuthProfile = useCallback(
    (id: string) => setAuthProfiles((ps) => ps.filter((p) => p.id !== id)),
    []
  );

  // ---- request actions ----
  // saveRequest/saveCurrent are declared before openRequest so openRequest's
  // useCallback can depend on the stable saveRequest reference.
  const saveRequest = useCallback(
    (req: RequestConfig) => {
      let colId = sourceCollectionId;
      if (!colId || !collections.some((c) => c.id === colId)) {
        colId = collections[0]?.id ?? null;
        if (!colId) {
          const col: Collection = { id: uid(), name: t("My collection"), requests: [] };
          setCollections([{ ...col, requests: [structuredClone(req)] }]);
          setSourceCollectionId(col.id);
          setDirty(false);
          return;
        }
      }
      setCollections((cols) =>
        cols.map((c) => {
          if (c.id !== colId) return c;
          const exists = c.requests.some((r) => r.id === req.id);
          return {
            ...c,
            requests: exists
              ? c.requests.map((r) => (r.id === req.id ? structuredClone(req) : r))
              : [...c.requests, structuredClone(req)],
          };
        })
      );
      setSourceCollectionId(colId);
      setDirty(false);
    },
    [sourceCollectionId, collections, t]
  );
  const saveCurrent = useCallback(() => saveRequest(current), [current, saveRequest]);

  const openRequest = useCallback(
    (collectionId: string, req: RequestConfig) => {
      // Unsaved edits must survive switching requests: silently dropping them is
      // how auth settings "reset" (edit request B → peek at request A → return to
      // B and the changes are gone). Auto-save the current request first; when
      // re-opening the same request, keep the edited copy (the `req` object from
      // the sidebar render props is the stale pre-save version).
      if (dirty && current) saveRequest(current);
      const target = dirty && current && current.id === req.id ? current : req;
      setCurrent(structuredClone(target));
      setSourceCollectionId(collectionId);
      setDirty(false);
      setResponse(null);
      setRespError(null);
      setDbResult(null);
      setGrpcResult(null);
      setWsResult(null);
    },
    [dirty, current, saveRequest]
  );

  const changeCurrent = useCallback((req: RequestConfig) => {
    setCurrent(req);
    setDirty(true);
  }, []);

  const executeSend = useCallback(async () => {
    // Captured now (before any await) so a slow response can tell, on arrival,
    // whether the user is still looking at the request it was sent for — a
    // faster response for a request opened afterwards must not get clobbered
    // by this one landing late (a2, same idea as fetchToken below).
    const reqId = current.id;
    setSending(true);
    setResponse(null);
    setRespError(null);
    setDbResult(null);
    setGrpcResult(null);
    setWsResult(null);
    try {
      if (current.kind === "db") {
        const vars = activeVars(activeEnv);
        const url = resolveVars(current.db.url.trim(), vars);
        const query = resolveVars(current.db.query, vars);
        if (!url) throw t("Enter the database connection string");
        if (!query.trim()) throw t("Enter an SQL query");
        const res = await dbExecute({
          url,
          query,
          timeout_ms: 30000,
          username: resolveVars(current.db.username, vars),
          password: current.db.password,
        });
        if (currentIdRef.current === reqId) setDbResult(res);
      } else if (current.kind === "grpc") {
        const vars = activeVars(activeEnv);
        const g = current.grpc;
        if (!g.endpoint.trim()) throw t("Enter the gRPC server address");
        if (!g.service || !g.method) throw t("Select a method (load a .proto)");
        const res = await grpcCall({
          endpoint: resolveVars(g.endpoint.trim(), vars),
          proto_path: g.proto_path,
          includes: splitLines(g.import_paths),
          service: g.service,
          method: g.method,
          body: resolveVars(g.body, vars),
          timeout_ms: 30000,
          tls: toTlsSpec(g.tls),
        });
        if (currentIdRef.current === reqId) setGrpcResult(res);
      } else if (current.kind === "ws") {
        const vars = activeVars(activeEnv);
        const w = current.ws;
        if (!w.url.trim()) throw t("Enter a ws:// address");
        const res = await wsCall({
          url: resolveVars(w.url.trim(), vars),
          message: resolveVars(w.message, vars),
          timeout_ms: 5000,
        });
        if (currentIdRef.current === reqId) setWsResult(res);
      } else {
        if (!current.url.trim()) return;
        const built = buildRequest(current, activeEnv);
        // Attach datasets only when the request references {{$data.*}}, so a
        // normal request doesn't trigger a DB/file fetch on every send.
        const usesData = builtStrings(built).some((s) => !!s && s.includes("{{$data."));
        const resp = await sendRequest({
          ...built,
          timeout_ms: 30000,
          datasets: usesData ? datasets.filter((d) => d.name.trim()).map(toDatasetSpec) : [],
        });
        if (currentIdRef.current === reqId) setResponse(resp);
      }
    } catch (e) {
      if (currentIdRef.current === reqId) setRespError(String(e));
    } finally {
      // a2: `sending` is a single GLOBAL UI flag (it gates the Send/Run
      // button), so it must always be cleared here regardless of which
      // request is currently open — otherwise switching requests mid-flight
      // leaves the button stuck disabled forever, since currentIdRef would
      // never match `reqId` again. Only the RESULT setters above are gated on
      // the id match (so a late response can't clobber a newer request).
      setSending(false);
    }
  }, [current, activeEnv, datasets]);

  // Warn before sending if the request still has unset {{variables}}.
  const doSend = useCallback(() => {
    let missing: string[];
    if (current.kind === "db") {
      const vars = activeVars(activeEnv);
      missing = unresolvedVars([
        resolveVars(current.db.url, vars),
        resolveVars(current.db.query, vars),
      ]);
    } else {
      if (!current.url.trim()) return;
      missing = unresolvedVars(builtStrings(buildRequest(current, activeEnv)));
    }
    if (missing.length) {
      setPreflight({ missing, action: t("send the request"), proceed: executeSend });
      return;
    }
    executeSend();
  }, [current, activeEnv, executeSend]);

  // ---- OAuth2 token acquisition ----
  const fetchToken = useCallback(async (): Promise<string> => {
    const cfg = current.auth.oauth2;
    const vars = activeVars(activeEnv);
    const resolved = {
      ...cfg,
      token_url: resolveVars(cfg.token_url, vars),
      auth_url: resolveVars(cfg.auth_url, vars),
      client_id: resolveVars(cfg.client_id, vars),
      client_secret: resolveVars(cfg.client_secret, vars),
      scope: resolveVars(cfg.scope, vars),
      username: resolveVars(cfg.username, vars),
      password: resolveVars(cfg.password, vars),
    };
    if (!resolved.token_url.trim()) {
      throw t("Token URL is not set (the token endpoint).");
    }
    const missing = unresolvedVars(
      cfg.grant === "authorization_code"
        ? [resolved.token_url, resolved.auth_url]
        : [resolved.token_url]
    );
    if (missing.length) {
      throw `${t("Unset variables in the OAuth URLs:")} ${missing
        .map((v) => `{{${v}}}`)
        .join(", ")} — ${t("set them in Environments (button at the top).")}`;
    }
    // The request the fetch was started FOR — the SSO round-trip can take a
    // minute, during which the user may edit fields or switch requests. Merging
    // the whole stale `current` back (as before) silently reverted those edits.
    const reqId = current.id;
    const resp =
      cfg.grant === "authorization_code"
        ? await oauthAuthorizationCode(resolved)
        : await fetchOAuthToken(resolved);
    const expires_at = resp.expires_in ? Date.now() + resp.expires_in * 1000 : null;
    const mergeToken = (r: RequestConfig): RequestConfig => ({
      ...r,
      auth: {
        ...r.auth,
        oauth2: {
          ...r.auth.oauth2,
          access_token: resp.access_token,
          refresh_token: resp.refresh_token ?? r.auth.oauth2.refresh_token,
          expires_at,
        },
      },
    });
    // Merge ONLY the token fields into the editor — and only if the same
    // request is still open (functional update: no stale snapshot).
    setCurrent((cur) => (cur.id === reqId ? mergeToken(cur) : cur));
    // Persist the token into the collection copy by id, wherever it lives now.
    setCollections((cols) =>
      cols.map((c) => ({
        ...c,
        requests: c.requests.map((r) => (r.id === reqId ? mergeToken(r) : r)),
      }))
    );
    // …а Token URL попадает в общую коллекцию подсказок (без секретов).
    const rawUrl = cfg.token_url.trim();
    if (rawUrl) {
      setTokenUrls((h) => [rawUrl, ...h.filter((u) => u !== rawUrl)].slice(0, 15));
    }
    return resp.access_token;
  }, [current, activeEnv, t]);

  // ---- load test actions ----
  const executeLoadTest = useCallback(async () => {
    // Synchronous guard: a second click before this render commits still sees
    // the stale (pre-update) `ltRunning` from the closure, so without gating
    // on a ref here a fast double-click starts two overlapping runs (pa1).
    // The same ref fences stale *_progress/*_finished/*_error from a run
    // that's since been superseded (a1) — see the listeners above.
    if (ltGenRef.current) return;
    ltGenRef.current = ++nextRunGen.current;
    ltSinceRef.current = Date.now();
    setLtRunning(true);
    setLtError(null);
    setLtResult(null);
    setLtProgress(null);
    setLtTimeline([]);
    setTokenRefreshes(0);
    const rps_limit = ltConfig.rpsLimit === "" ? null : ltConfig.rpsLimit;
    try {
      if (current.kind === "db") {
        const vars = activeVars(activeEnv);
        await startDbLoadTest({
          url: resolveVars(current.db.url.trim(), vars),
          query: resolveVars(current.db.query, vars),
          vus: ltConfig.vus,
          duration_secs: ltConfig.durationSecs,
          rps_limit,
          timeout_ms: ltConfig.timeoutMs,
          username: resolveVars(current.db.username, vars),
          password: current.db.password,
        });
      } else if (current.kind === "grpc") {
        const vars = activeVars(activeEnv);
        const g = current.grpc;
        await grpcStartLoad({
          endpoint: resolveVars(g.endpoint.trim(), vars),
          proto_path: g.proto_path,
          includes: splitLines(g.import_paths),
          service: g.service,
          method: g.method,
          body: resolveVars(g.body, vars),
          vus: ltConfig.vus,
          duration_secs: ltConfig.durationSecs,
          rps_limit,
          timeout_ms: ltConfig.timeoutMs,
          tls: toTlsSpec(g.tls),
        });
      } else if (current.kind === "ws") {
        const vars = activeVars(activeEnv);
        const w = current.ws;
        await wsStartLoad({
          url: resolveVars(w.url.trim(), vars),
          message: resolveVars(w.message, vars),
          vus: ltConfig.vus,
          duration_secs: ltConfig.durationSecs,
          rps_limit,
          timeout_ms: ltConfig.timeoutMs,
        });
      } else {
        const built = buildRequest(current, activeEnv);
        // Only ship datasets when the request references {{$data.*}} — otherwise
        // every load run would needlessly execute the dataset's DB SQL / S3 fetch.
        const usesData = builtStrings(built).some((s) => !!s && s.includes("{{$data."));
        await startLoadTest({
          method: built.method,
          url: built.url,
          headers: built.headers,
          body: built.body,
          tls: built.tls,
          vus: ltConfig.vus,
          duration_secs: ltConfig.durationSecs,
          rps_limit,
          timeout_ms: ltConfig.timeoutMs,
          auth_refresh: buildAuthRefresh(current, activeEnv),
          multipart: built.multipart,
          datasets: usesData ? datasets.filter((d) => d.name.trim()).map(toDatasetSpec) : [],
          file_pools: built.file_pools,
        });
      }
    } catch (e) {
      ltGenRef.current = 0; // roll back — no run actually started (pa1)
      setLtRunning(false);
      setLtError(String(e));
    }
  }, [ltConfig, current, activeEnv, datasets]);

  const doStartLoadTest = useCallback(() => {
    let missing: string[];
    if (current.kind === "db") {
      const vars = activeVars(activeEnv);
      missing = unresolvedVars([
        resolveVars(current.db.url, vars),
        resolveVars(current.db.query, vars),
      ]);
    } else if (current.kind === "grpc") {
      const vars = activeVars(activeEnv);
      missing = unresolvedVars([
        resolveVars(current.grpc.endpoint, vars),
        resolveVars(current.grpc.body, vars),
      ]);
    } else if (current.kind === "ws") {
      const vars = activeVars(activeEnv);
      missing = unresolvedVars([
        resolveVars(current.ws.url, vars),
        resolveVars(current.ws.message, vars),
      ]);
    } else {
      missing = unresolvedVars(builtStrings(buildRequest(current, activeEnv)));
    }
    if (missing.length) {
      setPreflight({ missing, action: t("run the load test"), proceed: executeLoadTest });
      return;
    }
    executeLoadTest();
  }, [current, activeEnv, t, executeLoadTest]);

  const doStopLoadTest = useCallback(() => {
    stopLoadTest().catch(() => {});
  }, []);

  // ---- scenario (multi-endpoint) load ----
  const openScenario = useCallback((collectionId: string) => {
    // Switching collections (the sidebar's ⚡ icon calls this directly,
    // without going through closeScenario) while a previous run's stop
    // hasn't been confirmed must not let its stale events land on THIS view
    // — send the stop signal if we hadn't already, and stop tracking it (a1).
    if (scRunning) stopLoadTest().catch(() => {});
    scGenRef.current = 0;
    setScRunning(false);
    setScenarioColId(collectionId);
    setScResult(null);
    setScProgress(null);
    setScProgressLog([]);
    setScError(null);
  }, [scRunning]);

  const closeScenario = () => {
    if (scRunning) stopLoadTest().catch(() => {});
    // Invalidate tracking so a *_progress/*_finished still in flight for this
    // (now-hidden) run is dropped by the listeners above instead of landing
    // on whatever's opened next (a1); also unblocks Run immediately rather
    // than waiting on a stop confirmation nobody will see.
    scGenRef.current = 0;
    setScRunning(false);
    setScenarioColId(null);
  };

  const scenarioCollection = collections.find((c) => c.id === scenarioColId) ?? null;

  const executeScenario = async (
    targets: ScenarioTargetSpec[],
    filePools: FilePoolSpec[],
    config: ScenarioRunConfig
  ) => {
    if (scGenRef.current) return; // already starting/running (pa1)
    scGenRef.current = ++nextRunGen.current;
    scSinceRef.current = Date.now();
    setScRunning(true);
    setScError(null);
    setScResult(null);
    setScProgress(null);
    setScProgressLog([]);
    setTokenRefreshes(0);
    try {
      // Ship datasets only when some target references {{$data.*}} — otherwise a
      // scenario run would needlessly execute the dataset's DB SQL / S3 fetch.
      const usesData = targets.some((tg) =>
        [tg.url, tg.body ?? "", ...tg.headers.flat()].some((s) => s.includes("{{$data."))
      );
      await startScenarioLoadTest({
        duration_secs: config.durationSecs,
        timeout_ms: config.timeoutMs,
        targets,
        datasets: usesData ? datasets.filter((d) => d.name.trim()).map(toDatasetSpec) : [],
        file_pools: filePools,
      });
    } catch (e) {
      scGenRef.current = 0;
      setScRunning(false);
      setScError(String(e));
    }
  };

  // Unresolved {{vars}} of one request in the active environment — the scenario
  // panel shows this inline as soon as an endpoint is checked (not only at Run).
  const scenarioMissingVars = useCallback(
    (r: RequestConfig) => unresolvedVars(builtStrings(buildRequest(r, activeEnv))),
    [activeEnv]
  );

  const startScenario = async (config: ScenarioRunConfig) => {
    if (!scenarioCollection) return;
    const { targets, filePools } = buildScenarioTargets(config);
    const missing = unresolvedVars(
      targets.flatMap((t) => [t.url, t.body, ...t.headers.flatMap(([k, v]) => [k, v])])
    );
    if (missing.length) {
      setPreflight({
        missing,
        action: t("run the service load test"),
        proceed: () => executeScenario(targets, filePools, config),
      });
      return;
    }
    executeScenario(targets, filePools, config);
  };

  // ---- streams (request chaining) load ----
  const openStreams = useCallback((collectionId: string) => {
    // See openScenario above — same reasoning (a1).
    if (stRunning) stopLoadTest().catch(() => {});
    stGenRef.current = 0;
    setStRunning(false);
    setStreamsColId(collectionId);
    setStResult(null);
    setStProgress(null);
    setStError(null);
  }, [stRunning]);
  const closeStreams = () => {
    if (stRunning) stopLoadTest().catch(() => {});
    // See closeScenario above — same reasoning (a1).
    stGenRef.current = 0;
    setStRunning(false);
    setStreamsColId(null);
  };
  const streamsCollection = collections.find((c) => c.id === streamsColId) ?? null;

  const streamsMissingVarsCb = useCallback(
    (uiStreams: UiStream[]) => {
      if (!streamsCollection) return [];
      const byId = new Map(streamsCollection.requests.map((r) => [r.id, r]));
      return streamsMissingVars(uiStreams, byId, activeEnv);
    },
    [streamsCollection, activeEnv]
  );

  const executeStreams = async (config: StreamRunConfig) => {
    if (!streamsCollection) return;
    if (stGenRef.current) return; // already starting/running (pa1)
    stGenRef.current = ++nextRunGen.current;
    stSinceRef.current = Date.now();
    setStRunning(true);
    setStError(null);
    setStResult(null);
    setStProgress(null);
    try {
      const byId = new Map(streamsCollection.requests.map((r) => [r.id, r]));
      const spec = buildStreamsScenario(
        config.streams,
        byId,
        activeEnv,
        config.durationSecs,
        config.timeoutMs,
        datasets.filter((d) => d.name.trim()).map(toDatasetSpec)
      );
      if (spec.streams.length === 0) {
        stGenRef.current = 0;
        setStRunning(false);
        setStError(t("No runnable streams (every step's request is missing)."));
        return;
      }
      await startStreamsLoadTest(spec);
    } catch (e) {
      stGenRef.current = 0;
      setStRunning(false);
      setStError(String(e));
    }
  };

  const startStreams = (config: StreamRunConfig) => {
    const missing = streamsMissingVarsCb(config.streams);
    if (missing.length) {
      setPreflight({
        missing,
        action: t("run the chained load test"),
        proceed: () => executeStreams(config),
      });
      return;
    }
    executeStreams(config);
  };

  const buildScenarioTargets = (
    config: ScenarioRunConfig,
    forExport = false
  ): { targets: ScenarioTargetSpec[]; filePools: FilePoolSpec[] } => {
    if (!scenarioCollection) return { targets: [], filePools: [] };
    const byId = new Map(scenarioCollection.requests.map((r) => [r.id, r]));
    const targets: ScenarioTargetSpec[] = [];
    const filePools: FilePoolSpec[] = [];
    for (const item of config.items) {
      const req = byId.get(item.requestId);
      if (!req) continue;
      const built = buildRequest(req, activeEnv, forExport);
      filePools.push(...built.file_pools);
      targets.push({
        name: req.name,
        method: built.method,
        url: built.url,
        headers: built.headers,
        body: built.body,
        rps: item.rps,
        tls: built.tls,
        auth_refresh: buildAuthRefresh(req, activeEnv, forExport),
        multipart: built.multipart,
      });
    }
    return { targets, filePools };
  };

  const exportScenarioConfig = async (config: ScenarioRunConfig) => {
    if (!scenarioCollection) return;
    // forExport: secret env vars become ${KEY} so the pipeline injects them.
    const { targets, filePools } = buildScenarioTargets(config, true);
    const cfg = {
      name: scenarioCollection.name,
      duration_secs: config.durationSecs,
      timeout_ms: config.timeoutMs,
      targets,
      datasets: datasets.filter((d) => d.name.trim()).map(toDatasetSpec),
      file_pools: filePools,
      // f6: keep in sync with DEFAULT_MAX_ERROR_RATE_PCT in cliExport.ts —
      // the single-request CLI export uses that constant, and the
      // scenario export must use the same default gate.
      thresholds: { max_error_rate: 5.0, max_p95_ms: 500 },
    };
    const safeName = scenarioCollection.name.replace(/[^\wа-яА-Я.-]+/g, "-").toLowerCase();
    // Open the export dialog: it lists the ${ENV} vars the config needs, lets the
    // user rename them, then saves the file.
    setExportCfg({
      json: JSON.stringify(cfg, null, 2),
      defaultName: `maelstrom-${safeName || "scenario"}.json`,
    });
  };

  const saveExportedConfig = async (finalJson: string, defName: string) => {
    try {
      const path = await save({
        defaultPath: defName,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!path) return;
      await writeTextFile(path, finalJson);
    } catch (e) {
      alert(`${t("Failed to save the file:")}\n${String(e)}`);
    }
  };

  // Export the CURRENT single request (from the Load tab) as a CLI scenario.json.
  const exportSingleRequestConfig = useCallback(() => {
    if (current.kind === "db") {
      alert(t("The headless CLI doesn't run database load tests — export is available for HTTP, gRPC and WebSocket."));
      return;
    }
    const name = current.name || "load";
    const vExport = envVars(activeEnv, true); // secrets -> ${KEY}
    const r = (s: string) => resolveVars(s, vExport);
    let cfg: Record<string, unknown>;
    if (current.kind === "grpc") {
      const g = current.grpc;
      cfg = buildGrpcScenario(name, ltConfig, {
        endpoint: r(g.endpoint.trim()),
        proto_path: g.proto_path,
        includes: splitLines(g.import_paths),
        service: g.service,
        method: g.method,
        body: r(g.body),
        tls: toTlsSpec(g.tls),
      });
    } else if (current.kind === "ws") {
      const w = current.ws;
      cfg = buildWsScenario(name, ltConfig, { url: r(w.url.trim()), message: r(w.message) });
    } else {
      const built = buildRequest(current, activeEnv, true);
      const usesData = [built.url, built.body ?? "", ...built.headers.flat()].some((s) =>
        s.includes("{{$data.")
      );
      cfg = buildHttpScenario(name, ltConfig, {
        method: built.method,
        url: built.url,
        headers: built.headers,
        body: built.body,
        tls: built.tls,
        multipart: built.multipart,
        authRefresh: buildAuthRefresh(current, activeEnv, true),
        datasets: usesData ? datasets.filter((d) => d.name.trim()).map(toDatasetSpec) : [],
        filePools: built.file_pools,
      });
    }
    const safeName = name.replace(/[^\wа-яА-Я.-]+/g, "-").toLowerCase();
    setExportCfg({
      json: JSON.stringify(cfg, null, 2),
      defaultName: `maelstrom-${safeName || "load"}.json`,
    });
  }, [current, t, ltConfig, activeEnv, datasets]);

  const exportScenarioReport = async () => {
    if (!scResult) return;
    try {
      const stamp = scResult.started_at.replace(/[: ]/g, "-");
      const path = await save({
        defaultPath: `scenario-${stamp}.html`,
        filters: [{ name: t("HTML report"), extensions: ["html"] }],
      });
      if (!path) return;
      await writeTextFile(path, buildScenarioReportHtml(scResult));
    } catch (e) {
      alert(`${t("Failed to save the file:")}\n${String(e)}`);
    }
  };

  const exportReport = useCallback(async (kind: "html" | "json") => {
    if (!ltResult) return;
    try {
      const stamp = ltResult.started_at.replace(/[: ]/g, "-");
      const path = await save({
        defaultPath: `loadtest-${stamp}.${kind}`,
        filters: [
          kind === "html"
            ? { name: t("HTML report"), extensions: ["html"] }
            : { name: "JSON", extensions: ["json"] },
        ],
      });
      if (!path) return;
      const contents =
        kind === "html" ? buildReportHtml(ltResult) : JSON.stringify(ltResult, null, 2);
      await writeTextFile(path, contents);
    } catch (e) {
      alert(`${t("Failed to save the file:")}\n${String(e)}`);
    }
  }, [ltResult, t]);

  const importSpec = useCallback(async () => {
    const path = await open({
      multiple: false,
      filters: [
        { name: "OpenAPI / Swagger", extensions: ["json", "yaml", "yml"] },
        { name: t("All files"), extensions: ["*"] },
      ],
    });
    if (typeof path !== "string") return;
    try {
      const text = await readTextFile(path);
      const result = importOpenApi(text);
      setCollections((cols) => [...cols, result.collection]);
      logEvent(
        "IMPORT",
        `${result.serviceName}: ${result.operationCount} ${t("endpoints")}${
          result.warnings.length ? ` | ${t("warnings")}: ${result.warnings.join(" ")}` : ""
        }`
      ).catch(() => {});
      const first = result.collection.requests[0];
      if (first) openRequest(result.collection.id, first);
      const warn = result.warnings.length ? `\n\n⚠ ${result.warnings.join("\n")}` : "";
      alert(
        tr2("Imported service “{name}”: {count} requests.", {
          name: result.serviceName,
          count: result.operationCount,
        }) + warn
      );
    } catch (e) {
      alert(`${t("Failed to import the specification:")}\n${String(e)}`);
    }
  }, [openRequest, t]);

  // ---- sidebar actions ----
  const addCollection = useCallback(
    () =>
      setCollections((c) => [
        ...c,
        { id: uid(), name: tr2("Collection {n}", { n: c.length + 1 }), requests: [] },
      ]),
    []
  );

  const addRequest = useCallback(
    (collectionId: string) => {
      const req = newRequest();
      setCollections((cols) =>
        cols.map((c) =>
          c.id === collectionId ? { ...c, requests: [...c.requests, req] } : c
        )
      );
      openRequest(collectionId, req);
    },
    [openRequest]
  );

  const deleteRequest = useCallback(
    (collectionId: string, requestId: string) =>
      setCollections((cols) =>
        cols.map((c) =>
          c.id === collectionId
            ? { ...c, requests: c.requests.filter((r) => r.id !== requestId) }
            : c
        )
      ),
    []
  );

  const deleteCollection = useCallback(
    (collectionId: string) =>
      setCollections((cols) => cols.filter((c) => c.id !== collectionId)),
    []
  );

  const renameCollection = useCallback(
    (collectionId: string, name: string) =>
      setCollections((cols) =>
        cols.map((c) => (c.id === collectionId ? { ...c, name } : c))
      ),
    []
  );

  // a7 perf: the load-test panel embeds live progress/timeline state that
  // ticks ~1/s while a run is active. Building this element inline in the
  // JSX below would hand RequestEditor (React.memo'd) a brand-new `loadTestPanel`
  // ReactNode on every App render — defeating its memoization even when
  // nothing load-test-related changed (e.g. editing the sidebar, switching
  // environments). Memoizing it here means RequestEditor only re-renders for
  // this prop when one of the load-test values below actually changes.
  const loadTestPanel = useMemo(
    () => (
      <LoadTestPanel
        running={ltRunning}
        progress={ltProgress}
        timeline={ltTimeline}
        result={ltResult}
        error={ltError}
        config={ltConfig}
        setConfig={setLtConfig}
        onStart={doStartLoadTest}
        onStop={doStopLoadTest}
        onExportHtml={() => exportReport("html")}
        onExportJson={() => exportReport("json")}
        onExportConfig={exportSingleRequestConfig}
        tokenRefreshes={tokenRefreshes}
      />
    ),
    [
      ltRunning,
      ltProgress,
      ltTimeline,
      ltResult,
      ltError,
      ltConfig,
      doStartLoadTest,
      doStopLoadTest,
      exportReport,
      exportSingleRequestConfig,
      tokenRefreshes,
    ]
  );

  return (
    <div className="app">
      {stateLoadError && (
        <div
          className="state-load-error"
          style={{
            padding: "10px 16px",
            background: "rgba(240, 173, 78, 0.15)",
            borderBottom: "1px solid rgba(240, 173, 78, 0.5)",
            display: "flex",
            alignItems: "center",
            gap: 12,
          }}
        >
          <span style={{ flex: 1 }}>
            ⚠{" "}
            {t(
              "Couldn't load your saved data, so defaults are shown. Your saved file is left untouched — nothing is overwritten until you continue."
            )}
          </span>
          <button onClick={() => setStateLoadError(null)}>
            {t("Continue with defaults")}
          </button>
        </div>
      )}
      {stateSaveError && (
        <div
          className="state-load-error"
          style={{
            padding: "10px 16px",
            background: "rgba(248, 81, 73, 0.15)",
            borderBottom: "1px solid rgba(248, 81, 73, 0.5)",
            display: "flex",
            alignItems: "center",
            gap: 12,
          }}
        >
          <span style={{ flex: 1 }}>
            ⚠ {tr2("Couldn't save your last change: {error}", { error: stateSaveError })}
          </span>
          <button onClick={() => setStateSaveError(null)}>{t("Close")}</button>
        </div>
      )}
      <div className="topbar">
        <span className="logo">
          <span className="accent">⚡ Mael</span>strom
        </span>
        <span className="spacer" />
        <select
          className="env-select"
          value={activeEnvId ?? ""}
          onChange={(e) => setActiveEnvId(e.target.value || null)}
          title={t("Active environment: its {{...}} variables are substituted into requests")}
        >
          <option value="">{t("No environment")}</option>
          {environments.map((env) => (
            <option key={env.id} value={env.id}>
              {env.name}
            </option>
          ))}
        </select>
        <button
          onClick={() => setEnvModalOpen(true)}
          title={t("Environments and {{...}} variables: base URLs, tokens, secrets (dev/stage/prod)")}
        >
          {t("Environments")}
        </button>
        <button
          onClick={() => setDatasetsModalOpen(true)}
          title={t("Data sets for {{$data.name.column}} substitution under load (CSV/JSON/S3/DB)")}
        >
          {t("Data")}
        </button>
        <button
          onClick={() => setLogModalOpen(true)}
          title={t("Request and load log (secrets masked) — for debugging")}
        >
          {t("Logs")}
        </button>
        <select
          className="env-select"
          value={lang}
          onChange={(e) => setLang(e.target.value as "en" | "ru")}
          title={t("Language")}
        >
          <option value="en">EN</option>
          <option value="ru">RU</option>
        </select>
      </div>
      <div className="main">
        <Sidebar
          collections={collections}
          activeRequestId={current.id}
          onOpen={openRequest}
          onAddCollection={addCollection}
          onImport={importSpec}
          onAddRequest={addRequest}
          onDeleteRequest={deleteRequest}
          onDeleteCollection={deleteCollection}
          onRenameCollection={renameCollection}
          onLoadService={openScenario}
          onStreams={openStreams}
        />
        <div className="workspace">
          <div className="split">
            <div
              className="editor-pane"
              style={editorTab === "load" ? { flex: "1 1 100%" } : undefined}
            >
              <RequestEditor
                request={current}
                dirty={dirty}
                sending={sending}
                onChange={changeCurrent}
                onSend={doSend}
                onSave={saveCurrent}
                onFetchToken={fetchToken}
                tokenUrls={tokenUrls}
                authProfiles={authProfiles}
                onSaveAuthProfile={saveAuthProfile}
                onDeleteAuthProfile={deleteAuthProfile}
                onTabChange={setEditorTab}
                loadTestRunning={ltRunning}
                loadTestPanel={loadTestPanel}
              />
            </div>
            {editorTab !== "load" &&
              (current.kind === "db" ? (
                <DbResultView result={dbResult} error={respError} sending={sending} />
              ) : current.kind === "grpc" ? (
                <GrpcResultView result={grpcResult} error={respError} sending={sending} />
              ) : current.kind === "ws" ? (
                <WsResultView result={wsResult} error={respError} sending={sending} />
              ) : (
                <ResponseView
                  response={response}
                  error={respError}
                  sending={sending}
                  assertions={current.assertions}
                />
              ))}
          </div>
        </div>
      </div>
      {scenarioCollection && (
        <ScenarioPanel
          collection={scenarioCollection}
          running={scRunning}
          progress={scProgress}
          progressLog={scProgressLog}
          result={scResult}
          error={scError}
          onStart={startScenario}
          onStop={doStopLoadTest}
          onExportHtml={exportScenarioReport}
          onExportConfig={exportScenarioConfig}
          onClose={closeScenario}
          tokenRefreshes={tokenRefreshes}
          missingVars={scenarioMissingVars}
        />
      )}
      {streamsCollection && (
        <StreamsPanel
          collection={streamsCollection}
          running={stRunning}
          progress={stProgress}
          result={stResult}
          error={stError}
          onStart={startStreams}
          onStop={doStopLoadTest}
          onClose={closeStreams}
          missingVars={streamsMissingVarsCb}
        />
      )}
      {datasetsModalOpen && (
        <DatasetsModal
          datasets={datasets}
          onChange={setDatasets}
          onClose={() => setDatasetsModalOpen(false)}
        />
      )}
      {logModalOpen && <LogModal onClose={() => setLogModalOpen(false)} />}
      {exportCfg && (
        <ExportConfigModal
          json={exportCfg.json}
          defaultName={exportCfg.defaultName}
          onSave={saveExportedConfig}
          onClose={() => setExportCfg(null)}
        />
      )}
      {preflight && (
        <div className="modal-overlay" onClick={() => setPreflight(null)}>
          <div className="modal warn-modal" onClick={(e) => e.stopPropagation()} style={{ width: 460 }}>
            <div className="modal-head">
              <span>⚠ {t("Unset variables")}</span>
              <button className="ghost" onClick={() => setPreflight(null)}>
                ✕
              </button>
            </div>
            <div className="modal-body">
              <p style={{ marginBottom: 10 }}>
                {tr2(
                  "The request still has unresolved variables — set them in an environment, otherwise you will {action} with invalid values:",
                  { action: preflight.action }
                )}
              </p>
              <div className="warn-vars">
                {preflight.missing.map((v) => (
                  <code key={v}>{`{{${v}}}`}</code>
                ))}
              </div>
              {preflight.missing.some(looksLikeDatasetTypo) && (
                <p className="lt-hint" style={{ marginTop: 8 }}>
                  {t("Looks like a dataset reference — the syntax is")}{" "}
                  <code>{"{{$" + "data.name.column}}"}</code> ({t("note the $")})
                </p>
              )}
              <div className="warn-actions">
                <button onClick={() => setPreflight(null)}>{t("Cancel")}</button>
                <button
                  onClick={() => {
                    setPreflight(null);
                    setEnvModalOpen(true);
                  }}
                >
                  {t("Open environments")}
                </button>
                <button
                  className="primary"
                  onClick={() => {
                    const p = preflight;
                    setPreflight(null);
                    p.proceed();
                  }}
                >
                  {t("Anyway")}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}
      {envModalOpen && (
        <EnvironmentModal
          environments={environments}
          onChange={setEnvironments}
          onClose={() => setEnvModalOpen(false)}
        />
      )}
    </div>
  );
}
