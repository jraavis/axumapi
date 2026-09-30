//! Terminal methods that write: `update`, `delete`, `create`, the
//! `*_or_create` family and the bulk operations.
//!
//! Multi-statement operations run in [`Db::transaction`] (a savepoint when
//! the queryset's [`Db`] is already a transaction), so they are all-or-nothing.

use super::QuerySet;
use super::fetch::{is_flat, unordered};
use crate::Feature;
use crate::capabilities::RowLocking;
use crate::db::Db;
use crate::error::{BackendError, OrmError, QueryError};
use crate::expr::{Expr, Ident};
use crate::model::Model;
use crate::ops::ModelOps;
use crate::persist::{all_columns, insert_one, insert_values, pk_field, restore_order};
use crate::plan::SelectExpr;
use crate::types::DbType;
use crate::value::Value;
use crate::write::{DeletePlan, InsertPlan, UpdatePlan, WritePlan};

impl<M: Model> QuerySet<M> {
    /// `WHERE` clause selecting exactly this queryset's rows.
    ///
    /// A plain filter is used directly; anything else (joins, limit, offset,
    /// distinct) becomes `pk IN (SELECT pk ...)`, since neither `UPDATE` nor
    /// `DELETE` can carry those clauses.
    fn write_filter(&self) -> Result<Option<Expr>, QueryError> {
        let plan = &self.plan;
        if !plan.grouping.is_empty() || plan.having.is_some() || !plan.compound.is_empty() {
            return Err(QueryError::InvalidPlan(
                "cannot update or delete a grouped or combined queryset".into(),
            ));
        }
        if is_flat(plan) && plan.joins.is_empty() {
            return Ok(plan.filter.clone());
        }
        let pk = pk_field::<M>()?.column;
        let mut keys = unordered(plan);
        keys.projection = vec![SelectExpr::new(Expr::col(pk), None)];
        Ok(Some(Expr::col(pk).in_subquery(keys)))
    }

    /// Set `assignments` (`User::age.set(5)`, `Post::likes.set_expr(Post::likes + 1)`)
    /// on every matching row; returns the number of rows changed.
    ///
    /// Runs one `UPDATE`. `auto_now` columns are not touched: name them in
    /// `assignments` if they should change.
    ///
    /// # Errors
    /// Capability or backend errors ([`BackendError::Constraint`] for
    /// violated constraints).
    pub async fn update(
        self,
        assignments: impl IntoIterator<Item = (Ident, Expr)>,
    ) -> Result<u64, OrmError> {
        self.ready()?;
        if self.empty {
            return Ok(0);
        }
        let plan = WritePlan::Update(UpdatePlan {
            table: M::META.table.into(),
            assignments: assignments.into_iter().collect(),
            filter: self.write_filter()?,
            returning: Vec::new(),
        });
        Ok(self.db.execute(&plan).await?.rows_affected)
    }

