//! `axumapi-orm`: backend-neutral ORM core.
//!
//! This crate owns the [`QueryPlan`] intermediate representation, the typed
//! expression AST, [`BackendCapabilities`], the [`Backend`] trait and the ORM
//! error hierarchy. It has **no HTTP dependencies**; the HTTP layer converts
//! [`OrmError`] into API errors.
//!
//! Phase 1 ships the IR and capability model only. `Model`, `Manager` and
//! `QuerySet` are built on top of it in Phase 4.
#![forbid(unsafe_code)]

pub mod backend;
pub mod capabilities;
pub mod error;
pub mod expr;
pub mod plan;
pub mod value;

pub use backend::{Backend, QueryResult, Row};
pub use capabilities::{BackendCapabilities, BackendKind, Feature, RowLocking, TransactionSupport};
pub use error::{BackendCapabilityError, BackendError, OrmError, QueryError};
pub use expr::{BinaryOp, Column, Expr, Lookup, UnaryOp};
pub use plan::{
    DistinctMode, JoinExpr, JoinKind, LockMode, OrderDirection, OrderExpr, QueryPlan, QuerySource,
    SelectExpr,
};
pub use value::Value;
