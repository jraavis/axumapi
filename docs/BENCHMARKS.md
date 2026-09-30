# Benchmarks

Criterion benchmarks live in the `siderite-bench` crate (not published). Its README, [`crates/siderite-bench/README.md`](../crates/siderite-bench/README.md), is the reference: it lists what each benchmark measures, notes on how to read them, and the latest results with the machine they were taken on.

```bash
cargo bench -p siderite-bench -- --warm-up-time 1 --measurement-time 2
```

Run a single group by name, for example `cargo bench -p siderite-bench -- routing`. The groups are `routing`, `extract` and `orm`; the sources are in [`crates/siderite-bench/benches`](../crates/siderite-bench/benches). Criterion writes HTML reports to `target/criterion`.

Numbers are machine-specific. Compare runs only on the same machine, and treat the README's table as a snapshot, not a guarantee.
