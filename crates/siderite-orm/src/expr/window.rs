//! Window functions: `RowNumber::new().partition_by([..]).order_by([..])`.
//!
//! Windows need [`Feature::WindowFunctions`](crate::Feature::WindowFunctions).
//! Window frames are not modelled: every function uses the database default
//! frame (`RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW` when an
//! ordering is given), so `LastValue` returns the last *peer* of the current
//! row, exactly as raw SQL would.

// `RowNumber::new()` and friends are constructors of the `Window` they name.
#![allow(clippy::new_ret_no_self)]

use super::{Aggregate, Expr};
use crate::plan::OrderExpr;

/// The function evaluated over the window.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowFunc {
    /// `ROW_NUMBER()`.
    RowNumber,
    /// `RANK()`.
    Rank,
    /// `DENSE_RANK()`.
    DenseRank,
    /// `PERCENT_RANK()`.
    PercentRank,
    /// `CUME_DIST()`.
    CumeDist,
    /// `NTILE(n)`.
    Ntile(u32),
    /// `LAG(expr, offset [, default])`.
    Lag {
        /// Value read from a preceding row.
        expr: Box<Expr>,
        /// Rows to look back.
        offset: u32,
        /// Value when there is no such row.
        default: Option<Box<Expr>>,
    },
    /// `LEAD(expr, offset [, default])`.
    Lead {
        /// Value read from a following row.
        expr: Box<Expr>,
        /// Rows to look ahead.
        offset: u32,
        /// Value when there is no such row.
        default: Option<Box<Expr>>,
    },
    /// `FIRST_VALUE(expr)`.
    FirstValue(Box<Expr>),
    /// `LAST_VALUE(expr)`.
    LastValue(Box<Expr>),
    /// An aggregate evaluated over the window (`SUM(x) OVER (..)`, running totals).
    Aggregate(Aggregate),
}

/// A window function with its `PARTITION BY` and `ORDER BY`.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// Function.
    pub func: WindowFunc,
    /// `PARTITION BY` expressions.
    pub partition_by: Vec<Expr>,
    /// `ORDER BY` terms inside the window.
    pub order_by: Vec<OrderExpr>,
}

impl Window {
    /// Window evaluating `func` over the whole result (no partition or ordering yet).
    pub fn over(func: WindowFunc) -> Self {
        Self {
            func,
            partition_by: Vec::new(),
            order_by: Vec::new(),
        }
    }

    /// Restart the function for every distinct value of `exprs`.
    #[must_use]
    pub fn partition_by<E: Into<Expr>>(mut self, exprs: impl IntoIterator<Item = E>) -> Self {
        self.partition_by = exprs.into_iter().map(Into::into).collect();
        self
    }

    /// Order rows inside each partition.
    #[must_use]
    pub fn order_by(mut self, terms: impl IntoIterator<Item = OrderExpr>) -> Self {
        self.order_by = terms.into_iter().collect();
        self
    }

    /// Every expression inside the window: function arguments, partition keys
    /// and ordering terms.
    pub(super) fn children(&self) -> Vec<&Expr> {
        let func: Vec<&Expr> = match &self.func {
            WindowFunc::Lag { expr, default, .. } | WindowFunc::Lead { expr, default, .. } => {
                std::iter::once(&**expr).chain(default.as_deref()).collect()
            }
            WindowFunc::FirstValue(expr) | WindowFunc::LastValue(expr) => vec![expr],
            WindowFunc::Aggregate(agg) => agg.children(),
            _ => Vec::new(),
        };
        func.into_iter()
            .chain(&self.partition_by)
            .chain(self.order_by.iter().map(|o| &o.expr))
            .collect()
    }

    /// Mutable counterpart of [`children`](Self::children).
    pub(super) fn children_mut(&mut self) -> Vec<&mut Expr> {
        let func: Vec<&mut Expr> = match &mut self.func {
            WindowFunc::Lag { expr, default, .. } | WindowFunc::Lead { expr, default, .. } => {
                std::iter::once(&mut **expr)
                    .chain(default.as_deref_mut())
                    .collect()
            }
            WindowFunc::FirstValue(expr) | WindowFunc::LastValue(expr) => vec![expr],
            WindowFunc::Aggregate(agg) => agg.children_mut(),
            _ => Vec::new(),
        };
        func.into_iter()
            .chain(&mut self.partition_by)
            .chain(self.order_by.iter_mut().map(|o| &mut o.expr))
            .collect()
    }
}

impl From<Window> for Expr {
    fn from(window: Window) -> Self {
        Expr::Window(Box::new(window))
    }
}

macro_rules! window_fn {
    ($($(#[$doc:meta])* $name:ident => $func:ident;)*) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, Copy)]
        pub struct $name;

        impl $name {
            /// The window function; add `partition_by` / `order_by` on the result.
            pub fn new() -> Window {
                Window::over(WindowFunc::$func)
            }
        }
    )*};
}

window_fn! {
    /// `ROW_NUMBER()`.
    RowNumber => RowNumber;
    /// `RANK()`.
    Rank => Rank;
    /// `DENSE_RANK()`.
    DenseRank => DenseRank;
    /// `PERCENT_RANK()`.
    PercentRank => PercentRank;
    /// `CUME_DIST()`.
    CumeDist => CumeDist;
}

/// `NTILE(n)`.
#[derive(Debug, Clone, Copy)]
pub struct Ntile;

impl Ntile {
    /// Split each partition into `buckets` groups.
    pub fn new(buckets: u32) -> Window {
        Window::over(WindowFunc::Ntile(buckets))
    }
}

macro_rules! shift_fn {
    ($($(#[$doc:meta])* $name:ident => $func:ident;)*) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, Copy)]
        pub struct $name;

        impl $name {
            /// `expr` read `offset` rows away; `NULL` past the partition edge.
            pub fn new(expr: impl Into<Expr>, offset: u32) -> Window {
                Window::over(WindowFunc::$func {
                    expr: Box::new(expr.into()),
                    offset,
                    default: None,
                })
            }

            /// Like [`new`](Self::new) with `default` past the partition edge.
            pub fn or_default(
                expr: impl Into<Expr>,
                offset: u32,
                default: impl Into<Expr>,
            ) -> Window {
                Window::over(WindowFunc::$func {
                    expr: Box::new(expr.into()),
                    offset,
                    default: Some(Box::new(default.into())),
                })
            }
        }
    )*};
}

shift_fn! {
    /// `LAG(expr, offset)`.
    Lag => Lag;
    /// `LEAD(expr, offset)`.
    Lead => Lead;
}

macro_rules! value_fn {
    ($($(#[$doc:meta])* $name:ident => $func:ident;)*) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, Copy)]
        pub struct $name;

        impl $name {
            /// The window function over `expr`.
            pub fn new(expr: impl Into<Expr>) -> Window {
                Window::over(WindowFunc::$func(Box::new(expr.into())))
            }
        }
    )*};
}

value_fn! {
    /// `FIRST_VALUE(expr)`.
    FirstValue => FirstValue;
    /// `LAST_VALUE(expr)`.
    LastValue => LastValue;
}
