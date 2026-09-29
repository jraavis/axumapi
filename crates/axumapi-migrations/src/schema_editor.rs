//! DDL generation for PostgreSQL, SQLite and MySQL.
//!
//! PostgreSQL and SQLite identifiers are double-quoted (embedded `"`
//! doubled). SQLite `AlterField` / `RemoveField` / constraint changes rebuild
//! the table (create new, copy, drop, rename). MySQL has its own dialect
//! (backticks, `AUTO_INCREMENT`, `MODIFY COLUMN`, no transactional DDL); see
//! the [`mysql`] module. Other backends return
//! [`MigrationError::UnsupportedBackend`] before any I/O.

pub mod mysql;

use crate::error::MigrationError;
use crate::operation::Operation;
use crate::state::{
    ConstraintState, DbDefault, FieldState, ForeignKeyState, ModelState, OnDelete, ProjectState,
    SqlType, auto_index_name,
};
use axumapi_orm::BackendKind;

/// Quote `ident` with double quotes; embed a quote by doubling it.
pub fn quote_ident(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 2);
    out.push('"');
    for ch in ident.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Quote `ident` for the SQL dialect of `kind`: backticks for MySQL, double
/// quotes for everything else.
pub fn quote_identifier(kind: BackendKind, ident: &str) -> String {
    match kind {
        BackendKind::MySql => mysql::quote_ident(ident),
        _ => quote_ident(ident),
    }
}

/// Quote a SQL string literal with single quotes.
pub fn quote_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push('\'');
        }
        out.push(ch);
    }
    out.push('\'');
    out
}

/// Render `operations` against `state`, applying each to a cloned state so
/// later ops see earlier ones (needed for SQLite rebuilds).
///
/// # Errors
/// [`MigrationError::UnsupportedBackend`] for anything other than PostgreSQL,
/// SQLite or MySQL, or [`MigrationError::State`] when an op cannot be rendered.
pub fn statements(
    kind: BackendKind,
    state: &ProjectState,
    operations: &[Operation],
) -> Result<Vec<String>, MigrationError> {
    require_sql(kind)?;
    let mut current = state.clone();
    let mut out = Vec::new();
    for op in operations {
        out.extend(render(kind, op, &current)?);
        op.apply_to_state(&mut current)?;
    }
    Ok(out)
}

/// Render a single operation against the current schema `state` (before the op).
///
/// # Errors
/// See [`statements`].
pub fn render(
    kind: BackendKind,
    op: &Operation,
    state: &ProjectState,
) -> Result<Vec<String>, MigrationError> {
    require_sql(kind)?;
    if kind == BackendKind::MySql {
        return mysql::render(op, state);
    }
    match op {
        Operation::CreateModel { model } => Ok(create_model_sql(kind, model)),
        Operation::DeleteModel { name } => {
            let model = state.require(name)?;
            Ok(vec![format!("DROP TABLE {}", quote_ident(&model.table))])
        }
        Operation::AddField { model, field } => add_field_sql(kind, state.require(model)?, field),
        Operation::RemoveField { model, name } => {
            remove_field_sql(kind, state.require(model)?, name)
        }
        Operation::AlterField { model, name, field } => {
            alter_field_sql(kind, state.require(model)?, name, field)
        }
        Operation::RenameField {
            model,
            old_name,
            new_name,
        } => rename_field_sql(kind, state.require(model)?, old_name, new_name),
        Operation::CreateIndex { model, index } => {
            let m = state.require(model)?;
            Ok(vec![create_index_sql(&m.table, index)])
        }
        Operation::DeleteIndex { model, name } => {
            state.require(model)?;
            Ok(vec![format!("DROP INDEX {}", quote_ident(name))])
        }
        Operation::AddConstraint { model, constraint } => {
            add_constraint_sql(kind, state.require(model)?, constraint)
        }
        Operation::DeleteConstraint { model, name } => {
            delete_constraint_sql(kind, state.require(model)?, name)
        }
        Operation::RunSQL { sql, .. } => Ok(vec![sql.clone()]),
        Operation::RunRust { .. } => Ok(Vec::new()),
    }
}

fn require_sql(kind: BackendKind) -> Result<(), MigrationError> {
    match kind {
        BackendKind::Postgres | BackendKind::Sqlite | BackendKind::MySql => Ok(()),
        other => Err(MigrationError::UnsupportedBackend(other)),
    }
}

fn create_model_sql(kind: BackendKind, model: &ModelState) -> Vec<String> {
    let mut stmts = vec![create_table_sql(kind, model, &model.table)];
    stmts.extend(index_sqls(model));
    stmts
}

