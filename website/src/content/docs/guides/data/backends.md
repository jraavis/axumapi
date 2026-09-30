---
title: Backends
description: SQLite, PostgreSQL, MySQL, MongoDB, and Redis — capabilities, dialects, and how unsupported features fail.
---

Every QuerySet compiles to a backend-neutral `QueryPlan`. The backend
checks `required_features()` against `BackendCapabilities` **before any
I/O** and returns a typed `BackendCapabilityError` for anything it cannot
run. Nothing is silently ignored.

The compact matrix is in [Backend matrix](/axumapi/reference/backend-matrix/). This
page covers how to connect, dialect notes, and Redis.

## Status

| Backend | Feature | Execution | Notes |
|---|---|---|---|
| SQLite | default | `SqliteBackend` | Row locking, regex, `DISTINCT ON`, arrays, `STDDEV` / `VARIANCE` are capability errors |
| PostgreSQL | `postgres` | `PgBackend` | Live suite against `postgres:17` |
| MySQL 8 | `mysql` | `MySqlBackend` | 8.0.31+; live suite against `mysql:8.4` |
| MongoDB | `mongodb` | `MongoBackend` | 5.0+; replica set for transactions; supported QuerySet subset |
| Redis | `redis` | `RedisStore` | Key/hash/set client. **Not** a QuerySet backend |

DynamoDB is a design note only; it is not planned for v1.

```rust
use axumapi_backends::sqlite::SqliteBackend;
use axumapi::orm::Db;

let db = Db::new(SqliteBackend::connect("sqlite::memory:").await?);
```

Live tests skip themselves when the matching URL variable is unset. Start
the four servers from the root `docker-compose.yml`. See
[Testing](/axumapi/guides/production/testing/).

## Canonical storage

PostgreSQL stores every `Value` natively. SQLite has fewer storage classes,
so the adapter binds and expects the canonical forms from
`axumapi_orm::types`. Decoding accepts both the native and the canonical
form.

| Rust type | PostgreSQL | SQLite | MySQL | MongoDB |
|---|---|---|---|---|
| `bool` | `boolean` | integer 0 / 1 | `TINYINT(1)` | boolean |
| `i16` / `i32` / `i64` | `smallint` / `integer` / `bigint` | integer | `SMALLINT` / `INT` / `BIGINT` | int64 |
| `f32` / `f64` | `real` / `double precision` | real | `FLOAT` / `DOUBLE` | double |
| `Decimal` | `numeric` | `NUMERIC` column; bound as text | `DECIMAL(p,s)` | Decimal128 |
| `Uuid` | `uuid` | text (hyphenated) | `CHAR(36)` | Binary subtype 4 |
| `NaiveDate` | `date` | text `YYYY-MM-DD` | `DATE` | text `YYYY-MM-DD` |
| `NaiveTime` | `time` | text `HH:MM:SS.ffffff` | `TIME(6)` | text `HH:MM:SS.ffffff` |
| `DateTime<Utc>` | `timestamptz` | text RFC3339 (fixed width) | `DATETIME(6)`, UTC session | BSON datetime (ms; µs truncated) |
| `serde_json::Value` | `jsonb` | text | `JSON` | embedded document |
| `Vec<u8>` | `bytea` | blob | `BLOB` | Binary |

Decimals on SQLite: a `TEXT` column would compare `"9.99"` after `"15.00"`.
With `NUMERIC` affinity SQLite stores numbers. Very long decimals lose
precision beyond a double.

Numeric results on PostgreSQL: `SUM` and `AVG` return `numeric`; integer
and float types decode a `Decimal`, so `row.get_as::<i64>("total")` works
on both backends.

Bulk operations chunk rows to `max_params` (SQLite 32766, PostgreSQL/MySQL
65535, MongoDB 50000 rows per `insert_many`).

## SQL dialect notes

- Placeholders are `$n` in PostgreSQL and `?` in SQLite and MySQL. The same
  in raw SQL. Parameters are numbered across the whole query, subqueries
  included.
- Identifiers are double-quoted on PostgreSQL/SQLite and backtick-quoted on
  MySQL.
- `LIKE` patterns escape `%`, `_`, and `\` and add `ESCAPE '\'`.
- SQLite `LIKE` is ASCII case-insensitive, so case-sensitive `contains` /
  `startswith` / `endswith` compile to `instr` and `substr`.
- An empty `IN` list compiles to `(1=0)`. An empty needle matches
  everything.
