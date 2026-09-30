---
title: Dependency injection
description: Depends, request-scoped caching, provide, overrides, global dependencies, and async teardown.
---

siderite’s DI is inspired by FastAPI’s `Depends`, expressed with plain Rust
traits. Items live in `siderite_core::di`.

## Declaring a dependency

```rust
struct Db { tenant: String }

impl Dependency for Db {
    async fn resolve(ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        let config = ctx.resolve::<Config>().await?;
        Ok(Db { tenant: config.tenant.clone() })
    }
    fn describe(op: &mut Operation, r: &mut SchemaRegistry) { /* OpenAPI */ }
}

async fn handler(db: Depends<Db>) -> String { db.tenant.clone() }
```

`Depends<T>` derefs to `T` and holds an `Arc<T>`.

## Semantics

- **Request-scoped and cached.** Within one request a type resolves once, no
  matter how many arguments or nested dependencies ask for it. Failures are
  not cached.
- **Cycles** (`A -> B -> A`) produce an opaque 500 problem; the logged
  error is `DependencyError::Cycle { chain }`. OpenAPI generation is also
  cycle-safe.
- **Application scope:** `App::provide(value)` / `provide_arc(arc)` share a
  value. It wins over `T::resolve` for `Depends<T>`. For types that are not
  dependencies, use the `Provided<T>` extractor.
- **Overrides (tests):** `App::override_dependency::<T, _, _>(|head| async { .. })`
  and `App::override_value(value)` replace `T::resolve`, including when `T`
  is requested by another dependency. The closure receives an owned
  `RequestHead` (method, URI, headers); a borrowed `Parts` cannot cross the
  returned future.
- **Global dependencies:** `App::dependency::<T>()` resolves `T` before
  every routed request of the app and its mounts; its error short-circuits
  (for example 401). 404s and the generated docs endpoints are not covered.
  The value stays cached for handlers. Global dependencies are not yet
  reflected in OpenAPI.
- **Mounts:** a child sees its parent’s registrations; the child’s own take
  precedence inside the child.
- **Per-route dependencies** are not a separate feature: add a `Depends<T>`
  argument (an unused `_auth: Depends<Auth>` is enough).

`TestClient::builder` exposes the same override helpers. See
[Testing](/siderite/guides/production/testing/).

## Teardown

`Drop` cannot be async, so register cleanup with
`ctx.on_teardown(async move { .. })` (output `()` or `Result<(), E: Display>`).
Teardowns run **after the response is produced**, in **LIFO** order, on a
spawned task. Failures and panics are logged and do not stop the remaining
teardowns.

Teardown does not wait for a streaming body to finish, and is skipped (with
a warning) if no Tokio runtime is available.

## See also

- [Extractors and responses](/siderite/guides/http/extractors/)
- [Security](/siderite/guides/http/security/)
- [Testing](/siderite/guides/production/testing/)
