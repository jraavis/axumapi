//! Dialect-aware SQL compilation.

mod compiler;
mod dialect;

#[cfg(feature = "mysql")]
pub(crate) use compiler::compile_write_bare;
pub use compiler::{CompiledQuery, compile, compile_write};
pub use dialect::{Dialect, MySql, Postgres, Sqlite};
