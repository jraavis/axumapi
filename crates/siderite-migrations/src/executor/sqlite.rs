//! Sqlite helpers for the migration executor.

use super::*;

/// One `sqlite_master` object dropped with the old table but unknown to the
/// new model state (a trigger or a hand-made index).
struct SqlitePreservedObject {
    obj_type: String,
    name: String,
    sql: String,
}

/// Triggers and hand-made indexes to re-create after a SQLite rebuild.
pub(super) struct SqlitePreserved {
    table: String,
    objects: Vec<SqlitePreservedObject>,
}

/// Read the triggers and hand-made indexes a SQLite rebuild would drop.
///
/// SQLite-only; returns `None` when `op` does not rebuild a table. Objects
/// whose `sql` is `NULL` (SQLite auto-indexes) carry no `CREATE` statement
/// and are skipped, as are indexes the rebuild recreates itself (matched by
/// name). Views that mention the table fail closed here: the `DROP` would
/// succeed and leave them dangling, so the migration aborts instead. The
/// view check is conservative on purpose — SQLite has no dependency catalog,
/// so any view whose SQL mentions the table name refuses the rebuild; drop
/// or migrate the view first.
pub(super) async fn load_sqlite_rebuild_extras(
    db: &Db,
    kind: BackendKind,
    state: &ProjectState,
    op: &Operation,
) -> Result<Option<SqlitePreserved>, MigrationError> {
    if kind != BackendKind::Sqlite {
        return Ok(None);
    }
    let Some(target) = schema_editor::sqlite_rebuild_target(state, op) else {
        return Ok(None);
    };
    let views = db
        .raw_sql(
            "SELECT name, sql FROM sqlite_master WHERE type = 'view' AND sql IS NOT NULL",
            Vec::new(),
        )
        .await?;
    for row in &views.rows {
        let name = text_at(row, "name");
        let sql = text_at(row, "sql");
        if schema_editor::sqlite_sql_mentions_ident(&sql, &target.table) {
            return Err(MigrationError::state(format!(
                "refusing to rebuild table `{}`: view `{name}` depends on it; drop or migrate the view first",
                target.table,
            )));
        }
    }
    let found = db
        .raw_sql(
            "SELECT type, name, sql FROM sqlite_master WHERE type IN ('trigger','index') AND tbl_name = ? AND sql IS NOT NULL",
            vec![Value::Text(target.table.clone())],
        )
        .await?;
    let mut objects = Vec::new();
    for row in &found.rows {
        let obj_type = text_at(row, "type");
        let name = text_at(row, "name");
        let sql = text_at(row, "sql");
        if name.is_empty() || sql.is_empty() {
            continue;
        }
        // The rebuild recreates these by name; re-running their old
        // `CREATE` would fail with "already exists".
        if obj_type == "index" && target.recreated_indexes.contains(&name) {
            continue;
        }
        objects.push(SqlitePreservedObject {
            obj_type,
            name,
            sql,
        });
    }
    if objects.is_empty() {
        return Ok(None);
    }
    // Fail closed before touching the table when an extra references a
    // column the rebuild drops or renames; re-creating it afterwards would
    // fail with a bare "no such column".
    let removed: Vec<&str> = target
        .old_columns
        .iter()
        .map(String::as_str)
        .filter(|c| !target.new_columns.iter().any(|n| n == *c))
        .collect();
    let mut blocked = Vec::new();
    for obj in &objects {
        if removed
            .iter()
            .any(|c| schema_editor::sqlite_sql_mentions_ident(&obj.sql, c))
        {
            blocked.push(format!("{} `{}`", obj.obj_type, obj.name));
        }
    }
    if !blocked.is_empty() {
        let verb = if blocked.len() == 1 {
            "references"
        } else {
            "reference"
        };
        return Err(MigrationError::state(format!(
            "refusing to rebuild table `{}`: {} {verb} a removed/renamed column; update or drop them first",
            target.table,
            blocked.join(", "),
        )));
    }
    Ok(Some(SqlitePreserved {
        table: target.table,
        objects,
    }))
}

