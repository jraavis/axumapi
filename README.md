# axumapi

A FastAPI-style Rust web framework with Pydantic-style validation and a Django-style ORM. It is async-first, type-safe, and targets stable Rust (edition 2024, MSRV 1.92).

> **Status: Phase 3 (validation and serialization), pre-alpha.** The APIs will change. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) lists what is implemented and what is scaffolding.

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

A typed query DSL compiles to a backend-neutral `QueryPlan`:

```rust
let plan = QueryPlan::from_table("posts")
    .filter(Post::title.icontains("rust").or(Post::likes.gt(Post::dislikes)))
    .order_by(Post::likes, OrderDirection::Desc)
    .limit(10);
```

## Workspace

| Crate | Purpose |
|---|---|
| `axumapi` | Facade and prelude. Most users depend only on this crate. |
| `axumapi-core` | App, routing, extractors, responses, RFC 7807 errors |
| `axumapi-validation` | Validation errors, rules, constrained types, schema metadata |
| `axumapi-orm` | QueryPlan IR, typed expressions, backend capabilities |
| `axumapi-backends` | SQL compiler (PostgreSQL/SQLite) and SQLite executor |
| `axumapi-testkit` | In-process `TestClient` |
| `axumapi-macros` | Route attributes, `routes![]`, `#[derive(Schema)]` |
| `axumapi-openapi` | OpenAPI 3.1 model, builder, docs UIs |
| `axumapi-migrations`, `-cli` | Scaffolding for later phases |

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p hello_world
```

## Roadmap

1. Foundation: done
2. HTTP framework: done (route macros, DI, middleware, lifespan, WebSockets, OpenAPI 3.1)
3. Validation and serialization: done (Pydantic-style pipeline, validators, computed fields, dump options, constrained types)
4. ORM models, QuerySet, relations, transactions, migrations
5. MySQL, MongoDB, Redis
6. CLI, benchmarks, release tooling

## License

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
