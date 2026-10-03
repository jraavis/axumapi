# Native MySQL driver probe

This is the first executable experiment for plan 3's native backend program.
It compares SQLx 0.8.6 with mysql_async 0.37.1 before porting the production
adapter. It measures driver work; it does not compare HTTP frameworks or
implement ORM RETURNING, transactions or migrations for a native adapter.

## Run

Use a dedicated local MySQL service and an account allowed to create/drop
databases. The probe creates a UUID-named database, then drops it on success
and ordinary errors. It never resets the database named in the supplied URL.
`MYSQL_PROBE_URL` takes precedence over `MYSQL_URL` (used by live-test CI).
Do not use a production service for load measurements.

```sh
cargo build --release -p mysql_driver_probe
MYSQL_PROBE_URL='mysql://user:password@localhost/mysql' \
  target/release/mysql_driver_probe \
  --requests 50000 --concurrency 10 --pool 10 --pairs 5 > trials.jsonl
```

Options also include `--timeout SECONDS` (120 for measurement by default) and
`--held`. Held mode leases one connection per worker for its entire trial,
isolating execution from per-insert checkout/release. It requires concurrency
no higher than pool capacity. Compare held and pooled modes at the same
concurrency; held mode with fewer connections answers a different question.

The measurement timeout includes warm-up and row validation. Pool opening
uses driver connection timeouts; pool close has a separate 30-second bound.

Request counts default to 10,000. Short runs are diagnostics only. Increase
the count until every measured cell lasts at least ten seconds before using
the experiment to decide adoption. There is no automatic calibration here.
The five paired passes alternate forward and reverse driver order. The
native-reset mode remains in the middle; a balanced Latin-square order is
still needed to eliminate that systematic position in publication runs.

## Matched workload

- Same InnoDB table, UTF-8 title, two bound values, prepared INSERT and
  generated integer key. Each insert is one independent autocommit.
- Same configured pool capacity and worker concurrency. Every connection is
  opened, held and warmed with ten inserts before schema-preserving DELETE.
- Same UTC, autocommit, SQL mode and GROUP_CONCAT session initialization.
  Native reset mode restores these settings after every reset. That cost is
  included; retaining mode assumes this fixed workload does not mutate them.
- Every returned ID must be nonzero and unique. Outside timing, an independent
  SQLx connection reads all stored rows and verifies the complete ID set,
  title and boolean value. A query error invalidates the run and exits nonzero.
- Per-insert latency includes checkout in pooled mode. Connection recycling
  drains before counter collection and validation. The elapsed interval ends
  at the final acknowledgement; release work after it is outside that timer.
- JSON lines retain each trial's throughput, latency percentiles, row count
  and server-global counter deltas. The manifest records effective server
  durability settings and driver versions. It omits credentials.

The three modes are `sqlx`, `native_reset` and `native_retain`. SQLx disables
acquire testing but retains its normal release ping. mysql_async defaults to
reset on release, which removes prepared statements; `native_retain` disables
that reset. Neither mode is a general-purpose production cleanup policy.

Server-global counters are supporting diagnostics, not packet traces.
Other clients can contaminate them. Use an otherwise idle service, retain
storage/runtime/build metadata separately, and record invalid or interrupted
runs. Tokio worker count follows the runtime default. Timing and ID collection
add equal diagnostic work to both drivers; production adapter cost may differ.

## Live contracts

```sh
MYSQL_PROBE_URL='mysql://user:password@localhost/mysql' \
  cargo test -p mysql_driver_probe -- --include-ignored
```

The live test uses one-slot pools to check typed transaction-drop rollback,
cancelled-query cleanup, cancelled-checkout capacity and session reset versus
retention. It also runs the full probe across three profiles with queueing
and uneven work allocation, verifying all committed rows independently.
Missing credentials fail explicitly rather than skipping the test.

These tests do not establish safe raw SQL session mutation, unknown-commit
recovery, server restart, TLS authentication, full type decoding, trigger-aware
RETURNING or migration locks. Those remain adoption gates for the native
adapter. A process kill or service loss may require manual cleanup of the
UUID-named disposable database; asynchronous cleanup is not crash durability.

## Pinned source audit

Locally inspected source: mysql_async 0.37.1, `src/conn/pool/recycler.rs`,
`src/conn/pool/futures/get_conn.rs`, `src/conn/mod.rs` and `src/io/mod.rs`.

- Default pool release performs reset, not guaranteed zero network work.
- A clean connection with reset disabled enters the available queue without
  that reset. Dirty typed transactions/pending results are cleaned first;
  cleanup failure discards the connection.
- Idle acquisition uses a stream check. The inspected TCP check attempts
  nonblocking reads rather than sending a MySQL ping.
- `init` is for connection creation; `setup` also runs after reset. The probe
  uses `setup` so the reset profile preserves required session settings.
- Session variables survive retained release. A future native adapter must
  own session mutations explicitly and discard/reset uncertain leases.

References: [pool reset policy][reset], [pinned source][source].

[reset]:
  https://docs.rs/mysql_async/0.37.1/mysql_async/struct.PoolOpts.html
[source]:
  https://docs.rs/crate/mysql_async/0.37.1/source/src/conn/
