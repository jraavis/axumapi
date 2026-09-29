//! [`QueryPlan`]: the backend-neutral intermediate representation.
//!
//! Every high-level query API compiles into a `QueryPlan`; backends compile a
//! plan into SQL, a MongoDB pipeline, etc. Relational-only parts (joins,
//! locking, `DISTINCT ON`) are reported through
//! [`QueryPlan::required_features`] so they can be checked against
//! [`BackendCapabilities`] before execution.

use crate::capabilities::{BackendCapabilities, Feature};
use crate::error::BackendCapabilityError;
use crate::expr::{Expr, Ident, Lookup};

/// What the query reads from.
#[derive(Debug, Clone, PartialEq)]
pub struct QuerySource {
    /// Table or collection name.
    pub name: Ident,
    /// Optional alias.
    pub alias: Option<Ident>,
}

impl QuerySource {
    /// Unaliased source.
    pub fn table(name: impl Into<Ident>) -> Self {
        Self {
            name: name.into(),
            alias: None,
        }
    }
}

/// A projected expression.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectExpr {
    /// Expression.
    pub expr: Expr,
    /// Output name.
    pub alias: Option<Ident>,
}

/// Join type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    /// `INNER JOIN`.
    Inner,
    /// `LEFT OUTER JOIN`.
    Left,
}

/// A join to another source (relational capability).
#[derive(Debug, Clone, PartialEq)]
pub struct JoinExpr {
    /// Join kind.
    pub kind: JoinKind,
    /// Joined source.
    pub source: QuerySource,
    /// Join condition.
    pub on: Expr,
}

/// Sort direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderDirection {
    /// Ascending.
    Asc,
    /// Descending.
    Desc,
}

/// One ordering term.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderExpr {
    /// Expression.
    pub expr: Expr,
    /// Direction.
    pub direction: OrderDirection,
}

/// Distinct mode.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DistinctMode {
    /// No de-duplication.
    #[default]
    None,
    /// `DISTINCT`.
    All,
    /// `DISTINCT ON (...)` (PostgreSQL only).
    On(Vec<Expr>),
}

/// Row-lock request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// `FOR UPDATE`.
    ForUpdate,
    /// `FOR UPDATE NOWAIT`.
    ForUpdateNoWait,
    /// `FOR UPDATE SKIP LOCKED`.
    ForUpdateSkipLocked,
}

/// Backend-neutral read query.
///
/// Built immutably: every builder method consumes and returns `self`, so a
/// `QuerySet` can clone a base plan and branch safely.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryPlan {
    /// Root source.
    pub source: QuerySource,
    /// Projection; empty means "all columns of the root source".
    pub projection: Vec<SelectExpr>,
    /// Joins.
    pub joins: Vec<JoinExpr>,
    /// `WHERE`.
    pub filter: Option<Expr>,
    /// `GROUP BY`.
    pub grouping: Vec<Expr>,
    /// `HAVING`.
    pub having: Option<Expr>,
    /// `ORDER BY`.
    pub ordering: Vec<OrderExpr>,
    /// `LIMIT`.
    pub limit: Option<u64>,
    /// `OFFSET`.
    pub offset: Option<u64>,
    /// Distinct mode.
    pub distinct: DistinctMode,
    /// Row lock.
    pub lock: Option<LockMode>,
}

impl QueryPlan {
    /// Plan selecting every row of `table`.
    pub fn from_table(table: impl Into<Ident>) -> Self {
        Self {
            source: QuerySource::table(table),
            projection: Vec::new(),
            joins: Vec::new(),
            filter: None,
            grouping: Vec::new(),
            having: None,
            ordering: Vec::new(),
            limit: None,
            offset: None,
            distinct: DistinctMode::None,
            lock: None,
        }
    }

    /// Add a projected expression.
    #[must_use]
    pub fn select(mut self, expr: impl Into<Expr>, alias: Option<&'static str>) -> Self {
        self.projection.push(SelectExpr {
            expr: expr.into(),
            alias: alias.map(Into::into),
        });
        self
    }

    /// AND a predicate into the filter.
    #[must_use]
    pub fn filter(mut self, predicate: Expr) -> Self {
        self.filter = Some(match self.filter.take() {
            Some(existing) => existing.and(predicate),
            None => predicate,
        });
        self
    }

    /// AND the negation of a predicate into the filter (Django `exclude`).
    #[must_use]
    pub fn exclude(self, predicate: Expr) -> Self {
        self.filter(!predicate)
    }

    /// Append an ordering term.
    #[must_use]
    pub fn order_by(mut self, expr: impl Into<Expr>, direction: OrderDirection) -> Self {
        self.ordering.push(OrderExpr {
            expr: expr.into(),
            direction,
        });
        self
    }

