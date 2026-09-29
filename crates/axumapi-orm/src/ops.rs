//! Instance operations: [`ModelOps`] gives every [`Model`] `save`, `delete`
//! and `refresh` (Django `Model.save()` / `delete()` / `refresh_from_db()`).
//!
//! # Semantics
//!
//! * **One statement, no hooks.** Each operation is a single SQL statement
//!   (`save` may be two, see below). There are no pre/post-save signals; put
//!   such logic in the calling code, and use [`Db::on_commit`] for work that
//!   must run only after the surrounding transaction commits.
//! * **Atomicity.** A single statement is atomic. `save` on a manual primary
//!   key runs `UPDATE` and, if no row matched, `INSERT`; outside a transaction
//!   another writer can slip in between the two, so wrap concurrent upserts in
//!   [`Db::transaction`] (or use `QuerySet::get_or_create`).
//! * **Timestamps.** `auto_now_add` columns are stamped on insert and
//!   `auto_now` columns on every save, with the current UTC time.
//! * **Stored state wins.** `save` reads the stored row back (`RETURNING *`)
//!   and replaces `*self`, so database defaults, generated keys and stamped
//!   timestamps are visible immediately. Cached related objects of
//!   [`ForeignKey`](crate::ForeignKey) fields are dropped by that replacement.

use crate::db::Db;
use crate::error::{OrmError, QueryError};
use crate::model::Model;
use crate::persist::{insert_one, pk_filter, update_one};
use crate::write::{DeletePlan, WritePlan};
use std::future::Future;

/// Persistence operations available on every [`Model`].
pub trait ModelOps: Model {
    /// Insert or update this object.
    ///
    /// An [`is_unsaved`](Model::is_unsaved) object is inserted (generated
    /// columns omitted) and receives its key. Otherwise the row is updated by
    /// primary key, falling back to an insert when no row exists.
    ///
    /// # Errors
    /// Capability or backend errors; [`BackendError::Constraint`](crate::BackendError::Constraint)
    /// for unique / foreign-key violations.
    fn save(&mut self, db: &Db) -> impl Future<Output = Result<(), OrmError>> + Send;

    /// Delete this object's row, returning the number of rows removed
    /// (`0` if it no longer exists). Dependent rows follow the database's
    /// foreign-key actions.
    ///
    /// # Errors
    /// Capability or backend errors (e.g. a `RESTRICT` foreign key).
    fn delete(&self, db: &Db) -> impl Future<Output = Result<u64, OrmError>> + Send;

    /// Reload every column from the database, replacing `*self`.
    ///
    /// # Errors
    /// [`QueryError::DoesNotExist`] if the row is gone.
    fn refresh(&mut self, db: &Db) -> impl Future<Output = Result<(), OrmError>> + Send;
}

impl<M: Model> ModelOps for M {
    async fn save(&mut self, db: &Db) -> Result<(), OrmError> {
        if self.is_unsaved() || !update_one(db, self).await? {
            insert_one(db, self).await?;
        }
        Ok(())
    }

    async fn delete(&self, db: &Db) -> Result<u64, OrmError> {
        let plan = WritePlan::Delete(DeletePlan {
            table: M::META.table.into(),
            filter: Some(pk_filter::<M>(&self.pk())?),
            returning: Vec::new(),
        });
        Ok(db.execute(&plan).await?.rows_affected)
    }

    async fn refresh(&mut self, db: &Db) -> Result<(), OrmError> {
        let fresh = M::objects(db)
            .filter(pk_filter::<M>(&self.pk())?)
            .first()
            .await?
            .ok_or(QueryError::DoesNotExist)?;
        *self = fresh;
        Ok(())
    }
}
