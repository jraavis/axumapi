//! Dynamic projections: `project`, `rows`, `values`, `values_list`,
//! `aggregate`, and subquery builders.

use super::QuerySet;
use super::fetch::{is_flat, unordered};
use crate::backend::Row;
use crate::decode::FromValues;
use crate::error::{OrmError, QueryError};
use crate::expr::Expr;
use crate::model::Model;
use crate::plan::{QueryPlan, SelectExpr};

impl<M: Model> QuerySet<M> {
    /// Select only `fields` (Django `values()` as a builder): the queryset
    /// then yields dynamic rows through [`rows`](Self::rows) instead of
    /// models. Fields may be `Field` / `Joined` handles (`Book::title.select()`),
    /// column names, `("name", expr)` pairs, or annotation names.
    ///
    /// Call `project` first and `annotate` after it to group: aggregates
    /// group by the projected columns. The model's default ordering is
    /// dropped.
    #[must_use]
    pub fn project<S: Into<SelectExpr>>(mut self, fields: impl IntoIterator<Item = S>) -> Self {
        let projection: Vec<SelectExpr> = fields
            .into_iter()
            .map(|field| {
                let mut field: SelectExpr = field.into();
                field.expr = self.resolve(field.expr);
                field
            })
            .collect();
        if self.default_ordering {
            self.default_ordering = false;
            self.plan.ordering.clear();
        }
        self.with_plan(|mut plan| {
            plan.projection = projection;
            plan
        })
    }

    /// Run the query and return its rows undecoded.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn rows(self) -> Result<Vec<Row>, OrmError> {
        self.ready()?;
        if self.empty {
            return Ok(Vec::new());
        }
        Ok(self.db.fetch(&self.plan).await?.rows)
    }

    /// Rows holding only `fields` (Django `values(..)`).
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn values<S: Into<SelectExpr>>(
        self,
        fields: impl IntoIterator<Item = S>,
    ) -> Result<Vec<Row>, OrmError> {
        self.project(fields).rows().await
    }

    /// `fields` decoded by position into `T`, a scalar or tuple (Django
    /// `values_list(..)`): `values_list::<(String, i64)>([..])`.
    ///
    /// # Errors
    /// [`QueryError::Decode`] if a column does not fit `T`; capability or
    /// backend errors.
    pub async fn values_list<T: FromValues, S: Into<SelectExpr>>(
        self,
        fields: impl IntoIterator<Item = S>,
    ) -> Result<Vec<T>, OrmError> {
        let rows = self.values(fields).await?;
        Ok(rows.iter().map(T::from_values).collect::<Result<_, _>>()?)
    }

    /// One row of aggregates over the whole queryset (Django `aggregate`):
    /// `aggregate([("total", Sum::of(Book::price))])`.
    ///
    /// Ordering is ignored. A queryset with a limit, distinct or grouping is
    /// aggregated over its result rows.
    ///
    /// # Errors
    /// Capability (e.g. `StdDev` on SQLite) or backend errors.
    pub async fn aggregate<S: Into<SelectExpr>>(
        self,
        aggregates: impl IntoIterator<Item = S>,
    ) -> Result<Row, OrmError> {
        self.ready()?;
        let projection: Vec<SelectExpr> = aggregates
            .into_iter()
            .map(|a| {
                let mut a: SelectExpr = a.into();
                a.expr = self.resolve(a.expr);
                a
            })
            .collect();
        let mut plan = if is_flat(&self.plan) {
            unordered(&self.plan)
        } else {
            QueryPlan::from_subquery(unordered(&self.plan), "aggregated")
        };
        if self.empty {
            plan.filter = Some(Expr::Or(Vec::new()));
        }
        plan.projection = projection;
        plan.grouping.clear();
        plan.having = None;
        let result = self.db.fetch(&plan.resolve_relations()).await?;
        result.rows.into_iter().next().ok_or_else(|| {
            QueryError::Decode {
                column: "aggregate".into(),
                reason: "no row returned".into(),
            }
            .into()
        })
    }

    /// This queryset as a subquery selecting only `field`, for
    /// `Field::in_subquery` and [`Expr::subquery`].
    #[must_use]
    pub fn subquery(self, field: impl Into<SelectExpr>) -> QueryPlan {
        let qs = self.project([field]);
        let mut plan = unordered(&qs.plan);
        plan.origin = Some(qs.db.origin());
        plan
    }

    /// `EXISTS (this queryset)`, for correlated filters:
    /// `Author::objects(&db).filter(Book::objects(&db).filter(..).exists_expr())`.
    #[must_use]
    pub fn exists_expr(self) -> Expr {
        let mut plan = unordered(&self.plan);
        plan.projection = vec![SelectExpr::new(Expr::val(1), None)];
        plan.origin = Some(self.db.origin());
        Expr::exists(plan)
    }
}
