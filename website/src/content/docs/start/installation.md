---
title: Installation
description: Add axumapi to a Rust project, pick cargo features, and set the MSRV.
---

axumapi is a Cargo workspace. Until the first crates.io release, depend on the
git repository (or a path checkout).

```toml
[dependencies]
axumapi = { git = "https://github.com/jraavis/axumapi" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The `axumapi` crate is the public facade. Most applications depend only on it
and `use axumapi::prelude::*;`.

## Toolchain

| Requirement | Value |
|---|---|
| Rust edition | 2024 |
| MSRV | 1.92 |
| Async runtime | Tokio |

```bash
rustup toolchain install 1.92
```

## Cargo features

SQLite is always available. Other backends are opt-in on `axumapi-backends`
(re-exported through the workspace). Enable them on the crate that opens the
connection — typically your binary, an example, or `axumapi-cli`.

| Feature | What it unlocks | Live-test variable |
|---|---|---|
| *(default)* | SQLite `QuerySet` backend | none |
| `postgres` | PostgreSQL | `DATABASE_URL=postgres://...` |
| `mysql` | MySQL 8 | `MYSQL_URL=mysql://...` |
| `mongodb` | MongoDB (supported QuerySet subset) | `MONGODB_URL=mongodb://...` |
| `redis` | Redis key/hash/set client and `RedisCache` | `REDIS_URL=redis://...` |

A URL for a backend that was not compiled in is an error. Unsupported QuerySet
features fail with a `BackendCapabilityError` before any I/O. Redis is a typed
client, not a QuerySet backend.

See [Backends](/axumapi/guides/data/backends/) for the feature matrix.

## Workspace crates

| Crate | Role |
|---|---|
| `axumapi` | Facade and prelude |
| `axumapi-core` | App, routing, extractors, RFC 7807 errors |
| `axumapi-validation` | `Validate`, rules, constrained types, `Schema` |
| `axumapi-orm` | `Model`, `QuerySet`, relations, transactions |
| `axumapi-backends` | SQLite, PostgreSQL, MySQL, MongoDB, Redis |
| `axumapi-macros` | Route attributes, `routes![]`, derives |
| `axumapi-openapi` | OpenAPI 3.1 document and UIs |
| `axumapi-migrations` | Autodetector, JSON migrations, schema editor |
| `axumapi-config` | Layered settings, `Secret`, tracing |
| `axumapi-cache` | Memory/Redis cache, `RouteCache` |
| `axumapi-cli` | `AppCli` and the standalone `axumapi` binary |
| `axumapi-testkit` | In-process `TestClient` and `TestDatabase` |

The full map and dependency rules live in [Crate map](/axumapi/reference/crates/).

## Clone and run the examples

```bash
git clone https://github.com/jraavis/axumapi
cd axumapi
cargo run -p hello_world
```

Open [http://127.0.0.1:8000/hello/ann](http://127.0.0.1:8000/hello/ann) and
[http://127.0.0.1:8000/docs](http://127.0.0.1:8000/docs) (Swagger UI).

PostgreSQL, MySQL, MongoDB, and Redis examples need the matching URL. Start
them from the repository `docker-compose.yml`:

```bash
docker compose up -d --wait
```

The compose header lists host ports and credentials. Details are in
[Testing](/axumapi/guides/production/testing/).

## Next

- [First application](/axumapi/start/first-app/) — walk the Hello World app
- [Core concepts](/axumapi/start/concepts/) — `App`, prelude, `Db`, `Schema` vs `Model`
