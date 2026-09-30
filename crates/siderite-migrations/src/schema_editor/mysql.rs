//! MySQL / MariaDB DDL (MySQL 8.0.31+).
//!
//! The editor mirrors the PostgreSQL one in [`super`], with the dialect
//! differences MySQL forces on a schema tool:
//!
//! * **Quoting.** Identifiers use backticks (embedded backticks doubled).
//!   String literals escape backslashes as well as quotes, because MySQL
//!   treats `\` as an escape character unless `NO_BACKSLASH_ESCAPES` is set.
//! * **No transactional DDL.** Every DDL statement commits implicitly, so a
//!   migration that fails half way leaves its earlier statements applied and
//!   no history row is written. [`Migrator`](crate::executor::Migrator) does
//!   not wrap MySQL migrations in a transaction for that reason. Keep MySQL
//!   migrations small (one schema change each) so a failure is easy to repair.
//! * **`AUTO_INCREMENT`** primary keys: `BIGINT NOT NULL AUTO_INCREMENT
//!   PRIMARY KEY`.
//! * **`MODIFY COLUMN`** replaces the whole column definition, so an
//!   `AlterField` always restates type, nullability and default.
//! * **Foreign keys** are table-level `CONSTRAINT .. FOREIGN KEY` clauses
//!   named `<table>_<column>_fk`. MySQL parses an inline `REFERENCES` on a
//!   column but ignores it, so it is never used. InnoDB also rejects
//!   `ON DELETE SET DEFAULT`, which is reported as an error.
//! * **Keyed `TEXT`.** `TEXT`, `BLOB` and `JSON` columns cannot be a primary
//!   key, `UNIQUE` or indexed without a prefix length, and this editor never
//!   guesses one. Give such fields a `max_length` (a `VARCHAR`) instead.
//!   Literal defaults on these types are written as `DEFAULT ('..')`.
//! * `DROP INDEX` needs its table (`DROP INDEX name ON table`); a unique
//!   constraint is dropped as an index and a check constraint with
//!   `DROP CHECK`.
//! * `DbDefault::Now` is `CURRENT_TIMESTAMP(6)`, matching `DATETIME(6)`.
//!
//! Column types follow the MySQL backend's storage table: `TINYINT(1)`,
//! `DATETIME(6)` (UTC), `TIME(6)`, `CHAR(36)` for UUIDs, `JSON`, `DOUBLE`.
//! A decimal without precision becomes `DECIMAL(38,10)` (a bare `DECIMAL` is
//! `DECIMAL(10,0)` and would round to an integer).

use crate::error::MigrationError;
use crate::operation::Operation;
use crate::state::{
    ConstraintState, DbDefault, FieldState, ForeignKeyState, IndexState, ModelState, OnDelete,
    ProjectState, SqlType, auto_index_name,
};
use std::collections::BTreeSet;

/// Quote `ident` with backticks; embed a backtick by doubling it.
pub fn quote_ident(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len() + 2);
    out.push('`');
    for ch in ident.chars() {
        if ch == '`' {
            out.push('`');
        }
        out.push(ch);
    }
    out.push('`');
    out
}

/// Quote a MySQL string literal (quotes doubled, backslashes escaped).
pub fn quote_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        match ch {
            '\'' => out.push_str("''"),
            '\\' => out.push_str("\\\\"),
            '\0' => out.push_str("\\0"),
            other => out.push(other),
        }
    }
    out.push('\'');
    out
}

/// Render one operation against the schema `state` before the operation.
///
/// # Errors
/// [`MigrationError::State`] when the operation needs something MySQL cannot
/// express (keyed `TEXT`, `ON DELETE SET DEFAULT`) or refers to an unknown
/// model, field or constraint.
pub(super) fn render(op: &Operation, state: &ProjectState) -> Result<Vec<String>, MigrationError> {
    match op {
        Operation::CreateModel { model } => create_model_sql(model),
        Operation::DeleteModel { name } => {
            let model = state.require(name)?;
            Ok(vec![format!("DROP TABLE {}", quote_ident(&model.table))])
        }
        Operation::RenameModel {
            old_name, table, ..
        } => {
            let model = state.require(old_name)?;
            if model.table == *table {
                Ok(Vec::new())
            } else {
                Ok(vec![format!(
                    "ALTER TABLE {} RENAME TO {}",
                    quote_ident(&model.table),
                    quote_ident(table)
                )])
            }
        }
        Operation::AddField { model, field } => add_field_sql(state.require(model)?, field),
        Operation::RemoveField { model, name } => remove_field_sql(state.require(model)?, name),
        Operation::AlterField { model, name, field } => {
            alter_field_sql(state.require(model)?, name, field)
        }
        Operation::RenameField {
            model,
            old_name,
            new_name,
        } => rename_field_sql(state.require(model)?, old_name, new_name),
        Operation::CreateIndex { model, index } => {
            let m = state.require(model)?;
            Ok(vec![create_index_sql(m, index)?])
        }
        Operation::DeleteIndex { model, name } => {
            let m = state.require(model)?;
            Ok(vec![drop_index_sql(&m.table, name)])
        }
        Operation::AddConstraint { model, constraint } => {
            add_constraint_sql(state.require(model)?, constraint)
        }
        Operation::DeleteConstraint { model, name } => {
            delete_constraint_sql(state.require(model)?, name)
        }
        Operation::RunSQL { sql, .. } => Ok(vec![sql.clone()]),
        Operation::RunRust { .. } => Ok(Vec::new()),
    }
}

