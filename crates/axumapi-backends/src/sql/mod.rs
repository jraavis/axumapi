//! Dialect-aware SQL compilation.

mod compiler;
mod dialect;

pub use compiler::{CompiledQuery, compile};
pub use dialect::{Dialect, Postgres, Sqlite};
