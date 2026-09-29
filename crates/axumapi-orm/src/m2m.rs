//! Many-to-many operations on [`ManyToManyManager`]: the join table is
//! read and written directly, so relations with an auto-created table need
//! no through model.
//!
//! Mutations run in a transaction (a savepoint inside an open one). An
//! explicit through model with further required columns cannot be filled by
//! `add`; insert its rows directly instead.

use crate::db::Db;
use crate::error::OrmError;
use crate::expr::Expr;
use crate::model::Model;
use crate::persist::value_key;
use crate::plan::QueryPlan;
use crate::queryset::QuerySet;
use crate::relations::ManyToManyManager;
use crate::types::DbType;
use crate::value::Value;
use crate::write::{DeletePlan, InsertPlan, WritePlan};
use std::collections::{HashMap, HashSet};

impl<S: Model, T: Model> ManyToManyManager<S, T> {
    /// `through.source = <this object>` predicate.
    fn owner(&self) -> Expr {
        Expr::col(self.meta().source_column).eq(Expr::Value(self.source_pk().clone()))
    }

    /// Lazy queryset over the related objects (`WHERE pk IN (SELECT target
    /// FROM through WHERE source = ..)`); filter or order it further.
    pub fn queryset(&self) -> QuerySet<T> {
        let meta = self.meta();
        let targets = QueryPlan::from_table(meta.through_table)
            .select(Expr::col(meta.target_column), None)
            .filter(self.owner());
        let pk = crate::expr::pk_column::<T>();
        T::objects(self.db()).filter(Expr::col(pk).in_subquery(targets))
    }

