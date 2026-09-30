---
title: Benchmarks
description: How to run Criterion benches and end-to-end HTTP benchmarks, and how to read the numbers.
---

Criterion benchmarks live in the `siderite-bench` crate (not published). Its
README, `crates/siderite-bench/README.md`, lists what each benchmark
measures and the latest snapshot with the machine they were taken on.

```bash
cargo bench -p siderite-bench -- --warm-up-time 1 --measurement-time 2
```

Run a single group by name, for example `cargo bench -p siderite-bench -- routing`.
The groups are `routing`, `extract`, and `orm`. Criterion writes HTML
reports to `target/criterion`.

Numbers are machine-specific. Compare runs only on the same machine, and
treat the README’s table as a snapshot, not a guarantee.

| Group | Measures |
|---|---|
| `routing` | dispatch through siderite’s `RouterService` versus a raw axum `Router` |
| `extract` | validating `Json<T>` versus raw `axum::Json`; `Dump` versus `serde_json::to_vec` |
| `orm` | QuerySet → QueryPlan, SQL compilation (SQLite and PostgreSQL), fetch of 100 SQLite rows |

## HTTP benchmarks versus FastAPI

End-to-end throughput measured with ApacheBench (`ab -l`) on the same
machine, one server at a time (DB rounds) or side by side on different ports
(plain-HTTP round). Siderite runs a `--release` build; FastAPI runs under
Uvicorn with a single worker, no reload, and warning log level. Each figure
is the median of 3 runs with 0 failed requests.

Machine: Darwin arm64 (Apple M3 Pro), rustc 1.96.0, Python 3.13.14,
FastAPI 0.142.1. Databases run in local containers: PostgreSQL 17,
MySQL 8.4, MongoDB 8 (single-node replica set).

### Plain HTTP (`examples/hello_world`)

`GET /` (`-n 20000 -c 100`), `GET /hello/world` (`-n 20000 -c 100`),
`POST /echo` with a small JSON body (`-n 10000 -c 50`).

| Test | Siderite (req/s) | FastAPI (req/s) |
|---|---|---|
| `GET /` | 28,674 | 5,039 |
| `GET /hello/{name}` | 28,023 | 5,238 |
| `POST /echo` (JSON) | 29,573 | 3,541 |

Two response-shape caveats: `GET /` returns `PlainText` from siderite but a
JSON string from FastAPI, and `POST /echo` returns `200` from the example
app versus `201` from the FastAPI equivalent. Both are 2xx with tiny bodies,
so the comparison still reflects per-request framework overhead.

### Database-backed Todo API

A minimal Todo API (`id`, `title`, `done`) with `GET /todos` (latest 20),
`GET /todos/{id}`, and `POST /todos` → 201, implemented once with the
siderite ORM (`PgBackend`, `MySqlBackend`, `MongoBackend`, pool of 10) and
once with FastAPI (`asyncpg`, `aiomysql`, `motor`, pool of 10). Tables are
truncated and reseeded with 100 rows before each phase. Reads use
`-n 5000 -c 50`; inserts use `-n 2000 -c 20`.

| DB | Test | Siderite (req/s) | FastAPI (req/s) |
|---|---|---|---|
| PostgreSQL | list 20 | 8,114 | 5,031 |
| PostgreSQL | get one | 7,825 | 7,476 |
| PostgreSQL | insert | 7,116 | 4,543 |
| MySQL | list 20 | 7,103 | 3,565 |
| MySQL | get one | 8,335 | 6,665 |
| MySQL | insert | 2,445 | 2,644 |
| MongoDB | list 20 | 14,280 | 2,952 |
| MongoDB | get one | 11,714 | 3,792 |
| MongoDB | insert | 2,526 | 2,363 |

Writes are DB-bound and land near parity (except PostgreSQL inserts).
Reads favor siderite, most clearly on MongoDB list/get (Rust driver versus
Motor) and MySQL list. Per-run spread is roughly ±20–30%, so treat ratios
near 1.0–1.3x as noise; the larger gaps reproduce across reruns.

## See also

- [Development](/siderite/contributing/development/)
- [Architecture](/siderite/internals/architecture/)
