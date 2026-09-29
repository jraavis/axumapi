//! [`QuerySet`]: a lazy, immutable query over one model.
//!
//! Builder methods consume and return the queryset and never perform I/O;
//! terminal methods (`all`, `first`, `get`, `count`, `exists`, ...) run the
//! compiled [`QueryPlan`] on the queryset's [`Db`]. Clone a queryset to branch
//! from a common base.

use crate::db::Db;
use crate::error::{OrmError, QueryError};
use crate::expr::Expr;
use crate::model::Model;
use crate::plan::{OrderExpr, QueryPlan};
use std::marker::PhantomData;

/// Lazy query over model `M`.
pub struct QuerySet<M: Model> {
    db: Db,
    plan: QueryPlan,
    _model: PhantomData<fn() -> M>,
}

impl<M: Model> Clone for QuerySet<M> {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            plan: self.plan.clone(),
            _model: PhantomData,
        }
    }
}

impl<M: Model> std::fmt::Debug for QuerySet<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuerySet")
            .field("model", &M::META.name)
            .field("plan", &self.plan)
            .finish_non_exhaustive()
    }
}

impl<M: Model> QuerySet<M> {
    /// Every row of `M`'s table, in the model's default ordering.
    pub fn new(db: &Db) -> Self {
        let mut plan = QueryPlan::from_table(M::META.table);
        for (column, direction) in M::META.ordering {
            plan = plan.order_by(Expr::col(*column), *direction);
        }
        Self {
            db: db.clone(),
            plan,
            _model: PhantomData,
        }
    }

    /// The database handle this queryset runs on.
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The plan built so far.
    pub fn plan(&self) -> &QueryPlan {
        &self.plan
    }

    /// Replace the plan (for extension code building on this queryset).
    #[must_use]
    pub fn with_plan(mut self, f: impl FnOnce(QueryPlan) -> QueryPlan) -> Self {
        self.plan = f(self.plan);
        self
    }

    /// Keep rows matching `predicate` (ANDed with earlier filters).
    #[must_use]
    pub fn filter(self, predicate: Expr) -> Self {
        self.with_plan(|p| p.filter(predicate))
    }

    /// Drop rows matching `predicate`.
    #[must_use]
    pub fn exclude(self, predicate: Expr) -> Self {
        self.with_plan(|p| p.exclude(predicate))
    }

    /// Replace the ordering (`order_by([User::name.asc(), User::id.desc()])`).
    #[must_use]
    pub fn order_by(self, terms: impl IntoIterator<Item = OrderExpr>) -> Self {
        self.with_plan(|mut p| {
            p.ordering = terms.into_iter().collect();
            p
        })
    }

    /// Return at most `n` rows.
    #[must_use]
    pub fn limit(self, n: u64) -> Self {
        self.with_plan(|p| p.limit(n))
    }

    /// Skip the first `n` rows.
    #[must_use]
    pub fn offset(self, n: u64) -> Self {
        self.with_plan(|p| p.offset(n))
    }

    /// Run the query and decode every row.
    ///
    /// # Errors
    /// Capability, backend or decode errors.
    pub async fn all(self) -> Result<Vec<M>, OrmError> {
        let result = self.db.fetch(&self.plan).await?;
        result
            .rows
            .iter()
            .map(|row| M::from_row(row, "").map_err(OrmError::from))
            .collect()
    }

    /// The first row, if any.
    ///
    /// # Errors
    /// As [`all`](Self::all).
    pub async fn first(self) -> Result<Option<M>, OrmError> {
        Ok(self.limit(1).all().await?.into_iter().next())
    }

    /// Exactly one row matching `predicate`.
    ///
    /// # Errors
    /// [`QueryError::DoesNotExist`], [`QueryError::MultipleObjectsReturned`],
    /// or as [`all`](Self::all).
    pub async fn get(self, predicate: Expr) -> Result<M, OrmError> {
        let mut rows = self.filter(predicate).limit(2).all().await?;
        match rows.len() {
            0 => Err(QueryError::DoesNotExist.into()),
            1 => rows.pop().ok_or_else(|| QueryError::DoesNotExist.into()),
            n => Err(QueryError::MultipleObjectsReturned(n as u64).into()),
        }
    }
}

impl Db {
    /// Queryset over `M` on this handle (same as `M::objects(&db)`).
    pub fn objects<M: Model>(&self) -> QuerySet<M> {
        QuerySet::new(self)
    }
}
