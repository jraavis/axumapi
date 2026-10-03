//! Progress helpers for the migration executor.

use super::*;

/// Progress of a non-transactional migration, so a re-run resumes after the
/// last completed *statement* instead of replaying committed statements.
///
/// MySQL commits every DDL statement implicitly, and a PostgreSQL migration
/// with `atomic: false` runs outside a transaction where every statement
/// autocommits, so a failed migration of either leaves earlier statements
/// applied and no history row. The next `migrate` reads this row, skips the
/// operations it records and, inside the operation it stopped in, skips the
/// statements already committed — one operation can render several (a
/// `CreateModel` with indexes, an `AlterField` changing type *and* an index),
/// and replaying those fails with "already exists". Directions are tracked
/// separately because forward and reverse walk different operation lists.
pub(super) const PROGRESS_TABLE: &str = "siderite_migration_progress";
pub(super) const PROGRESS_APPLY: &str = "apply";
pub(super) const PROGRESS_UNAPPLY: &str = "unapply";

/// How far a re-run has to skip: `ops` operations are complete and `stmts`
/// statements of operation `ops` are already committed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Progress {
    pub(super) ops: usize,
    pub(super) stmts: usize,
}

pub(super) fn progress_ddl(kind: BackendKind) -> String {
    // MySQL cannot key a TEXT column without a prefix length.
    let id_type = if kind == BackendKind::MySql {
        "VARCHAR(255)"
    } else {
        "TEXT"
    };
    let dir_type = if kind == BackendKind::MySql {
        "VARCHAR(16)"
    } else {
        "TEXT"
    };
    format!(
        "CREATE TABLE IF NOT EXISTS {} ({} {id_type} PRIMARY KEY, {} INTEGER NOT NULL, {} INTEGER NOT NULL DEFAULT 0, {} {dir_type} NOT NULL, {} {id_type} NOT NULL)",
        quote_star(kind, PROGRESS_TABLE),
        quote_star(kind, "migration_id"),
        quote_star(kind, "op_index"),
        quote_star(kind, "stmt_index"),
        quote_star(kind, "direction"),
        quote_star(kind, "checksum"),
    )
}

/// Drop a progress row left by the other direction: a half-applied forward
/// run must not skip reverse operations, or vice versa.
pub(super) async fn reset_progress_direction(
    db: &Db,
    kind: BackendKind,
    id: &str,
    direction: &str,
) -> Result<(), MigrationError> {
    db.execute_script(&progress_ddl(kind)).await?;
    db.raw_execute(
        &format!(
            "DELETE FROM {} WHERE {} = {} AND {} <> {}",
            quote_star(kind, PROGRESS_TABLE),
            quote_star(kind, "migration_id"),
            placeholder(kind, 1),
            quote_star(kind, "direction"),
            placeholder(kind, 2),
        ),
        vec![
            Value::Text(id.to_owned()),
            Value::Text(direction.to_owned()),
        ],
    )
    .await?;
    Ok(())
}

/// Completed-statement progress for `id`/`direction` (zero when no row).
///
/// The row records the checksum of the migration that wrote it. Indices into
/// an edited migration point at different operations, so a mismatch refuses
/// to resume instead of skipping statements that never ran.
pub(super) async fn load_progress(
    db: &Db,
    kind: BackendKind,
    id: &str,
    checksum: &str,
    direction: &str,
) -> Result<Progress, MigrationError> {
    let rows = db
        .raw_sql(
            &format!(
                "SELECT {}, {}, {} FROM {} WHERE {} = {} AND {} = {}",
                quote_star(kind, "op_index"),
                quote_star(kind, "stmt_index"),
                quote_star(kind, "checksum"),
                quote_star(kind, PROGRESS_TABLE),
                quote_star(kind, "migration_id"),
                placeholder(kind, 1),
                quote_star(kind, "direction"),
                placeholder(kind, 2),
            ),
            vec![
                Value::Text(id.to_owned()),
                Value::Text(direction.to_owned()),
            ],
        )
        .await?;
    let Some(row) = rows.rows.first() else {
        return Ok(Progress::default());
    };
    let stored = match row.get("checksum") {
        Some(Value::Text(stored)) => stored.as_str(),
        _ => "",
    };
    if stored != checksum {
        return Err(MigrationError::state(format!(
            "migration `{id}` changed since a partial {direction} stopped \
             (progress checksum {stored:?}, file {checksum:?}); restore the \
             original file, or reconcile the schema by hand and delete its \
             row from `{PROGRESS_TABLE}`"
        )));
    }
    Ok(Progress {
        ops: super::recovery::read_index(row, "op_index")?,
        stmts: super::recovery::read_index(row, "stmt_index")?,
    })
}

