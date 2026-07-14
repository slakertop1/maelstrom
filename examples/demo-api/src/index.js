// Maelstrom demo API — a tiny public API (a few endpoints + an OpenAPI/Swagger
// spec) so users can try Maelstrom: import the spec, send requests, run a load
// test, and gate a CI pipeline. Deploy your own copy with `wrangler deploy`.

const products = [
  { id: 1, name: "Keyboard", price: 49.9, inStock: true },
  { id: 2, name: "Mouse", price: 19.9, inStock: true },
  { id: 3, name: "Monitor", price: 199.0, inStock: false },
  { id: 4, name: "Webcam", price: 39.5, inStock: true },
];

const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "GET,POST,OPTIONS",
  "access-control-allow-headers": "content-type",
};

function json(data, status = 200) {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "content-type": "application/json", ...CORS },
  });
}

function openapi(origin) {
  return {
    openapi: "3.0.3",
    info: {
      title: "Maelstrom Demo API",
      version: "1.0.0",
      description: "A tiny demo API for trying out Maelstrom (import this spec, send requests, load-test).",
    },
    servers: [{ url: origin }],
    paths: {
      "/health": {
        get: { summary: "Health check", responses: { 200: { description: "OK" } } },
      },
      "/api/products": {
        get: { summary: "List products", responses: { 200: { description: "Array of products" } } },
      },
      "/api/products/{id}": {
        get: {
          summary: "Get a product by id",
          parameters: [{ name: "id", in: "path", required: true, schema: { type: "integer" } }],
          responses: { 200: { description: "Product" }, 404: { description: "Not found" } },
        },
      },
      "/api/orders": {
        post: {
          summary: "Create an order",
          requestBody: {
            required: true,
            content: {
              "application/json": {
                schema: {
                  type: "object",
                  properties: { productId: { type: "integer" }, qty: { type: "integer" } },
                  required: ["productId", "qty"],
                },
              },
            },
          },
          responses: { 201: { description: "Order created" } },
        },
      },
    },
  };
}

export default {
  async fetch(request) {
    const url = new URL(request.url);
    const path = url.pathname;
    const method = request.method;

    if (method === "OPTIONS") return new Response(null, { headers: CORS });

    if (path === "/" || path === "/health") return json({ status: "ok", service: "maelstrom-demo-api" });
    if (path === "/openapi.json") return json(openapi(url.origin));
    if (path === "/api/products" && method === "GET") return json(products);

    const m = path.match(/^\/api\/products\/(\d+)$/);
    if (m && method === "GET") {
      const prod = products.find((x) => x.id === Number(m[1]));
      return prod ? json(prod) : json({ error: "product not found", id: Number(m[1]) }, 404);
    }

    if (path === "/api/orders" && method === "POST") {
      let body = {};
      try {
        body = await request.json();
      } catch {
        /* empty / invalid body -> defaults below */
      }
      const productId = Number(body.productId) || 1;
      const qty = Number(body.qty) || 1;
      const prod = products.find((x) => x.id === productId) || products[0];
      return json(
        {
          id: Math.floor(Math.random() * 1_000_000),
          productId,
          qty,
          total: Number((prod.price * qty).toFixed(2)),
          createdAt: new Date().toISOString(),
        },
        201
      );
    }

    return json({ error: "not found", path }, 404);
  },
};