    /// Every related object.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn all(&self) -> Result<Vec<T>, OrmError> {
        self.queryset().all().await
    }

    /// Number of related objects.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn count(&self) -> Result<u64, OrmError> {
        self.queryset().count().await
    }

    /// Whether `object` is related.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn contains(&self, object: &T) -> Result<bool, OrmError> {
        self.queryset().contains(object).await
    }

    /// Relate `objects` (already related ones are skipped).
    ///
    /// # Errors
    /// Capability or backend errors ([`BackendError::Constraint`](crate::BackendError::Constraint)
    /// when a target does not exist).
    pub async fn add<'a>(&self, objects: impl IntoIterator<Item = &'a T>) -> Result<(), OrmError> {
        self.add_pks(objects.into_iter().map(Model::pk)).await
    }

    /// [`add`](Self::add) by primary key.
    ///
    /// # Errors
    /// As [`add`](Self::add).
    pub async fn add_pks(&self, keys: impl IntoIterator<Item = T::Pk>) -> Result<(), OrmError> {
        let keys = distinct(keys);
        self.db()
            .transaction(|tx| async move {
                let present = self.linked(&tx, Some(&keys)).await?;
                let missing: Vec<Value> = keys
                    .into_iter()
                    .filter(|k| !present.contains_key(&value_key(k)))
                    .collect();
                self.link(&tx, missing).await
            })
            .await
    }

    /// Unrelate `objects`; returns the number of links removed.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn remove<'a>(
        &self,
        objects: impl IntoIterator<Item = &'a T>,
    ) -> Result<u64, OrmError> {
        self.remove_pks(objects.into_iter().map(Model::pk)).await
    }

    /// [`remove`](Self::remove) by primary key.
    ///
    /// # Errors
    /// As [`remove`](Self::remove).
    pub async fn remove_pks(&self, keys: impl IntoIterator<Item = T::Pk>) -> Result<u64, OrmError> {
        let keys = distinct(keys);
        self.db()
            .transaction(|tx| async move { self.unlink(&tx, Some(keys)).await })
            .await
    }

    /// Unrelate everything; returns the number of links removed.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn clear(&self) -> Result<u64, OrmError> {
        self.unlink(self.db(), None).await
    }

    /// Make `objects` exactly the related set: add the missing, remove the rest.
    ///
    /// # Errors
    /// Capability or backend errors; nothing changes on failure.
    pub async fn set<'a>(&self, objects: impl IntoIterator<Item = &'a T>) -> Result<(), OrmError> {
        self.set_pks(objects.into_iter().map(Model::pk)).await
    }

    /// [`set`](Self::set) by primary key.
    ///
    /// # Errors
    /// As [`set`](Self::set).
    pub async fn set_pks(&self, keys: impl IntoIterator<Item = T::Pk>) -> Result<(), OrmError> {
        let wanted = distinct(keys);
        self.db()
            .transaction(|tx| async move {
                let current = self.linked(&tx, None).await?;
                let wanted_keys: HashSet<String> = wanted.iter().map(value_key).collect();
                let stale: Vec<Value> = current
                    .iter()
                    .filter(|(key, _)| !wanted_keys.contains(*key))
                    .map(|(_, value)| value.clone())
                    .collect();
                let missing: Vec<Value> = wanted
                    .into_iter()
                    .filter(|k| !current.contains_key(&value_key(k)))
                    .collect();
                if !stale.is_empty() {
                    self.unlink(&tx, Some(stale)).await?;
                }
                self.link(&tx, missing).await
            })
            .await
    }

    /// Keys (as [`value_key`]s) of the targets already linked; restricted to
    /// `among` when given.
    async fn linked(
        &self,
        db: &Db,
        among: Option<&[Value]>,
    ) -> Result<HashMap<String, Value>, OrmError> {
        let meta = self.meta();
        let mut filter = self.owner();
        if let Some(among) = among {
            filter = filter.and(Expr::col(meta.target_column).is_in(among.iter().cloned()));
        }
        let plan = QueryPlan::from_table(meta.through_table)
            .select(Expr::col(meta.target_column), None)
            .filter(filter);
        let mut linked = HashMap::new();
        for row in db.fetch(&plan).await?.rows {
            // Normalise through the key type: storage forms differ by backend.
            let key = row.decode_at::<T::Pk>(0)?.to_value();
            linked.insert(value_key(&key), key);
        }
        Ok(linked)
    }

    async fn link(&self, db: &Db, keys: Vec<Value>) -> Result<(), OrmError> {
        let meta = self.meta();
        let per_statement = (db.capabilities().max_params / 2).max(1);
        for keys in keys.chunks(per_statement) {
            let plan = WritePlan::Insert(InsertPlan {
                table: meta.through_table.into(),
                columns: vec![meta.source_column.into(), meta.target_column.into()],
                rows: keys
                    .iter()
                    .map(|k| vec![self.source_pk().clone(), k.clone()])
                    .collect(),
                returning: Vec::new(),
            });
            db.execute(&plan).await?;
        }
        Ok(())
    }

    async fn unlink(&self, db: &Db, keys: Option<Vec<Value>>) -> Result<u64, OrmError> {
        let meta = self.meta();
        let per_statement = db.capabilities().max_params.saturating_sub(1).max(1);
        let batches: Vec<Option<&[Value]>> = match &keys {
            Some(keys) => keys.chunks(per_statement).map(Some).collect(),
            None => vec![None],
        };
        let mut removed = 0;
        for batch in batches {
            let mut filter = self.owner();
            if let Some(batch) = batch {
                filter = filter.and(Expr::col(meta.target_column).is_in(batch.iter().cloned()));
            }
            let plan = WritePlan::Delete(DeletePlan {
                table: meta.through_table.into(),
                filter: Some(filter),
                returning: Vec::new(),
            });
            removed += db.execute(&plan).await?.rows_affected;
        }
        Ok(removed)
    }
}

/// Key values without duplicates, in first-seen order.
fn distinct<K: DbType>(keys: impl IntoIterator<Item = K>) -> Vec<Value> {
    let mut seen = HashSet::new();
    keys.into_iter()
        .map(|k| k.to_value())
        .filter(|v| seen.insert(value_key(v)))
        .collect()
}
