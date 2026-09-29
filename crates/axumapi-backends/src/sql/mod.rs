//! Dialect-aware SQL compilation.

mod compiler;
mod dialect;

pub use compiler::{CompiledQuery, compile, compile_write};
pub use dialect::{Dialect, Postgres, Sqlite};
