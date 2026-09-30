//! Aggregate functions: `Count::all()`, `Sum::of(Book::price)`, ...
//!
//! Attach them to a queryset with `annotate("total", Sum::of(..))` (one value
//! per group or row) or `aggregate([("total", Sum::of(..).into())])` (one row
//! for the whole queryset).

use super::Expr;

/// The aggregate function computed.
#[derive(Debug, Clone, PartialEq)]
pub enum AggFunc {
    /// `COUNT`.
    Count,
    /// `SUM`.
    Sum,
    /// `AVG`.
    Avg,
    /// `MIN`.
    Min,
    /// `MAX`.
    Max,
    /// Standard deviation (needs [`Feature::StatisticalAggregates`](crate::Feature::StatisticalAggregates)).
    StdDev {
        /// Sample (`true`) or population (`false`) deviation.
        sample: bool,
    },
    /// Variance (needs [`Feature::StatisticalAggregates`](crate::Feature::StatisticalAggregates)).
    Variance {
        /// Sample (`true`) or population (`false`) variance.
        sample: bool,
    },
    /// Collect values into an array (needs [`Feature::Arrays`](crate::Feature::Arrays)).
    ArrayAgg,
    /// Join text values with a separator (`STRING_AGG` / `group_concat`).
    StringAgg {
        /// Separator, bound as a parameter.
        separator: String,
    },
}

/// An aggregate call: function, argument, `DISTINCT` and `FILTER (WHERE ..)`.
#[derive(Debug, Clone, PartialEq)]
pub struct Aggregate {
    /// Function.
    pub func: AggFunc,
    /// Argument; `None` means `COUNT(*)`.
    pub arg: Option<Box<Expr>>,
    /// Aggregate over distinct values only.
    pub distinct: bool,
    /// Only rows matching this predicate contribute.
    pub filter: Option<Box<Expr>>,
}

impl Aggregate {
    fn new(func: AggFunc, arg: Option<Expr>) -> Self {
        Self {
            func,
            arg: arg.map(Box::new),
            distinct: false,
            filter: None,
        }
    }

    /// Aggregate distinct values only (`COUNT(DISTINCT x)`).
    #[must_use]
    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    /// Restrict the rows that contribute (`FILTER (WHERE ..)`).
    #[must_use]
    pub fn filter(mut self, predicate: Expr) -> Self {
        self.filter = Some(Box::new(match self.filter.take() {
            Some(existing) => existing.and(predicate),
            None => predicate,
        }));
        self
    }

    /// The argument and filter expressions.
    pub(super) fn children(&self) -> Vec<&Expr> {
        self.arg
            .as_deref()
            .into_iter()
            .chain(self.filter.as_deref())
            .collect()
    }

    /// Mutable counterpart of [`children`](Self::children).
    pub(super) fn children_mut(&mut self) -> Vec<&mut Expr> {
        self.arg
            .as_deref_mut()
            .into_iter()
            .chain(self.filter.as_deref_mut())
            .collect()
    }
}

impl From<Aggregate> for Expr {
    fn from(agg: Aggregate) -> Self {
        Expr::Aggregate(agg)
    }
}

macro_rules! aggregate_of {
    ($($(#[$doc:meta])* $name:ident => $func:expr;)*) => {$(
        $(#[$doc])*
        #[derive(Debug, Clone, Copy)]
        pub struct $name;

        impl $name {
            /// Aggregate over `expr`.
            pub fn of(expr: impl Into<Expr>) -> Aggregate {
                Aggregate::new($func, Some(expr.into()))
            }
        }
    )*};
}

aggregate_of! {
    /// `SUM(expr)`. `NULL` for an empty group; PostgreSQL returns `numeric` for integer sums.
    Sum => AggFunc::Sum;
    /// `AVG(expr)`.
    Avg => AggFunc::Avg;
    /// `MIN(expr)`.
    Min => AggFunc::Min;
    /// `MAX(expr)`.
    Max => AggFunc::Max;
    /// Collect values into an array (PostgreSQL only).
    ArrayAgg => AggFunc::ArrayAgg;
}

/// `COUNT(*)` and `COUNT(expr)`.
#[derive(Debug, Clone, Copy)]
pub struct Count;

impl Count {
    /// `COUNT(*)`: every row of the group.
    pub fn all() -> Aggregate {
        Aggregate::new(AggFunc::Count, None)
    }

    /// `COUNT(expr)`: rows where `expr` is not `NULL`.
    pub fn of(expr: impl Into<Expr>) -> Aggregate {
        Aggregate::new(AggFunc::Count, Some(expr.into()))
    }
}

/// Standard deviation (PostgreSQL only).
#[derive(Debug, Clone, Copy)]
pub struct StdDev;

impl StdDev {
    /// Population standard deviation.
    pub fn population(expr: impl Into<Expr>) -> Aggregate {
        Aggregate::new(AggFunc::StdDev { sample: false }, Some(expr.into()))
    }

    /// Sample standard deviation.
    pub fn sample(expr: impl Into<Expr>) -> Aggregate {
        Aggregate::new(AggFunc::StdDev { sample: true }, Some(expr.into()))
    }
}

/// Variance (PostgreSQL only).
#[derive(Debug, Clone, Copy)]
pub struct Variance;

impl Variance {
    /// Population variance.
    pub fn population(expr: impl Into<Expr>) -> Aggregate {
        Aggregate::new(AggFunc::Variance { sample: false }, Some(expr.into()))
    }

    /// Sample variance.
    pub fn sample(expr: impl Into<Expr>) -> Aggregate {
        Aggregate::new(AggFunc::Variance { sample: true }, Some(expr.into()))
    }
}

/// Join text values with a separator (`STRING_AGG` on PostgreSQL, `group_concat` on SQLite).
#[derive(Debug, Clone, Copy)]
pub struct StringAgg;

impl StringAgg {
    /// Concatenate `expr` (text) separated by `separator`.
    pub fn of(expr: impl Into<Expr>, separator: impl Into<String>) -> Aggregate {
        Aggregate::new(
            AggFunc::StringAgg {
                separator: separator.into(),
            },
            Some(expr.into()),
        )
    }
}
