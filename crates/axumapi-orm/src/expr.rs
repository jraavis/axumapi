//! Typed expression AST (Django `Q` / `F` / lookup equivalents).
//!
//! Expressions are pure data: building them never performs I/O. Backends
//! compile them into their own dialect.
//!
//! Two layers exist:
//! * [`Expr`] – untyped tree stored inside a [`QueryPlan`].
//! * [`Field<M, T>`] – a typed column handle. The `Model` derive (Phase 4)
//!   generates one associated constant per field (`User::name`), so lookups
//!   are checked at compile time: `icontains` exists only on text fields,
//!   comparison values must convert into the field's type.

use crate::plan::{OrderDirection, OrderExpr, QueryPlan};
use crate::value::Value;
use std::borrow::Cow;
use std::marker::PhantomData;

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

    /// `self = rhs`.
    pub fn eq(self, rhs: impl Into<Expr>) -> Self {
        self.binary(BinaryOp::Eq, rhs)
    }
    /// `self <> rhs`.
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

    /// Inclusive range.
    pub fn range(self, lo: impl Into<Value>, hi: impl Into<Value>) -> Self {
        self.lookup(Lookup::Range(lo.into(), hi.into()))
    }

    /// `IS NULL` / `IS NOT NULL`.
    pub fn is_null(self, yes: bool) -> Self {
        self.lookup(Lookup::IsNull(yes))
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

/// Typed column handle for model `M` whose Rust type is `T`.
///
/// Generated as associated constants by the `Model` derive (Phase 4):
/// `impl User { pub const name: Field<User, String> = Field::new("name"); }`.
pub struct Field<M, T> {
    name: &'static str,
    _marker: PhantomData<fn() -> (M, T)>,
}

impl<M, T> Clone for Field<M, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<M, T> Copy for Field<M, T> {}

impl<M, T> std::fmt::Debug for Field<M, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Field").field(&self.name).finish()
    }
}

impl<M, T> Field<M, T> {
    /// Create a field handle. Intended for generated code.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _marker: PhantomData,
        }
    }

    /// Column name.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Untyped expression for this column.
    pub fn expr(self) -> Expr {
        Expr::col(self.name)
    }

    /// Ascending ordering on this column.
    pub fn asc(self) -> OrderExpr {
        self.expr().asc()
    }

    /// Descending ordering on this column.
    pub fn desc(self) -> OrderExpr {
        self.expr().desc()
    }
}

impl<M, T> From<Field<M, T>> for Expr {
    fn from(f: Field<M, T>) -> Self {
        f.expr()
    }
}

/// Typed operand accepted by comparisons on `Field<M, T>`: a value of `T`
/// or another field of the same model with the same type.
pub trait Operand<M, T> {
    /// Convert into an expression.
    fn into_expr(self) -> Expr;
}

macro_rules! scalar_operand {
    ($($t:ty),*) => {$(
        impl<M> Operand<M, $t> for $t {
            fn into_expr(self) -> Expr {
                Expr::Value(self.into())
            }
        }
    )*};
}
scalar_operand!(
    bool,
    i16,
    i32,
    i64,
    f32,
    f64,
    String,
    Vec<u8>,
    serde_json::Value
);

impl<M> Operand<M, String> for &str {
    fn into_expr(self) -> Expr {
        Expr::Value(self.into())
    }
}

impl<M, T> Operand<M, T> for Field<M, T> {
    fn into_expr(self) -> Expr {
        self.expr()
    }
}

macro_rules! typed_cmp {
    ($($method:ident),*) => {
        impl<M, T> Field<M, T> {$(
            #[doc = concat!("Typed `", stringify!($method), "` comparison.")]
            pub fn $method(self, rhs: impl Operand<M, T>) -> Expr {
                self.expr().$method(rhs.into_expr())
            }
        )*}
    };
}
typed_cmp!(eq, ne, lt, le, gt, ge);

impl<M, T> Field<M, T> {
    /// `IS NULL`.
    pub fn is_null(self) -> Expr {
        self.expr().is_null(true)
    }
}

impl<M, T: Into<Value>> Field<M, T> {
    /// Membership test.
    pub fn is_in(self, values: impl IntoIterator<Item = T>) -> Expr {
        self.expr().is_in(values)
    }

    /// Inclusive range.
    pub fn range(self, lo: T, hi: T) -> Expr {
        self.expr().range(lo, hi)
    }
}

macro_rules! text_lookup {
    ($($method:ident => $variant:ident, $ci:expr);* $(;)?) => {
        impl<M> Field<M, String> {$(
            #[doc = concat!("Text lookup `", stringify!($method), "`.")]
            pub fn $method(self, needle: impl Into<String>) -> Expr {
                self.expr().lookup(Lookup::$variant { needle: needle.into(), case_insensitive: $ci })
            }
        )*}
    };
}
text_lookup!(
    contains => Contains, false;
    icontains => Contains, true;
    starts_with => StartsWith, false;
    istarts_with => StartsWith, true;
    ends_with => EndsWith, false;
    iends_with => EndsWith, true;
);

impl<M> Field<M, String> {
    /// Case-insensitive equality.
    pub fn iexact(self, v: impl Into<String>) -> Expr {
        self.expr().lookup(Lookup::IExact(Value::Text(v.into())))
    }

    /// Regex match (requires [`Feature::Regex`](crate::Feature::Regex)).
    pub fn regex(self, pattern: impl Into<String>) -> Expr {
        self.expr().lookup(Lookup::Regex(pattern.into()))
    }
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
    }

    #[test]
    fn typed_comparisons_build_trees() {
        let e = User::likes.gt(User::dislikes);
        assert_eq!(e, Expr::col("likes").gt(Expr::col("dislikes")));
        assert_eq!(User::likes.eq(3_i64), Expr::col("likes").eq(3_i64));
        assert_eq!(User::name.eq("bob"), Expr::col("name").eq("bob"));
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
    fn arithmetic_f_expressions() {
        let e = User::likes.expr() - User::dislikes.expr();
        assert!(matches!(
            e,
            Expr::Binary {
                op: BinaryOp::Sub,
                ..
            }
        ));
    }
}