fn create_table_sql(kind: BackendKind, model: &ModelState, table: &str) -> String {
    let mut parts: Vec<String> = model.fields.iter().map(|f| column_def(kind, f)).collect();
    for constraint in &model.constraints {
        parts.push(table_constraint_sql(constraint));
    }
    let body = parts.join(",\n  ");
    format!("CREATE TABLE {} (\n  {}\n)", quote_ident(table), body)
}

fn table_constraint_sql(constraint: &ConstraintState) -> String {
    match constraint {
        ConstraintState::Unique { name, columns } => {
            format!(
                "CONSTRAINT {} UNIQUE ({})",
                quote_ident(name),
                columns
                    .iter()
                    .map(|c| quote_ident(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        ConstraintState::Check { name, sql } => {
            format!("CONSTRAINT {} CHECK ({sql})", quote_ident(name))
        }
    }
}

fn column_def(kind: BackendKind, field: &FieldState) -> String {
    let mut s = format!("{} {}", quote_ident(&field.column), type_sql(kind, field));
    if field.auto && field.primary_key {
        if let Some(fk) = &field.fk {
            s.push(' ');
            s.push_str(&references_sql(fk));
        }
        return s;
    }
    if !field.nullable {
        s.push_str(" NOT NULL");
    }
    if let Some(default) = &field.default {
        s.push_str(" DEFAULT ");
        s.push_str(&default_sql(kind, default));
    }
    if field.unique && !field.primary_key {
        s.push_str(" UNIQUE");
    }
    if field.primary_key {
        s.push_str(" PRIMARY KEY");
    }
    if let Some(fk) = &field.fk {
        s.push(' ');
        s.push_str(&references_sql(fk));
    }
    s
}

fn type_sql(kind: BackendKind, field: &FieldState) -> String {
    if field.auto && field.primary_key {
        return match kind {
            BackendKind::Sqlite => "INTEGER PRIMARY KEY AUTOINCREMENT".to_owned(),
            BackendKind::Postgres => {
                let base = match field.sql_type {
                    SqlType::SmallInt | SqlType::Integer => "INTEGER",
                    _ => "BIGINT",
                };
                format!("{base} GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY")
            }
            _ => sql_type_name(kind, field),
        };
    }
    sql_type_name(kind, field)
}

fn sql_type_name(kind: BackendKind, field: &FieldState) -> String {
    match (kind, field.sql_type) {
        (_, SqlType::Decimal) => match (field.max_digits, field.decimal_places) {
            (Some(p), Some(s)) => format!("NUMERIC({p},{s})"),
            _ => "NUMERIC".to_owned(),
        },
        (_, SqlType::Text) => match field.max_length {
            Some(n) => format!("VARCHAR({n})"),
            None => "TEXT".to_owned(),
        },
        (_, SqlType::Duration) => "BIGINT".to_owned(),
        (_, SqlType::IpAddr) => "TEXT".to_owned(),
        (BackendKind::Postgres, SqlType::SmallInt) => "SMALLINT".to_owned(),
        (BackendKind::Postgres, SqlType::Integer) => "INTEGER".to_owned(),
        (BackendKind::Postgres, SqlType::BigInt) => "BIGINT".to_owned(),
        (BackendKind::Postgres, SqlType::Real) => "REAL".to_owned(),
        (BackendKind::Postgres, SqlType::Double) => "DOUBLE PRECISION".to_owned(),
        (BackendKind::Postgres, SqlType::Bool) => "BOOLEAN".to_owned(),
        (BackendKind::Postgres, SqlType::Binary) => "BYTEA".to_owned(),
        (BackendKind::Postgres, SqlType::Date) => "DATE".to_owned(),
        (BackendKind::Postgres, SqlType::Time) => "TIME".to_owned(),
        (BackendKind::Postgres, SqlType::Timestamp) => "TIMESTAMPTZ".to_owned(),
        (BackendKind::Postgres, SqlType::Uuid) => "UUID".to_owned(),
        (BackendKind::Postgres, SqlType::Json) => "jsonb".to_owned(),
        (BackendKind::Sqlite, SqlType::SmallInt | SqlType::Integer) => "INTEGER".to_owned(),
        (BackendKind::Sqlite, SqlType::BigInt) => "BIGINT".to_owned(),
        (BackendKind::Sqlite, SqlType::Real | SqlType::Double) => "REAL".to_owned(),
        (BackendKind::Sqlite, SqlType::Bool) => "INTEGER".to_owned(),
        (BackendKind::Sqlite, SqlType::Binary) => "BLOB".to_owned(),
        (BackendKind::Sqlite, SqlType::Date | SqlType::Time | SqlType::Timestamp) => {
            "TEXT".to_owned()
        }
        (BackendKind::Sqlite, SqlType::Uuid | SqlType::Json) => "TEXT".to_owned(),
        _ => "TEXT".to_owned(),
    }
}

fn default_sql(kind: BackendKind, default: &DbDefault) -> String {
    match default {
        DbDefault::Now => match kind {
            BackendKind::Postgres => "CURRENT_TIMESTAMP".to_owned(),
            BackendKind::Sqlite => {
                // `%f` is SS.SSS, so `%H:%M:%f` yields `HH:MM:SS.sss`. RFC3339
                // (used by `DateTime::from_value`) accepts the `Z` suffix.
                "(strftime('%Y-%m-%dT%H:%M:%fZ','now'))".to_owned()
            }
            _ => "CURRENT_TIMESTAMP".to_owned(),
        },
        DbDefault::Int(i) => i.to_string(),
        DbDefault::Bool(true) => match kind {
            BackendKind::Sqlite => "1".to_owned(),
            _ => "TRUE".to_owned(),
        },
        DbDefault::Bool(false) => match kind {
            BackendKind::Sqlite => "0".to_owned(),
            _ => "FALSE".to_owned(),
        },
        DbDefault::Text(s) => quote_string(s),
    }
}

fn references_sql(fk: &ForeignKeyState) -> String {
    format!(
        "REFERENCES {} ({}) ON DELETE {}",
        quote_ident(&fk.target_table),
        quote_ident(&fk.target_column),
        on_delete_sql(fk.on_delete)
    )
}

fn on_delete_sql(policy: OnDelete) -> &'static str {
    match policy {
        OnDelete::Cascade => "CASCADE",
        OnDelete::Protect => "RESTRICT",
        OnDelete::SetNull => "SET NULL",
        OnDelete::SetDefault => "SET DEFAULT",
        OnDelete::DoNothing => "NO ACTION",
    }
}

fn index_sqls(model: &ModelState) -> Vec<String> {
    let mut stmts = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for field in &model.fields {
        if field.index && !field.unique && !field.primary_key {
            let name = auto_index_name(&model.table, &field.column);
            if seen.insert(name.clone()) {
                stmts.push(create_index_sql(
                    &model.table,
                    &crate::state::IndexState {
                        name,
                        columns: vec![field.column.clone()],
                        unique: false,
                    },
                ));
            }
        }
    }
    for index in &model.indexes {
        if seen.insert(index.name.clone()) {
            stmts.push(create_index_sql(&model.table, index));
        }
    }
    stmts
}

fn create_index_sql(table: &str, index: &crate::state::IndexState) -> String {
    let unique = if index.unique { "UNIQUE " } else { "" };
    format!(
        "CREATE {unique}INDEX {} ON {} ({})",
        quote_ident(&index.name),
        quote_ident(table),
        index
            .columns
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn add_field_sql(
    kind: BackendKind,
    model: &ModelState,
    field: &FieldState,
) -> Result<Vec<String>, MigrationError> {
    if kind == BackendKind::Sqlite && (field.unique || field.primary_key) {
        let mut new_model = model.clone();
        new_model.fields.push(field.clone());
        return Ok(sqlite_rebuild(model, &new_model));
    }
    let def = column_def(kind, field);
    let mut stmts = vec![format!(
        "ALTER TABLE {} ADD COLUMN {def}",
        quote_ident(&model.table)
    )];
    if field.index && !field.unique && !field.primary_key {
        stmts.push(create_index_sql(
            &model.table,
            &crate::state::IndexState {
                name: auto_index_name(&model.table, &field.column),
                columns: vec![field.column.clone()],
                unique: false,
            },
        ));
    }
    Ok(stmts)
}

fn remove_field_sql(
    kind: BackendKind,
    model: &ModelState,
    name: &str,
) -> Result<Vec<String>, MigrationError> {
    let field = model.field(name).ok_or_else(|| {
        MigrationError::state(format!("field `{name}` not on model `{}`", model.name))
    })?;
    if kind == BackendKind::Sqlite {
        let mut new_model = model.clone();
        new_model.fields.retain(|f| f.name != name);
        new_model
            .indexes
            .retain(|i| !i.columns.iter().any(|c| c == &field.column));
        return Ok(sqlite_rebuild(model, &new_model));
    }
    Ok(vec![format!(
        "ALTER TABLE {} DROP COLUMN {}",
        quote_ident(&model.table),
        quote_ident(&field.column)
    )])
}

fn alter_field_sql(
    kind: BackendKind,
    model: &ModelState,
    name: &str,
    field: &FieldState,
) -> Result<Vec<String>, MigrationError> {
    let old = model.field(name).ok_or_else(|| {
        MigrationError::state(format!("field `{name}` not on model `{}`", model.name))
    })?;
    if kind == BackendKind::Sqlite {
        let mut new_model = model.clone();
        if let Some(existing) = new_model.field_mut(name) {
            *existing = field.clone();
        }
        return Ok(sqlite_rebuild(model, &new_model));
    }
    let mut stmts = Vec::new();
    let table = quote_ident(&model.table);
    if old.column != field.column {
        stmts.push(format!(
            "ALTER TABLE {table} RENAME COLUMN {} TO {}",
            quote_ident(&old.column),
            quote_ident(&field.column)
        ));
    }
    let col = quote_ident(&field.column);
    if old.sql_type != field.sql_type
        || old.max_length != field.max_length
        || old.max_digits != field.max_digits
        || old.decimal_places != field.decimal_places
    {
        stmts.push(format!(
            "ALTER TABLE {table} ALTER COLUMN {col} TYPE {}",
            sql_type_name(kind, field)
        ));
    }
    if old.nullable != field.nullable {
        if field.nullable {
            stmts.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {col} DROP NOT NULL"
            ));
        } else {
            stmts.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {col} SET NOT NULL"
            ));
        }
    }
    if old.default != field.default {
        match &field.default {
            Some(d) => stmts.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {col} SET DEFAULT {}",
                default_sql(kind, d)
            )),
            None => stmts.push(format!(
                "ALTER TABLE {table} ALTER COLUMN {col} DROP DEFAULT"
            )),
        }
    }
    Ok(stmts)
}