    /// Delete every matching row; returns the number of rows removed.
    ///
    /// Dependent rows follow the foreign keys' database actions
    /// (`ON DELETE CASCADE` / `SET NULL` / `RESTRICT`). A `RESTRICT`
    /// (`OnDelete::Protect`) violation is reported as
    /// [`BackendError::Constraint`]; nothing is deleted in that case.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn delete(self) -> Result<u64, OrmError> {
        self.ready()?;
        if self.empty {
            return Ok(0);
        }
        let plan = WritePlan::Delete(DeletePlan {
            table: M::META.table.into(),
            filter: self.write_filter()?,
            returning: Vec::new(),
        });
        Ok(self.db.execute(&plan).await?.rows_affected)
    }

    /// Insert `object` and return it as stored (generated key, database
    /// defaults and stamped timestamps filled in).
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn create(self, mut object: M) -> Result<M, OrmError> {
        insert_one(&self.db, &mut object).await?;
        Ok(object)
    }

    /// The single row matching `predicate`, or a new one built by `default`
    /// and inserted; the flag says whether it was created.
    ///
    /// If a concurrent writer inserts the row first (unique violation), the
    /// existing row is returned instead. When that re-fetch fails too, the
    /// [`BackendError::Constraint`] is returned with the re-fetch error
    /// appended to its message.
    ///
    /// # Errors
    /// [`QueryError::MultipleObjectsReturned`], or capability / backend errors.
    pub async fn get_or_create(
        self,
        predicate: Expr,
        default: impl FnOnce() -> M + Send,
    ) -> Result<(M, bool), OrmError> {
        self.ready()?;
        self.plan.check(&self.db.capabilities())?;
        let lookup = self.filter(predicate);
        match lookup.clone().one().await {
            Ok(found) => return Ok((found, false)),
            Err(OrmError::Query(QueryError::DoesNotExist)) => {}
            Err(other) => return Err(other),
        }
        let created = lookup
            .db
            .transaction(|tx| async move {
                let mut object = default();
                insert_one(&tx, &mut object).await?;
                Ok::<M, OrmError>(object)
            })
            .await;
        match created {
            Ok(object) => Ok((object, true)),
            Err(OrmError::Backend(BackendError::Constraint(message))) => match lookup.one().await {
                Ok(found) => Ok((found, false)),
                Err(refetch) => Err(conflict_with_context(message, &refetch)),
            },
            Err(other) => Err(other),
        }
    }

    /// Apply `update` to the single row matching `predicate` and save it, or
    /// insert the object built by `create`; the flag says whether it was
    /// created. Runs in a transaction.
    ///
    /// On backends with row locking the lookup is `SELECT ... FOR UPDATE`, so
    /// concurrent updates of an existing row serialize instead of losing
    /// writes. Two callers can still both miss and insert; guard against that
    /// with a unique constraint.
    ///
    /// # Errors
    /// [`QueryError::MultipleObjectsReturned`], or capability / backend errors.
    pub async fn update_or_create(
        self,
        predicate: Expr,
        create: impl FnOnce() -> M + Send,
        update: impl FnOnce(&mut M) + Send,
    ) -> Result<(M, bool), OrmError> {
        self.ready()?;
        let lock = self.db.capabilities().row_locking > RowLocking::None;
        let mut lookup = self.filter(predicate);
        if lock {
            lookup = lookup.select_for_update();
        }
        lookup
            .db
            .clone()
            .transaction(|tx| async move {
                match lookup.using(&tx).one().await {
                    Ok(mut found) => {
                        update(&mut found);
                        found.save(&tx).await?;
                        Ok((found, false))
                    }
                    Err(OrmError::Query(QueryError::DoesNotExist)) => {
                        let mut object = create();
                        insert_one(&tx, &mut object).await?;
                        Ok((object, true))
                    }
                    Err(other) => Err(other),
                }
            })
            .await
    }

    /// Insert many objects with multi-row `INSERT`s, chunked to the backend's
    /// parameter limit, in one transaction. Returns the stored rows in input
    /// order.
    ///
    /// Requires `RETURNING`. Rows are matched back to the inputs by key, not
    /// by the order the database returns them.
    ///
    /// # Errors
    /// Capability or backend errors; nothing is inserted on failure.
    pub async fn bulk_create(self, objects: Vec<M>) -> Result<Vec<M>, OrmError> {
        self.ready()?;
        if objects.is_empty() {
            return Ok(Vec::new());
        }
        let caps = self.db.capabilities();
        caps.require(Feature::Returning)?;
        let batches = insert_batches(&objects, caps.max_params);
        self.db
            .transaction(|tx| async move {
                let mut stored = Vec::with_capacity(objects.len());
                for batch in batches {
                    let rows = &objects[batch.start..batch.end];
                    stored.extend(insert_batch(&tx, rows).await?);
                }
                Ok::<_, OrmError>(stored)
            })
            .await
    }

    /// Write `columns` of every object in `objects` back to its row, using
    /// one `UPDATE .. SET col = CASE pk WHEN .. END` per chunk. Returns the
    /// number of rows changed.
    ///
    /// Values are written verbatim (`auto_now` is not refreshed). Objects
    /// whose row is missing are skipped.
    ///
    /// # Errors
    /// [`QueryError::InvalidPlan`] for an unknown or primary-key column;
    /// capability or backend errors.
    pub async fn bulk_update(
        self,
        objects: &[M],
        columns: &[&'static str],
    ) -> Result<u64, OrmError> {
        self.ready()?;
        let pk = pk_field::<M>()?.column;
        for column in columns {
            if M::META.column(column).is_none_or(|f| f.primary_key) {
                return Err(QueryError::InvalidPlan(format!(
                    "`{column}` is not an updatable column of {}",
                    M::META.name
                ))
                .into());
            }
        }
        if objects.is_empty() || columns.is_empty() {
            return Ok(0);
        }
        let per_chunk = (self.db.capabilities().max_params / (2 * columns.len() + 1)).max(1);
        self.db
            .transaction(|tx| async move {
                let mut changed = 0;
                for chunk in objects.chunks(per_chunk) {
                    let plan = bulk_update_plan::<M>(pk, chunk, columns);
                    changed += tx.execute(&plan).await?.rows_affected;
                }
                Ok::<_, OrmError>(changed)
            })
            .await
    }
}

