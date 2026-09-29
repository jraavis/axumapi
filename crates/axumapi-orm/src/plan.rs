//! [`QueryPlan`]: the backend-neutral intermediate representation.
//!
//! Every high-level query API compiles into a `QueryPlan`; backends compile a
//! plan into SQL, a MongoDB pipeline, etc. Relational-only parts (joins,
//! locking, `DISTINCT ON`) are reported through
//! [`QueryPlan::required_features`] so they can be checked against
//! [`BackendCapabilities`] before execution.

use crate::capabilities::{BackendCapabilities, Feature};
use crate::error::BackendCapabilityError;
use crate::expr::path_alias_of;
use crate::expr::{AggFunc, Aggregate, Column, Expr, Ident, Lookup, RelatedColumn, WindowFunc};

/// What the query reads from.
#[derive(Debug, Clone, PartialEq)]
pub struct QuerySource {
    /// Table or collection name (for a subquery source, its alias).
    pub name: Ident,
    /// Optional alias.
    pub alias: Option<Ident>,
    /// Derived table: the source is the result of this query.
    pub subquery: Option<Box<QueryPlan>>,
}

impl QuerySource {
    /// Unaliased source.
    pub fn table(name: impl Into<Ident>) -> Self {
        Self {
            name: name.into(),
            alias: None,
            subquery: None,
        }
    }

    /// Derived table `(plan) AS alias`.
    pub fn subquery(plan: QueryPlan, alias: impl Into<Ident>) -> Self {
        Self {
            name: alias.into(),
            alias: None,
            subquery: Some(Box::new(plan)),
        }
    }

    /// Name other parts of the query use to refer to this source.
    pub fn reference(&self) -> &Ident {
        self.alias.as_ref().unwrap_or(&self.name)
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

impl SelectExpr {
    /// Projection of `expr` under an optional output name.
    pub fn new(expr: Expr, alias: Option<Ident>) -> Self {
        Self { expr, alias }
    }
}

/// A column of the root model, named after itself.
impl From<&'static str> for SelectExpr {
    fn from(column: &'static str) -> Self {
        Self::new(Expr::col(column), Some(column.into()))
    }
}

/// An expression under an explicit output name: `("total", Sum::of(..))`.
impl<N: Into<Ident>, E: Into<Expr>> From<(N, E)> for SelectExpr {
    fn from((name, expr): (N, E)) -> Self {
        Self::new(expr.into(), Some(name.into()))
    }
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

/// How a query is combined with another one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOp {
    /// `UNION` (distinct rows).
    Union,
    /// `UNION ALL`.
    UnionAll,
    /// `INTERSECT`.
    Intersect,
    /// `EXCEPT`.
    Except,
}

/// Another query combined with the plan by a [`SetOp`].
#[derive(Debug, Clone, PartialEq)]
pub struct Compound {
    /// Operator.
    pub op: SetOp,
    /// Right-hand query.
    pub plan: QueryPlan,
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
    /// Queries combined with this one (`UNION` ...). `ordering`, `limit` and
    /// `offset` of this plan then apply to the combined result, and every
    /// member must project the same number of columns.
    pub compound: Vec<Compound>,
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
            compound: Vec::new(),
        }
    }

    /// Plan selecting every row of the derived table `(inner) AS alias`.
    pub fn from_subquery(inner: QueryPlan, alias: impl Into<Ident>) -> Self {
        let mut plan = Self::from_table("");
        plan.source = QuerySource::subquery(inner, alias);
        plan
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

    /// Every expression of this plan (not of nested plans).
    fn exprs(&self) -> impl Iterator<Item = &Expr> {
        let distinct_on = match &self.distinct {
            DistinctMode::On(exprs) => exprs.as_slice(),
            _ => &[],
        };
        self.projection
            .iter()
            .map(|s| &s.expr)
            .chain(self.joins.iter().map(|j| &j.on))
            .chain(self.filter.iter())
            .chain(self.having.iter())
            .chain(self.grouping.iter())
            .chain(self.ordering.iter().map(|o| &o.expr))
            .chain(distinct_on)
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
        let nested = self
            .compound
            .iter()
            .map(|c| &c.plan)
            .chain(self.source.subquery.as_deref());
        for plan in nested {
            plan.required_features().into_iter().for_each(&mut need);
        }
        for e in self.exprs() {
            visit(e, &mut need);
        }
        out
    }

    /// Replace [`Expr::Related`] columns by joined-table columns, adding the
    /// needed `LEFT JOIN`s (deduplicated by alias) to this plan and to nested
    /// plans. Idempotent; querysets call it after every builder step.
    #[must_use]
    pub fn resolve_relations(mut self) -> Self {
        let root = self.source.reference().clone();
        let mut joins = std::mem::take(&mut self.joins);
        // Later joins may reference earlier ones, so resolve their `ON` last.
        let mut on_clauses = std::mem::take(&mut joins)
            .into_iter()
            .map(|mut j| {
                resolve_expr(&mut j.on, &root, &mut joins);
                j
            })
            .collect::<Vec<_>>();
        let mut exprs: Vec<&mut Expr> = Vec::new();
        exprs.extend(self.projection.iter_mut().map(|s| &mut s.expr));
        exprs.extend(self.filter.iter_mut());
        exprs.extend(self.having.iter_mut());
        exprs.extend(self.grouping.iter_mut());
        exprs.extend(self.ordering.iter_mut().map(|o| &mut o.expr));
        if let DistinctMode::On(on) = &mut self.distinct {
            exprs.extend(on.iter_mut());
        }
        for e in exprs {
            resolve_expr(e, &root, &mut joins);
        }
        on_clauses.extend(joins);
        self.joins = on_clauses;
        self.compound = std::mem::take(&mut self.compound)
            .into_iter()
            .map(|c| Compound {
                op: c.op,
                plan: c.plan.resolve_relations(),
            })
            .collect();
        if let Some(inner) = self.source.subquery.take() {
            self.source.subquery = Some(Box::new(inner.resolve_relations()));
        }
        self
    }

    /// Check the plan against backend capabilities.
    pub fn check(&self, caps: &BackendCapabilities) -> Result<(), BackendCapabilityError> {
        self.required_features()
            .into_iter()
            .try_for_each(|f| caps.require(f))
    }
}

