# Changelog

All notable Maelstrom changes. Format based on [Keep a Changelog](https://keepachangelog.com/),
versions follow [SemVer](https://semver.org/).

## [0.3.0] — July 2026

### Added
- **Auth profiles** — fill in the authorization (Bearer / Basic / OAuth2) once, save it
  as a profile, then apply it to any other request from a dropdown instead of retyping.
- **Private S3 (AWS SigV4)** — URL datasets and file pools can sign the request with
  AWS credentials (access key / secret / session token / region), so private buckets
  work, not only public or presigned URLs. In CLI configs the secrets go in as `${VAR}`.
- **Fixed request rate** for single-request load tests: a set target RPS is now held
  regardless of how slowly the target responds (open model — same as the multi-endpoint
  scenario and the CLI). Leave the field empty for the classic VU-driven mode.
- **Shortfall counter**: when the target rate can't be sustained, the report, CLI and
  UI show exactly how many scheduled requests weren't delivered — instead of a silently
  lower RPS.

### Fixed
- Request edits (including auth settings) are no longer lost when switching to another
  request without pressing Save — they auto-save on switch.
- The "Unset variables" warning could open **behind** the service-load window (looking
  like Run did nothing) — it's now on top; unresolved variables are also shown inline
  the moment you check an endpoint, with a hint for the `{{$data.name.column}}` syntax.
- OpenAPI import: an `apiKey`-in-header security scheme no longer turns into a Bearer
  header.
- A failed read of the saved state no longer silently resets collections (a banner is
  shown and the file is protected from being overwritten).
- URL/S3 datasets: a non-2xx response is reported as an error instead of being parsed
  as CSV.
- Latency histogram: fixed double-counting on narrow ranges.

### Changed
- Environments hint now includes an end-to-end `DB_PASSWORD` example (variable → request
  → CI config → cluster secret); clearer tooltips for the load fields (the two load models).

## [0.2.0] — July 2026

### Added
- **gRPC**: call any service straight from a `.proto` file — no `protoc` needed
  (unary + server/client/bidirectional streaming), gRPC load testing,
  automatic resolution of proto import roots.
- **WebSocket**: connect, send/receive messages, load testing (message round-trip latency).
- **Response assertions**: status / latency / header / body text / JSON field checks.
- **Databases as a data source** (in the app and the CLI): feed request data straight
  from SQL; plus **file pools** (folder / file list / S3-URL) for multipart uploads.
- **Per-environment secrets**: mark a variable as secret and it exports to CI configs
  as a `${KEY}` placeholder — each cluster injects its own value.
- "Test connection" button for database datasets.
- **English UI** as the base language + EN/RU switcher.
- **Export for CI** dialog: rename env variables on export and get the exact list of
  variables to define in your cluster; also available from the single-request Load tab.
- **CLI distribution**: Windows/macOS/Linux binaries + Docker image
  `ghcr.io/slakertop1/maelstrom-cli`; ready-made GitHub Actions / GitLab CI /
  Kubernetes examples in [`deploy/`](deploy/); a live demo API with an OpenAPI spec
  and a ready load scenario in [`examples/demo-api/`](examples/demo-api/).
- **Linux desktop builds**: `.deb` (Debian/Ubuntu), `.rpm` (Fedora/RHEL) and
  a portable `.AppImage`.
- End-to-end logging with secret redaction; "Report a bug" button in the app.

## [0.1.0] — July 2026

First public beta.

### Features
- HTTP client with collections and `{{var}}` environments.
- Load testing (virtual users / duration / RPS cap), live charts, HTML report.
- OAuth2/SSO with automatic token refresh under load; mTLS client certificates.
- Database load testing (Postgres / MySQL / SQLite); OpenAPI/Swagger import.
- Multi-endpoint "service load"; headless CLI for CI/Kubernetes + Docker image.
