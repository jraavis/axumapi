//! The [`Backend`] trait implemented by every database adapter.

use crate::capabilities::BackendCapabilities;
use crate::error::OrmError;
use crate::plan::QueryPlan;
use crate::value::Value;
use async_trait::async_trait;

/// One decoded result row: column names paired with backend-neutral values.
///
/// Dynamic projections and annotations decode into `Row`; typed model
/// decoding (Phase 4) is layered on top via `FromRow`-style traits.
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

/// A database backend.
///
/// Implementations **must** call [`QueryPlan::check`] (or equivalent) and
/// return a capability error instead of ignoring unsupported features.
#[async_trait]
pub trait Backend: Send + Sync + 'static {
    /// Declared capabilities.
    fn capabilities(&self) -> BackendCapabilities;

    /// Execute a read plan.
    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError>;
}
