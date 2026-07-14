// OpenAPI / Swagger → Maelstrom collection.
// Supports Swagger 2.0 and OpenAPI 3.0/3.1, JSON or YAML. Produces one request
// per operation, with query/header params, a JSON body synthesized from the
// schema, and auth mapped from the spec's security schemes.
import { parse as parseYaml } from "yaml";
import {
  AuthConfig,
  Collection,
  KV,
  MultipartField,
  RequestConfig,
  newKV,
  newMultipartField,
  newOAuth2,
  newRequest,
  uid,
} from "./types";
import { tr, tr2 } from "./i18n";

const HTTP_METHODS = ["get", "post", "put", "patch", "delete", "head", "options"];

interface AnyObj {
  [k: string]: any;
}

export interface ImportResult {
  collection: Collection;
  operationCount: number;
  serviceName: string;
  version: string;
  warnings: string[];
}

/// Specs are often stored as deployment templates (`url: {{ .BaseURL }}`,
/// Go template / Helm). Unquoted `{{ … }}` is not a valid YAML scalar, so we
/// quote such values before parsing to keep the placeholder as a string.
function sanitizeTemplatedYaml(text: string): string {
  return text.replace(/^(\s*(?:- )?[\w.$-]+:\s*)(\{\{[^\n]*\}\})\s*$/gm, '$1"$2"');
}

export function parseSpecText(text: string): AnyObj {
  const trimmed = text.trim();
  let doc: any;
  if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
    doc = JSON.parse(text);
  } else {
    // yaml.parse also accepts JSON, so it's a safe fallback for either format.
    doc = parseYaml(sanitizeTemplatedYaml(text));
  }
  if (!doc || typeof doc !== "object") {
    throw new Error(tr("This file doesn't look like OpenAPI/Swagger (empty or not an object)"));
  }
  if (!doc.openapi && !doc.swagger) {
    throw new Error(
      tr("No openapi or swagger field found — this is not an OpenAPI/Swagger specification")
    );
  }
  return doc;
}

export function importOpenApi(text: string): ImportResult {
  const doc = parseSpecText(text);
  const isV3 = !!doc.openapi;
  const warnings: string[] = [];

  const serviceName = (doc.info?.title || tr("Imported service")).trim();
  const version = doc.info?.version ? String(doc.info.version) : "";
  const baseUrl = resolveBaseUrl(doc, isV3, warnings);
  const resolver = makeResolver(doc);

  const requests: RequestConfig[] = [];
  const paths: AnyObj = doc.paths || {};

  for (const rawPath of Object.keys(paths)) {
    const pathItem = resolver(paths[rawPath]);
    if (!pathItem) continue;
    const pathLevelParams: any[] = (pathItem.parameters || []).map(resolver);

    for (const method of HTTP_METHODS) {
      const op = pathItem[method];
      if (!op || typeof op !== "object") continue;

      const req = newRequest();
      req.method = method.toUpperCase();
      req.name = operationName(op, method, rawPath);
      req.url = joinUrl(baseUrl, templatePath(rawPath));

      const opParams: any[] = [...pathLevelParams, ...(op.parameters || []).map(resolver)];
      const seen = new Set<string>();
      const params: KV[] = [];
      const headers: KV[] = [];

      for (const p of opParams) {
        if (!p || !p.name) continue;
        const key = `${p.in}:${p.name}`;
        if (seen.has(key)) continue;
        seen.add(key);
        const value = paramSampleValue(p, resolver);
        const kv: KV = {
          id: uid(),
          key: p.name,
          value: value == null ? "" : String(value),
          enabled: !!p.required,
        };
        if (p.in === "query") params.push(kv);
        else if (p.in === "header") headers.push(kv);
        // path params stay in the URL as {{name}}; cookie params are skipped
      }

      // request body
      const bodyInfo = extractBody(op, opParams, isV3, resolver);
      if (bodyInfo) {
        if (bodyInfo.kind === "json") {
          req.body_type = "json";
          req.body = bodyInfo.text;
        } else if (bodyInfo.kind === "form") {
          req.body_type = "form";
          req.form_body = bodyInfo.fields.length ? bodyInfo.fields : [newKV()];
        } else if (bodyInfo.kind === "multipart") {
          req.body_type = "multipart";
          req.multipart_body = bodyInfo.mpFields?.length
            ? [...bodyInfo.mpFields, newMultipartField()]
            : [newMultipartField()];
        }
      }

      req.params = params.length ? [...params, newKV()] : [newKV()];
      const resolved = resolveAuth(doc, op, isV3);
      req.auth = resolved.auth;
      if (resolved.apiKeyHeader) {
        // An apiKey-in-header scheme → add the real custom header (disabled, for
        // the user to fill in) instead of a wrong `Authorization: Bearer …`.
        headers.push({ id: uid(), key: resolved.apiKeyHeader, value: "", enabled: false });
      }
      req.headers = headers.length ? [...headers, newKV()] : [newKV()];
      if (resolved.warning && !warnings.includes(resolved.warning)) {
        warnings.push(resolved.warning);
      }

      requests.push(req);
    }
  }

  if (requests.length === 0) {
    warnings.push(tr("No operations found in the specification."));
  }

  const collection: Collection = {
    id: uid(),
    name: version ? `${serviceName} (${version})` : serviceName,
    requests,
  };

  return {
    collection,
    operationCount: requests.length,
    serviceName,
    version,
    warnings,
  };
}

