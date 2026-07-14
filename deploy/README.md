# Maelstrom headless runner — CI / Kubernetes

Run load **from a pipeline or from inside Kubernetes** — for when laptops aren't
allowed to reach prod but a runner/pod inside the network is. Same engine as the
desktop app (multi-endpoint scenarios, per-endpoint RPS, OAuth with automatic
token refresh, mTLS) — just without the GUI.

## What it is

- `maelstrom` — the headless binary (crate `cli`, engine `core`). No GUI needed.
- Config is a JSON scenario: **exported straight from the app** (the "Service load"
  panel → "↓ Export for CI"), or hand-written.
- It writes JSON + HTML reports and **exits non-zero when a threshold is breached**,
  so the pipeline fails.

## Config format

```json
{
  "name": "orders-service",
  "duration_secs": 120,
  "timeout_ms": 10000,
  "targets": [
    { "name": "list", "method": "GET", "url": "https://orders.internal/v1/orders", "rps": 200,
      "auth_refresh": { "grant_type": "client_credentials", "token_url": "https://idp.internal/oauth/token",
                        "client_id": "loadtest", "client_secret": "${OAUTH_CLIENT_SECRET}", "client_auth": "body" } }
  ],
  "thresholds": { "max_error_rate": 1.0, "max_p95_ms": 400 }
}
```

Endpoint fields are the same as in the app: `method`, `url`, `headers`, `body`, `rps`,
`tls` (client certificate / CA), `auth_refresh` (OAuth with TTL-based auto-refresh).

### Secrets

Any `${NAME}` in the config is substituted from an environment variable, so
client secrets and tokens never land in the repo. In k8s that's a `Secret` →
`envFrom`; in GitLab, masked CI/CD variables.

## Run

```bash
maelstrom scenario.json \
  --out-json report.json \
  --out-html report.html \
  --max-error-rate 1 \
  --max-p95 400
# exit 0 — thresholds passed; exit 1 — breached; exit 2 — config/startup error
```

Thresholds can be set in the config (`thresholds`) and/or via flags (flags win).
`--duration N` overrides the duration. `Ctrl-C` is a graceful stop with a report.

## Docker

Prebuilt image (published by CI to GHCR):
```bash
docker pull ghcr.io/slakertop1/maelstrom-cli:latest
docker run --rm -v "$PWD:/work" ghcr.io/slakertop1/maelstrom-cli:latest \
  /work/scenario.json --out-json /work/report.json --max-error-rate 1
```

Or build it locally from the repo root:
```bash
docker build -t maelstrom-cli:local .
```

The image builds only `core` + `cli` (no Tauri); runtime is debian-slim with
ca-certificates, running as non-root.

## Kubernetes

- `k8s-job.yaml` — one-off run (Job) + example ConfigMap with the scenario and a Secret.
- `k8s-cronjob.yaml` — recurring run (e.g. nightly).

`backoffLimit: 0` — a breached threshold (exit 1) fails the Job and doesn't retry.

## GitHub Actions

See `github-actions-example.yml`. It runs the CLI (via the container image) as a
step, **fails the job on a breached threshold**, and keeps the HTML report as an
artifact. Secrets come from GitHub Actions secrets and reach the scenario as `${VAR}`.

```yaml
- name: Load test (gates the job)
  env:
    OAUTH_CLIENT_SECRET: ${{ secrets.OAUTH_CLIENT_SECRET }}
  run: |
    docker run --rm -v "$PWD:/work" -w /work -e OAUTH_CLIENT_SECRET \
      ghcr.io/slakertop1/maelstrom-cli:latest \
      scenario.json --out-json report.json --out-html report.html \
      --max-error-rate 1 --max-p95 400
- uses: actions/upload-artifact@v4
  if: always()
  with: { name: loadtest-report, path: "report.*" }
```

## GitLab CI

See `gitlab-ci-example.yml` — the job fails on a breached threshold and keeps the
HTML report as an artifact.
