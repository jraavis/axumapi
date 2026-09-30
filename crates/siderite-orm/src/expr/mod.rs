//! Typed expression AST (Django `Q` / `F` / lookup equivalents).
//!
//! Expressions are pure data: building them never performs I/O. Backends
//! compile them into their own dialect.
//!
//! Two layers exist:
//! * [`Expr`]: untyped tree stored inside a [`QueryPlan`].
//! * [`Field<M, T>`]: a typed column handle. The `Model` derive generates one
//!   associated constant per field (`User::name`), so lookups are checked at
//!   compile time: `icontains` exists only on text fields, comparison values
//!   must convert into the field's type. [`Calc`] (arithmetic results) and
//!   [`Joined`] (fields reached through a foreign key) offer the same lookups.
//!
//! Function, aggregate and window constructors live in [`functions`],
//! [`Count`]/[`Sum`]/... and [`RowNumber`]/[`Rank`]/....

mod aggregate;
mod field;
pub mod functions;
mod related;
mod window;

pub use aggregate::{
    AggFunc, Aggregate, ArrayAgg, Avg, Count, Max, Min, StdDev, StringAgg, Sum, Variance,
};
pub use field::{Calc, DateLike, Field, Literal, Numeric, Operand, TextLike, TimeLike};
pub use functions::{CaseBuilder, DatePart, Function};
pub use related::{FkSlot, Joined, RelHop, Related, RelatedColumn, path_alias_of};
pub use window::{
    CumeDist, DenseRank, FirstValue, Lag, LastValue, Lead, Ntile, PercentRank, Rank, RowNumber,
    Window, WindowFunc,
};

use crate::model::Model;
use crate::plan::{OrderDirection, OrderExpr, QueryPlan};
use crate::types::SqlType;
use crate::value::Value;
use std::borrow::Cow;

/// Identifier type used for tables, aliases and columns.
pub type Ident = Cow<'static, str>;

/// A (optionally source-qualified) column reference.
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    /// Table / alias the column belongs to. `None` means the plan's root source.
    pub source: Option<Ident>,
    /// Column name.
    pub name: Ident,
}

impl Column {
    /// Column on the root source.
    pub fn new(name: impl Into<Ident>) -> Self {
        Self {
            source: None,
            name: name.into(),
        }
    }

    /// Column qualified by a table or join alias.
    pub fn qualified(source: impl Into<Ident>, name: impl Into<Ident>) -> Self {
        Self {
            source: Some(source.into()),
            name: name.into(),
        }
    }
}

/// Binary operators: comparisons, boolean connectives and arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum BinaryOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Boolean negation.
    Not,
    /// Arithmetic negation.
    Neg,
}

/// Lookups that are not plain binary comparisons.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    /// Case-insensitive equality.
    IExact(Value),
    /// Substring match; `case_insensitive` selects `icontains`.
    Contains {
        /// Pattern (literal text, not a LIKE pattern; backends escape it).
        needle: String,
        /// Case-insensitive?
        case_insensitive: bool,
    },
    /// Prefix match.
    StartsWith {
        /// Prefix (literal text).
        needle: String,
        /// Case-insensitive?
        case_insensitive: bool,
    },
    /// Suffix match.
    EndsWith {
        /// Suffix (literal text).
        needle: String,
        /// Case-insensitive?
        case_insensitive: bool,
    },
    /// Regex match (capability-gated).
    Regex(String),
    /// Membership in a literal list.
    In(Vec<Value>),
    /// Membership in the single column produced by a subquery.
    InSubquery(Box<QueryPlan>),
    /// Inclusive range `lo <= x <= hi`.
    Range(Value, Value),
    /// `IS NULL` (true) / `IS NOT NULL` (false).
    IsNull(bool),
}