/// Record `progress` for `id`/`direction`. Called after every statement, so a
/// failure inside a multi-statement operation resumes at the next statement
/// rather than replaying the ones MySQL already committed.
pub(super) async fn save_progress(
    db: &Db,
    kind: BackendKind,
    id: &str,
    checksum: &str,
    direction: &str,
    progress: Progress,
) -> Result<(), MigrationError> {
    // The migrator holds the backend lock, so no concurrent writer can race
    // the UPDATE-then-INSERT.
    let updated = db
        .raw_execute(
            &format!(
                "UPDATE {} SET {} = {}, {} = {} WHERE {} = {} AND {} = {}",
                quote_star(kind, PROGRESS_TABLE),
                quote_star(kind, "op_index"),
                placeholder(kind, 1),
                quote_star(kind, "stmt_index"),
                placeholder(kind, 2),
                quote_star(kind, "migration_id"),
                placeholder(kind, 3),
                quote_star(kind, "direction"),
                placeholder(kind, 4),
            ),
            vec![
                Value::Int(progress.ops as i64),
                Value::Int(progress.stmts as i64),
                Value::Text(id.to_owned()),
                Value::Text(direction.to_owned()),
            ],
        )
        .await?;
    // MySQL reports *changed* rows, not matched ones, so an UPDATE writing the
    // values already stored returns 0. Upsert there so that case cannot turn
    // into a duplicate-key INSERT. Keyed on the connection, not the dialect:
    // unit tests drive the MySQL path over SQLite.
    if updated == 0 {
        let upsert = if db.capabilities().kind == BackendKind::MySql {
            format!(
                " ON DUPLICATE KEY UPDATE {op} = VALUES({op}), {stmt} = VALUES({stmt})",
                op = quote_star(kind, "op_index"),
                stmt = quote_star(kind, "stmt_index"),
            )
        } else {
            String::new()
        };
        db.raw_execute(
            &format!(
                "INSERT INTO {} ({}, {}, {}, {}, {}) VALUES ({}, {}, {}, {}, {}){upsert}",
                quote_star(kind, PROGRESS_TABLE),
                quote_star(kind, "migration_id"),
                quote_star(kind, "op_index"),
                quote_star(kind, "stmt_index"),
                quote_star(kind, "direction"),
                quote_star(kind, "checksum"),
                placeholder(kind, 1),
                placeholder(kind, 2),
                placeholder(kind, 3),
                placeholder(kind, 4),
                placeholder(kind, 5),
            ),
            vec![
                Value::Text(id.to_owned()),
                Value::Int(progress.ops as i64),
                Value::Int(progress.stmts as i64),
                Value::Text(direction.to_owned()),
                Value::Text(checksum.to_owned()),
            ],
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn clear_progress(
    db: &Db,
    kind: BackendKind,
    id: &str,
    direction: &str,
) -> Result<(), MigrationError> {
    db.raw_execute(
        &format!(
            "DELETE FROM {} WHERE {} = {} AND {} = {}",
            quote_star(kind, PROGRESS_TABLE),
            quote_star(kind, "migration_id"),
            placeholder(kind, 1),
            quote_star(kind, "direction"),
            placeholder(kind, 2),
        ),
        vec![
            Value::Text(id.to_owned()),
            Value::Text(direction.to_owned()),
        ],
    )
    .await?;
    Ok(())
}