fn rename_field_sql(
    _kind: BackendKind,
    model: &ModelState,
    old_name: &str,
    new_name: &str,
) -> Result<Vec<String>, MigrationError> {
    let field = model.field(old_name).ok_or_else(|| {
        MigrationError::state(format!("field `{old_name}` not on model `{}`", model.name))
    })?;
    if field.column == old_name && old_name != new_name {
        return Ok(vec![format!(
            "ALTER TABLE {} RENAME COLUMN {} TO {}",
            quote_ident(&model.table),
            quote_ident(&field.column),
            quote_ident(new_name)
        )]);
    }
    Ok(Vec::new())
}

fn add_constraint_sql(
    kind: BackendKind,
    model: &ModelState,
    constraint: &ConstraintState,
) -> Result<Vec<String>, MigrationError> {
    if kind == BackendKind::Sqlite {
        let mut new_model = model.clone();
        new_model.constraints.push(constraint.clone());
        return Ok(sqlite_rebuild(model, &new_model));
    }
    Ok(vec![format!(
        "ALTER TABLE {} ADD {}",
        quote_ident(&model.table),
        table_constraint_sql(constraint)
    )])
}

fn delete_constraint_sql(
    kind: BackendKind,
    model: &ModelState,
    name: &str,
) -> Result<Vec<String>, MigrationError> {
    if kind == BackendKind::Sqlite {
        let mut new_model = model.clone();
        new_model.constraints.retain(|c| c.name() != name);
        return Ok(sqlite_rebuild(model, &new_model));
    }
    Ok(vec![format!(
        "ALTER TABLE {} DROP CONSTRAINT {}",
        quote_ident(&model.table),
        quote_ident(name)
    )])
}

