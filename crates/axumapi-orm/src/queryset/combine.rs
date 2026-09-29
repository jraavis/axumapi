//! Set operations: `union`, `union_all`, `intersection`, `difference`.

use super::QuerySet;
use crate::error::QueryError;
use crate::expr::Expr;
use crate::model::Model;
use crate::plan::{Compound, QueryPlan, SetOp};

impl<M: Model> QuerySet<M> {
    /// Rows in either queryset, duplicates removed (`UNION`).
    ///
    /// Both querysets must project the same columns (same names, same
    /// order). Order and limit the *combined* result by calling `order_by` /
    /// `limit` on the returned queryset; ordering inside `other` is dropped,
    /// and `order_by` terms must name output columns.
    ///
    /// # Errors
    /// [`QueryError::InvalidPlan`] if the projections differ.
    pub fn union(self, other: QuerySet<M>) -> Result<Self, QueryError> {
        self.combine(SetOp::Union, other)
    }

    /// Rows in either queryset, keeping duplicates (`UNION ALL`).
    ///
    /// # Errors
    /// As [`union`](Self::union).
    pub fn union_all(self, other: QuerySet<M>) -> Result<Self, QueryError> {
        self.combine(SetOp::UnionAll, other)
    }

    /// Rows in both querysets (`INTERSECT`).
    ///
    /// # Errors
    /// As [`union`](Self::union).
    pub fn intersection(self, other: QuerySet<M>) -> Result<Self, QueryError> {
        self.combine(SetOp::Intersect, other)
    }

    /// Rows of this queryset that are not in `other` (`EXCEPT`).
    ///
    /// # Errors
    /// As [`union`](Self::union).
    pub fn difference(self, other: QuerySet<M>) -> Result<Self, QueryError> {
        self.combine(SetOp::Except, other)
    }

    fn combine(self, op: SetOp, other: QuerySet<M>) -> Result<Self, QueryError> {
        self.ready()?;
        other.ready()?;
        let (mut base, mut member) = (self.materialize_empty(), other.materialize_empty());
        if shape::<M>(&base.plan) != shape::<M>(&member.plan) {
            return Err(QueryError::InvalidPlan(
                "set operation members must project the same columns".into(),
            ));
        }
        member.plan.ordering.clear();
        base.plan.compound.push(Compound {
            op,
            plan: member.plan,
        });
        Ok(base)
    }

    /// Replace the `none()` flag by an always-false filter, so the queryset
    /// can take part in a set operation.
    fn materialize_empty(mut self) -> Self {
        if self.empty {
            self.empty = false;
            self.plan.filter = Some(Expr::Or(Vec::new()));
        }
        self
    }
}

/// Output column names of `plan`.
fn shape<M: Model>(plan: &QueryPlan) -> Vec<String> {
    if plan.projection.is_empty() {
        return M::META.fields.iter().map(|f| f.column.to_owned()).collect();
    }
    plan.projection
        .iter()
        .map(|s| match (&s.alias, &s.expr) {
            (Some(alias), _) => alias.to_string(),
            (None, Expr::Column(column)) => column.name.to_string(),
            (None, _) => String::new(),
        })
        .collect()
}
