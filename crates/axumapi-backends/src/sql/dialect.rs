//! SQL dialect abstraction: the small set of points where SQL flavours differ.

use axumapi_orm::BackendCapabilities;
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

    /// Whether `OFFSET` requires a preceding `LIMIT`.
    fn offset_requires_limit(&self) -> bool {
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

    fn offset_requires_limit(&self) -> bool {
        true
    }
}
