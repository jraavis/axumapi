//! Pieces shared by the SQLx-based adapters ([`sqlite`](crate::sqlite),
//! [`postgres`](crate::postgres)): error mapping, write-result shaping and
//! the open-transaction slot.

use siderite_orm::{BackendError, ExecResult, OrmError, QueryError, Row};
use tokio::sync::Mutex;

/// Classify a driver error: constraint violations (unique, foreign key,
/// check, not null) become [`BackendError::Constraint`], everything else
/// [`BackendError::Database`].
pub(crate) fn map_error(e: sqlx::Error) -> BackendError {
    match &e {
        sqlx::Error::Database(db)
            if db.constraint().is_some()
                || db.is_unique_violation()
                || db.is_foreign_key_violation()
                || db.is_check_violation() =>
        {
            BackendError::Constraint(db.message().to_owned())
        }
        _ => BackendError::Database(e.to_string()),
    }
}

/// Result of a write that used `RETURNING`: the rows are the outcome.
pub(crate) fn returning_result(rows: Vec<Row>) -> ExecResult {
    ExecResult {
        rows_affected: rows.len() as u64,
        returning: rows,
    }
}

/// Result of a write without `RETURNING`.
pub(crate) fn affected_result(rows_affected: u64) -> ExecResult {
    ExecResult {
        rows_affected,
        returning: Vec::new(),
    }
}

/// The transaction of a [`Transaction`](siderite_orm::Transaction) adapter.
/// Empty once committed or rolled back, so later use fails cleanly; dropping
/// it while open rolls the transaction back.
pub(crate) struct TxSlot<DB: sqlx::Database> {
    pub(crate) tx: Mutex<Option<sqlx::Transaction<'static, DB>>>,
}

impl<DB: sqlx::Database> TxSlot<DB> {
    pub(crate) fn new(tx: sqlx::Transaction<'static, DB>) -> Self {
        Self {
            tx: Mutex::new(Some(tx)),
        }
    }

    async fn take(&self) -> Result<sqlx::Transaction<'static, DB>, QueryError> {
        self.tx
            .lock()
            .await
            .take()
            .ok_or(QueryError::TransactionClosed)
    }

    pub(crate) async fn commit(&self) -> Result<(), OrmError> {
        let tx = self.take().await?;
        tx.commit().await.map_err(|e| map_error(e).into())
    }

    pub(crate) async fn rollback(&self) -> Result<(), OrmError> {
        let tx = self.take().await?;
        tx.rollback().await.map_err(|e| map_error(e).into())
    }
}

/// Run `$body` with `$conn` bound to the open transaction's connection.
macro_rules! with_tx {
    ($slot:expr, $conn:ident => $body:expr) => {{
        let mut guard = $slot.tx.lock().await;
        let tx = guard
            .as_mut()
            .ok_or(::siderite_orm::QueryError::TransactionClosed)?;
        let $conn = &mut **tx;
        $body
    }};
}
pub(crate) use with_tx;

/// A pooled connection held without an SQL transaction (migration lock).
/// Dropping it without [`take`](Self::take) closes the connection so session
/// locks (`pg_advisory_lock`, `GET_LOCK`) are not returned to the pool.
pub(crate) struct ConnSlot<DB: sqlx::Database> {
    pub(crate) conn: Mutex<Option<sqlx::pool::PoolConnection<DB>>>,
}

impl<DB: sqlx::Database> ConnSlot<DB> {
    pub(crate) fn new(conn: sqlx::pool::PoolConnection<DB>) -> Self {
        Self {
            conn: Mutex::new(Some(conn)),
        }
    }

    pub(crate) async fn take(&self) -> Result<sqlx::pool::PoolConnection<DB>, QueryError> {
        self.conn
            .lock()
            .await
            .take()
            .ok_or(QueryError::TransactionClosed)
    }
}

impl<DB: sqlx::Database> Drop for ConnSlot<DB> {
    fn drop(&mut self) {
        if let Some(mut conn) = self.conn.get_mut().take() {
            conn.close_on_drop();
        }
    }
}

macro_rules! with_conn {
    ($slot:expr, $conn:ident => $body:expr) => {{
        let mut guard = $slot.conn.lock().await;
        let conn = guard
            .as_mut()
            .ok_or(::siderite_orm::QueryError::TransactionClosed)?;
        let $conn = &mut **conn;
        $body
    }};
}
pub(crate) use with_conn;
