---
title: Hello World
description: Walk the hello_world example — routes, extractors, validation, and OpenAPI.
---

Package: `examples/hello_world`. This is the same app as
[First application](/siderite/start/first-app/), expanded into a checklist you can
run.

## Run

```bash
cargo run -p hello_world
```

The binary listens on `127.0.0.1:8000` unless `ADDR` is set.

## Routes

| Method | Path | Handler |
|---|---|---|
| `GET` | `/` | `index` — `PlainText("Hello, siderite!")` |
| `GET` | `/hello/{name}` | `hello` — path + query (`shout`) |
| `POST` | `/echo` | `echo` — JSON `Message` with `min_length = 1` |

```bash
curl -s http://127.0.0.1:8000/
curl -s 'http://127.0.0.1:8000/hello/ann?shout=true'
curl -s -X POST http://127.0.0.1:8000/echo \
  -H 'content-type: application/json' \
  -d '{"message":"hi"}'
curl -s -X POST http://127.0.0.1:8000/echo \
  -H 'content-type: application/json' \
  -d '{"message":""}'
```

The empty message is 422. OpenAPI lives at `/docs`, `/redoc`, and
`/openapi.json`.

## What to copy

- `#[get]` / `#[post]` plus `routes![]`
- `Path<T>` and `Query<T>` for URL data
- `#[derive(Deserialize, Validate, Schema)]` on request bodies
- `App::new().title(..).version(..).routes(..).run(..)`

Next: [Todo on SQLite](/siderite/tutorials/todo-sqlite/).
