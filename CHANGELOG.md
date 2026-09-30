# Changelog

## [Unreleased]

### Fixed
- Phase 6 review fixes:
  - **RouteCache:** requests carrying `X-API-Key`, `Proxy-Authorization` or a header registered with `bypass_header` skip the cache. Keys include `Accept` and `Accept-Encoding`, and responses that `Vary` on other headers are not cached. Streaming bodies and bodies above `max_body_bytes` are never buffered. Cached `HEAD` responses keep the original `Content-Length`.
  - **Security:** the schemes of one handler form a single requirement object (all are required); `Option<Scheme>` adds an anonymous alternative.
  - **ORM:** subquery plans remember their database, and `Db` rejects queries and bulk writes containing a subquery from another database.
  - **dbshell:** a `?password=` URL parameter is moved out of `psql`'s arguments, and `sslpassword` is refused.
  - **CLI:** `AppCli::configure_db` and `AppCli::database_router` hooks; `blog_postgres` attaches its receivers there instead of on every request.
  - **Config:** `set()` overrides win regardless of call order.

### Added
- Examples `blog_postgres`, `todo_mongo` and `polyglot`, and the configuration and cache guides (`docs/CONFIG.md`, `docs/CACHE.md`).
- Phase 6 production tooling:
  - **Configuration:** `axumapi-config` layers defaults, TOML, environment and overrides with figment. It supports per-alias database URLs and redacts `Secret` values. `init_tracing` installs a subscriber.
  - **Observability:** `http.request` spans record request id, method, matched route, status and latency. `orm.query` spans record query durations. Neither records bind parameters, headers or query strings.
  - **Security:** HTTP Bearer, HTTP Basic, API key (header, query or cookie) and OAuth2 password flow extractors document themselves in OpenAPI `securitySchemes`. `Security<T, S>` handles authentication and scopes, and `ApiError` can carry response headers.
  - **Signals:** `pre_save`, `post_save`, `pre_delete`, `post_delete` and `m2m_changed`, with explicit registration and `#[receiver]`.
  - **Database routing:** the `DatabaseRouter` trait, `Databases::{for_read, for_write, objects, using}` and `App::database`. Querysets bound to different databases cannot be combined.
  - **Cache:** the `axumapi-cache` crate with an in-memory LRU cache (with TTL), a Redis cache and `RouteCache` middleware.
  - **CLI:** `AppCli` provides `runserver`, `routes`, `check`, `dbshell` and the migration commands. `CliSettings` can be built from `Settings`. The standalone binary connects to PostgreSQL and MySQL, and there is a MySQL schema editor.
  - **Testkit:** `TestDatabase` provides in-memory SQLite with models or migrations and rolled-back isolation. `TestClient::builder` adds DI overrides.
  - **Benchmarks:** Criterion benchmarks, with measured medians in `crates/axumapi-bench/README.md`.
  - **Release tooling:** a CI workflow (lint, test, live databases, MSRV 1.92, cargo-deny), `docker-compose.yml` and the release guide.
- Phase 5 backends: MySQL 8 adapter (emulated `RETURNING`, dialect-aware compiler), MongoDB executor compiling the supported QuerySet subset to filters and aggregation pipelines (unsupported relational features return capability errors), and a typed Redis key/hash/set client. Live tests run when `MYSQL_URL`, `MONGODB_URL` or `REDIS_URL` is set.
- Phase 4 ORM: `#[derive(Model)]` with typed field constants, `QuerySet` (filter/order/annotate/aggregate/windows/set operations), `ForeignKey` / `OneToOne` / many-to-many, `select_related` / `prefetch_related`, instance `save` / `delete` / `refresh`, transactions and savepoints. `ForeignKey` implements `Validate`, `Schema` and `Dump` by delegating to the related primary key. PostgreSQL executor (`PgBackend`) and Django-style JSON migrations (autodetector, schema editor, CLI `migrate` / `rollback` / `showmigrations` / `squashmigrations`). Example: `todo_sqlite`.
- Phase 3 validation and serialization: type-driven `prepare` → Serde → `validate` pipeline reporting every error with its location; `#[derive(Validate)]`, `#[model_config]`, shared `#[field]` constraints; `#[model_hooks]` with field/model validators (before/after), computed fields and serializers; `Dump`/`DumpOptions`/`JsonDump`; extractors validate automatically (422); constrained URL, IP, UUID, decimal, float, integer and list types; validation guide and Pydantic equivalence table.
- Phase 2 HTTP framework: axumapi-owned `Handler`/extractor/response traits with OpenAPI `describe` hooks; OpenAPI 3.1 generation (components/$ref reuse, validated against the official schema) with Swagger UI and ReDoc; route attribute macros, `routes![]` and `#[derive(Schema)]`; dependency injection (`Depends`, request-scoped caching, overrides, cycle detection, teardown); middleware (CORS, compression, trusted hosts, HTTPS redirect, request id, logging, timeout, concurrency, body and rate limits) with documented ordering; lifespan hooks and resources; forms, multipart, typed headers, cookies, redirects, streaming and file responses, WebSockets, background tasks, static files.
- Phase 1 foundation: workspace, QueryPlan IR, typed expressions, backend capabilities, SQL compiler (PostgreSQL/SQLite), SQLite executor, core HTTP app, validation primitives, testkit.
