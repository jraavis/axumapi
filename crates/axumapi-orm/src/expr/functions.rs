//! Scalar functions, `CASE` and date-part extraction.
//!
//! ```ignore
//! use axumapi_orm::functions::*;
//! User::objects(&db)
//!     .filter(lower(User::name.expr()).eq("ann"))
//!     .annotate("label", coalesce([User::nick.expr(), User::name.expr()]))
//! ```

use super::Expr;
use crate::types::SqlType;

/// Scalar functions with the same meaning on every relational backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    /// `LOWER(text)`.
    Lower,
    /// `UPPER(text)`.
    Upper,
    /// Length in characters.
    Length,
    /// First non-`NULL` argument.
    Coalesce,
    /// Concatenation; `NULL` arguments count as empty text.
    Concat,
    /// `SUBSTR(text, start [, length])` with a 1-based start.
    Substr,
    /// `REPLACE(text, from, to)`.
    Replace,
    /// Remove leading and trailing spaces.
    Trim,
}

/// Component extracted by [`Expr::date_part`].
///
/// `Week` is the ISO 8601 week number on every backend. On SQLite the source
/// must use the canonical text forms documented in [`crate::types`]; on
/// PostgreSQL timestamps are read in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatePart {
    /// Calendar year.
    Year,
    /// Month `1..=12`.
    Month,
    /// Day of month.
    Day,
    /// ISO 8601 week `1..=53`.
    Week,
    /// Quarter `1..=4`.
    Quarter,
    /// Hour `0..=23`.
    Hour,
    /// Minute `0..=59`.
    Minute,
    /// Second `0..=59` (fractions are dropped).
    Second,
    /// The date, truncating a timestamp.
    Date,
}

fn call(func: Function, args: impl IntoIterator<Item = Expr>) -> Expr {
    Expr::Func {
        func,
        args: args.into_iter().collect(),
    }
}

/// `LOWER(expr)`.
pub fn lower(expr: impl Into<Expr>) -> Expr {
    call(Function::Lower, [expr.into()])
}

/// `UPPER(expr)`.
pub fn upper(expr: impl Into<Expr>) -> Expr {
    call(Function::Upper, [expr.into()])
}

/// Length of `expr` in characters.
pub fn length(expr: impl Into<Expr>) -> Expr {
    call(Function::Length, [expr.into()])
}

/// First non-`NULL` of `exprs`.
pub fn coalesce<E: Into<Expr>>(exprs: impl IntoIterator<Item = E>) -> Expr {
    call(Function::Coalesce, exprs.into_iter().map(Into::into))
}

/// Concatenate text expressions (`NULL` counts as empty text).
pub fn concat<E: Into<Expr>>(exprs: impl IntoIterator<Item = E>) -> Expr {
    call(Function::Concat, exprs.into_iter().map(Into::into))
}

/// `SUBSTR(expr, start, length)` with a 1-based `start`.
pub fn substr(expr: impl Into<Expr>, start: i64, length: Option<i64>) -> Expr {
    let mut args = vec![expr.into(), Expr::val(start)];
    args.extend(length.map(Expr::val));
    call(Function::Substr, args)
}

/// Replace every `from` in `expr` by `to`.
pub fn replace(expr: impl Into<Expr>, from: impl Into<String>, to: impl Into<String>) -> Expr {
    call(
        Function::Replace,
        [expr.into(), Expr::val(from.into()), Expr::val(to.into())],
    )
}

/// Strip leading and trailing spaces.
pub fn trim(expr: impl Into<Expr>) -> Expr {
    call(Function::Trim, [expr.into()])
}

/// `CAST(expr AS ty)`.
pub fn cast(expr: impl Into<Expr>, ty: SqlType) -> Expr {
    expr.into().cast(ty)
}

/// Start a searched `CASE`: `case().when(cond, then).otherwise(fallback)`.
pub fn case() -> CaseBuilder {
    CaseBuilder::default()
}

/// Builder for [`Expr::Case`].
#[derive(Debug, Clone, Default)]
pub struct CaseBuilder {
    branches: Vec<(Expr, Expr)>,
}

impl CaseBuilder {
    /// Add a `WHEN condition THEN result` branch.
    #[must_use]
    pub fn when(mut self, condition: Expr, result: impl Into<Expr>) -> Self {
        self.branches.push((condition, result.into()));
        self
    }

    /// Finish with an `ELSE` result.
    pub fn otherwise(self, fallback: impl Into<Expr>) -> Expr {
        Expr::Case {
            branches: self.branches,
            otherwise: Some(Box::new(fallback.into())),
        }
    }

    /// Finish without `ELSE`: unmatched rows yield `NULL`.
    pub fn end(self) -> Expr {
        Expr::Case {
            branches: self.branches,
            otherwise: None,
        }
    }
}

macro_rules! date_parts {
    ($($method:ident => $part:ident),* $(,)?) => {
        impl Expr {$(
            #[doc = concat!("The `", stringify!($part), "` component of a temporal expression.")]
            pub fn $method(self) -> Expr {
                self.date_part(DatePart::$part)
            }
        )*}
    };
}
date_parts!(
    year => Year, month => Month, day => Day, week => Week, quarter => Quarter,
    hour => Hour, minute => Minute, second => Second, date => Date,
);
