# siderite-bench

Criterion benchmarks for siderite (spec section 54). Not published.

```bash
cargo bench -p siderite-bench -- --warm-up-time 1 --measurement-time 2
```

## Benchmarks

- `routing`: dispatch through siderite's `RouterService` versus a raw axum
  `Router` with the same routes (a static route and a `{id}` path parameter),
  driven in process with `tower::ServiceExt::oneshot`.
- `extract`: a JSON echo handler using siderite's validating `Json<T>`
  (`#[derive(Validate, Schema, Deserialize)]` model) versus raw `axum::Json`;
  plus response serialization (`Json<T>` into a response through `Dump`)
  versus `serde_json::to_vec`.
- `orm`: `QuerySet` builder to `QueryPlan`, SQL compilation of that plan for
  SQLite and PostgreSQL, and fetching 100 rows from in-memory SQLite.

## Results

Numbers are machine-specific and only describe the run below. Do not compare
them across machines or treat them as guarantees.

Machine:

```
Darwin LN-2031.local 25.6.0 Darwin Kernel Version 25.6.0: Fri Jul 31 19:16:36 PDT 2026; root:xnu-12377.161.14~5/RELEASE_ARM64_T6030 arm64
rustc 1.96.0 (ac68faa20 2026-05-25)
CPU: Apple M3 Pro
```

Run with `--warm-up-time 1 --measurement-time 2` (release profile). Values are
Criterion's median point estimate per iteration.

| Benchmark | Median |
|---|---|
| `routing/siderite/static` | 1.23 us |
| `routing/axum/static` | 562 ns |
| `routing/siderite/path_param` | 1.41 us |
| `routing/axum/path_param` | 686 ns |
| `extract/siderite/json_validate_respond` | 3.74 us |
| `extract/axum/json_respond` | 1.69 us |
| `response_serialization/siderite/dump` | 741 ns |
| `response_serialization/serde_json/to_vec` | 92 ns |
| `orm/queryset_build` | 1.09 us |
| `orm/compile/sqlite` | 387 ns |
| `orm/compile/postgres` | 407 ns |
| `orm/sqlite_fetch_100_rows` | 122 us |

Notes on what is measured:

- Routing and extract iterations include building the request and cloning the
  service, for both sides equally.
- `response_serialization/siderite/dump` clones the model and builds a full
  response, while `serde_json/to_vec` only serializes to bytes, so the two are
  not the same amount of work.
- `orm/sqlite_fetch_100_rows` runs `Widget::objects(&db).all()` against a
  single-connection in-memory SQLite database seeded with 100 rows.

## End-to-end HTTP benchmarks

Throughput against equivalent FastAPI apps, measured with ApacheBench on the
same machine (median of 3 runs, 0 failures), is documented in
[`docs/BENCHMARKS.md`](../../docs/BENCHMARKS.md) and on the website's
[benchmarks page](https://jraavis.github.io/siderite/contributing/benchmarks/):
plain-HTTP routes from `examples/hello_world` plus a minimal Todo API on
SQLite, PostgreSQL, MySQL and MongoDB. Those automated harnesses live in
[`benchmarks/`](../../benchmarks/) and are driven via `python3 benchmarks/run_benchmarks.py`.
