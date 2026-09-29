//! `axumapi-backends`: query compilers and database adapters.
//!
//! * [`sql`] compiles a [`QueryPlan`](axumapi_orm::QueryPlan) into
//!   parameterised SQL for a [`Dialect`](sql::Dialect). Values are **always**
//!   bound, never interpolated.
//! * [`sqlite`] (feature `sqlite`, default) executes plans with SQLx.
//!
//! PostgreSQL execution, MySQL, MongoDB and Redis adapters are later phases;
//! the PostgreSQL *dialect* is available now so compiled SQL can be tested.
#![forbid(unsafe_code)]

pub mod sql;
#[cfg(feature = "sqlite")]
pub mod sqlite;