/// Expression tree node.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Column reference (Django `F`).
    Column(Column),
    /// Literal, always bound as a parameter.
    Value(Value),
    /// Binary operation.
    Binary {
        /// Operator.
        op: BinaryOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },
    /// Unary operation.
    Unary {
        /// Operator.
        op: UnaryOp,
        /// Operand.
        expr: Box<Expr>,
    },
    /// Conjunction of all children (empty = TRUE).
    And(Vec<Expr>),
    /// Disjunction of all children (empty = FALSE).
    Or(Vec<Expr>),
    /// Lookup applied to an expression.
    Lookup {
        /// Target expression.
        expr: Box<Expr>,
        /// Lookup.
        lookup: Lookup,
    },
    /// `EXISTS (subquery)`.
    Exists(Box<QueryPlan>),
    /// Scalar subquery.
    Subquery(Box<QueryPlan>),
    /// Scalar function call.
    Func {
        /// Function.
        func: Function,
        /// Arguments.
        args: Vec<Expr>,
    },
    /// `CAST(expr AS type)`.
    Cast {
        /// Operand.
        expr: Box<Expr>,
        /// Target type family.
        ty: SqlType,
    },
    /// Searched `CASE WHEN .. THEN .. [ELSE ..] END`.
    Case {
        /// `(condition, result)` branches, tried in order.
        branches: Vec<(Expr, Expr)>,
        /// `ELSE` result (`NULL` when absent).
        otherwise: Option<Box<Expr>>,
    },
    /// Aggregate function (`COUNT`, `SUM`, ...).
    Aggregate(Aggregate),
    /// Window function with its `OVER (..)` clause.
    Window(Box<Window>),
    /// Date or time component of a temporal expression.
    DatePart {
        /// Component to extract.
        part: DatePart,
        /// Temporal operand.
        expr: Box<Expr>,
    },
    /// Column of the enclosing query, used inside a correlated subquery.
    OuterRef(Column),
    /// Column reached through foreign keys; resolved into joins by
    /// [`QueryPlan::resolve_relations`].
    Related(RelatedColumn),
}

impl Expr {
    /// Column expression on the root source.
    pub fn col(name: impl Into<Ident>) -> Self {
        Expr::Column(Column::new(name))
    }

    /// Literal value expression.
    pub fn val(v: impl Into<Value>) -> Self {
        Expr::Value(v.into())
    }

    /// Reference to a column of the enclosing query (Django `OuterRef`).
    pub fn outer(name: impl Into<Ident>) -> Self {
        Expr::OuterRef(Column::new(name))
    }

    /// `EXISTS (plan)`.
    pub fn exists(plan: QueryPlan) -> Self {
        Expr::Exists(Box::new(plan))
    }

    /// Scalar subquery: `plan` must select exactly one column and one row.
    pub fn subquery(plan: QueryPlan) -> Self {
        Expr::Subquery(Box::new(plan))
    }

    fn binary(self, op: BinaryOp, rhs: impl Into<Expr>) -> Self {
        Expr::Binary {
            op,
            lhs: Box::new(self),
            rhs: Box::new(rhs.into()),
        }
    }

    fn lookup(self, lookup: Lookup) -> Self {
        Expr::Lookup {
            expr: Box::new(self),
            lookup,
        }
    }

    /// `self = rhs`. A `NULL` right-hand side compiles to `IS NULL`.
    pub fn eq(self, rhs: impl Into<Expr>) -> Self {
        self.binary(BinaryOp::Eq, rhs)
    }
    /// `self <> rhs`. A `NULL` right-hand side compiles to `IS NOT NULL`.
    pub fn ne(self, rhs: impl Into<Expr>) -> Self {
        self.binary(BinaryOp::Ne, rhs)
    }
    /// `self < rhs`.
    pub fn lt(self, rhs: impl Into<Expr>) -> Self {
        self.binary(BinaryOp::Lt, rhs)
    }
    /// `self <= rhs`.
    pub fn le(self, rhs: impl Into<Expr>) -> Self {
        self.binary(BinaryOp::Le, rhs)
    }
    /// `self > rhs`.
    pub fn gt(self, rhs: impl Into<Expr>) -> Self {
        self.binary(BinaryOp::Gt, rhs)
    }
    /// `self >= rhs`.
    pub fn ge(self, rhs: impl Into<Expr>) -> Self {
        self.binary(BinaryOp::Ge, rhs)
    }

    /// Logical AND; flattens nested ANDs.
    pub fn and(self, rhs: Expr) -> Self {
        match (self, rhs) {
            (Expr::And(mut a), Expr::And(b)) => {
                a.extend(b);
                Expr::And(a)
            }
            (Expr::And(mut a), b) => {
                a.push(b);
                Expr::And(a)
            }
            (a, b) => Expr::And(vec![a, b]),
        }
    }

