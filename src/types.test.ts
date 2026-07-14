import { describe, it, expect } from "vitest";
import {
  newRequest,
  migrateRequest,
  migrateAuth,
  toDatasetSpec,
  newDataset,
  toFilePoolSpec,
  newMultipartField,
  authProfileName,
} from "./types";

describe("newRequest", () => {
  it("has all body/auth/tls/db/multipart fields with sane defaults", () => {
    const r = newRequest();
    expect(r.kind).toBe("http");
    expect(r.method).toBe("GET");
    expect(r.auth.type).toBe("none");
    expect(r.auth.oauth2.auto_refresh).toBe(true);
    expect(r.tls.enabled).toBe(false);
    expect(Array.isArray(r.multipart_body)).toBe(true);
    expect(r.db.driver).toBe("postgres");
  });
});

describe("migrateRequest", () => {
  it("backfills fields missing on an old persisted request", () => {
    const old = { id: "x", name: "Legacy", method: "POST", url: "http://a", body_type: "json", body: "{}" };
    const r = migrateRequest(old);
    expect(r.name).toBe("Legacy");
    expect(r.method).toBe("POST");
    expect(r.url).toBe("http://a");
    // fields that didn't exist before are filled in
    expect(r.auth.oauth2.auto_refresh).toBe(true);
    expect(r.tls.enabled).toBe(false);
    expect(r.kind).toBe("http");
    expect(Array.isArray(r.multipart_body)).toBe(true);
  });
});

describe("migrateAuth", () => {
  it("backfills a partial auth (e.g. a profile saved by an older build)", () => {
    const a = migrateAuth({ type: "oauth2", oauth2: { client_id: "svc" } });
    expect(a.oauth2.client_id).toBe("svc");
    expect(a.oauth2.auto_refresh).toBe(true); // backfilled default
    expect(a.oauth2.access_token).toBe(""); // never undefined in the editor
    expect(a.token).toBe("");
  });

  it("turns a missing auth into a sane default", () => {
    const a = migrateAuth(undefined);
    expect(a.type).toBe("none");
    expect(a.oauth2.grant).toBe("client_credentials");
  });
});

describe("authProfileName", () => {
  it("derives oauth2 name from client_id and token-url host", () => {
    const r = newRequest();
    r.auth.type = "oauth2";
    r.auth.oauth2.client_id = "shop-svc";
    r.auth.oauth2.token_url = "https://sso.corp.io/realms/x/token";
    expect(authProfileName(r.auth)).toBe("shop-svc @ sso.corp.io");
  });

  it("survives an unparsable token URL and empty client_id", () => {
    const r = newRequest();
    r.auth.type = "oauth2";
    r.auth.oauth2.token_url = "{{sso_host}}/token"; // env var — not a valid URL
    expect(authProfileName(r.auth)).toContain("oauth2");
  });

  it("names bearer by token tail and basic by username", () => {
    const b = newRequest();
    b.auth.type = "bearer";
    b.auth.token = "sekret-token-123";
    expect(authProfileName(b.auth)).toBe("bearer …-123");

    const u = newRequest();
    u.auth.type = "basic";
    u.auth.username = "admin";
    expect(authProfileName(u.auth)).toBe("basic: admin");
  });
});

describe("toDatasetSpec", () => {
  it("maps a file dataset", () => {
    const d = { ...newDataset(), name: "people", source_kind: "file" as const, path: "people.csv", format: "csv" as const };
    const spec = toDatasetSpec(d);
    expect(spec).toEqual({ name: "people", mode: "sequential", source: { kind: "file", path: "people.csv", format: "csv" } });
  });

  it("maps a db dataset to url+query", () => {
    const d = { ...newDataset(), name: "u", source_kind: "db" as const, db_url: "postgres://x", query: "SELECT 1" };
    const spec = toDatasetSpec(d);
    expect(spec.source.kind).toBe("db");
    expect(spec.source.url).toBe("postgres://x");
    expect(spec.source.query).toBe("SELECT 1");
  });

  it("maps a url/S3 dataset", () => {
    const d = { ...newDataset(), name: "s", source_kind: "url" as const, url: "https://bucket/data.json" };
    const spec = toDatasetSpec(d);
    expect(spec.source.kind).toBe("url");
    expect(spec.source.url).toBe("https://bucket/data.json");
    // No credentials → no aws block (public / presigned URL).
    expect(spec.source.aws).toBeUndefined();
  });

  it("attaches AWS credentials to a private-S3 url dataset", () => {
    const d = {
      ...newDataset(),
      name: "clients",
      source_kind: "url" as const,
      url: "https://bucket.s3.amazonaws.com/clients.csv",
      aws_enabled: true,
      aws_region: "eu-central-1",
      aws_access_key_id: "  AKIA123  ",
      aws_secret_access_key: "  secret  ",
      aws_session_token: "",
    };
    const spec = toDatasetSpec(d);
    expect(spec.source.aws).toEqual({
      access_key_id: "AKIA123", // trimmed
      secret_access_key: "secret",
      region: "eu-central-1",
      session_token: null, // blank → omitted
    });
  });

  it("does not attach an aws block when the toggle is off, even with keys present", () => {
    const d = {
      ...newDataset(),
      name: "s",
      source_kind: "url" as const,
      url: "https://bucket/data.csv",
      aws_enabled: false,
      aws_access_key_id: "AKIA123",
      aws_secret_access_key: "secret",
    };
    expect(toDatasetSpec(d).source.aws).toBeUndefined();
  });
});

describe("toFilePoolSpec", () => {
  it("maps a folder pool with mask", () => {
    const f = {
      ...newMultipartField("file"),
      source: "pool" as const,
      pool_kind: "folder" as const,
      pool_path: "C:\\imgs",
      pool_mask: "*.jpg,*.png",
      pool_mode: "sequential" as const,
    };
    expect(toFilePoolSpec("p1", f)).toEqual({
      name: "p1",
      mode: "sequential",
      source: { kind: "folder", path: "C:\\imgs", mask: "*.jpg,*.png" },
    });
  });

  it("maps a list pool, splitting lines and dropping blanks", () => {
    const f = {
      ...newMultipartField("file"),
      source: "pool" as const,
      pool_kind: "list" as const,
      pool_paths: "a.png\n\n  b.jpg  \n",
    };
    const spec = toFilePoolSpec("p2", f);
    expect(spec.mode).toBe("random"); // default
    expect(spec.source).toEqual({ kind: "list", paths: ["a.png", "b.jpg"] });
  });

  it("maps a url/S3 pool", () => {
    const f = {
      ...newMultipartField("file"),
      source: "pool" as const,
      pool_kind: "url" as const,
      pool_urls: "https://bucket/a.png?sig\nhttps://bucket/b.png?sig",
    };
    const spec = toFilePoolSpec("p3", f);
    expect(spec.source.kind).toBe("url");
    expect(spec.source.urls).toEqual([
      "https://bucket/a.png?sig",
      "https://bucket/b.png?sig",
    ]);
  });
});