impl Expr {
    /// Features this expression (including nested subqueries) needs.
    pub fn required_features(&self) -> Vec<Feature> {
        let mut out = Vec::new();
        visit(self, &mut |f| {
            if !out.contains(&f) {
                out.push(f);
            }
        });
        out
    }
}

fn visit(expr: &Expr, need: &mut impl FnMut(Feature)) {
    match expr {
        Expr::Lookup {
            lookup: Lookup::Regex(_),
            ..
        } => need(Feature::Regex),
        Expr::Related(_) => need(Feature::Joins),
        Expr::Aggregate(agg) => aggregate_features(agg, need),
        Expr::Window(window) => {
            need(Feature::WindowFunctions);
            if let WindowFunc::Aggregate(agg) = &window.func {
                aggregate_features(agg, need);
            }
        }
        _ => {}
    }
    if let Some(plan) = expr.subplan() {
        plan.required_features().into_iter().for_each(&mut *need);
    }
    for child in expr.children() {
        visit(child, need);
    }
}

fn aggregate_features(agg: &Aggregate, need: &mut impl FnMut(Feature)) {
    match agg.func {
        AggFunc::StdDev { .. } | AggFunc::Variance { .. } => need(Feature::StatisticalAggregates),
        AggFunc::ArrayAgg => need(Feature::Arrays),
        _ => {}
    }
}

fn resolve_expr(expr: &mut Expr, root: &Ident, joins: &mut Vec<JoinExpr>) {
    if let Expr::Related(related) = expr {
        *expr = Expr::Column(join_path(related, root, joins));
        return;
    }
    if let Some(sub) = expr.subplan_mut() {
        let taken = std::mem::replace(sub, QueryPlan::from_table(""));
        *sub = taken.resolve_relations();
    }
    for child in expr.children_mut() {
        resolve_expr(child, root, joins);
    }
}

/// Add the joins for `related.path` (skipping existing aliases) and return
/// the qualified column.
fn join_path(related: &RelatedColumn, root: &Ident, joins: &mut Vec<JoinExpr>) -> Column {
    let mut parent = root.to_string();
    for depth in 1..=related.path.len() {
        let hop = &related.path[depth - 1];
        let alias = path_alias_of(&related.path[..depth]);
        if !joins
            .iter()
            .any(|j| j.source.alias.as_deref() == Some(alias.as_str()))
        {
            joins.push(JoinExpr {
                kind: JoinKind::Left,
                source: QuerySource {
                    name: hop.table.clone(),
                    alias: Some(alias.clone().into()),
                    subquery: None,
                },
                on: Expr::Column(Column::qualified(parent.clone(), hop.fk_column.clone())).eq(
                    Expr::Column(Column::qualified(alias.clone(), hop.pk_column.clone())),
                ),
            });
        }
        parent = alias;
    }
    Column::qualified(parent, related.column.clone())
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