    /// Set `LIMIT`.
    #[must_use]
    pub fn limit(mut self, n: u64) -> Self {
        self.limit = Some(n);
        self
    }

    /// Set `OFFSET`.
    #[must_use]
    pub fn offset(mut self, n: u64) -> Self {
        self.offset = Some(n);
        self
    }

    /// Add a join.
    #[must_use]
    pub fn join(mut self, kind: JoinKind, source: QuerySource, on: Expr) -> Self {
        self.joins.push(JoinExpr { kind, source, on });
        self
    }

    /// Request a row lock.
    #[must_use]
    pub fn lock(mut self, mode: LockMode) -> Self {
        self.lock = Some(mode);
        self
    }

    /// Set distinct mode.
    #[must_use]
    pub fn distinct(mut self, mode: DistinctMode) -> Self {
        self.distinct = mode;
        self
    }

    /// Features this plan needs from a backend (deduplicated).
    pub fn required_features(&self) -> Vec<Feature> {
        let mut out = Vec::new();
        let mut need = |f| {
            if !out.contains(&f) {
                out.push(f);
            }
        };
        if !self.joins.is_empty() {
            need(Feature::Joins);
        }
        match self.lock {
            Some(LockMode::ForUpdate) => need(Feature::RowLocking),
            Some(_) => {
                need(Feature::RowLocking);
                need(Feature::LockModifiers);
            }
            None => {}
        }
        if matches!(self.distinct, DistinctMode::On(_)) {
            need(Feature::DistinctOn);
        }
        let exprs = self
            .projection
            .iter()
            .map(|s| &s.expr)
            .chain(self.joins.iter().map(|j| &j.on))
            .chain(self.filter.iter())
            .chain(self.having.iter())
            .chain(self.grouping.iter())
            .chain(self.ordering.iter().map(|o| &o.expr));
        for e in exprs {
            visit(e, &mut need);
        }
        out
    }

    /// Check the plan against backend capabilities.
    pub fn check(&self, caps: &BackendCapabilities) -> Result<(), BackendCapabilityError> {
        self.required_features()
            .into_iter()
            .try_for_each(|f| caps.require(f))
    }
}

fn visit(expr: &Expr, need: &mut impl FnMut(Feature)) {
    match expr {
        Expr::Lookup { expr, lookup } => {
            if matches!(lookup, Lookup::Regex(_)) {
                need(Feature::Regex);
            }
            visit(expr, need);
        }
        Expr::Binary { lhs, rhs, .. } => {
            visit(lhs, need);
            visit(rhs, need);
        }
        Expr::Unary { expr, .. } => visit(expr, need),
        Expr::And(v) | Expr::Or(v) => v.iter().for_each(|e| visit(e, need)),
        Expr::Exists(p) | Expr::Subquery(p) => p.required_features().into_iter().for_each(need),
        Expr::Column(_) | Expr::Value(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::BackendKind;

    #[test]
    fn filters_accumulate_with_and() {
        let p = QueryPlan::from_table("users")
            .filter(Expr::col("a").eq(1))
            .filter(Expr::col("b").eq(2));
        assert!(matches!(p.filter, Some(Expr::And(ref v)) if v.len() == 2));
    }

    #[test]
    fn branching_is_independent() {
        let base = QueryPlan::from_table("users").filter(Expr::col("active").eq(true));
        let a = base.clone().limit(10);
        let b = base.clone().order_by(Expr::col("id"), OrderDirection::Desc);
        assert_eq!(base.limit, None);
        assert_eq!(a.limit, Some(10));
        assert!(b.limit.is_none() && b.ordering.len() == 1);
    }

    #[test]
    fn capability_check_detects_nested_regex_and_locks() {
        let sub = QueryPlan::from_table("t").filter(Expr::col("x").lookup_regex("^a"));
        let p = QueryPlan::from_table("users")
            .filter(Expr::Exists(Box::new(sub)))
            .lock(LockMode::ForUpdateSkipLocked);
        let feats = p.required_features();
        assert!(feats.contains(&Feature::Regex));
        assert!(feats.contains(&Feature::LockModifiers));
        let err = p.check(&BackendCapabilities::sqlite());
        assert_eq!(
            err,
            Err(BackendCapabilityError::RowLockingUnsupported {
                backend: BackendKind::Sqlite
            })
        );
        assert!(p.check(&BackendCapabilities::postgres()).is_ok());
    }

    trait RegexExt {
        fn lookup_regex(self, p: &str) -> Expr;
    }
    impl RegexExt for Expr {
        fn lookup_regex(self, p: &str) -> Expr {
            Expr::Lookup {
                expr: Box::new(self),
                lookup: Lookup::Regex(p.into()),
            }
        }
    }
}