// ---------- helpers ----------

function resolveBaseUrl(doc: AnyObj, isV3: boolean, warnings: string[]): string {
  if (isV3) {
    const server = Array.isArray(doc.servers) ? doc.servers[0] : null;
    if (!server?.url || typeof server.url !== "string") {
      warnings.push(tr("The specification has no servers — the base URL was left empty."));
      return "";
    }
    let url: string = server.url;
    // Deployment-template placeholder ({{ .BaseURL }}, Go template / Helm):
    // turn it into a Maelstrom environment variable {{BaseURL}}.
    const goTpl = /^\{\{[\s.]*([\w-]+)\s*\}\}$/.exec(url.trim());
    if (goTpl) {
      const varName = goTpl[1];
      warnings.push(
        tr2(
          "The base URL in the spec is a deployment template ({template}). It was replaced with the variable {{{varName}}} — set it in your environment (the «Environments» button).",
          { template: url.trim(), varName }
        )
      );
      return `{{${varName}}}`;
    }
    // substitute server variables with their defaults
    if (server.variables) {
      url = url.replace(/\{([^}]+)\}/g, (m, name) => {
        const v = server.variables[name];
        return v && v.default != null ? String(v.default) : m;
      });
    }
    return url.replace(/\/+$/, "");
  }
  // Swagger 2.0
  const schemes: string[] = Array.isArray(doc.schemes) ? doc.schemes : [];
  const scheme = schemes.includes("https") ? "https" : schemes[0] || "https";
  const host: string = doc.host || "";
  const basePath: string = doc.basePath || "";
  if (!host) {
    warnings.push(tr("The specification has no host — the base URL may be incomplete."));
    return basePath.replace(/\/+$/, "");
  }
  return `${scheme}://${host}${basePath}`.replace(/\/+$/, "");
}

function templatePath(path: string): string {
  // OpenAPI {id} → Maelstrom {{id}} so it's an editable variable
  return path.replace(/\{([^}]+)\}/g, "{{$1}}");
}

function joinUrl(base: string, path: string): string {
  if (!base) return path;
  if (!path) return base;
  return `${base}${path.startsWith("/") ? "" : "/"}${path}`;
}

function operationName(op: AnyObj, method: string, path: string): string {
  if (op.summary && String(op.summary).trim()) return String(op.summary).trim();
  if (op.operationId && String(op.operationId).trim()) return String(op.operationId).trim();
  return `${method.toUpperCase()} ${path}`;
}

/** Build a $ref resolver bound to this document (handles internal refs only). */
function makeResolver(doc: AnyObj) {
  const resolve = (node: any, depth = 0): any => {
    if (!node || typeof node !== "object" || depth > 100) return node;
    if (typeof node.$ref === "string" && node.$ref.startsWith("#/")) {
      const target = pointer(doc, node.$ref);
      return resolve(target, depth + 1);
    }
    return node;
  };
  return resolve;
}

function pointer(doc: AnyObj, ref: string): any {
  const parts = ref
    .slice(2)
    .split("/")
    .map((p) => p.replace(/~1/g, "/").replace(/~0/g, "~"));
  let cur: any = doc;
  for (const part of parts) {
    if (cur == null) return undefined;
    cur = cur[part];
  }
  return cur;
}

function paramSampleValue(p: AnyObj, resolver: (n: any) => any): any {
  if (p.example !== undefined) return p.example;
  const schema = resolver(p.schema) || p.schema;
  if (schema) {
    if (schema.example !== undefined) return schema.example;
    if (Array.isArray(schema.enum) && schema.enum.length) return schema.enum[0];
    if (schema.default !== undefined) return schema.default;
  }
  // Swagger 2.0 params carry type/default at the top level
  if (Array.isArray(p.enum) && p.enum.length) return p.enum[0];
  if (p.default !== undefined) return p.default;
  return "";
}

