---
title: Crate map
description: Workspace crates, what each owns, and dependency rules.
---

The `axumapi` crate is the public facade. Application code depends on it
and `use axumapi::prelude::*;`. The other crates exist so HTTP, validation,
and the ORM do not form a cycle.

```
user application
        │
        ▼
     axumapi (facade + prelude)
        │
        ├── axumapi-core        App, routing, extractors, ApiError
        │       ├── axumapi-validation
        │       └── axumapi-orm
        ├── axumapi-openapi
        ├── axumapi-orm
        ├── axumapi-config
        └── axumapi-cache
                └── axumapi-core

axumapi-backends ──► axumapi-orm
axumapi-migrations ──► axumapi-orm
axumapi-macros  ·generates code for·  core, orm, validation
axumapi-cli ──► migrations, config
axumapi-testkit ──► core
```

| Crate | Owns |
|---|---|
| `axumapi` | Public facade, `prelude` |
| `axumapi-core` | `App`, handler/extractor/response traits, DI, middleware, lifespan, WebSockets, forms, headers/cookies, background tasks, static files, RFC 7807 errors, security schemes |
| `axumapi-validation` | `Validate`, structured `ValidationError`, rules, constrained newtypes, `Schema` + `SchemaRegistry` |
| `axumapi-orm` | `Model`, `QuerySet`, relations, transactions, signals, database routing, `QueryPlan` IR, `Expr`, capabilities |
| `axumapi-backends` | Dialect-aware SQL compiler and executors: SQLite, PostgreSQL, MySQL; MongoDB (supported QuerySet subset); Redis key/hash/set client |
| `axumapi-macros` | Route attributes, `routes![]`, `#[derive(Model, Validate, Schema)]`, `#[model_hooks]`, `#[receiver]` |
| `axumapi-openapi` | Typed OpenAPI 3.1 model, document builder, Swagger UI / ReDoc |
| `axumapi-migrations` | Migration graph, operations, autodetector, schema editor (PostgreSQL, SQLite, MySQL), executor |
| `axumapi-config` | Layered configuration, `Secret`, `init_tracing` |
| `axumapi-cache` | `Cache` trait, in-memory LRU, Redis cache, `RouteCache` middleware |
| `axumapi-cli` | `AppCli` and the standalone `axumapi` binary |
| `axumapi-testkit` | `TestClient`, `TestDatabase` |
| `axumapi-bench` | Criterion benchmarks (not published) |

## Dependency rules

1. **The ORM has no HTTP knowledge.** `axumapi-orm` does not depend on
   `axumapi-core`. The core crate implements `From<OrmError> for ApiError`.
2. **Schema metadata lives in validation.** `Schema` / `SchemaObject` sit
   in `axumapi-validation` so that core (request bodies) and openapi
   (documents) share one definition without a cycle.
3. **Backends depend on the ORM, never the reverse.** The ORM defines the
   `Backend` trait and the capability model; each backend implements them.
4. **HTTP traits are axumapi’s own.** `FromRequestParts`, `FromRequest`,
   `IntoResponse`, and `Handler` carry `describe` hooks.
5. **Axum and SQLx are internal.** Public types are axumapi newtypes
   (`Json`, `Path`, `Query`, `State`, `MethodRouter`, …).

## See also

- [Architecture](/axumapi/internals/architecture/)
- [Prelude](/axumapi/reference/prelude/)
- [API rustdoc](/axumapi/reference/rustdoc/)