/// Whether the column type needs a prefix length to be keyed.
fn is_unkeyable(field: &FieldState) -> bool {
    match field.sql_type {
        SqlType::Text => field.max_length.is_none(),
        SqlType::Binary | SqlType::Json => true,
        _ => false,
    }
}

fn keyed_error(table: &str, column: &str, why: &str) -> MigrationError {
    MigrationError::state(format!(
        "MySQL cannot use column `{column}` of `{table}` as {why} without a length: \
         give the field a `max_length` so it becomes a VARCHAR"
    ))
}

/// Reject a key, unique or index over a column MySQL cannot key.
fn ensure_keyable(model: &ModelState, column: &str, why: &str) -> Result<(), MigrationError> {
    match model.column(column) {
        Some(field) if is_unkeyable(field) => Err(keyed_error(&model.table, column, why)),
        _ => Ok(()),
    }
}

fn fk_name(table: &str, column: &str) -> String {
    format!("{table}_{column}_fk")
}

fn create_model_sql(model: &ModelState) -> Result<Vec<String>, MigrationError> {
    check_field_keys(model)?;
    let mut parts = Vec::new();
    for field in &model.fields {
        parts.push(column_def(field, ColumnDef::Create)?);
    }
    for constraint in &model.constraints {
        if let ConstraintState::Unique { columns, .. } = constraint {
            for column in columns {
                ensure_keyable(model, column, "part of a UNIQUE constraint")?;
            }
        }
        parts.push(table_constraint_sql(constraint));
    }
    for field in &model.fields {
        if let Some(fk) = &field.fk {
            parts.push(foreign_key_clause(&model.table, &field.column, fk)?);
        }
    }
    let mut stmts = vec![format!(
        "CREATE TABLE {} (\n  {}\n)",
        quote_ident(&model.table),
        parts.join(",\n  ")
    )];
    stmts.extend(index_sqls(model)?);
    Ok(stmts)
}

/// Primary keys, unique columns and single-column indexes must be keyable.
fn check_field_keys(model: &ModelState) -> Result<(), MigrationError> {
    for field in &model.fields {
        check_field_key(&model.table, field)?;
    }
    Ok(())
}

fn check_field_key(table: &str, field: &FieldState) -> Result<(), MigrationError> {
    if !is_unkeyable(field) {
        return Ok(());
    }
    let why = if field.primary_key {
        "a primary key"
    } else if field.unique {
        "UNIQUE"
    } else if field.index {
        "indexed"
    } else {
        return Ok(());
    };
    Err(keyed_error(table, &field.column, why))
}

fn table_constraint_sql(constraint: &ConstraintState) -> String {
    match constraint {
        ConstraintState::Unique { name, columns } => {
            format!(
                "CONSTRAINT {} UNIQUE ({})",
                quote_ident(name),
                column_list(columns)
            )
        }
        ConstraintState::Check { name, sql } => {
            format!("CONSTRAINT {} CHECK ({sql})", quote_ident(name))
        }
    }
}

fn column_list(columns: &[String]) -> String {
    columns
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ")
}

fn foreign_key_clause(
    table: &str,
    column: &str,
    fk: &ForeignKeyState,
) -> Result<String, MigrationError> {
    Ok(format!(
        "CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({}) ON DELETE {}",
        quote_ident(&fk_name(table, column)),
        quote_ident(column),
        quote_ident(&fk.target_table),
        quote_ident(&fk.target_column),
        on_delete_sql(fk.on_delete, table, column)?
    ))
}

