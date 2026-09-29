//! Typed column handles: [`Field`], [`Calc`] and the operand traits that make
//! `User::age.eq(3)` compile only for values that fit the column.

use super::related::Joined;
use super::{Column, Expr, Ident};
use crate::model::Model;
use crate::plan::{OrderExpr, QueryPlan, SelectExpr};
use crate::relations::ForeignKey;
use crate::types::{DbType, SqlType};
use crate::value::Value;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use std::marker::PhantomData;
use uuid::Uuid;

/// Typed column handle for model `M` whose Rust type is `T`.
///
/// Generated as associated constants by the `Model` derive:
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

    /// Reference to this column of the *enclosing* query, for use inside a
    /// correlated subquery (Django `OuterRef`).
    pub fn outer_ref(self) -> Expr {
        Expr::OuterRef(Column::new(self.name))
    }

    /// `column = value` assignment for `QuerySet::update`.
    pub fn set(self, value: impl Operand<M, T>) -> (Ident, Expr) {
        (self.name.into(), value.into_expr())
    }

    /// `column = <expression>` assignment, e.g. `Post::likes.set_expr(Post::likes + 1)`.
    pub fn set_expr(self, value: impl Into<Expr>) -> (Ident, Expr) {
        (self.name.into(), value.into())
    }

    /// Project this column in `values(..)`, named after the column.
    pub fn select(self) -> SelectExpr {
        SelectExpr::new(Expr::col(self.name), Some(self.name.into()))
    }
}

impl<M, T> From<Field<M, T>> for Expr {
    fn from(f: Field<M, T>) -> Self {
        Expr::col(f.name)
    }
}

impl<M, T> From<Field<M, T>> for SelectExpr {
    fn from(f: Field<M, T>) -> Self {
        f.select()
    }
}

/// Result of arithmetic on typed handles: an expression that is still known
/// to have type `T`, so it can be compared with and assigned to `T` fields.
pub struct Calc<M, T> {
    expr: Expr,
    _marker: PhantomData<fn() -> (M, T)>,
}

impl<M, T> Calc<M, T> {
    fn new(expr: Expr) -> Self {
        Self {
            expr,
            _marker: PhantomData,
        }
    }

    /// Name this expression for `values(..)`.
    pub fn alias(self, name: impl Into<Ident>) -> SelectExpr {
        SelectExpr::new(self.expr, Some(name.into()))
    }
}

impl<M, T> Clone for Calc<M, T> {
    fn clone(&self) -> Self {
        Self::new(self.expr.clone())
    }
}

impl<M, T> std::fmt::Debug for Calc<M, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Calc").field(&self.expr).finish()
    }
}

impl<M, T> From<Calc<M, T>> for Expr {
    fn from(c: Calc<M, T>) -> Self {
        c.expr
    }
}

/// A value that can stand for a `T` column in a comparison or assignment:
/// a `T`, a `T` for an `Option<T>` column, a primary key or model reference
/// for a foreign key, or another typed handle of type `T`.
pub trait Operand<M, T> {
    /// Convert into an expression.
    fn into_expr(self) -> Expr;
}

/// A literal value that can stand for a `T` column (no column references),
/// used by `is_in` and `range`.
pub trait Literal<T> {
    /// Convert into a bind value.
    fn into_value(self) -> Value;
}

/// Implements [`Literal`] and [`Operand`] for one kind of literal.
macro_rules! literal {
    ([$($generics:tt)*] $from:ty => $to:ty, |$v:ident| $convert:expr) => {
        impl<$($generics)*> Literal<$to> for $from {
            fn into_value(self) -> Value {
                let $v = self;
                $convert
            }
        }
        impl<$($generics)*, M> Operand<M, $to> for $from {
            fn into_expr(self) -> Expr {
                Expr::Value(Literal::<$to>::into_value(self))
            }
        }
    };
}

