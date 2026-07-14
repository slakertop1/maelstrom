import { Environment } from "./types";

/// Replace {{name}} references with values from `vars`. Dynamic providers
/// ({{$uuid}}, {{$data...}}) start with `$` and are left untouched for the engine.
///
/// Resolution runs a few passes so a variable whose value itself references
/// another {{var}} — e.g. `BaseURL = "https://{{host}}"` with `host` defined —
/// is fully expanded. Bounded to avoid looping on self/cyclic references.
export function resolveVars(text: string, vars: Map<string, string>): string {
  let out = text;
  for (let i = 0; i < 5; i++) {
    const next = out.replace(/\{\{\s*([\w.-]+)\s*\}\}/g, (m, name) =>
      vars.has(name) ? vars.get(name)! : m
    );
    if (next === out) break;
    out = next;
  }
  return out;
}

/// Build the {{name}} → value map for an environment.
///
/// `forExport` is used when writing a config for the CLI pipeline: secret
/// variables become `${KEY}` placeholders instead of their local value, so each
/// system's pipeline injects its own secret via an OS environment variable
/// (dev/stage/prod stay separated). Non-secret variables are baked as-is.
export function envVars(env: Environment | null, forExport = false): Map<string, string> {
  const map = new Map<string, string>();
  if (!env) return map;
  for (const v of env.variables) {
    if (!v.enabled || !v.key.trim()) continue;
    const key = v.key.trim();
    map.set(key, v.secret && forExport ? `\${${key}}` : v.value);
  }
  return map;
}

/// Names of secret variables in an environment (for masking / OS-env lookup).
export function secretKeys(env: Environment | null): string[] {
  if (!env) return [];
  return env.variables.filter((v) => v.secret && v.key.trim()).map((v) => v.key.trim());
}
