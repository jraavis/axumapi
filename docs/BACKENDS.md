# Backends

## Status

| Backend | SQL compile | Execution | Notes |
|---|---|---|---|
| SQLite | yes | yes (`SqliteBackend`, SQLx) | Row locking, regex, `DISTINCT ON`, arrays and `STDDEV` / `VARIANCE` are rejected with a capability error |
| PostgreSQL | yes | yes (`PgBackend`, SQLx, feature `postgres`), **not yet run against a server** | The executor compiles and the dialect is covered by compiled-SQL assertions. `tests/postgres.rs` runs only when `DATABASE_URL` starts with `postgres` and is skipped otherwise, so it has not been executed in CI-less development. Run it once with `docker run -d -e POSTGRES_PASSWORD=postgres -p 5432:5432 postgres:16` and `DATABASE_URL=postgres://postgres:postgres@localhost/postgres cargo test -p axumapi-backends --all-features --test postgres` |
| MySQL | Phase 5 | Phase 5 | |
| MongoDB | Phase 5 | Phase 5 | Compiles only the supported subset to filters and pipelines |
| Redis | not applicable | Phase 5 | A specialised key/hash/set API; **not** a QuerySet backend |
| DynamoDB | not applicable | not applicable | Design note only; not planned for v1 |

## Feature matrix (implemented)

| Feature | PostgreSQL | SQLite |
|---|---|---|
| Filtering, ordering, limit, offset, distinct | yes | yes |
| `DISTINCT ON` | yes | capability error |
| Joins (`join`, `select_related`) | yes | yes |
| Transactions, savepoints | yes | yes |
| Isolation levels | read committed, repeatable read, serializable | serializable only |
| Row locking (`select_for_update`) | yes, plus `nowait`, `skip_locked` | capability error before any I/O |
| Window functions | yes | yes (3.25+) |
| Aggregates: count, sum, avg, min, max | yes | yes |
| `StdDev`, `Variance` | yes | capability error (`StatisticalAggregates`) |
| `StringAgg` | `STRING_AGG` | `group_concat` (no `DISTINCT`) |
| `ArrayAgg` | yes (decoded as a JSON array) | capability error (`Arrays`) |
| Regex lookup | `~` | capability error |
| Case-insensitive lookups | `ILIKE` | `LOWER(x) LIKE LOWER(?)` |
| Set operations | yes | yes |
| `RETURNING` | yes | yes (3.35+) |
| Bind parameter limit (`max_params`) | 65535 | 32766 |

Bulk operations chunk their rows to `max_params`, so a 10 000 row `bulk_create` of a seven-column model is three `INSERT`s on SQLite and two on PostgreSQL.

## Canonical storage forms

PostgreSQL stores every `Value` natively. SQLite has fewer storage classes, so the adapter binds and expects the canonical forms from `axumapi_orm::types`. Decoding accepts both the native and the canonical form.

| Rust type | PostgreSQL | SQLite |
|---|---|---|
| `bool` | `boolean` | integer 0 / 1 |
| `i16` / `i32` / `i64` | `smallint` / `integer` / `bigint` | integer |
| `f32` / `f64` | `real` / `double precision` | real |
| `Decimal` | `numeric` | declare the column `NUMERIC`; bound as text, which SQLite converts to a number for comparison and `SUM` |
| `Uuid` | `uuid` | text (hyphenated) |
| `NaiveDate` | `date` | text `YYYY-MM-DD` |
| `NaiveTime` | `time` | text `HH:MM:SS.ffffff` |
| `DateTime<Utc>` | `timestamptz` | text `YYYY-MM-DDTHH:MM:SS.ffffffZ` (fixed width, so text order is time order) |
| `serde_json::Value` | `jsonb` | text |
| `Vec<u8>` | `bytea` | blob |

Two details matter in practice:

* **Decimals on SQLite.** A `TEXT` column would compare and aggregate `"9.99"` after `"15.00"`. With `NUMERIC` affinity SQLite stores numbers, and the ORM reads them back through `Decimal::from_value`, which accepts integers and floats. Very long decimals lose precision beyond a double.
* **Numeric results on PostgreSQL.** `SUM` and `AVG` return `numeric`; integer and float types decode a `Decimal` (integral, respectively any), so `row.get_as::<i64>("total")` works on both backends.

## Dialect notes

* Placeholders are `$n` in PostgreSQL and `?` in SQLite, and the same in raw SQL: `db.raw_sql("SELECT .. WHERE id = ?", params![id])` on SQLite, `... WHERE id = $1` on PostgreSQL. Parameters are numbered across the whole query, subqueries included.
* Identifiers are always double-quoted, and any embedded `"` is doubled.
* `LIKE` patterns escape `%`, `_` and `\` and add `ESCAPE '\'`.
* SQLite `LIKE` is ASCII case-insensitive, so case-sensitive `contains`, `startswith` and `endswith` compile to `instr` and `substr` there.
* An empty `IN` list compiles to `(1=0)`. An empty needle matches everything.
* SQLite needs a `LIMIT` before `OFFSET`, so it emits `LIMIT -1 OFFSET n`.
* Date parts: PostgreSQL uses `CAST(EXTRACT(.. FROM x) AS BIGINT)` (the adapter pins each connection to UTC); SQLite uses `CAST(strftime(..) AS INTEGER)` on the canonical text forms. `week` is the ISO 8601 week on both, computed on SQLite from the Thursday of the week. `quarter` is derived from the month there.
* `concat` treats `NULL` as empty text on both: PostgreSQL `CONCAT`, SQLite `COALESCE(CAST(x AS TEXT), '') || ..`.
* Integer literals are bound as 64-bit integers. PostgreSQL functions that take `integer` (`SUBSTR`, `NTILE`, `LAG`) get a `CAST` or a literal from the compiler. `LAG(x, n, default)` needs `default` to have the column's exact type; cast it when the column is not `bigint`.
* A `NULL` value is written as the keyword `NULL`, not bound, so the server infers its type from context and SQLx's statement cache (keyed by SQL text) never reuses a statement that was prepared with an inferred parameter type for a later value. In `raw_sql` a `Value::Null` parameter is sent untyped (OID 0); keep `NULL` and non-`NULL` variants of a raw statement textually different (`WHERE x IS NULL`) or cast the parameter (`$1::text`).

## Locking notes

* `select_for_update()` keeps locks until the surrounding transaction ends; use it inside `Db::transaction`. Outside one, PostgreSQL releases the locks immediately.
* PostgreSQL refuses `FOR UPDATE` on the nullable side of an outer join, so do not combine it with `select_related` or filters through nullable foreign keys.

## Raw queries

`Db::raw_sql` returns a `QueryResult`; decode it with `result.decode::<Model>()` (columns by name), `result.decode_values::<(A, B)>()` (columns by position) or `result.scalar()`. `Row::get_as::<T>(column)` decodes one value. `Db::raw_execute` returns the affected row count. Values are always bound; never format untrusted input into the SQL text.

## Deferred

* Reverse foreign-key and many-to-many `prefetch_related` (the model struct has no slot to hold the children); use `ManyToManyManager::queryset()` or a filtered `QuerySet` for those.
* Multi-hop `prefetch_related` (use `select_related`).
* Window frames (`ROWS BETWEEN ..`).
* `INNER JOIN` for non-null foreign keys (all traversals use `LEFT JOIN`, which returns the same rows for them).
* MySQL, MongoDB and Redis adapters (Phase 5).
