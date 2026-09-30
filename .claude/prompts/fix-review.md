# Siderite review fixes — one finding per chat

Paste everything below the line into a new Claude Code chat (model: Sonnet) opened in
`/Users/raavi/dev/siderite`. Repeat in a fresh chat until the checklist is done.

---

You are fixing findings from a critical review of siderite (Rust web framework,
workspace in `crates/`). Work on **exactly one** finding this session, then stop.

## Procedure

1. Read `CLAUDE.md`, `AGENTS.md`, `CONTRIBUTING.md`. Obey them. In particular:
   **never add `Co-Authored-By` or any AI attribution to commits.**
2. Open `.claude/prompts/fix-review.md` and pick the **first unchecked** item (`- [ ]`)
   in the checklist below. Items are ordered by priority; do not skip ahead.
3. **Verify** the finding against the current code first. Reviewers read code without
   running it, so a finding can be wrong or already fixed. If it is invalid, mark it
   `- [~]` with a one-line reason, commit that, and stop.
4. Work on the branch `fix/review-findings` (check it out; create it from `master`
   only if it does not exist). All fixes stack there, one commit each, so this
   checklist's ticks carry forward to the next chat.
5. **Write a failing test first** that reproduces the bug (unit or integration, following
   neighbouring tests' style). Run it and confirm it fails for the stated reason.
6. Make the smallest fix that makes it pass. Match surrounding code style, comment
   density and naming. Update doc comments / `website/` docs / `CHANGELOG.md` if behaviour
   or defaults change.
7. Run, and make green:
   ```
   cargo fmt --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```
   Live-DB tests (Postgres/MySQL/Mongo/Redis) need `docker compose up -d --wait` and the
   URLs in `docker-compose.yml`'s header. Run them if the fix touches that backend; if you
   cannot, say so explicitly in the summary.
8. Commit with Conventional Commits (`fix(<crate>): ...`), no AI attribution.
   Tick the item `- [x]` in this file in the same commit, appending the commit's short
   subject.
9. Do not push or open a PR unless the user asks. Report: finding, root cause, test
   added, fix, commands run and results, anything left open. Then stop.

Scope rules: one finding per session. If the fix naturally reveals a closely coupled
bug, note it as a new unchecked item at the end of the list rather than fixing it.
If a finding needs a design decision (API break, new default), ask the user before
implementing.

## Checklist

### P0 — regressions from M1–M5 (do these first; inserted ahead of M6 per priority order)
- [x] M7 `executor.rs:229-247` + `postgres.rs:219-229` + `mysql.rs:321-331` leaked session lock — fix(backends): close lock-held connections instead of pooling them
- [x] M8 `sqlite.rs:237-262` `PRAGMA foreign_key_check` runs whole-DB — fix(backends): scope SQLite check to new violations, skip when FKs were off
- [x] M9 `sqlite.rs:162-182` SQLite `RunRust` runs with FKs off — fix(migrations): document FK-off RunRust + pin with test
- [x] M10 `executor.rs:873,970` SQLite all-or-nothing run — fix(migrations): document single-txn run, drop dead branches
- [x] M11 `executor.rs:519` MySQL `GET_LOCK` 30s timeout — fix(migrations): wait indefinitely like pg_advisory_lock
- [x] M12 M3 follow-up: per-op progress still missing — fix(migrations): MySQL resume + MysqlOpPartial for RunRust
- [x] M13 M4 override for false positives — fix(migrations): allow_drop_model/allow_drop_field hints
- [x] M14 `operation.rs:141-157` RenameModel stale fk.target_table — fix(migrations): retarget referencing FKs
- [x] M15 `executor.rs` Report.sql vs planned — fix(migrations): render sql under lock from re-read plan
- [x] M16 `executor.rs` MysqlPartial without DDL — fix(migrations): raise only after committed DDL
- [x] M18 `operation.rs:~162` `RenameModel` skips the renamed model, so self-referencing FKs keep the old table — fix(migrations): retarget self-referencing FKs on model rename
- [x] M19 `executor.rs` MySQL resume was per-operation, so a multi-statement operation replayed committed statements; `MysqlOpPartial` missed RunRust-only partials — fix(migrations): resume MySQL migrations per statement

### P1 — migrations (data loss)
- [x] M1 `crates/siderite-migrations/src/schema_editor.rs:~506` `sqlite_rebuild` emits `PRAGMA foreign_keys = OFF` inside the migration transaction (SQLite ignores it there; `runs_in_transaction` is true for SQLite, `executor.rs:481`). `DROP TABLE` then cascades/deletes child rows. Also forces FKs ON afterward regardless of prior state. Fix: `PRAGMA defer_foreign_keys = ON` + `PRAGMA foreign_key_check` after rebuild, or run the PRAGMA outside the txn; restore prior value. Test: parent+child with `ON DELETE CASCADE`, AlterField on parent, assert child rows survive. — fix(migrations): preserve child rows across SQLite table rebuilds
- [x] M2 `executor.rs:72-116` no lock around migrate; concurrent replicas double-apply. Add `pg_advisory_lock`, MySQL `GET_LOCK`, SQLite `BEGIN IMMEDIATE`; re-read history under lock. — fix(migrations): lock migrate against concurrent replicas
- [x] M3 `executor.rs:395-433` MySQL non-transactional DDL + history written only at end → partial apply, stuck re-runs. Record per-operation progress or split history per op; document. — fix(migrations): name the MySQL statement that failed after committed DDL
- [x] M4 `autodetector.rs:59-74` model rename becomes DeleteModel+CreateModel (data loss); unhinted field rename = Remove+Add. Add model rename hints and a loud warning/refusal when a diff drops and adds a same-shaped table/column. — fix(migrations): refuse unhinted same-shape renames
- [x] M5 `executor.rs:412-419` SQL ops run before all RunRust ops regardless of declared order (and same on reverse). Interleave in declared order. — fix(migrations): run RunRust in declared order among SQL
- [ ] M6a `schema_editor.rs:543-557` SQLite rebuild emits bare `CAST("col" AS <type>)` on type change; SQLite CAST is lossy/silent (`CAST('abc' AS INTEGER)`→0, REAL→INTEGER truncation). Fix: whitelist safe casts (same family, int widening, VARCHAR n→m>=n), else `MigrationError::state` directing to a `RunSQL` backfill. Test: TEXT 'abc'→Integer AlterField errors instead of CAST; safe widening still emits CAST.
- [ ] M6b `schema_editor.rs:527-536` rebuild pairs columns by `of.name == nf.name || of.column == nf.column` with first-match wins, so a rename-to-existing-column or swap pairs the wrong source and column reuse across Remove+Add copies unrelated data. Fix: two-pass pairing (pass 1 by field `name`, pass 2 by `column` only for unmatched names whose column is not already claimed), ambiguous/duplicate sources → `MigrationError::state`. Test: swapped columns preserve data by field; reused column does not copy.
- [ ] M6c `schema_editor.rs:565-572` rebuild `DROP TABLE` silently drops triggers and any `sqlite_master` indexes not in `ProjectState` (e.g. via `RunSQL`, partial/expression indexes); only state-tracked `index_sqls(new)` are recreated. Fix (do both): in the `executor.rs` SQLite path snapshot `SELECT type,name,sql FROM sqlite_master WHERE tbl_name=...` before DROP and re-apply user indexes/triggers after RENAME, and refuse with `MigrationError::state` when an object has NULL/unportable SQL. Test: table with trigger + extra index survives AlterField rebuild; unportable object errors loudly.
- [ ] M6d `schema_editor.rs:428-437` Postgres `ALTER COLUMN TYPE` has no `USING`, so incompatible conversions fail or mis-cast. Fix: always emit `ALTER TABLE {t} ALTER COLUMN {c} TYPE {ty} USING {c}::{ty}`; check the MySQL `MODIFY COLUMN` path (`mysql.rs:448-461`) for the same lossy case. Test: Postgres AlterField snapshot contains `USING`.
- [ ] M6e `schema_editor.rs:351-377` + `mysql.rs:383-405` `AddField NOT NULL` without default emits bare `ADD COLUMN ... NOT NULL`, which fails on non-empty tables (Postgres errors; SQLite/MySQL require a non-null default). Fix: return `MigrationError::state` when `!nullable && default.is_none()`, and provide the supported null-fill method (nullable-first `AddField` + `RunSQL` backfill `UPDATE` + `AlterField SET NOT NULL`, or a column `default` for new rows); document the recipe in `website/` docs. Test: NOT NULL-no-default AddField errors on all three backends; with-default still renders.

### P2 — exploitable defaults
- [ ] C1 `crates/siderite-core/src/middleware/limits.rs:187` rate limiter keys on the FIRST `X-Forwarded-For` entry (client-controlled). Use right-most, or Nth-from-right with configurable trusted hop count.
- [ ] C2 `limits.rs:203-210` bucket map unbounded past `MAX_TRACKED_CLIENTS`; O(n) retain under global mutex. Hard cap + amortised eviction.
- [ ] C3 `crates/siderite-cache/src/route.rs` unkeyed `X-Forwarded-*`, `Forwarded`, `X-Original-*`, `Accept-Language`, tenant headers → cache poisoning. Bypass/key on forwarding headers; configurable keyed-header set.
- [ ] C4 `crates/siderite-cache/src/memory.rs` capacity counts entries not bytes (1024×1 MiB); O(capacity) purge per insert when full. Byte-weighted budget; drop full-scan purge.
- [ ] C5 `crates/siderite-config/src/error.rs:36-49` redaction by leaf key name only; wrong-typed parent node echoes full value (e.g. `SIDERITE_DATABASES__DEFAULT=postgres://u:pw@...`). Empty-path TOML parse errors unredacted. Redact values by default, allowlist non-secret keys.
- [ ] C6 `crates/siderite-core/src/app.rs:72-77` docs routes on by default and outside app middleware; `crates/siderite-openapi/src/ui.rs` CDN scripts on floating majors without SRI. Ask the user which default they want before changing it.
- [ ] C7 `crates/siderite-testkit/src/database.rs:62-74` no guard against running fixtures/migrations on a real `DATABASE_URL`.
- [ ] C8 `crates/siderite-core/src/middleware/hosts.rs:116-137` `HttpsRedirect` uses unvalidated Host for `Location` and trusts `X-Forwarded-Proto` by default.

### P3 — correctness
- [ ] B1 `crates/siderite-backends/src/mysql.rs:72-109` + `sql/dialect.rs:193` MySQL `LIKE ... ESCAPE '\\'` depends on session `sql_mode`; `from_pool` skips setup; proxies lose it. Use an escape char independent of `NO_BACKSLASH_ESCAPES` (e.g. `!`) and set session vars at handshake.
- [ ] B2 `crates/siderite-backends/src/sqlite.rs:34` in-memory detection only matches `:memory:`; `mode=memory` URLs get a multi-connection pool of separate DBs.
- [ ] O1 `crates/siderite-orm/src/db.rs:373-394` nested transaction (savepoint) has no drop guard on cancellation; `ROLLBACK TO` failure replaces the caller's error (`:388-392`).
- [ ] O2 `crates/siderite-orm/src/queryset/write.rs:135` `get_or_create` treats any Constraint error as a lost race.
- [ ] O3 `crates/siderite-orm/src/queryset/related.rs:292-306` O(n²) prefetch dedupe via Debug strings; user prefetch querysets with limit apply per chunk.
- [ ] H1 `crates/siderite-core/src/middleware/limits.rs:45-82` no body-read timeout; `ConcurrencyLimit` queues unbounded.
- [ ] H2 `crates/siderite-core/src/middleware/cors.rs:124-140` credentials + any origin accepted silently.
- [ ] V1 `crates/siderite-validation/src/types/decimal.rs:141` lax `Decimal` from JSON float uses `from_f64_retain` (0.1 → 0.1000…0555); `19.99` fails `Decimal<10,2>`.
- [ ] V2 `crates/siderite-validation/src/model.rs:127-195` float `multiple_of` absolute epsilon; int/float compare via f64.
- [ ] V3 `crates/siderite-validation/src/rules.rs:59-76` email accepts `a@b.`, control chars, no length cap. Also fix `Constraint::Pattern` doc (model.rs) that claims full-string match — matching is intentionally unanchored per `rules.rs:40`.
- [ ] V4 `crates/siderite-validation/src/validate.rs:150-160` f32 overflow silently becomes inf.
- [ ] K1 `crates/siderite-macros/src/route/mod.rs` stacked route attrs emit duplicate fn; handler `#[cfg]` not copied to generated route fn. Add trybuild UI tests.
- [ ] K2 `crates/siderite-macros` Schema/Validate/serde divergence: `skip_serializing_if` ignored by Schema required; struct `tag`/`content`/container `rename` silently ignored; enum alias/skip handling differs (`schema/enums.rs` vs `validate/enums.rs`).
- [ ] P1 `crates/siderite-openapi/src/builder.rs:75-132` `{*rest}` path keys produce invalid spec; mid-segment params undocumented.
- [ ] L1 `crates/siderite-cli/src/dbshell.rs:93-172` sqlite path starting with `-` becomes a sqlite3 option; mysql drops `ssl-mode`.
- [ ] L2 `crates/siderite-cli/src/scaffold.rs:167-175` `siderite new` non-atomic; `--path` interpolated unescaped into Cargo.toml.

### P4 — CI, benches, API surface
- [ ] X1 `.github/workflows/ci.yml` add `cargo hack --each-feature` + `--no-default-features` job; `--locked`; MSRV runs tests; harden Mongo rs.initiate readiness.
- [ ] X2 `docs/BENCHMARKS.md`, `website/src/content/docs/contributing/benchmarks.md`, README table: FastAPI comparison unfair (1 Uvicorn worker vs multi-core, `ab` without `-k`, side-by-side on one machine, non-identical responses, no percentiles). Reconcile docs; ask user before re-running or removing numbers.
- [ ] X3 `crates/siderite-bench/benches/extract.rs:61-86` clone inside timed loop; unequal baselines.
- [ ] X4 `crates/siderite/src/lib.rs:15,32` `pub use siderite_core::*` glob widens semver surface — ask the user before changing public API.
- [ ] X5 `examples/blog_postgres/src/auth.rs:114-134` FNV password hash in an example; switch to argon2.
- [ ] M17 PostgreSQL `atomic = false` migrations run outside a transaction with history written only at the end — the same stuck re-run M3 had on MySQL — but progress/resume is MySQL-only, so a failure still replays committed statements. Extend the progress table to every non-transactional run, or document the gap.
