//! Opt-in native MySQL adapter (feature `mysql-native`).
//!
//! Completed ORM statements retain clean prepared-statement sessions.
//! Interrupted exchanges and unfinished transactions retire their socket.
//! Raw SQL/scripts and schema connections are retired after use because
//! their session state and locks cannot be safely inferred. Writes are never
//! retried after a connection error or an ambiguous commit outcome.
//!
//! This is an experimental adapter. Adoption still requires live recovery,
//! TLS, migration and matched end-to-end performance evidence. The existing
//! SQLx adapter remains the compatibility default.

mod codec;
#[cfg(test)]
mod contract;
mod lease;
#[cfg(test)]
mod options_tests;

use super::io::MySqlIo;
use super::metadata::TableCache;
use super::returning::{Prepared, prepare, synthesize, synthesized_result};
use super::returning::{table_of, write_returning};
use crate::shared::affected_result;
use crate::sql::{CompiledQuery, MySql, compile, compile_write};
use async_trait::async_trait;
use lease::Lease;
use mysql_async::{Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts};
use siderite_orm::{Backend, BackendCapabilities, BackendError, ExecResult};
use siderite_orm::{Executor, IsolationLevel, OrmError, QueryError, QueryPlan};
use siderite_orm::{QueryResult, Transaction, Value, WritePlan};
use std::fmt::{self, Debug, Formatter};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{MappedMutexGuard, Mutex, MutexGuard, Semaphore};

type Reply<T> = Result<T, OrmError>;
type Args = Vec<Value>;
type Scope = Box<dyn Transaction>;
type ReadResult = Reply<QueryResult>;

/// Native pool admission and checkout limits.
#[derive(Debug, Clone)]
pub struct NativeMySqlOptions {
    /// Maximum simultaneously held database connections.
    pub max_connections: usize,
    /// Maximum admitted callers waiting for a connection.
    pub max_waiters: usize,
    /// Deadline for obtaining a configured connection.
    pub acquire_timeout: Duration,
}

impl Default for NativeMySqlOptions {
    fn default() -> Self {
        Self {
            max_connections: 10,
            max_waiters: 100,
            acquire_timeout: Duration::from_secs(10),
        }
    }
}

/// MySQL ORM adapter using retained clean mysql_async sessions.
#[derive(Clone)]
pub struct NativeMySqlBackend {
    pool: Pool,
    admission: Arc<Semaphore>,
    acquire_timeout: Duration,
    max_connections: usize,
    tables: Arc<TableCache>,
}

impl NativeMySqlBackend {
    /// Connect with the default bounded admission settings.
    ///
    /// Args:
    ///     url: MySQL connection URL, including the database name.
    ///
    /// Returns:
    ///     A connected native backend, or a configuration/connection error.
    pub async fn connect(url: &str) -> Result<Self, BackendError> {
        Self::connect_with(url, NativeMySqlOptions::default()).await
    }

    /// Connect with explicit admission and checkout limits.
    ///
    /// Args:
    ///     url: Native-driver MySQL connection URL.
    ///     options: Connection count, waiting count and checkout deadline.
    ///
    /// Returns:
    ///     A connected backend, or an invalid configuration/connection error.
    pub async fn connect_with(
        url: &str,
        options: NativeMySqlOptions,
    ) -> Result<Self, BackendError> {
        let parsed = Opts::from_url(url).map_err(|_| {
            let message = "invalid native MySQL URL";
            connection_error(message)
        })?;
        Self::connect_options(parsed, options).await
    }

