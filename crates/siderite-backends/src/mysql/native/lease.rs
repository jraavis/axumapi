//! Own clean leases and retire interrupted native protocol exchanges.

use super::super::io::{MySqlIo, WriteDone};
use super::codec;
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, Error, Params};
use siderite_orm::{BackendError, OrmError, QueryError, QueryResult, Value};
use std::future::Future;
use std::task::{Context, Waker};
use tokio::sync::OwnedSemaphorePermit;

type Reply<T> = Result<T, OrmError>;
type Args = Vec<Value>;

/// One admitted connection, reusable only after complete clean operations.
pub(super) struct Lease {
    conn: Option<Conn>,
    permit: Option<OwnedSemaphorePermit>,
    pub(super) retire: bool,
    pub(super) in_transaction: bool,
}

impl Lease {
    #[cfg(test)]
    pub(super) fn id(&self) -> Result<u32, QueryError> {
        self.conn
            .as_ref()
            .map(Conn::id)
            .ok_or(QueryError::TransactionAborted)
    }
    /// Own an acquired connection and its admission permit.
    ///
    /// Args:
    ///     conn: Configured, clean native pool connection.
    ///     permit: Capacity held through checkout and connection ownership.
    ///
    /// Returns:
    ///     A clean lease ready for a protocol exchange.
    pub(super) fn new(conn: Conn, permit: OwnedSemaphorePermit) -> Self {
        Self {
            conn: Some(conn),
            permit: Some(permit),
            retire: false,
            in_transaction: false,
        }
    }

    fn flight(&mut self) -> Result<Flight<'_>, OrmError> {
        let conn = self.conn.take().ok_or(QueryError::TransactionAborted)?;
        Ok(Flight {
            home: self,
            conn: Some(conn),
        })
    }

    /// Fully drain an internal control statement or a marked raw script.
    ///
    /// Args:
    ///     sql: Parameter-free SQL; callers mark untrusted session changes.
    ///
    /// Returns:
    ///     Success after all results drain, or the primary driver error.
    pub(super) async fn control(&mut self, sql: &str) -> Reply<()> {
        let mut flight = self.flight()?;
        let done = flight.connection()?.query_drop(sql).await;
        flight.finish(&done);
        done.map_err(error)
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if (self.retire || self.in_transaction)
            && let Some(conn) = self.conn.take()
        {
            retire(conn);
        }
    }
}

/// Moving the connection out makes cancellation destroy it immediately,
/// even when an escaped transaction handle still owns the empty lease.
struct Flight<'a> {
    home: &'a mut Lease,
    conn: Option<Conn>,
}

impl Flight<'_> {
    fn connection(&mut self) -> Result<&mut Conn, OrmError> {
        self.conn
            .as_mut()
            .ok_or(QueryError::TransactionAborted.into())
    }

    fn finish<T>(&mut self, result: &Result<T, Error>) {
        // Nonfatal server errors have consumed their error packet. Keep the
        // transaction connection so savepoint rollback remains possible.
        if result.as_ref().err().is_none_or(|error| !error.is_fatal()) {
            self.home.conn = self.conn.take();
        }
    }
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            retire(conn);
            self.home.permit.take();
        }
    }
}

impl MySqlIo for Lease {
    async fn fetch(&mut self, sql: &str, params: Args) -> Reply<QueryResult> {
        let params = parameters(params)?;
        let mut flight = self.flight()?;
        let result = fetch_all(flight.connection()?, sql, params).await;
        // Decoding happens after the entire protocol response drains. A
        // decode error must not strand a result set or break rollback.
        flight.finish(&result);
        let rows = result
            .map_err(error)?
            .into_iter()
            .map(codec::decode)
            .collect::<Result<_, _>>()?;
        Ok(QueryResult { rows })
    }

    async fn run(&mut self, sql: &str, params: Args) -> Reply<WriteDone> {
        let params = parameters(params)?;
        let mut flight = self.flight()?;
        let result = flight.connection()?.exec_drop(sql, params).await;
        let done = match &result {
            Ok(()) => {
                let conn = flight.connection()?;
                Some(WriteDone {
                    affected: conn.affected_rows(),
                    key: conn.last_insert_id().unwrap_or(0),
                })
            }
            Err(_) => None,
        };
        flight.finish(&result);
        result.map_err(error)?;
        done.ok_or(QueryError::TransactionAborted.into())
    }
}

fn parameters(values: Vec<Value>) -> Result<Params, QueryError> {
    values
        .into_iter()
        .map(codec::encode)
        .collect::<Result<Vec<_>, _>>()
        .map(Params::Positional)
}

async fn fetch_all(
    conn: &mut Conn,
    sql: &str,
    params: Params,
) -> Result<Vec<mysql_async::Row>, Error> {
    let mut query = conn.exec_iter(sql, params).await?;
    let mut rows = Vec::new();
    while !query.is_empty() {
        if let Some(row) = query.next().await? {
            rows.push(row);
        }
    }
    query.drop_result().await?;
    Ok(rows)
}

/// Poll once to set the pinned driver's disconnected flag before its first
/// await. Dropping the future then drops the socket; the pool recycler
/// discards this connection instead of cleaning and reusing it. No detached
/// cleanup task is spawned. Re-audit this invariant when upgrading 0.37.1.
fn retire(conn: Conn) {
    let future = conn.disconnect();
    let mut future = std::pin::pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    let _ = future.as_mut().poll(&mut context);
}

/// Classify native failures using the SQLx control's constraint code set.
///
/// Args:
///     failure: Native server/protocol failure.
///
/// Returns:
///     Backend error preserving server messages and constraint classification.
pub(super) fn error(failure: Error) -> OrmError {
    let message = failure.to_string();
    let constraint = matches!(
        &failure,
        Error::Server(server)
            if matches!(server.code,
                1022 | 1062 | 1169 | 1586 | 1859 | 1216 | 1217
                | 1451 | 1452 | 1830 | 1834 | 3819)
            || (server.code == 4025 && server.state == "23000")
    );
    if constraint {
        BackendError::Constraint(message).into()
    } else {
        BackendError::Database(message).into()
    }
}
