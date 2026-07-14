// Pure builder: turn the UI streams model (UiStream[] referencing collection
// requests) + the active environment into the StreamScenarioSpec the engine
// runs. Each step reuses buildRequest (auth/headers/multipart/TLS), then adds
// its extract rules. Kept out of App.tsx so it's unit-testable.
import { buildRequest, unresolvedVars, builtStrings } from "./requestBuilder";
import type {
  RequestConfig,
  Environment,
  UiStream,
  StreamSpec,
  StreamStepSpec,
  StreamScenarioSpec,
  DatasetSpec,
  FilePoolSpec,
  ExtractRule,
} from "./types";

/// A step references `{{$data.*}}` anywhere → the run needs the datasets.
function usesData(...strings: (string | null)[]): boolean {
  return strings.some((s) => (s ?? "").includes("{{$data."));
}

export function buildStreamsScenario(
  uiStreams: UiStream[],
  byId: Map<string, RequestConfig>,
  env: Environment | null,
  durationSecs: number,
  timeoutMs: number,
  datasets: DatasetSpec[],
  forExport = false
): StreamScenarioSpec {
  const filePools: FilePoolSpec[] = [];
  let dataUsed = false;

  const streams: StreamSpec[] = uiStreams
    .map((s): StreamSpec => {
      const steps: StreamStepSpec[] = s.steps
        .map((st): StreamStepSpec | null => {
          const req = byId.get(st.requestId);
          if (!req) return null; // request deleted from the collection
          const built = buildRequest(req, env, forExport);
          filePools.push(...built.file_pools);
          if (usesData(built.url, built.body, ...built.headers.flat())) dataUsed = true;
          const extract: ExtractRule[] = st.extract
            .filter((e) => e.name.trim() && e.expr.trim())
            .map((e) => ({ name: e.name.trim(), from: e.from, expr: e.expr.trim() }));
          return {
            name: req.name,
            method: built.method,
            url: built.url,
            headers: built.headers,
            body: built.body,
            tls: built.tls,
            multipart: built.multipart,
            extract,
          };
        })
        .filter((x): x is StreamStepSpec => x !== null);
      return { name: s.name, rps: s.rps, steps };
    })
    // Drop streams with no runnable steps (all requests deleted / empty).
    .filter((s) => s.steps.length > 0);

  return {
    duration_secs: durationSecs,
    timeout_ms: timeoutMs,
    streams,
    datasets: dataUsed ? datasets.filter((d) => d.name.trim()) : [],
    file_pools: filePools,
  };
}

/// Unresolved {{env vars}} across every step of every stream — for the same
/// preflight warning the single-send / scenario flows use.
export function streamsMissingVars(
  uiStreams: UiStream[],
  byId: Map<string, RequestConfig>,
  env: Environment | null
): string[] {
  // Extracted vars are chain-scoped (per stream) at runtime, so a var extracted
  // in stream A must NOT silence the same {{var}} used unextracted in stream B.
  // Check each stream against ITS OWN extracted names, then union the leftovers.
  const missing = new Set<string>();
  for (const s of uiStreams) {
    const strings: (string | null)[] = [];
    for (const st of s.steps) {
      const req = byId.get(st.requestId);
      if (req) strings.push(...builtStrings(buildRequest(req, env)));
    }
    const extracted = new Set(s.steps.flatMap((st) => st.extract.map((e) => e.name.trim())));
    for (const v of unresolvedVars(strings)) if (!extracted.has(v)) missing.add(v);
  }
  return [...missing];
}
