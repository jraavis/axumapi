---
title: First application
description: Build a small siderite app with routes, extractors, validation, and OpenAPI.
---

This page walks `examples/hello_world`. By the end you have three routes, a
validated JSON body, and generated docs at `/docs`.

## The app

```rust
use siderite::prelude::*;

#[derive(Deserialize, Validate, Schema)]
struct Greeting {
    shout: Option<bool>,
}

#[derive(Serialize, Deserialize, Validate, Schema)]
struct Message {
    #[field(min_length = 1, max_length = 280)]
    message: String,
}

/// Plain-text greeting.
#[get("/")]
async fn index() -> PlainText<&'static str> {
    PlainText("Hello, siderite!")
}

/// Greet somebody by name.
#[get("/hello/{name}", tag = "greetings")]
async fn hello(Path(name): Path<String>, Query(greeting): Query<Greeting>) -> PlainText<String> {
    let text = format!("Hello, {name}!");
    PlainText(if greeting.shout.unwrap_or(false) {
        text.to_uppercase()
    } else {
        text
    })
}

/// Echo the posted message back.
#[post("/echo", tag = "greetings")]
async fn echo(Json(message): Json<Message>) -> Json<Message> {
    Json(message)
}

fn app() -> App {
    App::new()
        .title("Hello World")
        .version("1.0.0")
        .routes(routes![index, hello, echo])
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    siderite_cli::AppCli::new(app).run().await
}
```

Run it:

```bash
cd examples/hello_world
siderite run
```

Or `cargo run -p hello_world -- run`.

Build an optimized binary with `siderite build --release` (the same as
`cargo build --release -p hello_world`); it lands in `target/release/`.

## What each piece does

**Route macros** (`#[get]`, `#[post]`, …) leave the function callable and emit
a sibling that builds a `Route`. `routes![index, hello, echo]` collects those
values. Nothing is registered at import time.

**Path parameters** use `{name}` in the template and `Path<T>` in the
signature. Query strings bind to `Query<T>`. JSON bodies bind to `Json<T>`.

**Validation** runs because `Greeting` and `Message` derive `Validate`.
`Json<Message>` never calls the handler when the body is invalid. The client
gets a 422 problem document that lists every error.

Try it:

```bash
curl -s http://127.0.0.1:8000/hello/ann
curl -s 'http://127.0.0.1:8000/hello/ann?shout=true'
curl -s -X POST http://127.0.0.1:8000/echo \
  -H 'content-type: application/json' \
  -d '{"message":""}'
```

The empty message returns 422 with `code: too_short` at `["body", "message"]`.

**OpenAPI 3.1** is built from the same signatures:

| URL | Serves |
|---|---|
| `/openapi.json` | The generated document |
| `/docs` | Swagger UI |
| `/redoc` | ReDoc |

The first line of a handler’s doc comment becomes the operation summary. `tag`
groups operations in the UI.

## Add a status code

```rust
#[post("/echo", status = 201, tag = "greetings")]
async fn echo(Json(message): Json<Message>) -> Json<Message> {
    Json(message)
}
```

`status` replaces a `200` returned by the handler and moves the documented
success response to that code.

## Next

- [Routing](/siderite/guides/http/routing/) — function API, `routes![]`, mounting
- [Validation](/siderite/guides/http/validation/) — pipeline, `#[field]`, 422 shape
- [Todo on SQLite](/siderite/tutorials/todo-sqlite/) — models and CRUD
