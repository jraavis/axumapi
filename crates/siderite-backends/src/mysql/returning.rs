//! MySQL stored-row reconstruction and RETURNING emulation.

use super::io::{MySqlIo, WriteDone};
use super::metadata::{TableInfo, get};
use crate::shared::affected_result;
use crate::sql::{CompiledQuery, MySql};
use crate::sql::{compile, compile_write_bare};
use siderite_orm::InsertPlan;
use siderite_orm::expr::Ident;
use siderite_orm::types::canonical_text;
use siderite_orm::{BinaryOp, ExecResult, Expr, LockMode, OrmError};
use siderite_orm::{QueryError, QueryPlan, Row, Value, WritePlan};
use std::collections::HashMap;

/// The statements a write with emulated `RETURNING` needs, compiled up front
/// so plan errors (capabilities, structure) surface before any I/O.
pub(super) struct Prepared {
    /// The write itself, without `RETURNING`.
    pub(super) write: CompiledQuery,
    /// `DELETE`: the locking read of the affected rows. (An `UPDATE`'s read
    /// selects the primary key, known only once the table is looked up; see
    /// [`reads`].)
    read: Option<CompiledQuery>,
}

pub(super) fn table_of(plan: &WritePlan) -> &str {
    match plan {
        WritePlan::Insert(p) => &p.table,
        WritePlan::Update(p) => &p.table,
        WritePlan::Delete(p) => &p.table,
    }
}

pub(super) fn prepare(plan: &WritePlan) -> Result<Prepared, OrmError> {
    let write = compile_write_bare(plan, &MySql)?;
    let read = match plan {
        WritePlan::Insert(_) | WritePlan::Update(_) => None,
        WritePlan::Delete(p) => {
            let filter = p.filter.as_ref();
            Some(locking_read(&p.table, filter, &p.returning))
        }
    };
    let read = read.map(|plan| compile(&plan, &MySql)).transpose()?;
    Ok(Prepared { write, read })
}

/// `SELECT columns FROM table WHERE filter FOR UPDATE`.
fn locking_read<C: Clone + Into<Ident>>(
    table: &str,
    filter: Option<&Expr>,
    columns: &[C],
) -> QueryPlan {
    let mut plan = QueryPlan::from_table(table.to_owned());
    plan.filter = filter.cloned();
    plan.lock = Some(LockMode::ForUpdate);
    for column in columns {
        plan = plan.select(Expr::col(column.clone()), None);
    }
    plan
}

/// The key of a filter of the form `pk = value` on a single-column key.
fn key_filter(info: &TableInfo, filter: Option<&Expr>) -> Option<Vec<Value>> {
    let [pk] = info.primary_key.as_slice() else {
        return None;
    };
    let Some(Expr::Binary {
        op: BinaryOp::Eq,
        lhs,
        rhs,
    }) = filter
    else {
        return None;
    };
    match (&**lhs, &**rhs) {
        (Expr::Column(c), Expr::Value(v)) | (Expr::Value(v), Expr::Column(c))
            if c.source.is_none() && c.name == pk.as_str() && !v.is_null() =>
        {
            Some(vec![v.clone()])
        }
        _ => None,
    }
}

pub(super) async fn write_returning<C: MySqlIo>(
    conn: &mut C,
    info: &TableInfo,
    plan: &WritePlan,
    prepared: Prepared,
) -> Result<ExecResult, OrmError> {
    let columns = plan.returning();
    let Prepared { write, read } = prepared;
    match plan {
        WritePlan::Insert(p) => {
            if let Some(row) = synthesize(info, p) {
                let done = conn.run(&write.sql, write.params).await?;
                return synthesized_result(info, row, &done);
            }
            let keys = InsertKeys::of(info, p)?;
            let done = conn.run(&write.sql, write.params).await?;
            let inserted = done.rows_affected();
            let keys = match keys {
                InsertKeys::Supplied(keys) => keys,
                InsertKeys::Generated => {
                    let first = done.last_insert_id();
                    generated_keys(conn, first, inserted).await?
                }
            };
            let rows = read_by_keys(conn, info, columns, &keys).await?;
            if rows.len() as u64 != inserted {
                return Err(QueryError::Model(format!(
                    "inserted {inserted} rows into `{}` but read back {}",
                    info.table,
                    rows.len()
                ))
                .into());
            }
            Ok(ExecResult {
                rows_affected: inserted,
                returning: rows,
            })
        }
        WritePlan::Update(p) => {
            if p.assignments.iter().any(|(column, _)| is_key(info, column)) {
                return Err(QueryError::InvalidPlan(
                    concat!(
                        "MySQL cannot return the rows of an update ",
                        "that changes the primary key",
                    )
                    .into(),
                )
                .into());
            }
            let keys = match key_filter(info, p.filter.as_ref()) {
                Some(keys) => vec![keys],
                None => reads(conn, info, p.filter.as_ref()).await?,
            };
            if keys.is_empty() {
                return Ok(affected_result(0));
            }
            let done = conn.run(&write.sql, write.params).await?;
            let rows = if done.rows_affected() == 0 {
                Vec::new()
            } else {
                read_by_keys(conn, info, columns, &keys).await?
            };
            Ok(ExecResult {
                rows_affected: done.rows_affected(),
                returning: rows,
            })
        }
        WritePlan::Delete(_) => {
            // Read and lock the rows before deleting them.
            let missing = || QueryError::Model("missing delete read".into());
            let read = read.ok_or_else(missing)?;
            let rows = conn.fetch(&read.sql, read.params).await?.rows;
            let done = conn.run(&write.sql, write.params).await?;
            Ok(ExecResult {
                rows_affected: done.rows_affected(),
                returning: rows,
            })
        }
    }
}