    /// Logical OR; flattens nested ORs.
    pub fn or(self, rhs: Expr) -> Self {
        match (self, rhs) {
            (Expr::Or(mut a), Expr::Or(b)) => {
                a.extend(b);
                Expr::Or(a)
            }
            (Expr::Or(mut a), b) => {
                a.push(b);
                Expr::Or(a)
            }
            (a, b) => Expr::Or(vec![a, b]),
        }
    }

    /// Membership test.
    pub fn is_in<I, V>(self, values: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: Into<Value>,
    {
        self.lookup(Lookup::In(values.into_iter().map(Into::into).collect()))
    }

    /// Membership in the single column selected by `plan`.
    pub fn in_subquery(self, plan: QueryPlan) -> Self {
        self.lookup(Lookup::InSubquery(Box::new(plan)))
    }

    /// Inclusive range.
    pub fn range(self, lo: impl Into<Value>, hi: impl Into<Value>) -> Self {
        self.lookup(Lookup::Range(lo.into(), hi.into()))
    }

    /// `IS NULL` / `IS NOT NULL`.
    pub fn is_null(self, yes: bool) -> Self {
        self.lookup(Lookup::IsNull(yes))
    }

    /// Case-insensitive equality (text).
    pub fn iexact(self, v: impl Into<Value>) -> Self {
        self.lookup(Lookup::IExact(v.into()))
    }

    /// Regex match (requires [`Feature::Regex`](crate::Feature::Regex)).
    pub fn regex(self, pattern: impl Into<String>) -> Self {
        self.lookup(Lookup::Regex(pattern.into()))
    }

    /// `CAST(self AS ty)`.
    pub fn cast(self, ty: SqlType) -> Self {
        Expr::Cast {
            expr: Box::new(self),
            ty,
        }
    }

    /// Extract a date or time component.
    pub fn date_part(self, part: DatePart) -> Self {
        Expr::DatePart {
            part,
            expr: Box::new(self),
        }
    }

    /// Ascending ordering term.
    pub fn asc(self) -> OrderExpr {
        OrderExpr {
            expr: self,
            direction: OrderDirection::Asc,
        }
    }

    /// Descending ordering term.
    pub fn desc(self) -> OrderExpr {
        OrderExpr {
            expr: self,
            direction: OrderDirection::Desc,
        }
    }

    /// Direct sub-expressions (subquery plans are reached through
    /// [`subplan`](Self::subplan) instead).
    pub fn children(&self) -> Vec<&Expr> {
        match self {
            Expr::Column(_)
            | Expr::Value(_)
            | Expr::OuterRef(_)
            | Expr::Related(_)
            | Expr::Exists(_)
            | Expr::Subquery(_) => Vec::new(),
            Expr::Binary { lhs, rhs, .. } => vec![lhs, rhs],
            Expr::Unary { expr, .. }
            | Expr::Lookup { expr, .. }
            | Expr::Cast { expr, .. }
            | Expr::DatePart { expr, .. } => vec![expr],
            Expr::And(items) | Expr::Or(items) => items.iter().collect(),
            Expr::Func { args, .. } => args.iter().collect(),
            Expr::Case {
                branches,
                otherwise,
            } => branches
                .iter()
                .flat_map(|(when, then)| [when, then])
                .chain(otherwise.as_deref())
                .collect(),
            Expr::Aggregate(agg) => agg.children(),
            Expr::Window(window) => window.children(),
        }
    }

    /// Mutable counterpart of [`children`](Self::children).
    pub fn children_mut(&mut self) -> Vec<&mut Expr> {
        match self {
            Expr::Column(_)
            | Expr::Value(_)
            | Expr::OuterRef(_)
            | Expr::Related(_)
            | Expr::Exists(_)
            | Expr::Subquery(_) => Vec::new(),
            Expr::Binary { lhs, rhs, .. } => vec![lhs, rhs],
            Expr::Unary { expr, .. }
            | Expr::Lookup { expr, .. }
            | Expr::Cast { expr, .. }
            | Expr::DatePart { expr, .. } => vec![expr],
            Expr::And(items) | Expr::Or(items) => items.iter_mut().collect(),
            Expr::Func { args, .. } => args.iter_mut().collect(),
            Expr::Case {
                branches,
                otherwise,
            } => branches
                .iter_mut()
                .flat_map(|(when, then)| [when, then])
                .chain(otherwise.as_deref_mut())
                .collect(),
            Expr::Aggregate(agg) => agg.children_mut(),
            Expr::Window(window) => window.children_mut(),
        }
    }

    /// The nested query of `EXISTS`, a scalar subquery or `IN (subquery)`.
    pub fn subplan(&self) -> Option<&QueryPlan> {
        match self {
            Expr::Exists(plan)
            | Expr::Subquery(plan)
            | Expr::Lookup {
                lookup: Lookup::InSubquery(plan),
                ..
            } => Some(plan),
            _ => None,
        }
    }

    /// Mutable counterpart of [`subplan`](Self::subplan).
    pub fn subplan_mut(&mut self) -> Option<&mut QueryPlan> {
        match self {
            Expr::Exists(plan)
            | Expr::Subquery(plan)
            | Expr::Lookup {
                lookup: Lookup::InSubquery(plan),
                ..
            } => Some(plan),
            _ => None,
        }
    }

    /// Whether this expression computes an aggregate over the query's rows
    /// (so a filter on it belongs in `HAVING`). Aggregates inside window
    /// functions and subqueries do not count.
    pub fn contains_aggregate(&self) -> bool {
        matches!(self, Expr::Aggregate(_)) || self.children().iter().any(|c| c.contains_aggregate())
    }

    /// Whether this expression contains a window function.
    pub fn contains_window(&self) -> bool {
        matches!(self, Expr::Window(_)) || self.children().iter().any(|c| c.contains_window())
    }
}

impl std::ops::Not for Expr {
    type Output = Expr;
    fn not(self) -> Expr {
        Expr::Unary {
            op: UnaryOp::Not,
            expr: Box::new(self),
        }
    }
}

impl std::ops::Neg for Expr {
    type Output = Expr;
    fn neg(self) -> Expr {
        Expr::Unary {
            op: UnaryOp::Neg,
            expr: Box::new(self),
        }
    }
}

macro_rules! arith_ops {
    ($($trait:ident $method:ident $op:ident),*) => {$(
        impl<R: Into<Expr>> std::ops::$trait<R> for Expr {
            type Output = Expr;
            fn $method(self, rhs: R) -> Expr { self.binary(BinaryOp::$op, rhs) }
        }
    )*};
}
arith_ops!(Add add Add, Sub sub Sub, Mul mul Mul, Div div Div, Rem rem Mod);

impl<T: Into<Value>> From<T> for Expr {
    fn from(v: T) -> Self {
        Expr::Value(v.into())
    }
}

/// Model whose primary key column is `M`'s; helper for generated code and
/// relation handles.
pub(crate) fn pk_column<M: Model>() -> Ident {
    M::META.pk().map_or("id", |f| f.column).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct User;
    #[allow(non_upper_case_globals)]
    impl User {
        const name: Field<User, String> = Field::new("name");
        const likes: Field<User, i64> = Field::new("likes");
        const dislikes: Field<User, i64> = Field::new("dislikes");
        const age: Field<User, Option<i32>> = Field::new("age");
    }

    #[test]
    fn typed_comparisons_build_trees() {
        let e = User::likes.gt(User::dislikes);
        assert_eq!(e, Expr::col("likes").gt(Expr::col("dislikes")));
        assert_eq!(User::likes.eq(3_i64), Expr::col("likes").eq(3_i64));
        assert_eq!(User::name.eq("bob"), Expr::col("name").eq("bob"));
        assert_eq!(User::age.eq(3), Expr::col("age").eq(3));
    }

    #[test]
    fn boolean_composition_flattens() {
        let e = User::name
            .icontains("al")
            .or(User::name.ends_with("x"))
            .or(User::likes.is_null());
        assert!(matches!(e, Expr::Or(ref v) if v.len() == 3));
        assert!(matches!(
            !e,
            Expr::Unary {
                op: UnaryOp::Not,
                ..
            }
        ));
    }

    #[test]
    fn arithmetic_is_typed_and_composable() {
        let net = User::likes - User::dislikes;
        let e = User::likes.eq(net.clone() * 2_i64);
        assert!(matches!(
            e,
            Expr::Binary {
                op: BinaryOp::Eq,
                ..
            }
        ));
        assert!(net.gt(0_i64).children().len() == 2);
    }

    #[test]
    fn aggregates_are_found_outside_windows_and_subqueries() {
        let sum = Sum::of(Expr::col("x"));
        assert!(Expr::from(sum.clone()).gt(1_i64).contains_aggregate());
        let windowed = Window::over(WindowFunc::Aggregate(sum));
        assert!(!Expr::from(windowed).contains_aggregate());
    }
}
