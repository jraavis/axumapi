---
title: Prelude
description: What use axumapi::prelude::* brings into scope.
---

`use axumapi::prelude::*;` is the intended import for application code.

## Re-exported items

From `axumapi-core`:

`ApiError`, `ApiResult`, `App`, `BackgroundTasks`, `Cookies`, `Dependency`,
`Depends`, `Form`, `FromRequest`, `FromRequestParts`, `Header`, `Html`,
`IntoResponse`, `Json`, `Message`, `MethodRouter`, `NoContent`, `Path`,
`PlainText`, `Provided`, `Query`, `Redirect`, `ResolveContext`, `Resource`,
`Route`, `ServerError`, `State`, `WebSocket`, `WebSocketUpgrade`,
`WithStatus`, `delete`, `get`, `head`, `options`, `patch`, `post`, `put`.

From `axumapi-macros`:

`Model`, `Schema`, `Validate`, `delete`, `get`, `head`, `model_hooks`,
`options`, `patch`, `post`, `put`, `routes`, `ws`.

From `axumapi-orm`:

`Db`, `DbType`, `Expr`, `Field`, `ForeignKey`, `Model`, `ModelOps`,
`OneToOne`, `OrmError`, `QuerySet`, `Related`.

From `axumapi-validation`:

`Schema`, `SchemaObject`, `SchemaRegistry`, `Validate`, `ValidationError`,
`ValidationResult`.

Also: `chrono::{DateTime, Utc}` and `serde::{Deserialize, Serialize}`.

The prelude also exposes `axumapi::prelude::orm`, which is the ORM module
plus `chrono`, `uuid`, and `rust_decimal`.

## Outside the prelude

These stay behind explicit paths (they are still public):

| Path | Contents |
|---|---|
| `axumapi::config` | `Settings`, `Secret`, `load`, `init_tracing` |
| `axumapi::cache` | `Cache`, `MemoryCache`, `RedisCache`, `RouteCache` |
| `axumapi::security` | `HttpBearer`, `HttpBasic`, `ApiKey`, `Security`, … |
| `axumapi::openapi` | OpenAPI model and `DocsConfig` |
| `axumapi::orm::signals` | `Signals`, `SignalEvent`, `Receiver` |
| `axumapi::receiver` | `#[receiver]` |
| `axumapi_cli` | `AppCli`, `CliSettings` (separate crate) |
| `axumapi_testkit` | `TestClient`, `TestDatabase` (dev-dependency) |
| `axumapi_backends::*` | `SqliteBackend`, `PgBackend`, … |

Macro support code lives in `axumapi::__private` and is not part of the
public API.

## See also

- [Core concepts](/axumapi/start/concepts/)
- [Crate map](/axumapi/reference/crates/)
- [API rustdoc](/axumapi/reference/rustdoc/)
