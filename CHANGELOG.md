# Changelog

## [Unreleased]

### Changed
- Site mark is a rhombohedral crystal (header logos and favicon), replacing the old triangle.
- GitHub Actions use Node 24-capable versions (`actions/checkout@v5`, `upload-pages-artifact@v5`, `deploy-pages@v5`).
- Renamed the project from `axumapi` to `siderite` (crates, rust paths, CLI binary, config file `siderite.toml`, env prefix `SIDERITE_`, migration history table `siderite_migrations`, docs site). Axum remains the HTTP engine.
- CLI serve command is `run` (was `runserver`). The `siderite` binary wraps `cargo run` in an application package and adds `siderite new`.

### Added
- `siderite build` runs `cargo build` in the app package and forwards its arguments, e.g. `siderite build --release`.
- GitHub Pages documentation site (`website/`, Astro Starlight) covering getting started, tutorials, HTTP/data/production guides, reference, internals, and contributing. Deployed from `.github/workflows/pages.yml` with rustdoc at `/api/`.

### Fixed
- **Migrations:** SQLite table rebuilds no longer CASCADE-delete child rows. The migrator turns `PRAGMA foreign_keys` off on the dedicated connection before `BEGIN` (SQLite ignores that pragma inside a transaction), runs `PRAGMA foreign_key_check` before commit, and restores the previous value afterwards.
- **Migrations:** `migrate` and `rollback` take a backend lock (PostgreSQL `pg_advisory_lock`, MySQL `GET_LOCK`, SQLite `BEGIN IMMEDIATE`) and re-read history under it so concurrent replicas cannot double-apply.
- **Migrations:** a MySQL migration that fails after earlier DDL has committed reports `MigrationError::MysqlPartial` with the 1-based statement index, because MySQL cannot roll the earlier statements back.
- **Migrations:** `RenameHints::rename_model` emits `RenameModel` (and `ALTER TABLE … RENAME TO` when the table name changes). An unhinted delete+create of a same-shaped table, or remove+add of a same-shaped column, is refused so data is not dropped. **Breaking:** `diff` / `diff_with` return `Result`.
- **Migrations:** `RunRust` operations run in the declared order among SQL operations, including on reverse.
- **Migrations:** lock-held PostgreSQL/MySQL migration connections are closed instead of returned to the pool, so a session lock that was not released can never be handed to another caller.
- **Migrations:** the SQLite pre-commit `foreign_key_check` fails only on *new* violations and is skipped when foreign keys were already off, so pre-existing violations in unrelated tables no longer block every migration. The baseline is diffed row by row (child table, rowid, parent), so a migration that repairs one violation while adding another fails instead of passing on an unchanged count.
- **Migrations:** documented that SQLite `RunRust` data code runs with foreign keys off and must stay FK-clean by hand; the commit error names the offending table and row.
- **Migrations:** documented that a SQLite `migrate`/`rollback` run is a single all-or-nothing transaction and removed the now-dead per-migration SQLite branches in the executor.
- **Migrations:** MySQL `GET_LOCK` now waits indefinitely for the migration lock, matching PostgreSQL `pg_advisory_lock`, so a slow migration no longer makes other replicas time out and crash-loop.
- **Migrations:** MySQL records per-statement progress (`siderite_migration_progress`) and resumes a failed migration at the first statement that did not commit, including inside one operation (a `CreateModel` with indexes), instead of replaying committed statements; a `RunRust` failure after committed statements or after another `RunRust` wrote data reports `MigrationError::MysqlOpPartial` naming the operation.
- **Migrations:** `MysqlPartial` is raised only when earlier DDL actually committed (first-keyword check), so a failure after non-DDL statements reports the plain error instead of a misleading partial.
- **Migrations:** `RenameHints::allow_drop_model` / `allow_drop_field` approve an intentional drop that collides with a same-shaped create, so the refusal no longer forces a two-migration split. The shape predicate stays conservative on purpose: weakening it would silently drop data on genuine renames.
- **Migrations:** `RenameModel` retargets other models' foreign keys that pointed at the old table, so hand-written renames leave project state consistent.
- **Migrations:** `RenameModel` also retargets the renamed model's own self-referencing foreign keys (a `parent_id` tree), which previously kept pointing at the old table name.
- **Migrations:** `Report.sql` is rendered under the migration lock from the re-read plan, so a concurrent replica applying migrations in between no longer desyncs it from `Report.planned`.
- **RouteCache is opt-in.** It stores only responses marked `Cache-Control: public` (new `Cached::public(ttl, response)` helper), or every response of a route whose layer sets `RouteCache::default_ttl`. Entry lifetime follows `s-maxage`/`max-age`, and `no-cache` responses are no longer stored. New `MethodRouter::layer` applies middleware to a single route. `Cached` is in the prelude, and the `todo_sqlite` example caches `GET /todos`. **Breaking:** `RouteCache::new` takes only the cache; use `.default_ttl(ttl)` for the old store-everything behaviour.
- **Extractors:** `Option<T>` is `None` only when the input is absent (a missing header, query string or credential). Present but invalid input now fails with `T`'s error instead of becoming `None`. `ApiError::absent()` / `is_absent()` mark and detect such errors. **Breaking:** `Option<Security<..>>` with an invalid token returns `401` instead of treating the request as anonymous, and custom extractors must mark their missing-input error with `.absent()` to keep returning `None`.
- Security review fixes:
  - **Body:** `Body::into_bytes` stops at `DEFAULT_BODY_LIMIT` (2 MiB) instead of buffering without limit; `Body::into_bytes_limited` sets another cap. `BodyError::is_too_large` reports the overflow and `?` into `ApiError` returns a `413` problem response. **Breaking:** callers reading larger bodies must use `into_bytes_limited`.
  - **Config:** secret detection in error messages matches key substrings and suffixes (`database_url`, `access_token`, `private-key`, `smtp_pass`, `*_dsn`, ...), and out-of-range integer errors are redacted too.
  - **Validation:** `#[field(url)]` now rejects values that are not absolute URLs (`url_parsing`). **Breaking:** `Constraint` has a new `Url` variant.
  - **RouteCache:** the key includes scheme and `Host` and is length-prefixed, so virtual hosts sharing a cache stay isolated and split header values cannot collide. Headers whose names look like credentials (`X-Auth-Token`, `X-Session-Id`, ...) bypass the cache by default; other credential headers still need `bypass_header`.
  - **Observability:** the docs routes (`/openapi.json`, `/docs`, `/redoc`) record their matched route in `http.request` spans instead of `<unmatched>`.
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
  - **Configuration:** `siderite-config` layers defaults, TOML, environment and overrides with figment. It supports per-alias database URLs and redacts `Secret` values. `init_tracing` installs a subscriber.
  - **Observability:** `http.request` spans record request id, method, matched route, status and latency. `orm.query` spans record query durations. Neither records bind parameters, headers or query strings.
  - **Security:** HTTP Bearer, HTTP Basic, API key (header, query or cookie) and OAuth2 password flow extractors document themselves in OpenAPI `securitySchemes`. `Security<T, S>` handles authentication and scopes, and `ApiError` can carry response headers.
  - **Signals:** `pre_save`, `post_save`, `pre_delete`, `post_delete` and `m2m_changed`, with explicit registration and `#[receiver]`.
  - **Database routing:** the `DatabaseRouter` trait, `Databases::{for_read, for_write, objects, using}` and `App::database`. Querysets bound to different databases cannot be combined.
  - **Cache:** the `siderite-cache` crate with an in-memory LRU cache (with TTL), a Redis cache and `RouteCache` middleware.
  - **CLI:** `AppCli` provides `run`, `routes`, `check`, `dbshell` and the migration commands. `CliSettings` can be built from `Settings`. The standalone binary connects to PostgreSQL and MySQL, and there is a MySQL schema editor.
  - **Testkit:** `TestDatabase` provides in-memory SQLite with models or migrations and rolled-back isolation. `TestClient::builder` adds DI overrides.
  - **Benchmarks:** Criterion benchmarks, with measured medians in `crates/siderite-bench/README.md`.
  - **Release tooling:** a CI workflow (lint, test, live databases, MSRV 1.92, cargo-deny), `docker-compose.yml` and the release guide.
