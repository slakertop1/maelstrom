// Pure builder: turn the UI streams model (UiStream[] referencing collection
// requests) + the active environment into the StreamScenarioSpec the engine
// runs. Each step reuses buildRequest (auth/headers/multipart/TLS), then adds
// its extract rules. Kept out of App.tsx so it's unit-testable.
import { buildRequest, unresolvedVars, builtStrings } from "./requestBuilder";
import type {
  RequestConfig,
  Environment,
  UiStream,
  UiStreamStep,
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

/// Names extracted by steps strictly BEFORE `uptoIndex` in the chain — used to
/// keep buildRequest from statically baking an Environment value that
/// collides with a chain var (see requestBuilder.ts f2 comment). Order-scoped
/// (f2): a step only sees the vars a chain has actually extracted by the time
/// it runs, matching the runtime semantics streamsMissingVars (f3) already
/// uses. An env var whose name matches an extract name from a LATER step must
/// still resolve normally in earlier steps.
function chainExtractNamesBefore(steps: UiStreamStep[], uptoIndex: number): Set<string> {
  return new Set(
    steps
      .slice(0, uptoIndex)
      .flatMap((st) => st.extract.map((e) => e.name.trim()).filter(Boolean))
  );
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
        .map((st, i): StreamStepSpec | null => {
          const req = byId.get(st.requestId);
          if (!req) return null; // request deleted from the collection
          // f2: only exclude names the chain has extracted STRICTLY BEFORE
          // this step (index i), not every extract in the whole stream — an
          // env var whose name matches a LATER step's extract must still
          // resolve normally here.
          const chainVars = chainExtractNamesBefore(s.steps, i);
          const built = buildRequest(req, env, forExport, chainVars);
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
  // Within a stream, a chain runs its steps in order: a var is only available
  // to a step once an EARLIER step has extracted it, so step N's unresolved
  // vars must be checked against extract names from steps strictly before N —
  // not the whole stream — or an extract further down the chain would wrongly
  // silence a genuinely-missing var used higher up.
  const missing = new Set<string>();
  for (const s of uiStreams) {
    const extractedSoFar = new Set<string>();
    for (let i = 0; i < s.steps.length; i++) {
      const st = s.steps[i];
      const req = byId.get(st.requestId);
      if (req) {
        // f2: order-scoped, same exclusion buildStreamsScenario applies, so
        // this reflects the request that's actually sent (a chain-var
        // collision is only excluded from the Environment once an earlier
        // step has actually extracted it — see requestBuilder.ts f2 comment).
        const chainVars = chainExtractNamesBefore(s.steps, i);
        const strings = builtStrings(buildRequest(req, env, false, chainVars));
        for (const v of unresolvedVars(strings)) {
          if (!extractedSoFar.has(v)) missing.add(v);
        }
      }
      for (const e of st.extract) {
        const name = e.name.trim();
        if (name) extractedSoFar.add(name);
      }
    }
  }
  return [...missing];
}