    /// Connect with native options, including verified TLS and custom roots.
    ///
    /// Args:
    ///     connection: Driver options; TLS roots and identity policy retained.
    ///     options: Connection count, admission and checkout deadline.
    ///
    /// Returns:
    ///     Warm backend, or configuration/session/connection failure.
    ///
    /// Native pool bounds, session setup, found-rows and statement cache
    /// override corresponding driver settings to preserve adapter contracts.
    /// Custom initialization must leave no transaction or lock open.
    pub async fn connect_options(
        connection: Opts,
        options: NativeMySqlOptions,
    ) -> Result<Self, BackendError> {
        let capacity = options
            .max_connections
            .checked_add(options.max_waiters)
            .filter(|capacity| *capacity <= Semaphore::MAX_PERMITS)
            .ok_or_else(|| connection_error("invalid admission capacity"))?;
        // With zero idle TTL, the minimum is the retention threshold.
        // Retain every clean connection up to the configured pool bound.
        let count = options.max_connections;
        let bounds = PoolConstraints::new(count, count)
            .filter(|_| options.max_connections > 0)
            .ok_or_else(|| connection_error("invalid connection count"))?;
        if options.acquire_timeout.is_zero() {
            return Err(connection_error("checkout deadline must be positive"));
        }
        let mut setup = super::SESSION_SETUP.to_vec();
        setup.push("SET autocommit = 1");
        let acquire_timeout = options.acquire_timeout;
        let connect_options = OptsBuilder::from_opts(connection)
            .client_found_rows(true)
            .stmt_cache_size(128)
            .setup(setup)
            .pool_opts(
                PoolOpts::default()
                    .with_constraints(bounds)
                    .with_reset_connection(false),
            );
        let backend = Self {
            pool: Pool::new(connect_options),
            admission: Arc::new(Semaphore::new(capacity)),
            acquire_timeout,
            max_connections: count,
            tables: Arc::default(),
        };
        backend.warm().await?;
        Ok(backend)
    }

    /// Disconnect the pool after all held connections have been released.
    ///
    /// Returns:
    ///     Success after disconnect, or the native driver error.
    pub async fn close(&self) -> Result<(), BackendError> {
        self.pool
            .clone()
            .disconnect()
            .await
            .map_err(|error| connection_error(error.to_string()))
    }

    async fn checkout(&self) -> Result<Lease, OrmError> {
        let permit = Arc::clone(&self.admission)
            .try_acquire_owned()
            .map_err(|_| connection_error("native MySQL admission is full"))?;
        let wait = self.pool.get_conn();
        let conn = tokio::time::timeout(self.acquire_timeout, wait)
            .await
            .map_err(|_| connection_error("native MySQL checkout timed out"))?
            .map_err(lease::error)?;
        Ok(Lease::new(conn, permit))
    }

    /// Initialize all configured pool slots within one checkout deadline.
    ///
    /// Returns:
    ///     Success after every slot is configured, or a checkout/setup error.
    pub async fn warm(&self) -> Result<(), BackendError> {
        let work = async {
            let mut leases = Vec::with_capacity(self.max_connections);
            for _ in 0..self.max_connections {
                leases.push(self.checkout().await?);
            }
            Ok::<_, OrmError>(leases)
        };
        let leases = tokio::time::timeout(self.acquire_timeout, work)
            .await
            .map_err(|_| connection_error("native pool warming timed out"))?
            .map_err(|error| connection_error(error.to_string()))?;
        drop(leases);
        Ok(())
    }

    fn autocommit(&self) -> bool {
        true
    }
}

trait LeaseAccess {
    fn lease(&mut self) -> &mut Lease;
}

impl Debug for NativeMySqlBackend {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        // mysql_async's Pool Debug includes its URL-derived password.
        formatter
            .debug_struct("NativeMySqlBackend")
            .field("max_connections", &self.max_connections)
            .field("acquire_timeout", &self.acquire_timeout)
            .finish_non_exhaustive()
    }
}

impl LeaseAccess for Lease {
    fn lease(&mut self) -> &mut Lease {
        self
    }
}

impl LeaseAccess for MappedMutexGuard<'_, Lease> {
    fn lease(&mut self) -> &mut Lease {
        self
    }
}

/// Compile before checkout, and share executor behavior for pools/scopes.
macro_rules! executor {
    ($target:ty) => {
        #[async_trait]
        impl Executor for $target {
            fn capabilities(&self) -> BackendCapabilities {
                BackendCapabilities::mysql()
            }

            async fn fetch(&self, plan: &QueryPlan) -> Reply<QueryResult> {
                let query = compile(plan, &MySql)?;
                let mut access = self.checkout().await?;
                access.lease().fetch(&query.sql, query.params).await
            }

            async fn execute(&self, plan: &WritePlan) -> Reply<ExecResult> {
                let prepared = Write::prepare(plan)?;
                let mut access = self.checkout().await?;
                write(
                    access.lease(),
                    &self.tables,
                    plan,
                    prepared,
                    self.autocommit(),
                )
                .await
            }

            async fn fetch_raw(&self, sql: &str, params: Args) -> ReadResult {
                self.tables.clear();
                let mut access = self.checkout().await?;
                let conn = access.lease();
                conn.retire = true;
                conn.fetch(sql, params).await
            }

            async fn execute_raw(&self, sql: &str, args: Args) -> Reply<u64> {
                self.tables.clear();
                let mut access = self.checkout().await?;
                let conn = access.lease();
                conn.retire = true;
                conn.run(sql, args).await.map(|done| done.rows_affected())
            }

            async fn execute_script(&self, sql: &str) -> Reply<()> {
                self.tables.clear();
                let mut access = self.checkout().await?;
                let conn = access.lease();
                conn.retire = true;
                conn.control(sql).await
            }
        }
    };
}

