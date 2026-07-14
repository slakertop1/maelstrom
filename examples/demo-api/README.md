# Demo API — try Maelstrom end-to-end

A tiny public API (a few endpoints + an OpenAPI/Swagger spec) so you can try
Maelstrom against something real: import the spec, send requests, run a load
test, and gate a CI pipeline.

**Live instance:** `https://maelstrom-demo-api.cryptorelicday.workers.dev`
**Spec:** `https://maelstrom-demo-api.cryptorelicday.workers.dev/openapi.json`

## Endpoints

| Method | Path | What it does |
|--------|------|--------------|
| GET  | `/health` | `{ "status": "ok" }` |
| GET  | `/api/products` | list of products |
| GET  | `/api/products/{id}` | one product (404 if missing) |
| POST | `/api/orders` | create an order — body `{ "productId": 1, "qty": 2 }` |
| GET  | `/openapi.json` | the OpenAPI 3.0 spec |

## Quick try (no setup)

In the Maelstrom app: sidebar **↥ Import** → paste the spec URL above → it builds a
collection with every endpoint. Send a request; try the **Load** tab.

> The live instance is fine for a few requests. **For real load testing, deploy
> your own copy** (below) and point Maelstrom at it — so you're not hammering a
> shared endpoint (and its Cloudflare free-tier quota).

## Deploy your own (free, ~1 min)

```bash
npm i -g wrangler        # or: npx wrangler ...
wrangler login
cd examples/demo-api
wrangler deploy          # prints your https://<name>.<subdomain>.workers.dev URL
```

## Load-test it with the CLI

`scenario.json` hits three endpoints with per-endpoint RPS and thresholds. The
target host is a `${BASE_URL}` variable, so point it at your deployment:

```bash
# Docker
docker run --rm -v "$PWD:/work" -w /work \
  -e BASE_URL=https://<your>.workers.dev \
  ghcr.io/slakertop1/maelstrom-cli:latest \
  scenario.json --out-json report.json --out-html report.html --max-error-rate 1 --max-p95 800

# …or the binary
BASE_URL=https://<your>.workers.dev \
  maelstrom scenario.json --out-html report.html --max-error-rate 1 --max-p95 800
```

Exit code `0` = thresholds passed, `1` = breached → your pipeline fails. Drop this
into the [GitHub Actions / GitLab CI](../../deploy/) examples and you have a real,
gated load-test pipeline.
