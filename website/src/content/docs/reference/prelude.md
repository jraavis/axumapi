---
title: Prelude
description: What use siderite::prelude::* brings into scope.
---

`use siderite::prelude::*;` is the intended import for application code.

## Re-exported items

From `siderite-core`:

`ApiError`, `ApiResult`, `App`, `BackgroundTasks`, `Cached`, `Cookies`, `Dependency`,
`Depends`, `Form`, `FromRequest`, `FromRequestParts`, `Header`, `Html`,
`IntoResponse`, `Json`, `Message`, `MethodRouter`, `NoContent`, `Path`,
`PlainText`, `Provided`, `Query`, `Redirect`, `ResolveContext`, `Resource`,
`Route`, `ServerError`, `State`, `WebSocket`, `WebSocketUpgrade`,
`WithStatus`, `delete`, `get`, `head`, `options`, `patch`, `post`, `put`.

From `siderite-macros`:

`Model`, `Schema`, `Validate`, `delete`, `get`, `head`, `model_hooks`,
`options`, `patch`, `post`, `put`, `routes`, `ws`.

From `siderite-orm`:

`Db`, `DbType`, `Expr`, `Field`, `ForeignKey`, `Model`, `ModelOps`,
`OneToOne`, `OrmError`, `QuerySet`, `Related`.

From `siderite-validation`:

`Schema`, `SchemaObject`, `SchemaRegistry`, `Validate`, `ValidationError`,
`ValidationResult`.

Also: `chrono::{DateTime, Utc}` and `serde::{Deserialize, Serialize}`.

The prelude also exposes `siderite::prelude::orm`, which is the ORM module
plus `chrono`, `uuid`, and `rust_decimal`.

## Outside the prelude

These stay behind explicit paths (they are still public):

| Path | Contents |
|---|---|
| `siderite::config` | `Settings`, `Secret`, `load`, `init_tracing` |
| `siderite::cache` | `Cache`, `MemoryCache`, `RedisCache`, `RouteCache` |
| `siderite::security` | `HttpBearer`, `HttpBasic`, `ApiKey`, `Security`, … |
| `siderite::openapi` | OpenAPI model and `DocsConfig` |
| `siderite::orm::signals` | `Signals`, `SignalEvent`, `Receiver` |
| `siderite::receiver` | `#[receiver]` |
| `siderite_cli` | `AppCli`, `CliSettings` (separate crate) |
| `siderite_testkit` | `TestClient`, `TestDatabase` (dev-dependency) |
| `siderite_backends::*` | `SqliteBackend`, `PgBackend`, … |

Macro support code lives in `siderite::__private` and is not part of the
public API.

## See also

- [Core concepts](/siderite/start/concepts/)
- [Crate map](/siderite/reference/crates/)
- [API rustdoc](/siderite/reference/rustdoc/)