interface BodyInfo {
  kind: "json" | "form" | "multipart";
  text: string;
  fields: KV[];
  mpFields?: MultipartField[];
}

/// multipart/form-data schema → multipart editor fields (binary → file part).
function multipartFromSchema(
  schema: any,
  resolver: (n: any) => any
): MultipartField[] {
  schema = resolver(schema);
  const props = schema?.properties ?? {};
  const required: string[] = Array.isArray(schema?.required) ? schema.required : [];
  return Object.keys(props).map((name) => {
    const p = resolver(props[name]) ?? {};
    const isFile = p.format === "binary" || p.type === "file";
    return {
      ...newMultipartField(isFile ? "file" : "text"),
      name,
      value: isFile ? "" : p.example != null ? String(p.example) : "",
      enabled: required.includes(name),
    };
  });
}

function extractBody(
  op: AnyObj,
  opParams: any[],
  isV3: boolean,
  resolver: (n: any) => any
): BodyInfo | null {
  if (isV3) {
    const rb = resolver(op.requestBody);
    const content = rb?.content;
    if (!content) return null;
    const json =
      content["application/json"] ||
      content["application/*+json"] ||
      content[Object.keys(content).find((k) => k.includes("json")) || ""];
    if (json) {
      const sample =
        json.example !== undefined
          ? json.example
          : firstExample(json.examples) ??
            sampleFromSchema(resolver(json.schema), resolver, new Set());
      return { kind: "json", text: pretty(sample), fields: [] };
    }
    const form = content["application/x-www-form-urlencoded"];
    if (form) {
      const obj = sampleFromSchema(resolver(form.schema), resolver, new Set());
      return { kind: "form", text: "", fields: objToFields(obj) };
    }
    const multipart = content["multipart/form-data"];
    if (multipart) {
      return {
        kind: "multipart",
        text: "",
        fields: [],
        mpFields: multipartFromSchema(multipart.schema, resolver),
      };
    }
    // fallback: first content type as json-ish
    const firstKey = Object.keys(content)[0];
    if (firstKey) {
      const sample = sampleFromSchema(resolver(content[firstKey].schema), resolver, new Set());
      return { kind: "json", text: pretty(sample), fields: [] };
    }
    return null;
  }

  // Swagger 2.0: body/formData live in parameters
  const bodyParam = opParams.find((p) => p && p.in === "body");
  if (bodyParam) {
    const sample = sampleFromSchema(resolver(bodyParam.schema), resolver, new Set());
    return { kind: "json", text: pretty(sample), fields: [] };
  }
  const formParams = opParams.filter((p) => p && p.in === "formData");
  if (formParams.length) {
    const fields = formParams.map((p) => ({
      id: uid(),
      key: p.name,
      value: p.default != null ? String(p.default) : "",
      enabled: !!p.required,
    }));
    return { kind: "form", text: "", fields };
  }
  return null;
}

function firstExample(examples: any): any {
  if (!examples || typeof examples !== "object") return undefined;
  const first = Object.values(examples)[0] as any;
  return first?.value;
}

function objToFields(obj: any): KV[] {
  if (!obj || typeof obj !== "object" || Array.isArray(obj)) return [];
  return Object.entries(obj).map(([k, v]) => ({
    id: uid(),
    key: k,
    value: typeof v === "object" ? JSON.stringify(v) : String(v),
    enabled: true,
  }));
}

/** Synthesize an example value from a JSON schema, guarding against ref cycles. */
function sampleFromSchema(schema: any, resolver: (n: any) => any, seen: Set<any>): any {
  schema = resolver(schema);
  if (!schema || typeof schema !== "object") return null;
  if (seen.has(schema)) return null;
  seen.add(schema);

  try {
    if (schema.example !== undefined) return schema.example;
    if (schema.default !== undefined) return schema.default;
    if (Array.isArray(schema.enum) && schema.enum.length) return schema.enum[0];

    // composition
    const comp = schema.allOf || schema.oneOf || schema.anyOf;
    if (Array.isArray(comp) && comp.length) {
      if (schema.allOf) {
        const merged: AnyObj = {};
        for (const part of schema.allOf) {
          const v = sampleFromSchema(part, resolver, seen);
          if (v && typeof v === "object" && !Array.isArray(v)) Object.assign(merged, v);
        }
        return merged;
      }
      return sampleFromSchema(comp[0], resolver, seen);
    }

    const type = Array.isArray(schema.type) ? schema.type[0] : schema.type;

    if (type === "object" || schema.properties) {
      const obj: AnyObj = {};
      const props = schema.properties || {};
      for (const key of Object.keys(props)) {
        obj[key] = sampleFromSchema(props[key], resolver, seen);
      }
      return obj;
    }
    if (type === "array") {
      return [sampleFromSchema(schema.items, resolver, seen)];
    }
    return primitiveSample(type, schema.format);
  } finally {
    seen.delete(schema);
  }
}