literal!([T: DbType] T => T, |v| v.to_value());
literal!([T: DbType] T => Option<T>, |v| v.to_value());
literal!(['a] &'a str => String, |v| Value::Text(v.to_owned()));
literal!(['a] &'a str => Option<String>, |v| Value::Text(v.to_owned()));
literal!(['a, T: Model] &'a T => ForeignKey<T>, |v| v.pk().to_value());
literal!(['a, T: Model] &'a T => Option<ForeignKey<T>>, |v| v.pk().to_value());

/// Primary-key values compare with foreign keys pointing at models keyed by them.
macro_rules! key_literal {
    ($($key:ty),*) => {$(
        literal!([T: Model<Pk = $key>] $key => ForeignKey<T>, |v| v.to_value());
        literal!([T: Model<Pk = $key>] $key => Option<ForeignKey<T>>, |v| v.to_value());
    )*};
}
key_literal!(i16, i32, i64, Uuid, String);
literal!(['a, T: Model<Pk = String>] &'a str => ForeignKey<T>, |v| Value::Text(v.to_owned()));
literal!(['a, T: Model<Pk = String>] &'a str => Option<ForeignKey<T>>, |v| Value::Text(v.to_owned()));

/// Column types that support arithmetic.
pub trait Numeric {}
/// Column types that have a calendar date (`year`, `month`, ...).
pub trait DateLike {}
/// Column types that have a time of day (`hour`, `minute`, `second`).
pub trait TimeLike {}
/// Text column types (`icontains`, `starts_with`, ...).
pub trait TextLike {}

macro_rules! marker {
    ($trait:ident: $($t:ty),*) => {$(
        impl $trait for $t {}
        impl $trait for Option<$t> {}
    )*};
}
marker!(Numeric: i16, i32, i64, f32, f64, Decimal);
marker!(DateLike: NaiveDate, DateTime<Utc>);
marker!(TimeLike: NaiveTime, DateTime<Utc>);
marker!(TextLike: String);

/// Lookups shared by every typed handle `$handle<M, T>`.
macro_rules! typed_lookups {
    ($handle:ident) => {
        impl<M, T> Operand<M, T> for $handle<M, T> {
            fn into_expr(self) -> Expr {
                self.into()
            }
        }

        impl<M, T> $handle<M, T> {
            /// Untyped expression for this handle.
            pub fn expr(self) -> Expr {
                self.into()
            }

            /// Ascending ordering.
            pub fn asc(self) -> OrderExpr {
                self.expr().asc()
            }

            /// Descending ordering.
            pub fn desc(self) -> OrderExpr {
                self.expr().desc()
            }

            /// `IS NULL`.
            pub fn is_null(self) -> Expr {
                self.expr().is_null(true)
            }

            /// `IS NOT NULL`.
            pub fn is_not_null(self) -> Expr {
                self.expr().is_null(false)
            }

            /// Membership in a list of literals.
            pub fn is_in<L: Literal<T>>(self, values: impl IntoIterator<Item = L>) -> Expr {
                self.expr().is_in(values.into_iter().map(Literal::into_value))
            }

            /// Membership in the single column a subquery selects.
            pub fn in_subquery(self, plan: QueryPlan) -> Expr {
                self.expr().in_subquery(plan)
            }

            /// Inclusive range.
            pub fn range(self, lo: impl Literal<T>, hi: impl Literal<T>) -> Expr {
                self.expr().range(lo.into_value(), hi.into_value())
            }

            /// `CAST(self AS ty)`.
            pub fn cast(self, ty: SqlType) -> Expr {
                self.expr().cast(ty)
            }
        }

        impl<M, T: TextLike> $handle<M, T> {
            /// Case-insensitive equality.
            pub fn iexact(self, v: impl Into<String>) -> Expr {
                self.expr().iexact(v.into())
            }

            /// Regex match (requires [`Feature::Regex`](crate::Feature::Regex)).
            pub fn regex(self, pattern: impl Into<String>) -> Expr {
                self.expr().regex(pattern)
            }
        }

        impl<M, T: DateLike> $handle<M, T> {
            /// Year component.
            pub fn year(self) -> Expr {
                self.expr().year()
            }
            /// Month component.
            pub fn month(self) -> Expr {
                self.expr().month()
            }
            /// Day-of-month component.
            pub fn day(self) -> Expr {
                self.expr().day()
            }
            /// ISO week number.
            pub fn week(self) -> Expr {
                self.expr().week()
            }
            /// Quarter `1..=4`.
            pub fn quarter(self) -> Expr {
                self.expr().quarter()
            }
            /// The date part of a timestamp.
            pub fn date(self) -> Expr {
                self.expr().date()
            }
        }

        impl<M, T: TimeLike> $handle<M, T> {
            /// Hour component.
            pub fn hour(self) -> Expr {
                self.expr().hour()
            }
            /// Minute component.
            pub fn minute(self) -> Expr {
                self.expr().minute()
            }
            /// Second component.
            pub fn second(self) -> Expr {
                self.expr().second()
            }
        }

        impl<M, T: Numeric> std::ops::Neg for $handle<M, T> {
            type Output = Calc<M, T>;
            fn neg(self) -> Calc<M, T> {
                Calc::new(-self.expr())
            }
        }

        typed_lookups!(@arith $handle: Add add, Sub sub, Mul mul, Div div, Rem rem);
        typed_lookups!(@compare $handle: eq, ne, lt, le, gt, ge);
        typed_lookups!(@text $handle:
            contains => Contains, false;
            icontains => Contains, true;
            starts_with => StartsWith, false;
            istarts_with => StartsWith, true;
            ends_with => EndsWith, false;
            iends_with => EndsWith, true);
    };
    (@arith $handle:ident: $($trait:ident $method:ident),*) => {$(
        impl<M, T: Numeric, R: Operand<M, T>> std::ops::$trait<R> for $handle<M, T> {
            type Output = Calc<M, T>;
            fn $method(self, rhs: R) -> Calc<M, T> {
                Calc::new(std::ops::$trait::$method(self.expr(), rhs.into_expr()))
            }
        }
    )*};
    (@compare $handle:ident: $($method:ident),*) => {
        impl<M, T> $handle<M, T> {$(
            #[doc = concat!("Typed `", stringify!($method), "` comparison.")]
            pub fn $method(self, rhs: impl Operand<M, T>) -> Expr {
                self.expr().$method(rhs.into_expr())
            }
        )*}
    };
    (@text $handle:ident: $($method:ident => $variant:ident, $ci:expr);*) => {
        impl<M, T: TextLike> $handle<M, T> {$(
            #[doc = concat!("Text lookup `", stringify!($method), "`.")]
            pub fn $method(self, needle: impl Into<String>) -> Expr {
                Expr::Lookup {
                    expr: Box::new(self.expr()),
                    lookup: super::Lookup::$variant {
                        needle: needle.into(),
                        case_insensitive: $ci,
                    },
                }
            }
        )*}
    };
}

typed_lookups!(Field);
typed_lookups!(Calc);
typed_lookups!(Joined);
