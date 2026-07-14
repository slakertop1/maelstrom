import { describe, it, expect } from "vitest";
import { importOpenApi } from "./openapi";

const oas3 = {
  openapi: "3.0.3",
  info: { title: "Orders Service", version: "2.1.0" },
  servers: [{ url: "https://api.shop.example.com/v2" }],
  components: {
    securitySchemes: {
      oauth: {
        type: "oauth2",
        flows: {
          clientCredentials: { tokenUrl: "https://idp.example.com/token", scopes: { "orders.read": "" } },
        },
      },
    },
    schemas: {
      Order: {
        type: "object",
        properties: { id: { type: "string" }, total: { type: "integer" } },
      },
    },
  },
  security: [{ oauth: ["orders.read"] }],
  paths: {
    "/orders": {
      get: { summary: "List orders" },
      post: {
        operationId: "createOrder",
        requestBody: { content: { "application/json": { schema: { $ref: "#/components/schemas/Order" } } } },
      },
    },
    "/orders/{orderId}": {
      get: { summary: "Get order", parameters: [{ name: "orderId", in: "path", required: true, schema: { type: "string" } }] },
    },
  },
};

const swagger2 = {
  swagger: "2.0",
  info: { title: "Legacy API", version: "1.0" },
  host: "legacy.example.com",
  basePath: "/api",
  schemes: ["https"],
  securityDefinitions: { bt: { type: "basic" } },
  security: [{ bt: [] }],
  paths: { "/users": { get: { summary: "Users" } } },
};

describe("importOpenApi", () => {
  it("parses OpenAPI 3 into a named collection with per-op requests", () => {
    const r = importOpenApi(JSON.stringify(oas3));
    expect(r.collection.name).toBe("Orders Service (2.1.0)");
    expect(r.operationCount).toBe(3);
    const byName = Object.fromEntries(r.collection.requests.map((q) => [q.name, q]));
    expect(byName["List orders"].method).toBe("GET");
    expect(byName["List orders"].url).toBe("https://api.shop.example.com/v2/orders");
    // OAuth2 security mapped
    expect(byName["List orders"].auth.type).toBe("oauth2");
    expect(byName["List orders"].auth.oauth2.token_url).toBe("https://idp.example.com/token");
    expect(byName["List orders"].auth.oauth2.grant).toBe("client_credentials");
    // path param becomes a {{var}} template
    expect(byName["Get order"].url).toContain("{{orderId}}");
    // request body synthesized from schema
    expect(byName["createOrder"].body_type).toBe("json");
    expect(byName["createOrder"].body).toContain("total");
  });

  it("parses Swagger 2.0 incl. basic auth and host/basePath", () => {
    const r = importOpenApi(JSON.stringify(swagger2));
    expect(r.collection.name).toBe("Legacy API (1.0)");
    expect(r.operationCount).toBe(1);
    const req = r.collection.requests[0];
    expect(req.url).toBe("https://legacy.example.com/api/users");
    expect(req.auth.type).toBe("basic");
  });

  it("accepts YAML input", () => {
    const yaml =
      "openapi: 3.0.0\ninfo:\n  title: Y\n  version: '1'\nservers:\n  - url: https://y.example.com\npaths:\n  /ping:\n    get:\n      summary: Ping\n";
    const r = importOpenApi(yaml);
    expect(r.operationCount).toBe(1);
    expect(r.collection.requests[0].url).toBe("https://y.example.com/ping");
  });

  it("rejects non-spec input", () => {
    expect(() => importOpenApi('{"foo":1}')).toThrow();
  });

  it("survives a Go-template server URL and maps it to a {{var}}", () => {
    const yaml = [
      "openapi: 3.0.0",
      "servers:",
      "  - url: {{ .BaseURL }}", // unquoted deployment template — invalid YAML scalar
      "info:",
      "  title: Templated Svc",
      "  version: '1'",
      "paths:",
      "  /ping:",
      "    get:",
      "      summary: Ping",
    ].join("\n");
    const r = importOpenApi(yaml);
    expect(r.operationCount).toBe(1);
    expect(r.collection.requests[0].url).toBe("{{BaseURL}}/ping");
    expect(r.warnings.join(" ")).toContain("{{BaseURL}}");
  });

  it("maps an apiKey-in-header scheme to the real header, not Authorization: Bearer", () => {
    const spec = {
      openapi: "3.0.0",
      info: { title: "Keyed", version: "1" },
      servers: [{ url: "https://k.example.com" }],
      components: {
        securitySchemes: { apiKey: { type: "apiKey", in: "header", name: "X-API-Key" } },
      },
      security: [{ apiKey: [] }],
      paths: { "/ping": { get: { summary: "Ping" } } },
    };
    const r = importOpenApi(JSON.stringify(spec));
    const req = r.collection.requests[0];
    // NOT bearer — that would send `Authorization: Bearer …` and 401.
    expect(req.auth.type).toBe("none");
    // The real custom header is added (disabled, for the user to fill).
    const hdr = req.headers.find((h) => h.key === "X-API-Key");
    expect(hdr).toBeDefined();
    expect(hdr!.enabled).toBe(false);
    // And the user is told about it.
    expect(r.warnings.join(" ")).toContain("X-API-Key");
  });

  it("maps multipart/form-data body to multipart fields (binary → file)", () => {
    const spec = {
      openapi: "3.0.0",
      info: { title: "U", version: "1" },
      servers: [{ url: "https://u.example.com" }],
      paths: {
        "/upload": {
          post: {
            summary: "Upload",
            requestBody: {
              content: {
                "multipart/form-data": {
                  schema: {
                    type: "object",
                    required: ["client_id", "photo"],
                    properties: {
                      client_id: { type: "string", example: "4-148JP4DN" },
                      photo: { type: "string", format: "binary" },
                      note: { type: "string" },
                    },
                  },
                },
              },
            },
          },
        },
      },
    };
    const r = importOpenApi(JSON.stringify(spec));
    const req = r.collection.requests[0];
    expect(req.body_type).toBe("multipart");
    const byName = Object.fromEntries(req.multipart_body.map((f) => [f.name, f]));
    expect(byName["client_id"].kind).toBe("text");
    expect(byName["client_id"].value).toBe("4-148JP4DN");
    expect(byName["client_id"].enabled).toBe(true);
    expect(byName["photo"].kind).toBe("file");
    expect(byName["photo"].enabled).toBe(true);
    expect(byName["note"].enabled).toBe(false); // optional → disabled by default
  });
});
