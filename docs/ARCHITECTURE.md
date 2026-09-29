# axumapi Architecture

`axumapi` is an async-first Rust web framework. It takes its API ergonomics from FastAPI, its validation model from Pydantic v2 and its ORM ergonomics from the Django ORM. It favours **compilable, idiomatic Rust** over Python look-alike syntax: where a Python feature has no natural Rust mapping, we design a Rust equivalent and document how it differs.

> Status: **Phase 2 (HTTP framework) complete**. The table in [Crate map](#crate-map) says what is real and what is scaffolding.

## Crate map

```mermaid
flowchart TD
    user[User application] --> facade[axumapi<br/>facade + prelude]
    facade --> core[axumapi-core<br/>App, routing, extractors, ApiError]
    facade --> openapi[axumapi-openapi]
    facade --> orm
    core --> validation[axumapi-validation<br/>Validate, ValidationError, Schema]
    core --> orm[axumapi-orm<br/>QueryPlan IR, Expr, capabilities, Backend trait]
    core -. internal .-> axum[(axum / hyper / tower)]
    openapi --> validation
    backends[axumapi-backends<br/>SQL compiler, SQLite adapter] --> orm
    backends -. internal .-> sqlx[(SQLx)]
    migrations[axumapi-migrations] --> orm
    macros[axumapi-macros<br/>proc-macros] -. generates code for .-> core
    macros -. generates code for .-> orm
    cli[axumapi-cli] --> migrations
    testkit[axumapi-testkit] --> core
```

| Crate | Owns | Phase 1 status |
|---|---|---|
| `axumapi` | Public facade, `prelude` | Implemented |
| `axumapi-core` | `App`, own `Handler`/extractor/response traits, DI, middleware, lifespan, WebSockets, forms, headers/cookies, background tasks, static files, RFC 7807 errors | Implemented |
| `axumapi-validation` | `Validate`, structured `ValidationError`, rules, constrained newtypes, `Schema` + `SchemaRegistry` | Implemented (`Validate` derive: Phase 3) |
| `axumapi-orm` | `QueryPlan` IR, `Expr` AST, typed `Field<M, T>`, `BackendCapabilities`, `Backend` trait, ORM errors | Implemented |
| `axumapi-backends` | Dialect-aware SQL compiler (PostgreSQL, SQLite), SQLite executor | Implemented; PostgreSQL *execution* deferred |
| `axumapi-macros` | Route attributes, `routes![]`, `#[derive(Schema)]`; later `Validate`/`Model` | Route + Schema macros implemented |
| `axumapi-openapi` | Typed OpenAPI 3.1 model, document builder, Swagger UI / ReDoc | Implemented |
| `axumapi-migrations` | Migration graph, operations, schema diff | Scaffolding (Phase 4) |
| `axumapi-cli` | `axumapi` binary | Scaffolding (Phase 6) |
| `axumapi-testkit` | `TestClient` (in-process), lifespan-aware `start`/`shutdown` | Implemented |

### Dependency rules

1. **The ORM has no HTTP knowledge.** `axumapi-orm` does not depend on `axumapi-core`. The core crate implements `From<OrmError> for ApiError`; the orphan rule allows this because core owns `ApiError`.
2. **Schema metadata lives in validation.** `Schema`/`SchemaObject` sit in `axumapi-validation` so that core (request bodies) and openapi (documents) share one definition without a cycle.
3. **Backends depend on the ORM, never the reverse.** The ORM defines the `Backend` trait and the capability model; each backend implements them.
4. **HTTP traits are axumapi's own.** `FromRequestParts`, `FromRequest`, `IntoResponse` and `Handler` carry `describe` hooks, so OpenAPI is derived from handler signatures and custom extractors document themselves. See [ROUTING.md](ROUTING.md) and [OPENAPI.md](OPENAPI.md).
5. **Axum and SQLx are internal.** Public types are axumapi newtypes (`Json`, `Path`, `Query`, `State`, `MethodRouter`, …). Users can therefore upgrade axum or SQLx without breaking changes, and non-SQL backends are not second-class citizens.

## Key architectural decisions

### A. Query expression API: typed field constants, not a filter macro

```rust
User::objects(&db)
    .filter(User::name.icontains("john").or(User::email.ends_with("@example.com")))
    .order_by(User::name.asc())
```

`#[derive(Model)]` (Phase 4) generates one associated constant per field:
`pub const name: Field<User, String>`. The type parameters encode two things:

* The **model**, so a `Field<Post, _>` cannot be used by mistake in a `User` query.
* The **Rust type**. Lookups are inherent methods that exist only where they make sense. `icontains` exists only on `Field<M, String>`, and comparison operands must implement `Operand<M, T>`. That means `User::age.eq("x")` fails to **compile**.

We rejected a `filter!(User, name__icontains = "john")` macro. The typed-constant API gives IDE completion, rustdoc, and ordinary compiler errors. A macro would still have to generate these same constants in order to validate field names. Django `__` traversal becomes chained relation accessors, for example `Post::author().team().name`, generated in Phase 4.

Implemented now: `axumapi_orm::expr::{Expr, Field, Operand, Lookup}`.

### B. Model metadata generation

`#[derive(Model)]` generates, all at compile time:

* **Typed field markers**: the `Field<M, T>` constants from decision A.
* **Static metadata**: `const META: ModelMeta` with the table name, fields (column, SQL type family, nullability, constraints), indexes, unique/check constraints and ordering. It is a `&'static` data structure with no runtime registration.
* **Relation metadata**: FK target, `on_delete` policy, related name. The target is referenced by type, so a typo in a model name is a compile error.
* **Schema metadata**: a `Schema` implementation reused by OpenAPI.
* **Migration metadata**: the migration autodetector diffs `ModelMeta` snapshots, so migrations and the ORM share one source of truth.

### C. Backend capability detection

Unsupported features fail at the earliest possible point:

| When | Mechanism | Example |
|---|---|---|
| Compile time | Typed APIs; lookups exist only on suitable types | `icontains` on an `i64` field |
| Query construction / compile | `QueryPlan::required_features()` checked against `BackendCapabilities` before any I/O | `select_for_update()` on SQLite → `BackendCapabilityError::RowLockingUnsupported` |
| Execution | Server/version limits discovered at runtime | older SQLite without window functions |

Nothing is silently ignored. Capabilities are richer than booleans where needed: `TransactionSupport { None, Flat, Savepoints }` and `RowLocking { None, Basic, Full }`.

### D. Connection ownership

* An application registers backends under **aliases** (`"default"`, `"analytics"`). They are stored as `Arc<dyn Backend>` in typed application state.
* Handlers receive a `Db` handle through dependency injection. It is a cheap clone of the pool handle and never a raw SQLx pool.
* Transactions are **scoped closures**: `db.transaction(|tx| async move { … }).await`. The closure receives a `Tx` handle that implements the same executor trait as `Db`. Commit happens on `Ok`, rollback on `Err` or panic-unwind. Nested calls become savepoints only when `TransactionSupport::Savepoints` is declared.
* `QuerySet::using("analytics")` overrides the alias. A plan that references models with different aliases is rejected, so there are no cross-database joins.

### E. Query result decoding

* Every backend returns `QueryResult { rows: Vec<Row> }`. A `Row` holds ordered `(column, Value)` pairs, where `Value` is a closed, backend-neutral enum.
* Typed model decoding (Phase 4) is a `FromRow` trait generated by `#[derive(Model)]`. It reads by column name and returns `QueryError::Decode` instead of panicking.
* Dynamic projections (`values()`, annotations, aggregates) stay as `Row`. `values_list::<(A, B)>()` decodes into tuples through the same trait.
* Bind parameters are always `Value`s. They are never interpolated into SQL and never logged by default.

## Query pipeline

```mermaid
sequenceDiagram
    participant H as Handler
    participant QS as QuerySet (Phase 4)
    participant P as QueryPlan
    participant B as Backend
    H->>QS: .filter(..).order_by(..).limit(..)
    QS->>P: builds immutable plan (no I/O)
    H->>QS: .all().await
    QS->>B: fetch(&plan)
    B->>P: check(capabilities)
    B->>B: compile(plan, dialect) → SQL + params
    B-->>H: QueryResult / typed models
```

See [QUERY_PLAN.md](QUERY_PLAN.md) and [BACKENDS.md](BACKENDS.md).

## Error architecture

| Error | Crate | HTTP mapping |
|---|---|---|
| `ValidationError` | validation | 422 with `errors` extension |
| `QueryError::DoesNotExist` | orm | 404 |
| `QueryError::MultipleObjectsReturned` | orm | 500 |
| `BackendCapabilityError` | orm | 501 |
| `BackendError` | orm | 500 (the detail is logged, not returned to the client) |
| `ApiError` | core | Rendered as RFC 7807 `application/problem+json` |

Library code has no `unwrap`/`expect`: clippy `unwrap_used` and `expect_used` are denied workspace-wide. Every crate declares `#![forbid(unsafe_code)]`.