- Phase 5 backends: MySQL 8 adapter (emulated `RETURNING`, dialect-aware compiler), MongoDB executor compiling the supported QuerySet subset to filters and aggregation pipelines (unsupported relational features return capability errors), and a typed Redis key/hash/set client. Live tests run when `MYSQL_URL`, `MONGODB_URL` or `REDIS_URL` is set.
- Phase 4 ORM: `#[derive(Model)]` with typed field constants, `QuerySet` (filter/order/annotate/aggregate/windows/set operations), `ForeignKey` / `OneToOne` / many-to-many, `select_related` / `prefetch_related`, instance `save` / `delete` / `refresh`, transactions and savepoints. `ForeignKey` implements `Validate`, `Schema` and `Dump` by delegating to the related primary key. PostgreSQL executor (`PgBackend`) and Django-style JSON migrations (autodetector, schema editor, CLI `migrate` / `rollback` / `showmigrations` / `squashmigrations`). Example: `todo_sqlite`.
- Phase 3 validation and serialization: type-driven `prepare` → Serde → `validate` pipeline reporting every error with its location; `#[derive(Validate)]`, `#[model_config]`, shared `#[field]` constraints; `#[model_hooks]` with field/model validators (before/after), computed fields and serializers; `Dump`/`DumpOptions`/`JsonDump`; extractors validate automatically (422); constrained URL, IP, UUID, decimal, float, integer and list types; validation guide and Pydantic equivalence table.
- Phase 2 HTTP framework: siderite-owned `Handler`/extractor/response traits with OpenAPI `describe` hooks; OpenAPI 3.1 generation (components/$ref reuse, validated against the official schema) with Swagger UI and ReDoc; route attribute macros, `routes![]` and `#[derive(Schema)]`; dependency injection (`Depends`, request-scoped caching, overrides, cycle detection, teardown); middleware (CORS, compression, trusted hosts, HTTPS redirect, request id, logging, timeout, concurrency, body and rate limits) with documented ordering; lifespan hooks and resources; forms, multipart, typed headers, cookies, redirects, streaming and file responses, WebSockets, background tasks, static files.
- Phase 1 foundation: workspace, QueryPlan IR, typed expressions, backend capabilities, SQL compiler (PostgreSQL/SQLite), SQLite executor, core HTTP app, validation primitives, testkit.
