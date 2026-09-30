//! Functions, `CASE`, aggregates, window functions and date parts.

use super::Compiler;
use siderite_orm::expr::functions::{DatePart, Function};
use siderite_orm::{
    AggFunc, Aggregate, BackendKind, Expr, OrderDirection, SqlType, Window, WindowFunc,
};
use std::fmt::Write;

impl Compiler<'_> {
    /// Scalar function call; arity is validated here because `Expr::Func`
    /// carries an untyped argument list.
    pub(super) fn function(&mut self, func: Function, args: &[Expr]) {
        let (min, max) = match func {
            Function::Lower | Function::Upper | Function::Length | Function::Trim => (1, 1),
            Function::Replace => (3, 3),
            Function::Substr => (2, 3),
            Function::Coalesce | Function::Concat => (1, usize::MAX),
        };
        if args.len() < min || args.len() > max {
            self.fail(format!("{func:?} called with {} arguments", args.len()));
            self.push("NULL");
            return;
        }
        match func {
            Function::Lower => self.call("LOWER", args),
            Function::Upper => self.call("UPPER", args),
            // MySQL's LENGTH counts bytes; CHAR_LENGTH counts characters.
            Function::Length if self.kind == BackendKind::MySql => self.call("CHAR_LENGTH", args),
            Function::Length => self.call("LENGTH", args),
            Function::Trim => self.call("TRIM", args),
            Function::Replace => self.call("REPLACE", args),
            Function::Coalesce => self.call("COALESCE", args),
            Function::Concat if self.kind == BackendKind::Sqlite => self.sqlite_concat(args),
            Function::Concat if self.kind == BackendKind::MySql => self.mysql_concat(args),
            Function::Concat => self.call("CONCAT", args),
            Function::Substr => {
                // Positions are bound as 64-bit integers; PostgreSQL's
                // `substr` takes `integer`.
                let integer = self.dialect.cast_type(SqlType::Integer);
                self.push("SUBSTR(");
                self.expr(&args[0]);
                for position in &args[1..] {
                    self.push(", CAST(");
                    self.expr(position);
                    let _ = write!(self.sql, " AS {integer})");
                }
                self.push(")");
            }
        }
    }

    fn call(&mut self, name: &str, args: &[Expr]) {
        self.push(name);
        self.push("(");
        self.list(args, ", ", Self::expr);
        self.push(")");
    }

    /// SQLite has no `CONCAT` before 3.44: use `||` with `NULL` as empty text,
    /// matching PostgreSQL's `CONCAT`.
    fn sqlite_concat(&mut self, args: &[Expr]) {
        self.push("(");
        self.list(args, " || ", |c, arg| {
            c.push("COALESCE(CAST(");
            c.expr(arg);
            c.push(" AS TEXT), '')");
        });
        self.push(")");
    }

    /// MySQL's `CONCAT` returns `NULL` if any argument is `NULL`; the contract
    /// is `NULL` as empty text, as in PostgreSQL.
    fn mysql_concat(&mut self, args: &[Expr]) {
        self.push("CONCAT(");
        self.list(args, ", ", |c, arg| {
            c.push("COALESCE(");
            c.expr(arg);
            c.push(", '')");
        });
        self.push(")");
    }

    pub(super) fn case(&mut self, branches: &[(Expr, Expr)], otherwise: Option<&Expr>) {
        if branches.is_empty() {
            self.fail("CASE without branches");
        }
        self.push("CASE");
        for (when, then) in branches {
            self.push(" WHEN ");
            self.expr(when);
            self.push(" THEN ");
            self.expr(then);
        }
        if let Some(fallback) = otherwise {
            self.push(" ELSE ");
            self.expr(fallback);
        }
        self.push(" END");
    }

    pub(super) fn aggregate(&mut self, agg: &Aggregate) {
        let sqlite = self.kind == BackendKind::Sqlite;
        let mysql = self.kind == BackendKind::MySql;
        let name = match &agg.func {
            AggFunc::Count => "COUNT",
            AggFunc::Sum => "SUM",
            AggFunc::Avg => "AVG",
            AggFunc::Min => "MIN",
            AggFunc::Max => "MAX",
            AggFunc::StdDev { sample: false } => "STDDEV_POP",
            AggFunc::StdDev { sample: true } => "STDDEV_SAMP",
            AggFunc::Variance { sample: false } => "VAR_POP",
            AggFunc::Variance { sample: true } => "VAR_SAMP",
            AggFunc::ArrayAgg => "ARRAY_AGG",
            AggFunc::StringAgg { .. } if sqlite || mysql => "GROUP_CONCAT",
            AggFunc::StringAgg { .. } => "STRING_AGG",
        };
        // Without `FILTER (WHERE ..)`, the filter moves into the argument:
        // aggregates skip the NULLs that a filtered-out `CASE` yields.
        let inline_filter = if self.dialect.supports_aggregate_filter() {
            None
        } else {
            agg.filter.as_deref()
        };
        self.push(name);
        self.push("(");
        match agg.arg.as_deref() {
            None if agg.func == AggFunc::Count && !agg.distinct => match inline_filter {
                Some(filter) => {
                    self.push("CASE WHEN ");
                    self.expr(filter);
                    self.push(" THEN 1 END");
                }
                None => self.push("*"),
            },
            None => self.fail(format!("{name} needs an argument")),
            Some(arg) => {
                if agg.distinct {
                    self.push("DISTINCT ");
                }
                match inline_filter {
                    Some(filter) => {
                        self.push("CASE WHEN ");
                        self.expr(filter);
                        self.push(" THEN ");
                        self.expr(arg);
                        self.push(" END");
                    }
                    None => self.expr(arg),
                }
            }
        }
        if let AggFunc::StringAgg { separator } = &agg.func {
            if sqlite && agg.distinct {
                self.fail("SQLite cannot combine DISTINCT with a separator");
            }
            if mysql {
                // `SEPARATOR` takes a literal, not a bind parameter.
                self.push(" SEPARATOR ");
                self.mysql_string_literal(separator);
            } else {
                self.push(", ");
                self.bind(separator.clone().into());
            }
        }
        self.push(")");
        if let (Some(filter), None) = (agg.filter.as_deref(), inline_filter) {
            self.push(" FILTER (WHERE ");
            self.expr(filter);
            self.push(")");
        }
    }

    /// A MySQL string literal for text known at compile time (never user
    /// input at run time). Backslashes and quotes are doubled; the adapter
    /// keeps `NO_BACKSLASH_ESCAPES` off so `\\` means one backslash.
    fn mysql_string_literal(&mut self, text: &str) {
        self.push("'");
        for ch in text.chars() {
            if matches!(ch, '\'' | '\\') {
                self.sql.push(ch);
            }
            self.sql.push(ch);
        }
        self.push("'");
    }

    pub(super) fn window(&mut self, w: &Window) {
        match &w.func {
            WindowFunc::RowNumber => self.push("ROW_NUMBER()"),
            WindowFunc::Rank => self.push("RANK()"),
            WindowFunc::DenseRank => self.push("DENSE_RANK()"),
            WindowFunc::PercentRank => self.push("PERCENT_RANK()"),
            WindowFunc::CumeDist => self.push("CUME_DIST()"),
            WindowFunc::Ntile(buckets) => {
                // A literal: PostgreSQL's `ntile` takes `integer`, not a 64-bit bind.
                let _ = write!(self.sql, "NTILE({buckets})");
            }
            WindowFunc::Lag {
                expr,
                offset,
                default,
            } => self.shift("LAG", expr, *offset, default.as_deref()),
            WindowFunc::Lead {
                expr,
                offset,
                default,
            } => self.shift("LEAD", expr, *offset, default.as_deref()),
            WindowFunc::FirstValue(expr) => self.call("FIRST_VALUE", std::slice::from_ref(expr)),
            WindowFunc::LastValue(expr) => self.call("LAST_VALUE", std::slice::from_ref(expr)),
            WindowFunc::Aggregate(agg) => self.aggregate(agg),
        }
        self.push(" OVER (");
        if !w.partition_by.is_empty() {
            self.push("PARTITION BY ");
            self.list(&w.partition_by, ", ", Self::expr);
        }
        if !w.order_by.is_empty() {
            if !w.partition_by.is_empty() {
                self.push(" ");
            }
            self.push("ORDER BY ");
            self.list(&w.order_by, ", ", |c, o| {
                c.expr(&o.expr);
                c.push(match o.direction {
                    OrderDirection::Asc => " ASC",
                    OrderDirection::Desc => " DESC",
                });
            });
        }
        self.push(")");
    }

    fn shift(&mut self, name: &str, expr: &Expr, offset: u32, default: Option<&Expr>) {
        self.push(name);
        self.push("(");
        self.expr(expr);
        let _ = write!(self.sql, ", {offset}");
        if let Some(default) = default {
            self.push(", ");
            self.expr(default);
        }
        self.push(")");
    }

    pub(super) fn date_part(&mut self, part: DatePart, expr: &Expr) {
        match self.kind {
            BackendKind::Sqlite => self.sqlite_date_part(part, expr),
            BackendKind::MySql => self.mysql_date_part(part, expr),
            _ => self.extract_date_part(part, expr),
        }
    }

    /// MySQL's extraction functions; timestamps are read in the session time
    /// zone, which the adapter pins to UTC. `WEEK(x, 3)` is the ISO week.
    fn mysql_date_part(&mut self, part: DatePart, expr: &Expr) {
        let function = match part {
            DatePart::Date => "DATE",
            DatePart::Year => "YEAR",
            DatePart::Month => "MONTH",
            DatePart::Day => "DAY",
            DatePart::Week => "WEEK",
            DatePart::Quarter => "QUARTER",
            DatePart::Hour => "HOUR",
            DatePart::Minute => "MINUTE",
            DatePart::Second => "SECOND",
        };
        self.push(function);
        self.push("(");
        self.expr(expr);
        if part == DatePart::Week {
            self.push(", 3");
        }
        self.push(")");
    }

    /// `EXTRACT` yields `numeric` on PostgreSQL 14+; cast so it decodes as an
    /// integer. Timestamps are read in the session time zone, which the
    /// PostgreSQL backend pins to UTC.
    fn extract_date_part(&mut self, part: DatePart, expr: &Expr) {
        let field = match part {
            DatePart::Date => {
                self.push("CAST(");
                self.expr(expr);
                self.push(" AS DATE)");
                return;
            }
            DatePart::Year => "YEAR",
            DatePart::Month => "MONTH",
            DatePart::Day => "DAY",
            DatePart::Week => "WEEK",
            DatePart::Quarter => "QUARTER",
            DatePart::Hour => "HOUR",
            DatePart::Minute => "MINUTE",
            DatePart::Second => "SECOND",
        };
        self.push("CAST(");
        if part == DatePart::Second {
            self.push("FLOOR(");
        }
        let _ = write!(self.sql, "EXTRACT({field} FROM ");
        self.expr(expr);
        self.push(")");
        if part == DatePart::Second {
            self.push(")");
        }
        self.push(" AS BIGINT)");
    }

    /// `strftime` on the canonical text forms; it returns text, so cast.
    fn sqlite_date_part(&mut self, part: DatePart, expr: &Expr) {
        let format = match part {
            DatePart::Date => {
                self.push("date(");
                self.expr(expr);
                self.push(")");
                return;
            }
            DatePart::Week => {
                // ISO week: the week of the Thursday of the current week.
                self.push("((CAST(strftime('%j', date(");
                self.expr(expr);
                self.push(", '-3 days', 'weekday 4')) AS INTEGER) - 1) / 7 + 1)");
                return;
            }
            DatePart::Quarter => {
                self.push("((CAST(strftime('%m', ");
                self.expr(expr);
                self.push(") AS INTEGER) + 2) / 3)");
                return;
            }
            DatePart::Year => "%Y",
            DatePart::Month => "%m",
            DatePart::Day => "%d",
            DatePart::Hour => "%H",
            DatePart::Minute => "%M",
            DatePart::Second => "%S",
        };
        let _ = write!(self.sql, "CAST(strftime('{format}', ");
        self.expr(expr);
        self.push(") AS INTEGER)");
    }
}
