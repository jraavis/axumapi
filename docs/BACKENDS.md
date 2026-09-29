# Backends

## Phase 1 status

| Backend | SQL compile | Execution | Notes |
|---|---|---|---|
| SQLite | ✅ | ✅ (`SqliteBackend`, SQLx) | Row locking and regex are rejected with a capability error |
| PostgreSQL | ✅ | ⏳ Phase 4 | The dialect is tested through compiled-SQL assertions |
| MySQL | ⏳ Phase 5 | ⏳ | |
| MongoDB | ⏳ Phase 5 | ⏳ | Compiles only the supported subset to filters and pipelines |
| Redis | — | ⏳ Phase 5 | A specialised key/hash/set API; **not** a QuerySet backend |
| DynamoDB | — | — | Design note only; not planned for v1 |

## Dialect notes (implemented)

* Placeholders are `$n` in PostgreSQL and `?` in SQLite. Parameters are numbered across the whole query, subqueries included.
* Identifiers are always double-quoted, and any embedded `"` is doubled.
* `LIKE` patterns escape `%`, `_` and `\` and add `ESCAPE '\'`.
* SQLite `LIKE` is ASCII case-insensitive. Case-sensitive `contains`, `startswith` and `endswith` therefore compile to `instr` and `substr` there. Case-insensitive lookups use `LOWER(x) LIKE LOWER(?)`; PostgreSQL uses `ILIKE`.
* An empty `IN` list compiles to `(1=0)`. An empty needle matches everything.
* SQLite needs a `LIMIT` before `OFFSET`, so it emits `LIMIT -1 OFFSET n`.

## Planned feature matrix (targets, not claims)

| Feature | PostgreSQL | MySQL | SQLite | MongoDB | Redis |
|---|---|---|---|---|---|
| Basic filtering | Yes | Yes | Yes | Yes | Key/index lookups only |
| Transactions | Yes | Yes (InnoDB) | Yes | Replica set / sharded cluster only | MULTI/EXEC, no rollback |
| Savepoints | Yes | Yes | Yes | No ORM equivalent | No |
| Joins | Yes | Yes | Yes | `$lookup` (left outer only) | No |
| Row locking | Yes (+NOWAIT/SKIP LOCKED) | 8.0+ for NOWAIT/SKIP LOCKED | No | No | No |
| Window functions | Yes | 8.0+ | 3.25+ | `$setWindowFields` (5.0+) | No |
| JSON fields | jsonb | JSON | JSON1 functions | Native documents | Serialized string/hash |
| Many-to-many | Yes | Yes | Yes | Emulated with arrays | No |
