---
title: Benchmarks
description: How to run Criterion benches and how to read the numbers.
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

## See also

- [Development](/siderite/contributing/development/)
- [Architecture](/siderite/internals/architecture/)