/// Re-create preserved triggers and hand-made indexes after the rebuild.
///
/// Runs inside the same SQLite transaction, so a failure still rolls the
/// rebuild back; the error names the object and the underlying cause.
pub(super) async fn restore_sqlite_rebuild_extras(
    db: &Db,
    preserved: SqlitePreserved,
) -> Result<(), MigrationError> {
    // Indexes first for a deterministic replay (`index` < `trigger`).
    let mut objects = preserved.objects;
    objects.sort_by(|a, b| (&a.obj_type, &a.name).cmp(&(&b.obj_type, &b.name)));
    for obj in objects {
        if let Err(err) = db.execute_script(&obj.sql).await {
            return Err(MigrationError::state(format!(
                "re-creating {} `{}` on table `{}` failed after rebuild: {err}; update or drop it first",
                obj.obj_type, obj.name, preserved.table,
            )));
        }
    }
    Ok(())
}

/// Text column of a `sqlite_master` row (`""` when missing or non-text).
pub(super) fn text_at(row: &siderite_orm::Row, key: &str) -> String {
    match row.get(key) {
        Some(Value::Text(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Fail closed when a SQLite rebuild `CAST` would change stored values.
///
/// Runs before the copy while the old table still exists; a non-empty probe
/// means e.g. `'abc' -> 0` or `3.7 -> 3`, so the migration aborts instead of
/// cementing truncated data.
pub(super) async fn reject_lossy_sqlite_cast(
    db: &Db,
    kind: BackendKind,
    state: &ProjectState,
    op: &Operation,
) -> Result<(), MigrationError> {
    if kind != BackendKind::Sqlite {
        return Ok(());
    }
    for check in schema_editor::sqlite_lossy_cast_checks(state, op) {
        let rows = db.raw_sql(&check.sql, Vec::new()).await?;
        if !rows.rows.is_empty() {
            return Err(MigrationError::state(format!(
                "refusing to cast {}.{}: table `{}` has values that would change under the cast",
                check.table, check.column, check.table,
            )));
        }
    }
    Ok(())
}

/// Fail closed before a NOT NULL change that existing rows cannot satisfy.
///
/// Adding a NOT NULL column with no default to a table with rows errors on
/// Postgres/SQLite and silently fills zero values on non-strict MySQL; so does
/// tightening a column that still holds NULLs. Probe first and name the
/// column so the author can add a default or a backfill.
pub(super) async fn reject_not_null_violations(
    db: &Db,
    kind: BackendKind,
    state: &ProjectState,
    op: &Operation,
) -> Result<(), MigrationError> {
    let (model, column, sql) = match op {
        Operation::AddField { model, field } if !field.nullable && field.default.is_none() => {
            let table = &state.require(model)?.table;
            let sql = format!("SELECT 1 FROM {} LIMIT 1", quote_star(kind, table));
            (model, &field.column, sql)
        }
        Operation::AlterField { model, name, field } if !field.nullable => {
            let current = state.require(model)?;
            let Some(old) = current.field(name) else {
                return Ok(());
            };
            if !old.nullable {
                return Ok(());
            }
            let sql = format!(
                "SELECT 1 FROM {} WHERE {} IS NULL LIMIT 1",
                quote_star(kind, &current.table),
                quote_star(kind, &old.column),
            );
            (model, &field.column, sql)
        }
        _ => return Ok(()),
    };
    if db.raw_sql(&sql, Vec::new()).await?.rows.is_empty() {
        return Ok(());
    }
    let table = &state.require(model)?.table;
    Err(MigrationError::state(format!(
        "refusing NOT NULL on {table}.{column}: existing rows would violate it; \
         add a default, or backfill with RunRust/RunSql first"
    )))
}