executor!(NativeMySqlBackend);
executor!(NativeTransaction);

enum Write {
    Plain(CompiledQuery),
    Returning(Prepared),
}

impl Write {
    fn prepare(plan: &WritePlan) -> Result<Self, OrmError> {
        if plan.returning().is_empty() {
            compile_write(plan, &MySql).map(Self::Plain)
        } else {
            prepare(plan).map(Self::Returning)
        }
    }
}

async fn write(
    conn: &mut Lease,
    tables: &TableCache,
    plan: &WritePlan,
    prepared: Write,
    autocommit: bool,
) -> Reply<ExecResult> {
    let prepared = match prepared {
        Write::Returning(prepared) => prepared,
        Write::Plain(query) => {
            let done = conn.run(&query.sql, query.params).await?;
            return Ok(affected_result(done.rows_affected()));
        }
    };
    let info = tables.table(conn, table_of(plan)).await?;
    if autocommit {
        if let WritePlan::Insert(insert) = plan
            && let Some(row) = synthesize(&info, insert)
        {
            let write = prepared.write;
            let done = conn.run(&write.sql, write.params).await?;
            return synthesized_result(&info, row, &done);
        }
        begin(conn, None).await?;
    }
    let result = write_returning(conn, &info, plan, prepared).await?;
    if autocommit {
        conn.control("COMMIT").await?;
        conn.in_transaction = false;
    }
    Ok(result)
}

#[async_trait]
impl Backend for NativeMySqlBackend {
    fn read_parameter_count(&self, plan: &QueryPlan) -> Result<Option<usize>, OrmError> {
        Ok(Some(compile(plan, &MySql)?.params.len()))
    }

    async fn begin(&self, isolation: Option<IsolationLevel>) -> Reply<Scope> {
        if let Some(level) = isolation {
            self.capabilities()
                .require(siderite_orm::Feature::Isolation(level))?;
        }
        let mut conn = self.checkout().await?;
        begin(&mut conn, isolation).await?;
        Ok(Box::new(NativeTransaction {
            slot: Mutex::new(Some(conn)),
            tables: Arc::clone(&self.tables),
            transactional: true,
        }))
    }

    async fn begin_schema(&self, transactional: bool) -> Reply<Scope> {
        if transactional {
            return self.begin(None).await;
        }
        let mut conn = self.checkout().await?;
        conn.retire = true;
        Ok(Box::new(NativeTransaction {
            slot: Mutex::new(Some(conn)),
            tables: Arc::clone(&self.tables),
            transactional: false,
        }))
    }
}

async fn begin(conn: &mut Lease, level: Option<IsolationLevel>) -> Reply<()> {
    conn.in_transaction = true;
    if let Some(level) = level {
        let sql = format!(
            "SET TRANSACTION ISOLATION LEVEL {}",
            super::isolation_sql(level)
        );
        conn.control(&sql).await?;
    }
    conn.control("START TRANSACTION").await
}

struct NativeTransaction {
    slot: Mutex<Option<Lease>>,
    tables: Arc<TableCache>,
    transactional: bool,
}

impl NativeTransaction {
    async fn checkout(&self) -> Result<MappedMutexGuard<'_, Lease>, OrmError> {
        MutexGuard::try_map(self.slot.lock().await, Option::as_mut)
            .map_err(|_| QueryError::TransactionClosed.into())
    }

    fn autocommit(&self) -> bool {
        false
    }

    async fn finish(&self, commit: bool) -> Reply<()> {
        let mut conn = self
            .slot
            .lock()
            .await
            .take()
            .ok_or(QueryError::TransactionClosed)?;
        if self.transactional {
            let sql = if commit { "COMMIT" } else { "ROLLBACK" };
            conn.control(sql).await?;
            conn.in_transaction = false;
        }
        Ok(())
    }
}

#[async_trait]
impl Transaction for NativeTransaction {
    async fn commit(&self) -> Reply<()> {
        self.finish(true).await
    }

    async fn rollback(&self) -> Reply<()> {
        self.finish(false).await
    }
}

fn connection_error(message: impl Into<String>) -> BackendError {
    BackendError::Connection(message.into())
}