function primitiveSample(type: string | undefined, format: string | undefined): any {
  switch (format) {
    case "date":
      return "2024-01-01";
    case "date-time":
      return "2024-01-01T00:00:00Z";
    case "uuid":
      return "00000000-0000-0000-0000-000000000000";
    case "email":
      return "user@example.com";
    case "uri":
      return "https://example.com";
    case "int64":
    case "int32":
      return 0;
    case "float":
    case "double":
      return 0;
  }
  switch (type) {
    case "integer":
    case "number":
      return 0;
    case "boolean":
      return true;
    case "string":
      return "string";
    case "array":
      return [];
    case "object":
      return {};
    default:
      return null;
  }
}

function pretty(v: any): string {
  if (v == null) return "";
  return JSON.stringify(v, null, 2);
}

/** Map the operation's (or global) security requirement onto an AuthConfig. */
interface ResolvedAuth {
  auth: AuthConfig;
  /** apiKey-in-header schemes: the custom header name to add as a disabled KV. */
  apiKeyHeader?: string;
  /** A note for the import summary (e.g. an api-key the user must fill in). */
  warning?: string;
}

function resolveAuth(doc: AnyObj, op: AnyObj, isV3: boolean): ResolvedAuth {
  const base: AuthConfig = {
    type: "none",
    token: "",
    username: "",
    password: "",
    oauth2: newOAuth2(),
  };

  const requirement = op.security ?? doc.security;
  if (!Array.isArray(requirement) || requirement.length === 0) return { auth: base };
  const schemeName = Object.keys(requirement[0] || {})[0];
  if (!schemeName) return { auth: base };

  const defs = isV3 ? doc.components?.securitySchemes : doc.securityDefinitions;
  const scheme = defs?.[schemeName];
  if (!scheme) return { auth: base };

  const kind = String(scheme.type || "").toLowerCase();

  // Swagger 2.0 uses type: "basic" directly.
  if (kind === "basic") return { auth: { ...base, type: "basic" } };

  if (kind === "http") {
    const httpScheme = String(scheme.scheme || "").toLowerCase();
    if (httpScheme === "basic") return { auth: { ...base, type: "basic" } };
    // bearer and others → bearer token slot
    return { auth: { ...base, type: "bearer" } };
  }
  if (kind === "oauth2") {
    const flows = isV3 ? scheme.flows : { [scheme.flow]: scheme };
    const oauth = newOAuth2();
    const cc = flows?.clientCredentials || flows?.application;
    const pw = flows?.password;
    const ac = flows?.authorizationCode || flows?.accessCode;
    const impl = flows?.implicit;
    const chosen = cc || pw || ac || impl;
    if (chosen) {
      oauth.token_url = chosen.tokenUrl || "";
      oauth.auth_url = chosen.authorizationUrl || "";
      oauth.scope = chosen.scopes ? Object.keys(chosen.scopes).join(" ") : "";
      oauth.grant = cc
        ? "client_credentials"
        : pw
          ? "password"
          : "authorization_code";
    }
    return { auth: { ...base, type: "oauth2", oauth2: oauth } };
  }
  if (kind === "apikey") {
    const name = String(scheme.name || "").trim();
    if (String(scheme.in).toLowerCase() === "header" && name) {
      // An API key sent in a custom header (e.g. `X-API-Key`). Do NOT map it to
      // `Authorization: Bearer` — that sends the wrong header name and 401s.
      // Surface the real header for the user to fill in.
      return {
        auth: base,
        apiKeyHeader: name,
        warning: tr2(
          "«{scheme}» sends an API key in the «{header}» header — fill its value in the request headers.",
          { scheme: schemeName, header: name }
        ),
      };
    }
    // apiKey in query → the user adds the query param themselves.
    return { auth: base };
  }
  // openIdConnect / mutualTLS → leave none, user configures
  return { auth: base };
}
