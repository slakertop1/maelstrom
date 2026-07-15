# ⚡ Maelstrom

**A desktop API client with built-in load testing** — HTTP, gRPC, WebSocket and
databases in one app, plus a headless CLI for CI/Kubernetes.

**Stack:** Tauri 2 · Rust (reqwest/rustls, tokio, sqlx, tonic, hdrhistogram) · React + TypeScript · Vite

> [English](#english) · [Русский](#русский) · [Changelog](CHANGELOG.md) · [Download](https://github.com/slakertop1/maelstrom-releases/releases)
>
> Full developer documentation. A short user-facing quick-start is in
> [`community/README.md`](community/README.md); the version-by-version update
> history is in [`CHANGELOG.md`](CHANGELOG.md).

---

## English

### Overview

Maelstrom is a Postman-style desktop client (compose/send/inspect requests) with
a first-class **load-testing** engine, extended to gRPC, WebSocket and SQL
databases. The same engine ships as a **headless CLI** so the scenario you tune
on the desktop runs unchanged in a pipeline or a Kubernetes Job and gates the
build on thresholds via its exit code.

Two audiences, one engine:
- **Desktop app** — interactive: build a request, assert the response, run it
  under load, read an HTML report.
- **CLI** — non-interactive: replay a scenario from CI, exit non-zero when a
  threshold is breached.

### Features

**HTTP client**
- All methods, query params, headers, bodies (JSON / text / form-urlencoded /
  `multipart/form-data` with file parts), JSON-highlighted response view.
- Collections (auto-persisted), environments with `{{var}}` substitution in URL,
  headers, params, body, OAuth fields and DB connection strings.

**Auth & TLS**
- Bearer, Basic, and **OAuth 2.0 / SSO** — grants `client_credentials`,
  `password`, `refresh_token`, and `authorization_code` + PKCE (system-browser
  login with a local redirect receiver).
- **Automatic token refresh under load**: the engine re-issues the token in the
  background at ~80% of `expires_in` and injects the fresh one into every virtual
  user, so long runs don't 401.
- **mTLS**: client certificate (PEM + key), custom CA, optional server-cert
  verification off (dev).

**Load testing**
- **Single request** — replays the current request as-is with virtual users,
  duration, RPS limit and timeout; live RPS + latency-percentile charts each
  second; Rust/tokio engine with HdrHistogram (exact p50/p75/p90/p95/p99).
- **Service load (multi-endpoint)** — pick several endpoints of a collection,
  set a **per-endpoint RPS**, run them concurrently (open-model dispatcher), get
  per-endpoint and aggregate stats + a report.
- **Request chaining (streams)** — a run of parallel independent chains; each
  chain is ordered steps with its own RPS (chain iterations/sec, open model).
  Extract a value from a step's response (JSON path / header / regex) into a
  `{{name}}` var and reuse it in later steps of the same iteration. Reported
  three levels deep (overall → per chain → per step); a stream's RPS is the load
  on its **target (last) endpoint**. The CLI gates per-stream chain completion
  via `--min-success-rate`.
- **Reports**: a self-contained HTML file (inline SVG charts — verdict, metric
  cards, throughput & latency, response-time distribution, status codes,
  per-second timeline) + raw JSON export.

**Protocols**
- **gRPC** from a `.proto` with **no protoc** (dynamic via prost-reflect/protox):
  list methods, prefill the request from the message schema, unary + server /
  client / bidi streaming, and gRPC load. **TLS** with a custom CA and a client
  certificate (mTLS) is supported. Missing imports are auto-resolved by
  searching the tree; manual "import folders" also supported.
- **WebSocket**: connect, send, receive, and load (each virtual user holds the
  connection and measures message → response time).

**Data & databases**
- **Database as a data source and load target** — Postgres / MySQL (MariaDB) /
  SQLite via `sqlx` native drivers: run SQL to a table, use a query's rows as
  request data (streamed, works for ~1M rows), or load-test the query with a
  connection pool. "Test connection" button.
- **Datasets** — reference `{{$data.name.column}}` from CSV/JSON files, a URL/S3
  object, or a DB query; per-row round-robin or random.
- **Value generators** (no dataset needed) — `{{$randomInt(a,b)}}`,
  `{{$randomFrom(a|b|c)}}`, `{{$randomString(n)}}`, `{{$uuid}}`, `{{$timestamp}}`,
  `{{$counter}}`.
- **File pools** for multipart uploads (folder / list / URL-S3).

**QA**
- **Response assertions**: status, latency, format, and a specific JSON field —
  built for dynamic data (single-send).
- **OpenAPI / Swagger import** — feed a `.json`/`.yaml` (Swagger 2.0 or OAS
  3.0/3.1); a collection is generated per endpoint (URL, params, body from
  schema, auth from `security`).

**Secrets & CI**
- Mark environment variables **🔒 Secret**; on **"↓ Export for CI"** they become
  `${KEY}` placeholders (the CLI injects each system's value from an OS env var),
  keeping secrets out of the committed config. An export dialog lets you rename
  the placeholders and shows the exact list to set in the cluster.
- Export for CI is available both from the **Service load** panel and the
  single-request **Load** tab.
- Sweeping structured logging with **secret redaction** everywhere (tokens,
  passwords in userinfo, S3 `X-Amz-*` signatures).

**Other**
- **i18n**: English is the base/source language, with an EN/RU switch (persisted).

### Architecture

A Cargo workspace with a UI-agnostic engine reused by both the app and the CLI,
plus a React frontend driven over Tauri IPC.

```
core/       maelstrom-core   — UI-agnostic engine (shared by app + CLI)
db/         maelstrom-db     — SQL layer (Postgres/MySQL/SQLite via sqlx)
grpc/       maelstrom-grpc   — dynamic gRPC (prost-reflect/protox/tonic, no protoc)
cli/        maelstrom-cli    — headless runner, bin `maelstrom`
src-tauri/  maelstrom-app    — Tauri desktop backend (lib `maelstrom_lib`)
src/                         — React + TypeScript frontend (Vite)
```

**`core` modules** — `types` (shared specs), `scenario` (multi-endpoint
open-model runner + aggregator), `oauth` (token fetch + auto-refresh), `tls`
(reqwest client with mTLS/CA), `multipart` (file uploads), `dynval` (per-request
generators + dataset providers), `histogram` (HdrHistogram percentiles), `ws`
(WebSocket call/load), `report` (standalone HTML), `redact` (secret masking).

**`src-tauri` modules (IPC commands)** — `http_client` (single send),
`loadtest` (single-request load engine: tokio workers, global RPS semaphore,
per-second aggregator/events), `scenario` (multi-endpoint load), `oauth`, `tls`,
`db` (SQL exec + DB load + resolving DB-backed datasets), `grpc`, `ws`, `log`
(redacted logging), `storage` (atomic `state.json` persistence + `.bak`
fallback, file read/write).

**Frontend (`src/`)** — `App.tsx` (state, request assembly, `{{var}}`
substitution), `api.ts` (typed `invoke` wrappers), `i18n.tsx` + `locale/ru.ts`
(English-key i18n with RU overlay), `vars.ts` (var resolution + secret→`${KEY}`
export), `openapi.ts` (OpenAPI→collection), `report.ts` / `charts.ts` (HTML
report + SVG charts), `assertions.ts`, `config.ts`, `types.ts`, and
`components/` (RequestEditor, LoadTestPanel, ScenarioPanel, GrpcEditor, WsEditor,
DatasetsModal, EnvironmentModal, AssertionsEditor, ExportConfigModal, Sidebar,
result views, …).

### Repository layout

```
core/ db/ grpc/ cli/ src-tauri/   Rust workspace
src/                              React frontend
community/                        public releases-repo content (README, issue templates, release.yml template)
deploy/                           CI/k8s runner docs + manifests (Job/CronJob, GitHub Actions, GitLab CI)
examples/demo-api/                Cloudflare Worker demo API (OpenAPI) + a load scenario
.github/workflows/                CI (see below)
portable/                         portable .exe output
testdata/ samples/                fixtures
```

### CLI

The headless runner (`maelstrom`) takes a JSON scenario — **exported from the app**
(Service load / Load tab → "↓ Export for CI") or hand-written — and gates the
pipeline on thresholds.

Config shape:
```jsonc
{
  "name": "orders-service",
  "duration_secs": 120,
  "timeout_ms": 10000,
  "targets": [                      // HTTP scenario (per-endpoint rps required, > 0)
    { "name": "list", "method": "GET", "url": "https://api.internal/v1/orders", "rps": 200,
      "headers": [["Content-Type","application/json"]], "body": "…",
      "auth_refresh": { "grant_type": "client_credentials", "token_url": "…",
                        "client_id": "loadtest", "client_secret": "${OAUTH_CLIENT_SECRET}" } }
  ],
  "datasets": [ /* CSV/JSON/S3/DB */ ],
  "file_pools": [ /* uploads */ ],
  "grpc":      { /* or a gRPC load block: endpoint, proto_path, service, method, body, vus, rps_limit, tls */ },
  "websocket": { /* or a WebSocket load block: url, message, vus, rps_limit */ },
  "streams":   [ /* or request chains: { name, rps, steps:[{ name, method, url, body, extract:[{ name, from, expr }] }] } */ ],
  "thresholds": { "max_error_rate": 1.0, "max_p95_ms": 400, "min_success_rate": 99.0 }
}
```
`${VAR}` anywhere is expanded from the environment at run time (JSON-escaped).

Flags: `--out-json`, `--out-html`, `--duration N`, `--max-error-rate P`,
`--max-p95 MS`, `--min-success-rate P` (per-stream chain completion),
`--quiet`, `--log-file PATH` (secrets redacted).
Exit codes: **0** thresholds passed · **1** breached · **2** config/startup error.

Distribution: standalone binaries (Windows/macOS/Linux) on the release, and a
container image `ghcr.io/slakertop1/maelstrom-cli` (`:latest` + versioned tag).

### Build & develop

Prereqs: Node.js 20+, Rust (stable). Windows: MSVC Build Tools. macOS: Xcode CLT.

```bash
npm install
npm run tauri dev                       # desktop dev
npm run tauri build                     # app installer (msi/portable on Win, dmg/app on macOS)
cargo build --release -p maelstrom-cli  # CLI -> target/release/maelstrom
docker build -t maelstrom-cli .         # CLI container image
cd examples/demo-api && wrangler deploy # deploy the demo API (Cloudflare Worker)
```

### Tests

- **Rust** — `cargo test` (engine: data generators, datasets, multipart,
  histogram/percentiles, scenario integration against an in-process server;
  db/grpc; app: atomic storage).
- **Frontend** — `npm test` (Vitest): OpenAPI import, request migration,
  datasets, chart formatting/regression, assertions, export-for-CI helpers, and
  **App.flow** — a full UI flow (compose → send → assert response; load tab →
  run → report) with the Tauri backend mocked.

### CI / workflows (`.github/workflows/`)

- **`ci.yml`** — on every push/PR: clippy `-D warnings`, Rust tests, tsc +
  Vitest, Tauri Linux compile, CLI Docker build.
- **`release.yml`** — on tag `vX.Y.Z` (or manual dispatch): builds the desktop
  app (Windows + macOS) and the CLI (win/mac/linux) and pushes them to the
  **public releases repo**, plus the CLI image to GHCR. Needs the `RELEASE_TOKEN`
  secret (fine-grained PAT, Contents: R/W on the public repo).
- **`docker-cli.yml`** — build & push the CLI image to GHCR (built-in
  `GITHUB_TOKEN`, no extra secret).
- **`cli-binaries.yml`** — build the CLI for win/mac/linux as workflow artifacts.
- **`build-macos.yml`** — build the macOS app (artifact).
- **`demo-loadtest.yml`** — a real, runnable pipeline that load-tests the demo
  API with the CLI and gates on thresholds.

### Distribution model

**Public code repo** (this, `slakertop1/maelstrom`) + a **releases/landing repo**
(`slakertop1/maelstrom-releases`: user-facing README, Issues, Discussions, and the
published Releases with the desktop/CLI binaries). The CLI image lives in GHCR.
The version-by-version update history is in [`CHANGELOG.md`](CHANGELOG.md).

**Cutting a release** (SemVer; `0.x` — MINOR for features, PATCH for fixes):
1. Bump `version` in `core/db/grpc/cli/src-tauri` `Cargo.toml`,
   `src-tauri/tauri.conf.json`, and the image tag in `docker-cli.yml`.
2. Add a section to [`CHANGELOG.md`](CHANGELOG.md).
3. Commit, tag `vX.Y.Z`; CI builds and publishes the assets, or do it manually.

### Roadmap

Done: HTTP client, OAuth2/SSO + auto-refresh, mTLS (HTTP & gRPC), single &
multi-endpoint load, **request chaining (streams)**, **gRPC**, **WebSocket**,
DB as source + load, response assertions, datasets & generators, OpenAPI/Swagger
import, HTML reports, i18n (EN/RU), CLI + Docker distribution, Export for CI.

Planned:
- **Kafka** — produce/consume + topic load (rdkafka).
- **Load profiles** — staged ramp-up, stages within a run.
- **Request history**.
- **Request chaining** — extract a value from a response into the next request.
- **DriftCheck** — snapshot an endpoint's response schema and diff it on later
  runs (new/removed field, type change) to catch silent backend changes without
  an OpenAPI spec.

---

## Русский

### Обзор

Maelstrom — десктопный клиент в духе Postman (собрать/отправить/разобрать запрос)
с полноценным движком **нагрузочного тестирования**, расширенным на gRPC,
WebSocket и SQL-базы. Тот же движок поставляется как **headless-CLI**: сценарий,
настроенный в приложении, без изменений гоняется в пайплайне или Kubernetes Job и
гейтит сборку по порогам через код выхода.

Две аудитории, один движок:
- **Десктоп** — интерактивно: собрал запрос, проверил ответ, прогнал под
  нагрузкой, прочитал HTML-отчёт.
- **CLI** — без интерфейса: воспроизводит сценарий в CI, отдаёт ненулевой код при
  превышении порога.

### Возможности

**HTTP-клиент**
- Все методы, query-параметры, заголовки, тело (JSON / текст / form-urlencoded /
  `multipart/form-data` с файлами), просмотр ответа с подсветкой JSON.
- Коллекции (автосохранение), окружения с подстановкой `{{var}}` в URL,
  заголовках, параметрах, теле, OAuth-полях и строке подключения к БД.

**Авторизация и TLS**
- Bearer, Basic и **OAuth 2.0 / SSO** — гранты `client_credentials`, `password`,
  `refresh_token`, `authorization_code` + PKCE (вход через системный браузер с
  локальным приёмником redirect).
- **Авто-обновление токена под нагрузкой**: движок фоново перевыпускает токен на
  ~80% `expires_in` и подставляет свежий всем виртуальным пользователям — длинные
  прогоны не сыпятся в 401.
- **mTLS**: клиентский сертификат (PEM + ключ), свой CA, опционально отключение
  проверки серверного сертификата (dev).

**Нагрузочное тестирование**
- **Одиночный запрос** — воспроизводит текущий запрос как есть: виртуальные
  пользователи, длительность, лимит RPS, таймаут; живые графики RPS и перцентилей
  латентности каждую секунду; движок на Rust/tokio с HdrHistogram (точные
  p50/p75/p90/p95/p99).
- **Нагрузка сервиса (мультиэндпоинт)** — выбираешь несколько ручек коллекции,
  задаёшь **свой RPS на каждую**, гоняешь параллельно (open-model диспетчер),
  получаешь статистику по каждой и суммарно + отчёт.
- **Отчёты**: автономный HTML (встроенные SVG-графики — вердикт, карточки метрик,
  пропускная способность и латентность, распределение времени ответа, коды
  ответов, посекундная детализация) + экспорт сырого JSON.

**Протоколы**
- **gRPC** по `.proto` **без protoc** (динамически, prost-reflect/protox): список
  методов, преднаполнение запроса по схеме сообщения, unary + server / client /
  bidi-стриминг, нагрузка. Недостающие импорты подбираются автоматически поиском
  по дереву; есть и ручные «папки импорта».
- **WebSocket**: подключение, отправка, приём и нагрузка (каждый VU держит
  соединение и меряет время сообщение → ответ).

**Данные и базы**
- **БД как источник данных и цель нагрузки** — Postgres / MySQL (MariaDB) /
  SQLite через нативные драйверы `sqlx`: выполнить SQL в таблицу, использовать
  строки запроса как данные (стримингом, до ~1 млн строк), или нагрузить запрос
  пулом соединений. Кнопка «Тест подключения».
- **Датасеты** — `{{$data.имя.колонка}}` из CSV/JSON, URL/S3-объекта или
  SQL-запроса; по порядку (round-robin) или случайно.
- **Генераторы значений** (без датасета) — `{{$randomInt(a,b)}}`,
  `{{$randomFrom(a|b|c)}}`, `{{$randomString(n)}}`, `{{$uuid}}`, `{{$timestamp}}`,
  `{{$counter}}`.
- **Пулы файлов** для multipart-загрузок (папка / список / URL-S3).

**QA**
- **Проверки ответа (assertions)**: статус, время, формат и конкретное
  JSON-поле — под динамичные данные (одиночная отправка).
- **Импорт OpenAPI / Swagger** — скармливаешь `.json`/`.yaml` (Swagger 2.0 или
  OAS 3.0/3.1); на каждый endpoint создаётся запрос (URL, параметры, тело по
  схеме, авторизация из `security`).

**Секреты и CI**
- Помечаешь переменные окружения **🔒 Secret**; при **«↓ Export for CI»** они
  становятся `${KEY}` (CLI подставляет значение из OS-переменной каждой системы),
  так секреты не попадают в коммитнутый конфиг. Диалог экспорта позволяет
  переименовать плейсхолдеры и показывает точный список переменных для кластера.
- Экспорт для CI доступен и из панели **«Нагрузка сервиса»**, и с вкладки
  **Load** одиночного запроса.
- Сквозное структурированное логирование с **маскировкой секретов** везде
  (токены, пароли в userinfo, S3-подписи `X-Amz-*`).

**Прочее**
- **i18n**: английский — базовый язык, переключатель EN/RU (сохраняется).

### Архитектура

Cargo-воркспейс с UI-независимым движком, переиспользуемым приложением и CLI,
плюс React-фронтенд поверх Tauri IPC.

```
core/       maelstrom-core   — UI-независимый движок (общий для app + CLI)
db/         maelstrom-db     — слой SQL (Postgres/MySQL/SQLite через sqlx)
grpc/       maelstrom-grpc   — динамический gRPC (prost-reflect/protox/tonic, без protoc)
cli/        maelstrom-cli    — headless-раннер, бинарь `maelstrom`
src-tauri/  maelstrom-app    — бэкенд десктопа на Tauri (lib `maelstrom_lib`)
src/                         — фронтенд React + TypeScript (Vite)
```

**Модули `core`** — `types` (общие спеки), `scenario` (мультиэндпоинт
open-model раннер + агрегатор), `oauth` (получение + авто-обновление токена),
`tls` (reqwest-клиент с mTLS/CA), `multipart` (загрузка файлов), `dynval`
(генераторы + провайдеры датасетов на запрос), `histogram` (перцентили
HdrHistogram), `ws` (WebSocket вызов/нагрузка), `report` (автономный HTML),
`redact` (маскировка секретов).

**Модули `src-tauri` (IPC-команды)** — `http_client` (одиночная отправка),
`loadtest` (движок нагрузки одиночного запроса: воркеры tokio, глобальный
RPS-семафор, посекундный агрегатор/события), `scenario` (мультиэндпоинт-нагрузка),
`oauth`, `tls`, `db` (выполнение SQL + нагрузка на БД + резолв БД-датасетов),
`grpc`, `ws`, `log` (логирование с маскировкой), `storage` (атомарная запись
`state.json` + fallback `.bak`, чтение/запись файлов).

**Фронтенд (`src/`)** — `App.tsx` (состояние, сборка запросов, подстановка
`{{var}}`), `api.ts` (типизированные обёртки `invoke`), `i18n.tsx` +
`locale/ru.ts` (i18n с английскими ключами и русским оверлеем), `vars.ts`
(резолв переменных + экспорт секрет→`${KEY}`), `openapi.ts` (OpenAPI→коллекция),
`report.ts` / `charts.ts` (HTML-отчёт + SVG-графики), `assertions.ts`,
`config.ts`, `types.ts`, и `components/` (RequestEditor, LoadTestPanel,
ScenarioPanel, GrpcEditor, WsEditor, DatasetsModal, EnvironmentModal,
AssertionsEditor, ExportConfigModal, Sidebar, вьюхи результатов, …).

### Структура репозитория

```
core/ db/ grpc/ cli/ src-tauri/   Rust-воркспейс
src/                              React-фронтенд
community/                        контент публичного репо (README, шаблоны issue, шаблон release.yml)
deploy/                           доки/манифесты раннера CI/k8s (Job/CronJob, GitHub Actions, GitLab CI)
examples/demo-api/                demo-API на Cloudflare Worker (OpenAPI) + сценарий нагрузки
.github/workflows/                CI (см. ниже)
portable/                         портативный .exe
testdata/ samples/                фикстуры
```

### CLI

Headless-раннер (`maelstrom`) принимает JSON-сценарий — **экспортированный из
приложения** (Нагрузка сервиса / вкладка Load → «↓ Export for CI») или написанный
руками — и гейтит пайплайн по порогам.

Форма конфига:
```jsonc
{
  "name": "orders-service",
  "duration_secs": 120,
  "timeout_ms": 10000,
  "targets": [                      // HTTP-сценарий (rps на ручку обязателен, > 0)
    { "name": "list", "method": "GET", "url": "https://api.internal/v1/orders", "rps": 200,
      "headers": [["Content-Type","application/json"]], "body": "…",
      "auth_refresh": { "grant_type": "client_credentials", "token_url": "…",
                        "client_id": "loadtest", "client_secret": "${OAUTH_CLIENT_SECRET}" } }
  ],
  "datasets": [ /* CSV/JSON/S3/БД */ ],
  "file_pools": [ /* загрузки */ ],
  "grpc":      { /* или блок gRPC-нагрузки: endpoint, proto_path, service, method, body, vus, rps_limit */ },
  "websocket": { /* или блок WebSocket-нагрузки: url, message, vus, rps_limit */ },
  "thresholds": { "max_error_rate": 1.0, "max_p95_ms": 400 }
}
```
`${VAR}` в любом месте подставляется из окружения на запуске (с JSON-экранированием).

Флаги: `--out-json`, `--out-html`, `--duration N`, `--max-error-rate P`,
`--max-p95 MS`, `--quiet`, `--log-file PATH` (секреты маскируются).
Коды выхода: **0** пороги пройдены · **1** превышены · **2** ошибка конфига/запуска.

Распространение: отдельные бинарники (Windows/macOS/Linux) в релизе и образ
`ghcr.io/slakertop1/maelstrom-cli` (`:latest` + версионный тег).

### Сборка и разработка

Требуется: Node.js 20+, Rust (stable). Windows: MSVC Build Tools. macOS: Xcode CLT.

```bash
npm install
npm run tauri dev                       # dev десктопа
npm run tauri build                     # установщик (msi/портативка на Win, dmg/app на macOS)
cargo build --release -p maelstrom-cli  # CLI -> target/release/maelstrom
docker build -t maelstrom-cli .         # контейнер CLI
cd examples/demo-api && wrangler deploy # деплой demo-API (Cloudflare Worker)
```

### Тесты

- **Rust** — `cargo test` (движок: генераторы данных, датасеты, multipart,
  гистограмма/перцентили, интеграционный `scenario` против встроенного сервера;
  db/grpc; app: атомарное хранилище).
- **Фронтенд** — `npm test` (Vitest): импорт OpenAPI, миграция запросов,
  датасеты, форматирование/регресс графиков, assertions, хелперы export-for-CI, и
  **App.flow** — полный UI-флоу (собрать → отправить → проверить ответ; вкладка
  нагрузки → запустить → отчёт) с замоканным Tauri-бэкендом.

### CI / workflow (`.github/workflows/`)

- **`ci.yml`** — на каждый push/PR: clippy `-D warnings`, Rust-тесты, tsc +
  Vitest, компиляция Tauri под Linux, сборка Docker-образа CLI.
- **`release.yml`** — по тегу `vX.Y.Z` (или ручным запуском): собирает десктоп
  (Windows + macOS) и CLI (win/mac/linux) и выкладывает в **публичный релиз-репо**,
  плюс образ CLI в GHCR. Нужен секрет `RELEASE_TOKEN` (fine-grained PAT, Contents:
  R/W на публичный репо).
- **`docker-cli.yml`** — сборка и пуш образа CLI в GHCR (встроенный
  `GITHUB_TOKEN`, без доп. секретов).
- **`cli-binaries.yml`** — сборка CLI под win/mac/linux как артефактов.
- **`build-macos.yml`** — сборка macOS-приложения (артефакт).
- **`demo-loadtest.yml`** — реальный запускаемый пайплайн: нагрузка на demo-API
  через CLI с гейтом по порогам.

### Модель распространения

**Публичный репо с кодом** (этот, `slakertop1/maelstrom`) + **релиз-репо/лендинг**
(`slakertop1/maelstrom-releases`: пользовательский README, Issues, Discussions и
опубликованные Releases с бинарниками десктопа/CLI). Образ CLI — в GHCR.
Полная история версий — в [`CHANGELOG.md`](CHANGELOG.md).

**Выпуск релиза** (SemVer; `0.x` — MINOR на фичи, PATCH на фиксы):
1. Поднять `version` в `core/db/grpc/cli/src-tauri` `Cargo.toml`,
   `src-tauri/tauri.conf.json` и тег образа в `docker-cli.yml`.
2. Добавить секцию в [`CHANGELOG.md`](CHANGELOG.md).
3. Коммит, тег `vX.Y.Z`; CI собирает и публикует ассеты, или делаешь вручную.

### Дорожная карта

Сделано: HTTP-клиент, OAuth2/SSO + авто-обновление, mTLS, одиночная и
мультиэндпоинт-нагрузка, **gRPC**, **WebSocket**, БД как источник + нагрузка,
проверки ответа, датасеты и генераторы, импорт OpenAPI/Swagger, HTML-отчёты,
i18n (EN/RU), распространение CLI + Docker, Export for CI.

Планы:
- **Kafka** — produce/consume + нагрузка на топики (rdkafka).
- **Профили нагрузки** — ступенчатый ramp-up, стадии внутри прогона.
- **История запросов**.
- **Цепочки запросов** — вытащить значение из ответа в следующий запрос.
- **DriftCheck** — снапшот схемы ответа ручки и дифф на последующих прогонах
  (новое/пропавшее поле, смена типа), чтобы ловить тихие изменения бэкенда без
  OpenAPI-спеки.