/// SQLite table-rebuild: create new, copy overlapping columns, drop, rename,
/// recreate indexes.
fn sqlite_rebuild(old: &ModelState, new: &ModelState) -> Vec<String> {
    let tmp = format!("{}__axumapi_new", new.table);
    let mut stmts = vec!["PRAGMA foreign_keys = OFF".to_owned()];
    stmts.push(create_table_sql(BackendKind::Sqlite, new, &tmp));

    let copied: Vec<(&FieldState, &FieldState)> = new
        .fields
        .iter()
        .filter_map(|nf| {
            old.fields
                .iter()
                .find(|of| of.name == nf.name || of.column == nf.column)
                .map(|of| (of, nf))
        })
        .collect();
    if !copied.is_empty() {
        let dest = copied
            .iter()
            .map(|(_, nf)| quote_ident(&nf.column))
            .collect::<Vec<_>>()
            .join(", ");
        let src = copied
            .iter()
            .map(|(of, nf)| {
                if of.sql_type == nf.sql_type {
                    quote_ident(&of.column)
                } else {
                    format!(
                        "CAST({} AS {})",
                        quote_ident(&of.column),
                        sql_type_name(BackendKind::Sqlite, nf)
                    )
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        stmts.push(format!(
            "INSERT INTO {} ({dest}) SELECT {src} FROM {}",
            quote_ident(&tmp),
            quote_ident(&old.table)
        ));
    }

    stmts.push(format!("DROP TABLE {}", quote_ident(&old.table)));
    stmts.push(format!(
        "ALTER TABLE {} RENAME TO {}",
        quote_ident(&tmp),
        quote_ident(&new.table)
    ));
    stmts.extend(index_sqls(new));
    stmts.push("PRAGMA foreign_keys = ON".to_owned());
    stmts
}
