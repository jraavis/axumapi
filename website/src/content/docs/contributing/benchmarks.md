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
is the median of 3 runs with 0 failed requests. Automated benchmark harnesses
live in `benchmarks/`.

Machine: Darwin arm64 (Apple M3 Pro), rustc 1.96.0, Python 3.13.14,
FastAPI 0.142.1, Uvicorn 0.54.0. Databases run in local containers: PostgreSQL 17,
MySQL 8.4, MongoDB 8 (single-node replica set).

### Plain HTTP (`examples/hello_world`)

`GET /` (`-n 20000 -c 100`), `GET /hello/world` (`-n 20000 -c 100`),
`POST /echo` with a small JSON body (`-n 10000 -c 50`).

| Test | Siderite (req/s) | FastAPI (req/s) | Speedup | Latency p99 (S / F) |
|---|---|---|---|---|
| `GET /` | 36,281 | 4,321 | 8.40x | 6 ms / 77 ms |
| `GET /hello/{name}` | 17,789 | 4,015 | 4.43x | 19 ms / 53 ms |
| `POST /echo` (JSON) | 26,265 | 3,817 | 6.88x | 4 ms / 48 ms |

### Database-backed Todo API

A minimal Todo API (`id`, `title`, `done`) with `GET /todos` (latest 20),
`GET /todos/{id}`, and `POST /todos` → 201, implemented once with the
siderite ORM (`SqliteBackend`, `PgBackend`, `MySqlBackend`, `MongoBackend`, pool of 10) and
once with FastAPI (`sqlite3`, `asyncpg`, `aiomysql`, `motor`, pool of 10). Tables are
truncated and reseeded with 100 rows before each phase. Reads use
`-n 5000 -c 50`; inserts use `-n 10000 -c 20`.

| DB | Test | Siderite (req/s) | FastAPI (req/s) | Speedup |
|---|---|---|---|---|
| SQLite | list 20 | 26,659 | 5,249 | 5.08x |
| SQLite | get one | 39,053 | 5,666 | 6.89x |
| SQLite | insert | 2,129 | 1,942 | 1.10x |
| PostgreSQL | list 20 | 9,044 | 7,873 | 1.15x |
| PostgreSQL | get one | 10,715 | 8,187 | 1.31x |
| PostgreSQL | insert | 8,675 | 7,569 | 1.15x |
| MySQL | list 20 | 9,396 | 5,147 | 1.83x |
| MySQL | get one | 9,948 | 7,218 | 1.38x |
| MySQL | insert | 3,333 | 3,732 | 0.89x |
| MongoDB | list 20 | 11,214 | 3,717 | 3.02x |
| MongoDB | get one | 13,846 | 4,119 | 3.36x |
| MongoDB | insert | 3,049 | 2,533 | 1.20x |

Writes are DB-bound and land near parity.
Reads favor siderite, most clearly on SQLite, MongoDB list/get (Rust driver versus
Motor) and MySQL list. Per-run spread is roughly ±20–30%, so treat ratios
near 1.0–1.3x as noise; the larger gaps reproduce across reruns.

## See also

- [Development](/siderite/contributing/development/)
- [Architecture](/siderite/internals/architecture/)
