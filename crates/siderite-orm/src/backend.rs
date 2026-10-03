//! Executor, backend and transaction traits implemented by database adapters.
//!
//! Application code does not use these traits directly; it goes through the
//! [`Db`](crate::Db) handle, which wraps either a backend (pool) or an open
//! transaction behind the same API.

use crate::capabilities::{BackendCapabilities, IsolationLevel};
use crate::error::{OrmError, QueryError};
use crate::plan::QueryPlan;
use crate::value::Value;
use crate::write::WritePlan;
use async_trait::async_trait;

/// One decoded result row: column names paired with backend-neutral values.
///
/// Dynamic projections and annotations decode into `Row`; models decode from
/// it with [`Model::from_row`](crate::Model::from_row).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    columns: Vec<(String, Value)>,
}

impl Row {
    /// Build a row from `(column, value)` pairs.
    pub fn new(columns: Vec<(String, Value)>) -> Self {
        Self { columns }
    }

    /// Look up a value by column name.
    pub fn get(&self, column: &str) -> Option<&Value> {
        self.columns
            .iter()
            .find(|(c, _)| c == column)
            .map(|(_, v)| v)
    }

    /// Iterate over `(column, value)` pairs in projection order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.columns.iter().map(|(c, v)| (c.as_str(), v))
    }

    /// Consume into `(column, value)` pairs.
    pub fn into_columns(self) -> Vec<(String, Value)> {
        self.columns
    }

    /// Number of columns.
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    /// Whether the row has no columns.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

/// Result of executing a read plan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueryResult {
    /// Rows in backend order.
    pub rows: Vec<Row>,
}

/// Result of executing a write plan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ExecResult {
    /// Rows inserted, updated or deleted.
    pub rows_affected: u64,
    /// Rows produced by `RETURNING`, in statement order.
    pub returning: Vec<Row>,
}

/// Runs plans and raw statements on a pool or inside a transaction.
///
/// Implementations **must** check plans against [`capabilities`](Self::capabilities)
/// (`QueryPlan::check` / `WritePlan::check`) before any I/O and return a
/// capability error instead of ignoring unsupported features.
#[async_trait]
pub trait Executor: Send + Sync + 'static {
    /// Declared capabilities.
    fn capabilities(&self) -> BackendCapabilities;

    /// Execute a read plan.
    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError>;

    /// Execute a write plan.
    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError>;

    /// Run raw SQL returning rows. `params` are bound, never interpolated.
    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError>;

    /// Run raw SQL returning the affected-row count. `params` are bound.
    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError>;

    /// Run a multi-statement script without parameters (DDL, migrations).
    async fn execute_script(&self, sql: &str) -> Result<(), OrmError>;
}

/// A connection pool that can open transactions.
#[async_trait]
pub trait Backend: Executor {
    /// Count compiled read parameters without database I/O.
    ///
    /// Args:
    ///     _plan: Complete read plan whose existing binds consume capacity.
    ///
    /// Returns:
    ///     Exact compiled bind count, or None when the backend cannot report
    ///     it. SQL prefetch requires this contract for bounded batching.
    ///
    /// # Errors
    /// Capability or plan compilation errors.
    fn read_parameter_count(&self, _plan: &QueryPlan) -> Result<Option<usize>, OrmError> {
        Ok(None)
    }

    /// Begin a transaction on a dedicated connection.
    ///
    /// `isolation` is validated against
    /// [`BackendCapabilities::isolation_levels`] before any I/O.
    async fn begin(
        &self,
        isolation: Option<IsolationLevel>,
    ) -> Result<Box<dyn Transaction>, OrmError>;

    /// Connection for a schema migration.
    ///
    /// When `transactional` is true this is a transaction, same as
    /// [`begin`](Self::begin) with no isolation override. SQLite turns
    /// `PRAGMA foreign_keys` off on that connection *before* `BEGIN` (the
    /// pragma is a no-op inside a transaction), runs `PRAGMA foreign_key_check`
    /// before commit, and restores the previous pragma value afterwards.
    /// Other backends ignore the SQLite-specific work.
    ///
    /// # Errors
    /// The default implementation has no way to hold a connection outside a
    /// transaction, so `transactional == false` fails with a capability error
    /// instead of silently handing back a transaction.
    async fn begin_schema(&self, transactional: bool) -> Result<Box<dyn Transaction>, OrmError> {
        if !transactional {
            return Err(QueryError::InvalidPlan(
                "this backend does not support a non-transactional schema connection".into(),
            )
            .into());
        }
        self.begin(None).await
    }
}

/// An open transaction.
///
/// Dropping a transaction without calling [`commit`](Self::commit) rolls it
/// back (the connection is returned to the pool in a clean state).
#[async_trait]
pub trait Transaction: Executor {
    /// Commit. Later calls on this transaction fail with
    /// [`QueryError::TransactionClosed`].
    async fn commit(&self) -> Result<(), OrmError>;

    /// Roll back. Later calls fail like after [`commit`](Self::commit).
    async fn rollback(&self) -> Result<(), OrmError>;
}
