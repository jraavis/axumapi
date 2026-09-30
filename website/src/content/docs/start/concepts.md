---
title: Core concepts
description: App, prelude, handlers, Db, and the difference between Schema and Model.
---

A few types show up in every axumapi application. This page names them so the
guides can assume them.

## `App`

`App` is the application: routes, mounts, middleware, state, databases, and
the OpenAPI document. You build it as a value and then either `run` it or
hand it to `AppCli` / `TestClient`.

```rust
App::new()
    .title("Hello")
    .version("1.0.0")
    .routes(routes![hello, echo])
    .run("127.0.0.1:8000")
    .await?;
```

Duplicate `(path, method)` pairs, paths without a leading `/`, and OpenAPI
conflicts (`operationId`, schema names) are reported when the app is built —
from `run()`, `into_router_service()`, `App::openapi()`, or
`TestClient::try_new`. They are not panics on the first request.

## Prelude

`use axumapi::prelude::*;` is the intended import for application code. It
brings in routing, extractors, responses, `Validate` / `Schema` / `Model`,
`Db` / `QuerySet` / `ForeignKey`, and `serde`’s `Serialize` / `Deserialize`.
The full list is in [Prelude](/axumapi/reference/prelude/).

Axum and SQLx types do not appear in the public API. `Json`, `Path`, `Query`,
and `State` are axumapi newtypes, so you can upgrade those internals without
a breaking change of your own.

## Handlers

A handler is an `async fn` that returns something implementing `IntoResponse`.
Every argument except the last implements `FromRequestParts`. The last
argument may implement `FromRequest` and consume the body (`Json<T>`,
`Form<T>`).

Those traits are axumapi’s own. Each extractor and response can implement a
`describe` hook so OpenAPI is derived from the signature.

## `Db`

`Db` is a cheap clone of a pool handle or of an open transaction. There is no
global connection registry.

```rust
let adults = User::objects(&db)
    .filter(User::age.ge(18))
    .all()
    .await?;
```

`User::objects(&db)` captures the handle. Terminal methods (`.all()`,
`.get()`, `.delete()`, …) use that handle and take no extra argument. The
same code runs inside `db.transaction(|tx| async move { … })` if you pass
`tx` instead of `db`.

Handlers typically receive `Db` through `Provided<Db>`, `Depends<Db>`, or
`State<Databases>`. See [Dependency injection](/axumapi/guides/http/di/) and
[Database routing](/axumapi/guides/data/database-routing/).

## `Schema` vs `Model`

| Derive | Crate | Job |
|---|---|---|
| `Validate` | validation | Coerce, check constraints, run hooks. Used by extractors. |
| `Schema` | validation | JSON Schema / OpenAPI component. Also implements `Dump` when the type is `Serialize`. |
| `Model` | ORM | Table metadata, field constants, `QuerySet`, persistence. |

One struct can derive all of them plus `Serialize` / `Deserialize`. The
shared `#[field(…)]` attribute is read by each derive for the keys it
understands; unknown keys are compile errors on `Model` and ignored on
`Schema` so the attributes can be shared.

`ForeignKey<T>` validates and documents itself as `T`’s primary key, not as
a nested object.

## Errors

Invalid input is `ValidationError` → HTTP 422 with an `errors` array.
Missing rows are `QueryError::DoesNotExist` → 404. Constraint failures are
409 (the database detail is logged, not returned). Everything else that is
an `ApiError` renders as RFC 7807 `application/problem+json`.

The map is in [Errors](/axumapi/guides/http/errors/).

## Capabilities

Lookups exist only on suitable field types, so `User::age.icontains("x")`
does not compile. Features a backend cannot run (`select_for_update` on
SQLite, joins on MongoDB) become `BackendCapabilityError` when the plan is
checked, before any I/O. Nothing is silently ignored.

## Next

- [Installation](/axumapi/start/installation/)
- [Architecture](/axumapi/internals/architecture/) — crate graph and ADRs
- [Examples](/axumapi/start/examples/)
