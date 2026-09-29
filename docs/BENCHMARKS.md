# Benchmarks

Criterion benchmarks live in the `axumapi-bench` crate (not published). Its README, [`crates/axumapi-bench/README.md`](../crates/axumapi-bench/README.md), is the reference: it lists what each benchmark measures, notes on how to read them, and the latest results with the machine they were taken on.

```bash
cargo bench -p axumapi-bench -- --warm-up-time 1 --measurement-time 2
```

Run a single group by name, for example `cargo bench -p axumapi-bench -- routing`. The groups are `routing`, `extract` and `orm`; the sources are in [`crates/axumapi-bench/benches`](../crates/axumapi-bench/benches). Criterion writes HTML reports to `target/criterion`.

Numbers are machine-specific. Compare runs only on the same machine, and treat the README's table as a snapshot, not a guarantee.