fn on_delete_sql(
    policy: OnDelete,
    table: &str,
    column: &str,
) -> Result<&'static str, MigrationError> {
    match policy {
        OnDelete::Cascade => Ok("CASCADE"),
        OnDelete::Protect => Ok("RESTRICT"),
        OnDelete::SetNull => Ok("SET NULL"),
        OnDelete::DoNothing => Ok("NO ACTION"),
        OnDelete::SetDefault => Err(MigrationError::state(format!(
            "MySQL (InnoDB) does not support ON DELETE SET DEFAULT (column `{column}` of `{table}`)"
        ))),
    }
}

/// Which clauses of a column definition to render.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ColumnDef {
    /// `CREATE TABLE` / `ADD COLUMN`: includes `UNIQUE` and `PRIMARY KEY`.
    Create,
    /// `MODIFY COLUMN`: type, nullability, default and `AUTO_INCREMENT` only.
    Modify,
}

fn column_def(field: &FieldState, mode: ColumnDef) -> Result<String, MigrationError> {
    let mut s = format!("{} {}", quote_ident(&field.column), type_sql(field));
    if field.auto && field.primary_key {
        s.push_str(" NOT NULL AUTO_INCREMENT");
        if mode == ColumnDef::Create {
            s.push_str(" PRIMARY KEY");
        }
        return Ok(s);
    }
    if !field.nullable {
        s.push_str(" NOT NULL");
    }
    if let Some(default) = &field.default {
        s.push_str(" DEFAULT ");
        s.push_str(&default_sql(field, default));
    }
    if mode == ColumnDef::Create {
        if field.unique && !field.primary_key {
            s.push_str(" UNIQUE");
        }
        if field.primary_key {
            s.push_str(" PRIMARY KEY");
        }
    }
    Ok(s)
}

fn type_sql(field: &FieldState) -> String {
    match field.sql_type {
        SqlType::SmallInt => "SMALLINT".to_owned(),
        SqlType::Integer => "INT".to_owned(),
        SqlType::BigInt | SqlType::Duration => "BIGINT".to_owned(),
        SqlType::Real => "FLOAT".to_owned(),
        SqlType::Double => "DOUBLE".to_owned(),
        SqlType::Decimal => match (field.max_digits, field.decimal_places) {
            (Some(p), Some(s)) => format!("DECIMAL({p},{s})"),
            _ => "DECIMAL(38,10)".to_owned(),
        },
        SqlType::Bool => "TINYINT(1)".to_owned(),
        SqlType::Text => match field.max_length {
            Some(n) => format!("VARCHAR({n})"),
            None => "TEXT".to_owned(),
        },
        SqlType::Binary => "BLOB".to_owned(),
        SqlType::Date => "DATE".to_owned(),
        SqlType::Time => "TIME(6)".to_owned(),
        SqlType::Timestamp => "DATETIME(6)".to_owned(),
        SqlType::Uuid => "CHAR(36)".to_owned(),
        SqlType::Json => "JSON".to_owned(),
        SqlType::IpAddr => "VARCHAR(45)".to_owned(),
    }
}

fn default_sql(field: &FieldState, default: &DbDefault) -> String {
    let literal = match default {
        DbDefault::Now => return "CURRENT_TIMESTAMP(6)".to_owned(),
        DbDefault::Int(i) => return i.to_string(),
        DbDefault::Bool(true) => return "TRUE".to_owned(),
        DbDefault::Bool(false) => return "FALSE".to_owned(),
        DbDefault::Text(s) => quote_string(s),
    };
    // TEXT, BLOB and JSON only accept an expression default: `DEFAULT ('x')`.
    if is_unkeyable(field) {
        format!("({literal})")
    } else {
        literal
    }
}

fn index_sqls(model: &ModelState) -> Result<Vec<String>, MigrationError> {
    let mut stmts = Vec::new();
    let mut seen = BTreeSet::new();
    for field in &model.fields {
        if field.index && !field.unique && !field.primary_key {
            let name = auto_index_name(&model.table, &field.column);
            if seen.insert(name.clone()) {
                let index = IndexState {
                    name,
                    columns: vec![field.column.clone()],
                    unique: false,
                };
                stmts.push(create_index_sql(model, &index)?);
            }
        }
    }
    for index in &model.indexes {
        if seen.insert(index.name.clone()) {
            stmts.push(create_index_sql(model, index)?);
        }
    }
    Ok(stmts)
}

fn create_index_sql(model: &ModelState, index: &IndexState) -> Result<String, MigrationError> {
    for column in &index.columns {
        ensure_keyable(model, column, "part of an index")?;
    }
    let unique = if index.unique { "UNIQUE " } else { "" };
    Ok(format!(
        "CREATE {unique}INDEX {} ON {} ({})",
        quote_ident(&index.name),
        quote_ident(&model.table),
        column_list(&index.columns)
    ))
}

