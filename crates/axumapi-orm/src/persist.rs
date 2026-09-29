//! Building blocks shared by [`ModelOps`](crate::ModelOps) and the write
//! methods of [`QuerySet`](crate::QuerySet): which columns a save writes,
//! `auto_now` stamping, and single-object statements.

use crate::db::Db;
use crate::error::{OrmError, QueryError};
use crate::expr::{Expr, Ident};
use crate::model::{DbDefault, FieldMeta, Model};
use crate::types::{DbType, SqlType};
use crate::value::Value;
use crate::write::{InsertPlan, UpdatePlan, WritePlan};
use chrono::Utc;

/// The primary-key field of `M`.
pub(crate) fn pk_field<M: Model>() -> Result<&'static FieldMeta, QueryError> {
    M::META
        .pk()
        .ok_or_else(|| QueryError::Model(format!("{} has no primary key", M::META.name)))
}

/// `pk = value` predicate for one object.
pub(crate) fn pk_filter<M: Model>(pk: &M::Pk) -> Result<Expr, QueryError> {
    Ok(Expr::col(pk_field::<M>()?.column).eq(Expr::Value(pk.to_value())))
}

/// Every column of `M`, for `RETURNING`.
pub(crate) fn all_columns<M: Model>() -> Vec<Ident> {
    M::META.fields.iter().map(|f| f.column.into()).collect()
}

/// "Now" in the representation of `field`, if it is a stamped time column.
///
/// `auto_now` fields are stamped on every write; `auto_now_add` fields
/// (`default == Now`) only on insert.
fn stamp(field: &FieldMeta, insert: bool) -> Option<Value> {
    let stamped = field.auto_now || (insert && field.default == Some(DbDefault::Now));
    let now = Utc::now();
    match field.sql_type {
        SqlType::Timestamp if stamped => Some(Value::Timestamp(now)),
        SqlType::Date if stamped => Some(Value::Date(now.date_naive())),
        _ => None,
    }
}

/// Columns and values an INSERT of `object` writes.
///
/// An unsaved object omits `auto` columns so the database generates them;
/// a saved one (manual key, or an auto key that was assigned) writes them.
pub(crate) fn insert_values<M: Model>(object: &M) -> Vec<(&'static str, Value)> {
    let omit_auto = object.is_unsaved();
    object
        .to_values()
        .into_iter()
        .filter_map(|(column, value)| {
            let field = M::META.column(column);
            if omit_auto && field.is_some_and(|f| f.auto) {
                return None;
            }
            let value = field.and_then(|f| stamp(f, true)).unwrap_or(value);
            Some((column, value))
        })
        .collect()
}

/// `SET` list of an UPDATE of `object`: everything except the primary key,
/// generated columns and `auto_now_add` columns.
pub(crate) fn update_assignments<M: Model>(object: &M) -> Vec<(Ident, Expr)> {
    object
        .to_values()
        .into_iter()
        .filter_map(|(column, value)| {
            let field = M::META.column(column)?;
            let creation_time = field.default == Some(DbDefault::Now) && !field.auto_now;
            if field.primary_key || field.auto || creation_time {
                return None;
            }
            let value = stamp(field, false).unwrap_or(value);
            Some((column.into(), Expr::Value(value)))
        })
        .collect()
}

/// INSERT `object` (`RETURNING *`) and replace it with the stored row.
pub(crate) async fn insert_one<M: Model>(db: &Db, object: &mut M) -> Result<(), OrmError> {
    let values = insert_values(object);
    let plan = WritePlan::Insert(InsertPlan {
        table: M::META.table.into(),
        columns: values.iter().map(|(c, _)| (*c).into()).collect(),
        rows: vec![values.into_iter().map(|(_, v)| v).collect()],
        returning: all_columns::<M>(),
    });
    replace_from_returning(db, &plan, object).await?;
    Ok(())
}

/// UPDATE `object` by primary key (`RETURNING *`); `false` if no row matched.
pub(crate) async fn update_one<M: Model>(db: &Db, object: &mut M) -> Result<bool, OrmError> {
    let assignments = update_assignments(object);
    if assignments.is_empty() {
        // A key-only model has nothing to SET; saving is an existence check.
        let filter = pk_filter::<M>(&object.pk())?;
        return Ok(M::objects(db).filter(filter).first().await?.is_some());
    }
    let plan = WritePlan::Update(UpdatePlan {
        table: M::META.table.into(),
        assignments,
        filter: Some(pk_filter::<M>(&object.pk())?),
        returning: all_columns::<M>(),
    });
    replace_from_returning(db, &plan, object).await
}

/// Run `plan`, and if it returned a row, decode it into `object`.
async fn replace_from_returning<M: Model>(
    db: &Db,
    plan: &WritePlan,
    object: &mut M,
) -> Result<bool, OrmError> {
    let result = db.execute(plan).await?;
    match result.returning.first() {
        Some(row) => {
            *object = M::from_row(row, "")?;
            Ok(true)
        }
        None => Ok(false),
    }
}
