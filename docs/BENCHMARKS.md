# Benchmarks

Criterion benchmarks live in the `siderite-bench` crate (not published). Its README, [`crates/siderite-bench/README.md`](../crates/siderite-bench/README.md), is the reference: it lists what each benchmark measures, notes on how to read them, and the latest results with the machine they were taken on.

```bash
cargo bench -p siderite-bench -- --warm-up-time 1 --measurement-time 2
```

Run a single group by name, for example `cargo bench -p siderite-bench -- routing`. The groups are `routing`, `extract` and `orm`; the sources are in [`crates/siderite-bench/benches`](../crates/siderite-bench/benches). Criterion writes HTML reports to `target/criterion`.

Numbers are machine-specific. Compare runs only on the same machine, and treat the README's table as a snapshot, not a guarantee.

## HTTP benchmarks versus FastAPI

End-to-end throughput measured with ApacheBench (`ab -l`) on the same machine (Darwin arm64, rustc 1.96.0, Python 3.13.14, FastAPI 0.142.1; PostgreSQL 17, MySQL 8.4 and MongoDB 8 in local containers). Siderite runs a `--release` build; FastAPI runs under Uvicorn with a single worker, no reload, and warning log level. Each figure is the median of 3 runs with 0 failed requests.

Plain HTTP (`examples/hello_world`; `GET`s with `-n 20000 -c 100`, `POST /echo` with `-n 10000 -c 50`):

| Test | Siderite (req/s) | FastAPI (req/s) |
|---|---|---|
| `GET /` | 28,674 | 5,039 |
| `GET /hello/{name}` | 28,023 | 5,238 |
| `POST /echo` (JSON) | 29,573 | 3,541 |

Database-backed minimal Todo API (`GET /todos` latest 20, `GET /todos/{id}`, `POST /todos` → 201; siderite ORM versus `asyncpg` / `aiomysql` / `motor` with pools of 10; tables truncated and reseeded with 100 rows before each phase; reads `-n 5000 -c 50`, inserts `-n 2000 -c 20`):

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

Writes are DB-bound and land near parity (except PostgreSQL inserts). Reads favor siderite, most clearly on MongoDB list/get and MySQL list. Per-run spread is roughly ±20–30%, so treat ratios near 1.0–1.3x as noise.