- SQLite needs a `LIMIT` before `OFFSET`, so it emits `LIMIT -1 OFFSET n`.
  MySQL emits `LIMIT 18446744073709551615` for a bare `OFFSET`.
- `concat` treats `NULL` as empty text.
- A `NULL` value is written as the keyword `NULL`, not bound, so statement
  text stays distinct for SQLx’s prepared-statement cache. In `raw_sql` a
  `Value::Null` is sent untyped; keep `NULL` and non-`NULL` variants of a
  raw statement textually different (`WHERE x IS NULL`) or cast the
  parameter.

## MySQL

- **`RETURNING` emulation.** Generated keys come from `LAST_INSERT_ID()`
  stepped by `@@auto_increment_increment`, then rows are re-read by key.
  Other updates and deletes read the affected keys `FOR UPDATE` inside a
  transaction. Tables need a primary key; an update with `RETURNING` may
  not change it.
- `connect` pins `time_zone '+00:00'`, removes `NO_BACKSLASH_ESCAPES`, and
  raises `group_concat_max_len`. A pool given to `from_pool` must do the
  same.
- Default collation is case-insensitive. `contains` / `startswith` /
  `endswith` / regex are forced case-sensitive. Use `utf8mb4_bin` for
  case-sensitive equality.
- `UPDATE .. SET` assignments are reordered so every right-hand side sees
  the old values. A cycle (`SET a = b, b = a`) is `InvalidPlan`.
- Subqueries with `LIMIT`, and subqueries over the target of an
  `UPDATE`/`DELETE`, are wrapped in derived tables. A correlated `EXISTS`
  over the target table still fails with MySQL 1093.
- DDL commits implicitly. `TEXT` cannot be a primary key: use
  `VARCHAR(191)`. See [Migrations](/axumapi/guides/data/migrations/).

## MongoDB

- Requires MongoDB 5.0+. Transactions need a replica set (a single-node one
  is enough). No savepoints, so `bulk_create` inside a transaction is a
  capability error.
- A table is a collection. The primary key column (default `id`) is stored
  as `_id`. Missing keys are generated as consecutive `i64` values from
  `axumapi_counters`, **outside** the transaction, so rollbacks leave gaps.
- Reads compile to aggregation pipelines. Plans are checked and compiled
  entirely before any I/O.
- Predicates follow SQL three-valued logic: `= NULL` means `IS NULL`.
- `update()` / `delete()` on a queryset with a limit, `distinct`, or joins
  becomes `pk IN (subquery)` and is therefore a `Subqueries` capability
  error.
- No foreign keys or cascades. Only `_id` is unique, plus indexes made with
  `MongoBackend::create_unique_index`.
- Joins, subqueries, set operations, window functions, row locking,
  `DISTINCT ON`, and array aggregates are capability errors (HTTP 501).

## Redis

`RedisStore` wraps a `ConnectionManager` with an optional key prefix: keys
(`get`, `set` with TTL, `set_nx`, `del`, `exists`, `expire`, `ttl`,
`incr_by`), hashes, sets, `get_json` / `set_json`, and atomic `MULTI` /
`EXEC` pipelines. It does not implement QuerySet traits;
`BackendCapabilities::redis()` rejects every relational feature.

- Values are UTF-8; TTLs are milliseconds (`PX` / `PEXPIRE`), rejected
  below 1 ms. `set_nx` sets no TTL; `set` without a TTL clears an existing
  one.
- A command Redis rejects while queueing aborts the whole batch; a command
  failing during `EXEC` does not undo earlier ones.
- `pipeline` does not prefix keys: pass `store.key(..)`. `delete_namespace`
  matches `prefix*`, so end prefixes with a separator.
- Single server only: no cluster, pub/sub, lists, sorted sets, streams, or
  Lua.

`RedisCache` (feature `redis`) sits on this store. See
[Cache](/axumapi/guides/production/cache/).

## Raw queries

`Db::raw_sql` returns a `QueryResult`. Decode with
`result.decode::<Model>()` (columns by name),
`result.decode_values::<(A, B)>()` (columns by position), or
`result.scalar()`. `Row::get_as::<T>(column)` decodes one value.
`Db::raw_execute` returns the affected row count. Values are always bound;
never format untrusted input into the SQL text.

MongoDB rejects raw SQL (`Feature::RawSql`); use
`MongoBackend::raw_command`.

## See also

- [Backend matrix](/axumapi/reference/backend-matrix/)
- [QueryPlan IR](/axumapi/internals/query-plan/)
- [Transactions](/axumapi/guides/data/transactions/)
