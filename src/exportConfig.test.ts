import { describe, it, expect } from "vitest";
import { requiredEnvVars, applyRenames } from "./components/ExportConfigModal";

describe("export-for-CI env var handling", () => {
  const json = '{"a":"${TOKEN}","b":"${DB_PASSWORD}","c":"${TOKEN}","d":"literal"}';

  it("detects, de-dupes and sorts required vars", () => {
    expect(requiredEnvVars(json)).toEqual(["DB_PASSWORD", "TOKEN"]);
  });

  it("returns nothing when the config is self-contained", () => {
    expect(requiredEnvVars('{"x":"plain"}')).toEqual([]);
  });

  it("renames a placeholder everywhere it appears", () => {
    const out = applyRenames(json, { TOKEN: "API_TOKEN" });
    expect(out).toContain("${API_TOKEN}");
    expect(out).not.toContain("${TOKEN}");
    expect(requiredEnvVars(out)).toEqual(["API_TOKEN", "DB_PASSWORD"]);
  });

  it("ignores empty and identity renames", () => {
    expect(applyRenames(json, { TOKEN: "", DB_PASSWORD: "DB_PASSWORD" })).toBe(json);
  });
});
