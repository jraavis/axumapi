//! `axumapi-backends`: query compilers and database adapters.
//!
//! * [`sql`] compiles a [`QueryPlan`](axumapi_orm::QueryPlan) into
//!   parameterised SQL for a [`Dialect`](sql::Dialect). Values are **always**
//!   bound, never interpolated.
//! * [`sqlite`] (feature `sqlite`, default) executes plans with SQLx.
//! * [`postgres`] (feature `postgres`) executes plans with SQLx.
//!
//! MySQL, MongoDB and Redis adapters are later phases.
#![forbid(unsafe_code)]

#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
mod shared;
pub mod sql;
#[cfg(feature = "sqlite")]
pub mod sqlite;
