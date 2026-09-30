---
title: Observability
description: http.request and orm.query tracing spans, redaction rules, and JSON logs.
---

axumapi emits [`tracing`](https://docs.rs/tracing) spans and events.
Install a subscriber in your binary (`axumapi::config::init_tracing` builds
one from `LogSettings`; `RUST_LOG` wins) and the spans below appear in
your logs or exporter.

Bind parameters, passwords, tokens, API keys, and `Secret` values are
never recorded.

## HTTP request spans

`RequestLogging` wraps each request in an `info`-level `http.request` span:

| Field | Value |
|---|---|
| `request_id` | the id set by `RequestIdLayer` (register it before `RequestLogging`) |
| `method` | HTTP method |
| `route` | the matched path template (`/users/{id}`), or `<unmatched>`; never the raw URI |
| `status` | response status code, recorded when the response is ready |
| `latency_ms` | time spent in the inner service |

```rust
let app = App::new()
    .routes(routes![get_user])
    .request_id()
    .request_logging();
```

Headers (including `Authorization`, `Cookie`, and API keys), query strings,
and bodies are never recorded. Handler spans and ORM `orm.query` spans nest
inside `http.request`, so one request id ties a request to its queries.

Install a subscriber with `axumapi::config::init_tracing(&settings.log)`.
`RUST_LOG` overrides `log.level`, and `log.json = true` switches to JSON
output.

## ORM query spans

Every `Db` entry point that talks to the database runs inside an
`orm.query` span at `debug` level:

| Field | Value |
|---|---|
| `db.system` | backend name (`sqlite`, `postgresql`, `mysql`, `mongodb`) |
| `db.operation` | `select`, `insert`, `update`, `delete`, `raw`, or `script` |
| `db.table` | the table the plan targets; empty for raw SQL and scripts |
| `elapsed_ms` | duration in milliseconds, recorded when the call finishes |

Enable it with `RUST_LOG=axumapi_orm=debug`.

SQL text is logged only by the raw entry points and script execution, and
only at `trace` level (`RUST_LOG=axumapi_orm=trace`). Because raw SQL is
written by the developer, keep secrets out of it and pass values as bind
parameters. Parameters are never recorded, at any level.

## See also

- [Configuration](/axumapi/guides/production/config/)
- [Middleware and lifespan](/axumapi/guides/http/middleware/)
- [Errors](/axumapi/guides/http/errors/)
