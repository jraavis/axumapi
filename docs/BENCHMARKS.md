# Benchmarks

Criterion benchmarks live in the `siderite-bench` crate (not published). Its README, [`crates/siderite-bench/README.md`](../crates/siderite-bench/README.md), is the reference: it lists what each benchmark measures, notes on how to read them, and the latest results with the machine they were taken on.

```bash
cargo bench -p siderite-bench -- --warm-up-time 1 --measurement-time 2
```

Run a single group by name, for example `cargo bench -p siderite-bench -- routing`. The groups are `routing`, `extract` and `orm`; the sources are in [`crates/siderite-bench/benches`](../crates/siderite-bench/benches). Criterion writes HTML reports to `target/criterion`.

Numbers are machine-specific. Compare runs only on the same machine, and treat the README's table as a snapshot, not a guarantee.

## HTTP benchmarks versus FastAPI

End-to-end throughput measured with ApacheBench (`ab -l`) on the same machine (Darwin arm64, rustc 1.96.0, Python 3.13.14, FastAPI 0.142.1, Uvicorn 0.54.0; PostgreSQL 17, MySQL 8.4 and MongoDB 8 in local containers). Siderite runs a `--release` build; FastAPI runs under Uvicorn with a single worker, no reload, and warning log level. Each figure is the median of 3 runs with 0 failed requests. Automated harnesses live in [`benchmarks/`](../benchmarks/).

Plain HTTP (`examples/hello_world`; `GET`s with `-n 20000 -c 100`, `POST /echo` with `-n 10000 -c 50`):

| Test | Siderite (req/s) | FastAPI (req/s) | Speedup | Latency p99 (S / F) |
|---|---|---|---|---|
| `GET /` | 36,985 | 8,044 | 4.60x | 7 ms / 35 ms |
| `GET /hello/{name}` | 35,776 | 6,715 | 5.33x | 6 ms / 41 ms |
| `POST /echo` (JSON) | 37,701 | 7,045 | 5.35x | 2 ms / 14 ms |

Database-backed minimal Todo API (`GET /todos` latest 20, `GET /todos/{id}`, `POST /todos` → 201; siderite ORM versus `sqlite3` / `asyncpg` / `aiomysql` / `motor` with pools of 10; tables truncated and reseeded with 100 rows before each phase; reads `-n 5000 -c 50`, inserts `-n 2000 -c 20`):

| DB | Test | Siderite (req/s) | FastAPI (req/s) | Speedup |
|---|---|---|---|---|
| SQLite | list 20 | 25,252 | 5,191 | 4.86x |
| SQLite | get one | 44,876 | 5,627 | 7.98x |
| SQLite | insert | 1,914 | 1,921 | 1.00x |
| PostgreSQL | list 20 | 8,048 | 7,870 | 1.02x |
| PostgreSQL | get one | 8,050 | 8,032 | 1.00x |
| PostgreSQL | insert | 7,562 | 7,962 | 0.95x |
| MySQL | list 20 | 7,438 | 5,172 | 1.44x |
| MySQL | get one | 7,924 | 7,332 | 1.08x |
| MySQL | insert | 2,472 | 3,198 | 0.77x |
| MongoDB | list 20 | 13,726 | 3,705 | 3.70x |
| MongoDB | get one | 15,389 | 4,179 | 3.68x |
| MongoDB | insert | 2,963 | 2,608 | 1.14x |

Writes are DB-bound and land near parity. Reads favor siderite, most clearly on SQLite, MongoDB list/get (Rust driver versus Motor) and MySQL list. Per-run spread is roughly ±20–30%, so treat ratios near 1.0–1.3x as noise.
