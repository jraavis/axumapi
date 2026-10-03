//! Statically dispatched driver contract for shared RETURNING behavior.

use siderite_orm::{OrmError, QueryResult, Value};
use std::future::Future;

/// Result data needed for affected counts and generated-key reconstruction.
pub(super) struct WriteDone {
    pub(super) affected: u64,
    pub(super) key: u64,
}

impl WriteDone {
    /// Return the driver's affected row count.
    ///
    /// Returns:
    ///     Rows changed by the completed statement.
    pub(super) fn rows_affected(&self) -> u64 {
        self.affected
    }

    /// Return the driver's first generated key.
    ///
    /// Returns:
    ///     MySQL's last-insert identifier for the completed statement.
    pub(super) fn last_insert_id(&self) -> u64 {
        self.key
    }
}

/// Driver operations needed by metadata and stored-row reconstruction.
/// Implementations drain result sets before returning. The caller owns
/// connection lifetime, session cleanliness, transactions and cancellation.
pub(super) trait MySqlIo: Send {
    /// Fetch and fully drain a parameterized query.
    ///
    /// Args:
    ///     sql: Statement using MySQL placeholders.
    ///     params: Values in placeholder order.
    ///
    /// Returns:
    ///     Canonical decoded rows, or a driver/decoding error.
    fn fetch(
        &mut self,
        sql: &str,
        params: Vec<Value>,
    ) -> impl Future<Output = Result<QueryResult, OrmError>> + Send;

    /// Execute and fully drain a parameterized write.
    ///
    /// Args:
    ///     sql: Statement using MySQL placeholders.
    ///     params: Values in placeholder order.
    ///
    /// Returns:
    ///     Affected count and generated key, or a driver error.
    fn run(
        &mut self,
        sql: &str,
        params: Vec<Value>,
    ) -> impl Future<Output = Result<WriteDone, OrmError>> + Send;
}
