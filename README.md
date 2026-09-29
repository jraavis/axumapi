# axumapi

A FastAPI-style Rust web framework with Pydantic-style validation and a Django-style ORM. It is async-first, type-safe, and targets stable Rust (edition 2024, MSRV 1.92).

> **Status: Phase 1 (foundation), pre-alpha.** The APIs will change. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) lists what is implemented and what is scaffolding.

```rust
use axumapi::prelude::*;

#[derive(Deserialize)]
struct Greeting { shout: Option<bool> }

async fn hello(Path(name): Path<String>, Query(q): Query<Greeting>) -> ApiResult<PlainText<String>> {
    let msg = format!("hello, {name}");
    Ok(PlainText(if q.shout.unwrap_or(false) { msg.to_uppercase() } else { msg }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    App::new().title("Hello").route("/hello/{name}", get(hello)).run("127.0.0.1:8000").await?;
    Ok(())
}
```

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
| `axumapi-macros`, `-openapi`, `-migrations`, `-cli` | Scaffolding for later phases |

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p hello_world
```

## Roadmap

1. **Foundation**: done
2. HTTP framework: route macros, DI, middleware, WebSockets, OpenAPI
3. Validation derives and serialization
4. ORM models, QuerySet, relations, transactions, migrations
5. MySQL, MongoDB, Redis
6. CLI, benchmarks, release tooling

## License

Licensed under either [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