/// A returned column of a single-row `INSERT` whose stored value is known
/// without reading the row back.
pub(super) enum Known {
    /// The supplied value, stored unchanged.
    Supplied(Value),
    /// The omitted `AUTO_INCREMENT` column: `LAST_INSERT_ID()`.
    Generated,
}

/// The row a single-row `INSERT` returns, if every returned column is known
/// without a read-back (see the module docs). `None` means: read it back.
pub(super) fn synthesize(
    info: &TableInfo,
    plan: &siderite_orm::InsertPlan,
) -> Option<Vec<(String, Known)>> {
    let [row] = plan.rows.as_slice() else {
        return None;
    };
    if info.insert_triggers {
        return None;
    }
    plan.returning
        .iter()
        .map(|column| {
            let name: &str = column;
            let auto = info.auto_increment.as_deref() == Some(name);
            let known = match plan.columns.iter().position(|c| c == column) {
                // A supplied `AUTO_INCREMENT` value of 0 or NULL is generated.
                Some(_) if auto => return None,
                Some(i) => {
                    let value = row.get(i)?;
                    let meta = info.columns.iter().find(|c| c.name == name)?;
                    if !meta.stores_exactly(value) {
                        return None;
                    }
                    Known::Supplied(value.clone())
                }
                None if auto => Known::Generated,
                // Omitted: a default or a generated column.
                None => return None,
            };
            Some((name.to_owned(), known))
        })
        .collect()
}

