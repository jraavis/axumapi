//! SQL dialect abstraction: the small set of points where SQL flavours differ.

use axumapi_orm::{BackendCapabilities, SqlType};
use std::fmt::Write;

/// Differences between SQL flavours that the compiler needs to know about.
pub trait Dialect: Send + Sync {
    /// Declared capabilities; the compiler rejects plans that exceed them.
    fn capabilities(&self) -> BackendCapabilities;

    /// Write the placeholder for the `index`-th (1-based) bind parameter.
    fn write_placeholder(&self, out: &mut String, index: usize);

    /// Write a quoted identifier. Embedded quote characters are doubled.
    fn write_ident(&self, out: &mut String, ident: &str) {
        out.push('"');
        for ch in ident.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
    }

    /// Type name used in `CAST(.. AS <name>)`.
    fn cast_type(&self, ty: SqlType) -> &'static str;

    /// Whether `OFFSET` requires a preceding `LIMIT`.
    fn offset_requires_limit(&self) -> bool {
        false
    }

    /// The `LIMIT` written before a bare `OFFSET` when
    /// [`offset_requires_limit`](Self::offset_requires_limit) is set.
    fn unbounded_limit(&self) -> &'static str {
        "-1"
    }

    /// The SQL string literal used as the `LIKE .. ESCAPE` character (one
    /// backslash).
    fn like_escape_literal(&self) -> &'static str {
        r"'\'"
    }

    /// The tail of an `INSERT INTO t` that stores one row of defaults.
    fn default_values_sql(&self) -> &'static str {
        " DEFAULT VALUES"
    }

    /// Whether aggregates accept `FILTER (WHERE ..)`. When not, the compiler
    /// rewrites `AGG(x) FILTER (WHERE f)` as `AGG(CASE WHEN f THEN x END)`.
    fn supports_aggregate_filter(&self) -> bool {
        true
    }

    /// Whether `UPDATE .. SET a = .., b = ..` evaluates the assignments left
    /// to right, so a later right-hand side sees the *new* value of an
    /// earlier column (standard SQL reads the old values). The compiler then
    /// reorders assignments to keep the standard meaning.
    fn evaluates_assignments_in_order(&self) -> bool {
        false
    }
}

/// PostgreSQL dialect (`$1` placeholders, `ILIKE`, `~` regex).
#[derive(Debug, Clone, Copy, Default)]
pub struct Postgres;

impl Dialect for Postgres {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::postgres()
    }

    fn write_placeholder(&self, out: &mut String, index: usize) {
        // Writing to a String cannot fail.
        let _ = write!(out, "${index}");
    }

    fn cast_type(&self, ty: SqlType) -> &'static str {
        match ty {
            SqlType::SmallInt => "SMALLINT",
            SqlType::Integer => "INTEGER",
            SqlType::BigInt | SqlType::Duration => "BIGINT",
            SqlType::Real => "REAL",
            SqlType::Double => "DOUBLE PRECISION",
            SqlType::Decimal => "NUMERIC",
            SqlType::Bool => "BOOLEAN",
            SqlType::Binary => "BYTEA",
            SqlType::Date => "DATE",
            SqlType::Time => "TIME",
            SqlType::Timestamp => "TIMESTAMPTZ",
            SqlType::Uuid => "UUID",
            SqlType::Json => "JSONB",
            _ => "TEXT",
        }
    }
}

/// SQLite dialect (`?` placeholders, `LIMIT -1` before bare `OFFSET`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Sqlite;

impl Dialect for Sqlite {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::sqlite()
    }

    fn write_placeholder(&self, out: &mut String, _index: usize) {
        out.push('?');
    }

    fn cast_type(&self, ty: SqlType) -> &'static str {
        match ty {
            SqlType::SmallInt
            | SqlType::Integer
            | SqlType::BigInt
            | SqlType::Duration
            | SqlType::Bool => "INTEGER",
            SqlType::Real | SqlType::Double => "REAL",
            SqlType::Decimal => "NUMERIC",
            SqlType::Binary => "BLOB",
            _ => "TEXT",
        }
    }

    fn offset_requires_limit(&self) -> bool {
        true
    }
}

/// MySQL 8.0.31+ dialect: backtick identifiers, `?` placeholders,
/// `REGEXP_LIKE`, `GROUP_CONCAT`, and no `RETURNING`.
///
/// The dialect declares `returning: false`, so compiling a plan that asks for
/// `RETURNING` is a capability error; the MySQL adapter emulates it by
/// stripping the clause and re-reading the rows.
#[derive(Debug, Clone, Copy, Default)]
pub struct MySql;

impl Dialect for MySql {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            returning: false,
            ..BackendCapabilities::mysql()
        }
    }

    fn write_placeholder(&self, out: &mut String, _index: usize) {
        out.push('?');
    }

    fn write_ident(&self, out: &mut String, ident: &str) {
        out.push('`');
        for ch in ident.chars() {
            if ch == '`' {
                out.push('`');
            }
            out.push(ch);
        }
        out.push('`');
    }

    fn cast_type(&self, ty: SqlType) -> &'static str {
        match ty {
            SqlType::SmallInt
            | SqlType::Integer
            | SqlType::BigInt
            | SqlType::Duration
            | SqlType::Bool => "SIGNED",
            SqlType::Real => "FLOAT",
            SqlType::Double => "DOUBLE",
            // A bare DECIMAL is DECIMAL(10,0) and would round to an integer.
            SqlType::Decimal => "DECIMAL(38,10)",
            SqlType::Binary => "BINARY",
            SqlType::Date => "DATE",
            SqlType::Time => "TIME(6)",
            SqlType::Timestamp => "DATETIME(6)",
            SqlType::Uuid => "CHAR(36)",
            SqlType::Json => "JSON",
            _ => "CHAR",
        }
    }

    fn offset_requires_limit(&self) -> bool {
        true
    }

    fn unbounded_limit(&self) -> &'static str {
        "18446744073709551615"
    }

    fn like_escape_literal(&self) -> &'static str {
        // In a MySQL string literal `\\` is one backslash; a lone `\'` would
        // escape the closing quote.
        r"'\\'"
    }

    fn default_values_sql(&self) -> &'static str {
        " () VALUES ()"
    }

    fn supports_aggregate_filter(&self) -> bool {
        false
    }

    fn evaluates_assignments_in_order(&self) -> bool {
        true
    }
}
