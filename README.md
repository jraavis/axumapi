# axumapi

A FastAPI-style Rust web framework with Pydantic-style validation and a Django-style ORM. It is async-first, type-safe, and targets stable Rust (edition 2024, MSRV 1.92).

> **Status: Phase 6 (production tooling) complete, pre-alpha.** The APIs will change. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) lists what is implemented and what is deferred.

```rust
use axumapi::prelude::*;

#[derive(Deserialize, Validate, Schema)]
struct Greeting { shout: Option<bool> }

#[derive(Serialize, Deserialize, Validate, Schema)]
struct Message {
    #[field(min_length = 1, max_length = 280)]
    message: String,
}

/// Greet somebody by name.
#[get("/hello/{name}", tag = "greetings")]
async fn hello(Path(name): Path<String>, Query(q): Query<Greeting>) -> PlainText<String> {
    let msg = format!("hello, {name}");
    PlainText(if q.shout.unwrap_or(false) { msg.to_uppercase() } else { msg })
}

#[post("/echo", status = 201)]
async fn echo(Json(m): Json<Message>) -> Json<Message> { Json(m) }

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    App::new().title("Hello").routes(routes![hello, echo]).run("127.0.0.1:8000").await
}
```

Invalid input is rejected with a `422` that lists every error with its location. OpenAPI 3.1 is generated from the handler signatures and served at `/openapi.json`, `/docs` (Swagger UI) and `/redoc`.

Models are structs with `#[derive(Model)]`. Queries are lazy `QuerySet`s built from typed field constants:

```rust
let adults = User::objects(&db)
    .filter(User::age.ge(18).and(User::name.icontains("ann")))
    .order_by([User::created_at.desc()])
    .limit(20)
    .all()
    .await?;
```

## Workspace

| Crate | Purpose |
|---|---|
| `axumapi` | Facade and prelude. Most users depend only on this crate. |
| `axumapi-core` | App, routing, extractors, responses, RFC 7807 errors |
| `axumapi-validation` | Validation errors, rules, constrained types, schema metadata |
| `axumapi-orm` | `Model`, `QuerySet`, relations, transactions, QueryPlan IR |
| `axumapi-backends` | SQL compiler and executors: SQLite (default), PostgreSQL, MySQL, MongoDB (subset); Redis key/hash/set client |
| `axumapi-testkit` | In-process `TestClient`, `TestDatabase` fixtures and isolation |
| `axumapi-config` | Layered configuration (TOML, environment, overrides), `Secret` |
| `axumapi-cache` | Cache trait, in-memory LRU and Redis caches, `RouteCache` middleware |
| `axumapi-macros` | Route attributes, `routes![]`, `#[derive(Model, Validate, Schema)]` |
| `axumapi-openapi` | OpenAPI 3.1 model, builder, docs UIs |
| `axumapi-migrations` | Autodetector, JSON migrations, schema editor |
| `axumapi-cli` | `AppCli` (`runserver`, `routes`, `check`, `dbshell`, migrations) and the `axumapi` migration binary |
| `axumapi-bench` | Criterion benchmarks (not published) |

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p hello_world
cargo run -p todo_sqlite
```

Backends other than SQLite are behind cargo features (`postgres`, `mysql`, `mongodb`, `redis`). Their live tests are skipped unless a server URL is set:

| Backend | Feature | Live-test variable |
|---|---|---|
| PostgreSQL | `postgres` | `DATABASE_URL=postgres://...` |
| MySQL 8 | `mysql` | `MYSQL_URL=mysql://...` |
| MongoDB | `mongodb` | `MONGODB_URL=mongodb://...` |
| Redis | `redis` | `REDIS_URL=redis://...` |

To run them locally, start the databases from `docker-compose.yml` and export the URLs listed in its header ([docs/TESTING.md](docs/TESTING.md)):

```bash
docker compose up -d --wait
cargo test --workspace --all-features
```

Unsupported features fail with a `BackendCapabilityError` before any I/O; see [docs/BACKENDS.md](docs/BACKENDS.md) for the feature matrix. Redis is a key/hash/set client, not a `QuerySet` backend.

## Roadmap

1. Foundation: done
2. HTTP framework: done (route macros, DI, middleware, lifespan, WebSockets, OpenAPI 3.1)
3. Validation and serialization: done (Pydantic-style pipeline, validators, computed fields, dump options, constrained types)
4. ORM models, QuerySet, relations, transactions, migrations: done
5. MySQL, MongoDB, Redis: done (MongoDB compiles the supported QuerySet subset; Redis is a typed client)
6. Production tooling: done (configuration, observability, security schemes, signals, database routing, cache, CLI, testkit, benchmarks, CI, docker-compose; see [docs/RELEASING.md](docs/RELEASING.md))

## License

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