/// The result of a single-row `INSERT` whose row is [`synthesize`]d.
pub(super) fn synthesized_result(
    info: &TableInfo,
    row: Vec<(String, Known)>,
    done: &WriteDone,
) -> Result<ExecResult, OrmError> {
    if done.rows_affected() != 1 {
        return Err(QueryError::Model(format!(
            "inserted {} rows into `{}` instead of 1",
            done.rows_affected(),
            info.table
        ))
        .into());
    }
    let columns = row
        .into_iter()
        .map(|(name, known)| {
            let value = match known {
                Known::Supplied(value) => value,
                Known::Generated => {
                    let key = done.last_insert_id();
                    generated_value(info, key)?
                }
            };
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, QueryError>>()?;
    Ok(ExecResult {
        rows_affected: 1,
        returning: vec![Row::new(columns)],
    })
}

fn generated_value(info: &TableInfo, key: u64) -> Result<Value, QueryError> {
    match i64::try_from(key) {
        Ok(id) if id != 0 => Ok(Value::Int(id)),
        _ => Err(QueryError::Model(format!(
            "no usable generated key for `{}`",
            info.table,
        ))),
    }
}

/// How the keys of the rows of an `INSERT` are known.
enum InsertKeys {
    /// The statement supplies them (one entry per row, in primary-key order).
    Supplied(Vec<Vec<Value>>),
    /// The primary key is `AUTO_INCREMENT` and omitted.
    Generated,
}

impl InsertKeys {
    fn of(info: &TableInfo, plan: &InsertPlan) -> Result<Self, QueryError> {
        let positions: Option<Vec<usize>> = info
            .primary_key
            .iter()
            .map(|key| plan.columns.iter().position(|c| c == key.as_str()))
            .collect();
        if let Some(positions) = positions {
            let keys = plan
                .rows
                .iter()
                .map(|row| positions.iter().map(|&i| row[i].clone()).collect())
                .collect();
            return Ok(Self::Supplied(keys));
        }
        match (info.primary_key.as_slice(), &info.auto_increment) {
            ([key], Some(auto)) if key == auto => Ok(Self::Generated),
            _ => Err(QueryError::Model(format!(
                concat!(
                    "cannot read back rows inserted into `{}`: ",
                    "its primary key is neither supplied nor AUTO_INCREMENT",
                ),
                info.table
            ))),
        }
    }
}

/// Keys `first, first + step, ...` of a multi-row insert of `count` rows.
///
/// InnoDB allocates the block of values of a plain `INSERT .. VALUES` in one
/// go under every `innodb_autoinc_lock_mode`, and `LAST_INSERT_ID()` is the
/// first of them.
async fn generated_keys<C: MySqlIo>(
    conn: &mut C,
    first: u64,
    count: u64,
) -> Result<Vec<Vec<Value>>, OrmError> {
    let overflow = || QueryError::Model("generated key out of range".into());
    let step: i64 = if count > 1 {
        let sql = "SELECT CAST(@@auto_increment_increment AS SIGNED) AS step";
        let result = conn.fetch(sql, Vec::new()).await?;
        let missing = || QueryError::Model("missing increment step".into());
        let row = result.rows.first().ok_or_else(missing)?;
        get(row, "step")?
    } else {
        1
    };
    let first = i64::try_from(first).map_err(|_| overflow())?;
    (0..i64::try_from(count).map_err(|_| overflow())?)
        .map(|i| {
            i.checked_mul(step)
                .and_then(|offset| first.checked_add(offset))
                .map(|id| vec![Value::Int(id)])
                .ok_or_else(|| overflow().into())
        })
        .collect()
}

/// Primary keys of the rows `filter` selects, read with `FOR UPDATE` (the
/// filter of an `UPDATE`).
async fn reads<C: MySqlIo>(
    conn: &mut C,
    info: &TableInfo,
    filter: Option<&Expr>,
) -> Result<Vec<Vec<Value>>, OrmError> {
    let read = compile(
        &locking_read(&info.table, filter, &info.primary_key),
        &MySql,
    )?;
    let rows = conn.fetch(&read.sql, read.params).await?.rows;
    rows.iter()
        .map(|row| {
            info.primary_key
                .iter()
                .map(|key| {
                    let missing = || missing_key(key);
                    row.get(key).cloned().ok_or_else(missing)
                })
                .collect()
        })
        .collect()
}

fn ident(out: &mut String, name: &str) {
    use crate::sql::Dialect as _;
    MySql.write_ident(out, name);
}

/// Read `columns` of the rows with the given primary keys, in key order.
async fn read_by_keys<C: MySqlIo>(
    conn: &mut C,
    info: &TableInfo,
    columns: &[siderite_orm::expr::Ident],
    keys: &[Vec<Value>],
) -> Result<Vec<Row>, OrmError> {
    let mut sql = String::from("SELECT ");
    for (i, column) in columns.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        ident(&mut sql, column);
    }
    sql.push_str(" FROM ");
    ident(&mut sql, &info.table);
    sql.push_str(" WHERE ");
    let width = info.primary_key.len();
    let tuple = |sql: &mut String| {
        sql.push('(');
        for i in 0..width {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
        }
        sql.push(')');
    };
    if width == 1 {
        ident(&mut sql, &info.primary_key[0]);
    } else {
        sql.push('(');
        for (i, key) in info.primary_key.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            ident(&mut sql, key);
        }
        sql.push(')');
    }
    sql.push_str(" IN (");
    for i in 0..keys.len() {
        if i > 0 {
            sql.push_str(", ");
        }
        if width == 1 {
            sql.push('?');
        } else {
            tuple(&mut sql);
        }
    }
    sql.push_str(") ORDER BY ");
    for (i, key) in info.primary_key.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        ident(&mut sql, key);
    }
    let params: Vec<Value> = keys.iter().flatten().cloned().collect();
    let mut rows = conn.fetch(&sql, params).await?.rows;
    // Supplied keys keep their statement order (`ORDER BY` above is the key
    // order, which is insertion order for generated keys).
    let position: HashMap<String, usize> = keys
        .iter()
        .enumerate()
        .map(|(i, key)| (key_text(key.iter()), i))
        .collect();
    let in_row = |row: &Row| -> Option<usize> {
        let read = |key: &String| row.get(key);
        let key: Option<Vec<_>> = info.primary_key.iter().map(read).collect();
        position.get(&key_text(key?.into_iter())).copied()
    };
    if rows.iter().all(|row| in_row(row).is_some()) {
        rows.sort_by_key(|row| in_row(row));
    }
    Ok(rows)
}

/// Comparable text of a key, alike for a bound value and its decoded form
/// (a `Uuid` is bound as text and read back as `Text`).
fn key_text<'a>(key: impl Iterator<Item = &'a Value>) -> String {
    key.map(|value| {
        canonical_text(value).unwrap_or_else(|| match value {
            Value::Text(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => i64::from(*b).to_string(),
            other => format!("{other:?}"),
        })
    })
    .collect::<Vec<_>>()
    .join("\u{1f}")
}

fn is_key(info: &TableInfo, column: &Ident) -> bool {
    info.primary_key.iter().any(|key| column == key.as_str())
}

fn missing_key(key: &str) -> OrmError {
    let message = format!("key column `{key}` missing from read");
    QueryError::Model(message).into()
}