/// The constraint error of a failed insert, with the error of the follow-up
/// lookup appended so neither is lost.
fn conflict_with_context(message: String, refetch: &OrmError) -> OrmError {
    BackendError::Constraint(format!(
        "{message} (re-fetch after the conflict failed: {refetch})"
    ))
    .into()
}

/// A run of consecutive objects inserted with one statement.
struct Batch {
    start: usize,
    end: usize,
}

/// Split `objects` into runs that share a column list (unsaved and saved
/// objects differ in whether generated columns are written) and fit the
/// parameter limit.
fn insert_batches<M: Model>(objects: &[M], max_params: usize) -> Vec<Batch> {
    let mut batches: Vec<Batch> = Vec::new();
    let mut previous: Option<Vec<&'static str>> = None;
    for (i, object) in objects.iter().enumerate() {
        let columns: Vec<&'static str> = insert_values(object).iter().map(|(c, _)| *c).collect();
        let rows_per_statement = if columns.is_empty() {
            1
        } else {
            (max_params / columns.len()).max(1)
        };
        let extends = previous.as_ref() == Some(&columns)
            && batches
                .last()
                .is_some_and(|b| b.end - b.start < rows_per_statement);
        match batches.last_mut() {
            Some(batch) if extends => batch.end = i + 1,
            _ => batches.push(Batch {
                start: i,
                end: i + 1,
            }),
        }
        previous = Some(columns);
    }
    batches
}

async fn insert_batch<M: Model>(db: &Db, objects: &[M]) -> Result<Vec<M>, OrmError> {
    let rows: Vec<Vec<(&'static str, Value)>> = objects.iter().map(insert_values).collect();
    let columns: Vec<Ident> = rows
        .first()
        .map(|r| r.iter().map(|(c, _)| (*c).into()).collect())
        .unwrap_or_default();
    let plan = WritePlan::Insert(InsertPlan {
        table: M::META.table.into(),
        columns,
        rows: rows
            .into_iter()
            .map(|r| r.into_iter().map(|(_, v)| v).collect())
            .collect(),
        returning: all_columns::<M>(),
    });
    let result = db.execute(&plan).await?;
    if result.returning.len() != objects.len() {
        return Err(QueryError::Model(format!(
            "inserted {} rows but the database returned {}",
            objects.len(),
            result.returning.len()
        ))
        .into());
    }
    let stored = result
        .returning
        .iter()
        .map(|row| M::from_row(row, ""))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(restore_order(objects, stored))
}

fn bulk_update_plan<M: Model>(
    pk: &'static str,
    chunk: &[M],
    columns: &[&'static str],
) -> WritePlan {
    let assignments = columns
        .iter()
        .map(|column| {
            let branches = chunk
                .iter()
                .filter_map(|object| {
                    let value = object.to_values().into_iter().find(|(c, _)| c == column)?.1;
                    let matches_row = Expr::col(pk).eq(Expr::Value(object.pk().to_value()));
                    Some((matches_row, Expr::Value(value)))
                })
                .collect();
            let case = Expr::Case {
                branches,
                otherwise: Some(Box::new(Expr::col(*column))),
            };
            ((*column).into(), case)
        })
        .collect();
    WritePlan::Update(UpdatePlan {
        table: M::META.table.into(),
        assignments,
        filter: Some(Expr::col(pk).is_in(chunk.iter().map(|o| o.pk().to_value()))),
        returning: Vec::new(),
    })
}