fn drop_index_sql(table: &str, name: &str) -> String {
    format!("DROP INDEX {} ON {}", quote_ident(name), quote_ident(table))
}

fn add_field_sql(model: &ModelState, field: &FieldState) -> Result<Vec<String>, MigrationError> {
    check_field_key(&model.table, field)?;
    let table = quote_ident(&model.table);
    let mut stmts = vec![format!(
        "ALTER TABLE {table} ADD COLUMN {}",
        column_def(field, ColumnDef::Create)?
    )];
    if field.index && !field.unique && !field.primary_key {
        let index = IndexState {
            name: auto_index_name(&model.table, &field.column),
            columns: vec![field.column.clone()],
            unique: false,
        };
        stmts.push(create_index_sql(model, &index)?);
    }
    if let Some(fk) = &field.fk {
        stmts.push(format!(
            "ALTER TABLE {table} ADD {}",
            foreign_key_clause(&model.table, &field.column, fk)?
        ));
    }
    Ok(stmts)
}

fn require_field<'a>(model: &'a ModelState, name: &str) -> Result<&'a FieldState, MigrationError> {
    model.field(name).ok_or_else(|| {
        MigrationError::state(format!("field `{name}` not on model `{}`", model.name))
    })
}

fn remove_field_sql(model: &ModelState, name: &str) -> Result<Vec<String>, MigrationError> {
    let field = require_field(model, name)?;
    let table = quote_ident(&model.table);
    let mut stmts = Vec::new();
    if field.fk.is_some() {
        // The constraint must go before the column it covers.
        stmts.push(format!(
            "ALTER TABLE {table} DROP FOREIGN KEY {}",
            quote_ident(&fk_name(&model.table, &field.column))
        ));
    }
    stmts.push(format!(
        "ALTER TABLE {table} DROP COLUMN {}",
        quote_ident(&field.column)
    ));
    Ok(stmts)
}

/// Changes `AlterField` acts on: column name, type, nullability, default.
/// Unique and foreign-key changes are not applied (as on PostgreSQL).
fn alter_field_sql(
    model: &ModelState,
    name: &str,
    field: &FieldState,
) -> Result<Vec<String>, MigrationError> {
    let old = require_field(model, name)?;
    let table = quote_ident(&model.table);
    let mut stmts = Vec::new();
    if old.column != field.column {
        stmts.push(format!(
            "ALTER TABLE {table} RENAME COLUMN {} TO {}",
            quote_ident(&old.column),
            quote_ident(&field.column)
        ));
    }
    let definition_changed = old.sql_type != field.sql_type
        || old.max_length != field.max_length
        || old.max_digits != field.max_digits
        || old.decimal_places != field.decimal_places
        || old.nullable != field.nullable
        || old.default != field.default;
    if definition_changed {
        check_field_key(&model.table, field)?;
        stmts.push(format!(
            "ALTER TABLE {table} MODIFY COLUMN {}",
            column_def(field, ColumnDef::Modify)?
        ));
    }
    Ok(stmts)
}

fn rename_field_sql(
    model: &ModelState,
    old_name: &str,
    new_name: &str,
) -> Result<Vec<String>, MigrationError> {
    let field = require_field(model, old_name)?;
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
    model: &ModelState,
    constraint: &ConstraintState,
) -> Result<Vec<String>, MigrationError> {
    if let ConstraintState::Unique { columns, .. } = constraint {
        for column in columns {
            ensure_keyable(model, column, "part of a UNIQUE constraint")?;
        }
    }
    Ok(vec![format!(
        "ALTER TABLE {} ADD {}",
        quote_ident(&model.table),
        table_constraint_sql(constraint)
    )])
}

fn delete_constraint_sql(model: &ModelState, name: &str) -> Result<Vec<String>, MigrationError> {
    let constraint = model.constraint(name).ok_or_else(|| {
        MigrationError::state(format!("constraint `{name}` not on model `{}`", model.name))
    })?;
    Ok(vec![match constraint {
        // A unique constraint is an index in MySQL.
        ConstraintState::Unique { .. } => drop_index_sql(&model.table, name),
        ConstraintState::Check { .. } => format!(
            "ALTER TABLE {} DROP CHECK {}",
            quote_ident(&model.table),
            quote_ident(name)
        ),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_use_backticks() {
        assert_eq!(quote_ident("users"), "`users`");
        assert_eq!(quote_ident("we`ird"), "`we``ird`");
    }

    #[test]
    fn strings_escape_quotes_and_backslashes() {
        assert_eq!(quote_string("it's"), "'it''s'");
        assert_eq!(quote_string(r"a\b"), r"'a\\b'");
        assert_eq!(quote_string("a\0b"), "'a\\0b'");
    }
}
