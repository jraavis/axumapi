//! MySQL backend built on SQLx (MySQL 8.0.31+, tested on 9.x).
//!
//! # Storage
//!
//! | [`Value`] | Column type | Notes |
//! |---|---|---|
//! | `Bool` | `TINYINT(1)` | read back as `Bool` (a bare `TINYINT` reads as `Int`) |
//! | `Int` | `BIGINT` / `INT` / ... | unsigned columns above `i64::MAX` fail to decode |
//! | `Float` | `DOUBLE` | |
//! | `Decimal` | `DECIMAL(p, s)` | |
//! | `Text` | `VARCHAR(n)` / `TEXT` | comparisons follow the column collation |
//! | `Bytes` | `BLOB` / `VARBINARY` | |
//! | `Uuid` | `CHAR(36)` | bound as hyphenated text, not as 16 raw bytes |
//! | `Date` / `Time` | `DATE` / `TIME(6)` | |
//! | `Timestamp` | `DATETIME(6)` | UTC; connections are pinned to `+00:00` |
//! | `Json` | `JSON` | |
//!
//! # No `RETURNING`
//!
//! MySQL has no `RETURNING`, but the ORM's `save`, `create` and
//! `bulk_create` need the stored rows back, so this adapter emulates it:
//!
//! * **`INSERT`**: a generated `AUTO_INCREMENT` key is recovered from the
//!   OK packet's `LAST_INSERT_ID()` plus the consecutive values InnoDB hands
//!   out to one multi-row `INSERT` (`innodb_autoinc_lock_mode` 0, 1 and 2 all
//!   allocate the whole block of a plain `INSERT .. VALUES` at once; the step
//!   is `@@auto_increment_increment`). Supplied keys are used as they are.
//!   The rows are then re-read by key, in key order.
//! * **`UPDATE` / `DELETE`**: the affected keys are read `FOR UPDATE` (an
//!   `UPDATE` by primary-key equality skips that read), the statement runs,
//!   and the rows are read back (after an `UPDATE`) or were read beforehand
//!   (`DELETE`).
//!
//! Each emulated write that reads rows back, single-row ones included, runs
//! in a transaction (or the caller's), so the rows read back are the ones
//! written: no concurrent statement can change or delete them in between.
//!
//! A **single-row `INSERT`** skips the read-back, and with it the
//! transaction, when the stored row is known without asking: the table has no
//! `INSERT` trigger (which takes the `TRIGGER` privilege, granted directly
//! and not through a role, to find out; without it the row is read back),
//! and every returned column is either the omitted
//! `AUTO_INCREMENT` column (taken from the OK packet) or a supplied value its
//! column stores unchanged. That holds for integers within the range of a
//! signed integer column, booleans in `TINYINT(1)`, and text that fits a
//! `VARCHAR` / `TEXT` column (`utf8mb4`, or ASCII in any character set).
//! Everything else (defaults, generated columns, `DECIMAL`, temporal, `JSON`,
//! binary and `CHAR` columns) is read back as described above.
//!
//! The columns of a table and whether it has an `INSERT` trigger are looked up
//! once per table in `information_schema` and cached;
//! [`execute_script`](Executor::execute_script) clears the cache. A table or
//! trigger changed by other means (another process, `execute_raw`) while the
//! backend is in use is not noticed until then. A table without a primary key
//! cannot use the emulation.
//!
//! # Session setup
//!
//! [`MySqlBackend::connect`] configures every connection: `time_zone` is
//! `+00:00` (so `DATETIME`, `TIMESTAMP` and date parts agree with the UTC
//! values the ORM writes), `NO_BACKSLASH_ESCAPES` is removed from `sql_mode`
//! (the compiler writes `'\\'` string literals) and `group_concat_max_len` is
//! raised (`GROUP_CONCAT` silently truncates at the default 1024 bytes).
//! Pools built elsewhere and passed to [`MySqlBackend::from_pool`] should do
//! the same.

use crate::connection_init::ConnectionInit;
use crate::shared::{ConnSlot, TxSlot, affected_result, map_error, with_conn, with_tx};
use crate::sql::{MySql, compile, compile_write};
use async_trait::async_trait;
use siderite_orm::{
    Backend, BackendCapabilities, BackendError, ExecResult, Executor, IsolationLevel, OrmError,
    QueryPlan, QueryResult, Transaction, Value, WritePlan,
};
use sqlx::Executor as _;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions, MySqlQueryResult};
use std::sync::Arc;

mod io;
mod metadata;
#[cfg(feature = "mysql-native")]
pub mod native;
mod returning;
mod sqlx_io;

use metadata::TableCache;
use returning::{prepare, synthesize, synthesized_result, table_of, write_returning};
use sqlx_io::{execute_rows, fetch_rows, run};

/// Session statements run on every new connection (see the module docs).
const SESSION_SETUP: [&str; 3] = [
    "SET time_zone = '+00:00'",
    "SET SESSION sql_mode = REPLACE(@@sql_mode, 'NO_BACKSLASH_ESCAPES', '')",
    "SET SESSION group_concat_max_len = 4294967295",
];

/// MySQL adapter executing compiled plans on a connection pool.
#[derive(Debug, Clone)]
pub struct MySqlBackend {
    pool: MySqlPool,
    tables: Arc<TableCache>,
}

impl MySqlBackend {
    /// Connect to `url` (`mysql://user:pass@host/db`) with a pool of up to
    /// ten connections, each configured as described in the module docs.
    ///
    /// A connection is **not** checked when it is taken from the pool: SQLx's
    /// `test_before_acquire` costs a round trip per query. The price is that
    /// the first query on a connection the server has closed (a restart,
    /// `wait_timeout`) fails with a connection error instead of being retried
    /// on a fresh connection. To have connections checked, use
    /// [`connect_with`](Self::connect_with) with
    /// `MySqlPoolOptions::new().test_before_acquire(true)`.
    ///
    /// # Errors
    /// [`BackendError::Connection`] if the pool cannot be created.
    pub async fn connect(url: &str) -> Result<Self, BackendError> {
        let options = MySqlPoolOptions::new()
            .max_connections(10)
            .test_before_acquire(false);
        Self::connect_with(url, options).await
    }

    /// Connect with pool settings and mandatory adapter initialization.
    ///
    /// Replaces any after_connect callback already stored in options.
    /// SQLx does not expose that callback for chaining; pass custom setup to
    /// [`Self::connect_with_init`] instead. Other pool options are retained.
    ///
    /// Args:
    ///     url: Database URL.
    ///     options: Pool capacity, timeouts and checkout policy.
    ///
    /// Returns:
    ///     Adapter whose fresh sessions receive mandatory initialization.
    ///
    /// # Errors
    /// Connection or initialization failure within the acquire deadline.
    pub async fn connect_with(url: &str, options: MySqlPoolOptions) -> Result<Self, BackendError> {
        let init: ConnectionInit<sqlx::MySql> =
            std::sync::Arc::new(|_, _| Box::pin(async { Ok(()) }));
        Self::connect_with_init(url, options, init).await
    }

    /// Compose explicit custom setup with mandatory session settings.
    ///
    /// Args:
    ///     url: Database URL.
    ///     options: Pool settings; its stored after_connect is replaced.
    ///     init: Custom hook run first on every new/reconnected session.
    ///
    /// Returns:
    ///     Adapter after custom setup and required settings succeed.
    ///
    /// # Errors
    /// Connection or initialization failure within the acquire deadline.
    pub async fn connect_with_init(
        url: &str,
        options: MySqlPoolOptions,
        init: ConnectionInit<sqlx::MySql>,
    ) -> Result<Self, BackendError> {
        let pool = options
            .after_connect(move |conn, meta| {
                let init = init.clone();
                Box::pin(async move {
                    init(conn, meta).await?;
                    for statement in SESSION_SETUP {
                        conn.execute(statement).await?;
                    }
                    Ok(())
                })
            })
            .connect(url)
            .await
            .map_err(|e| BackendError::Connection(e.to_string()))?;
        Ok(Self::from_pool(pool))
    }

    /// Wrap an existing pool. Its connections should use the session setup
    /// described in the module docs.
    pub fn from_pool(pool: MySqlPool) -> Self {
        Self {
            pool,
            tables: Arc::default(),
        }
    }
}

#[async_trait]
impl Executor for MySqlBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::mysql()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &MySql)?;
        fetch_rows(&self.pool, &compiled.sql, compiled.params).await
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        if plan.returning().is_empty() {
            let compiled = compile_write(plan, &MySql)?;
            let affected = execute_rows(&self.pool, &compiled.sql, compiled.params).await?;
            return Ok(affected_result(affected));
        }
        let prepared = prepare(plan)?;
        let info = self.tables.table(&mut &self.pool, table_of(plan)).await?;
        if let WritePlan::Insert(p) = plan
            && let Some(row) = synthesize(&info, p)
        {
            // One statement and nothing to read back: no transaction needed.
            let write = prepared.write;
            let done = run(&self.pool, &write.sql, write.params).await?;
            return synthesized_result(&info, row, &done);
        }
        // A write that is read back runs in a transaction even for a single
        // row: the write's row lock is held until the read-back, so a
        // concurrent update or delete cannot change or remove the row in
        // between.
        let mut tx = self.pool.begin().await.map_err(db_error)?;
        let result = write_returning(&mut *tx, &info, plan, prepared).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(result)
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        fetch_rows(&self.pool, sql, params).await
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        execute_rows(&self.pool, sql, params).await
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        let done = sqlx::Executor::execute(&self.pool, sql).await;
        // The script may have changed keys or dropped tables.
        self.tables.clear();
        script_result(done)
    }
}

#[async_trait]
impl Backend for MySqlBackend {
    fn read_parameter_count(&self, plan: &QueryPlan) -> Result<Option<usize>, OrmError> {
        Ok(Some(compile(plan, &MySql)?.params.len()))
    }

    async fn begin(
        &self,
        isolation: Option<IsolationLevel>,
    ) -> Result<Box<dyn Transaction>, OrmError> {
        let tx = match isolation {
            None => self.pool.begin().await,
            Some(level) => {
                self.capabilities()
                    .require(siderite_orm::Feature::Isolation(level))?;
                // `SET TRANSACTION` applies to the next transaction only and
                // is an error once one has started, so it must come first.
                let statement = format!(
                    "SET TRANSACTION ISOLATION LEVEL {}; START TRANSACTION",
                    isolation_sql(level)
                );
                self.pool.begin_with(statement).await
            }
        }
        .map_err(db_error)?;
        Ok(Box::new(MySqlTransaction {
            slot: TxSlot::new(tx),
            tables: Arc::clone(&self.tables),
        }))
    }

    async fn begin_schema(&self, transactional: bool) -> Result<Box<dyn Transaction>, OrmError> {
        if transactional {
            return self.begin(None).await;
        }
        let conn = self.pool.acquire().await.map_err(db_error)?;
        Ok(Box::new(MySqlHeld {
            slot: ConnSlot::new(conn),
            tables: Arc::clone(&self.tables),
        }))
    }
}

fn isolation_sql(level: IsolationLevel) -> &'static str {
    match level {
        IsolationLevel::ReadCommitted => "READ COMMITTED",
        IsolationLevel::RepeatableRead => "REPEATABLE READ",
        IsolationLevel::Serializable => "SERIALIZABLE",
    }
}

/// An open MySQL transaction. Dropped without commit, it rolls back.
struct MySqlTransaction {
    slot: TxSlot<sqlx::MySql>,
    tables: Arc<TableCache>,
}

#[async_trait]
impl Executor for MySqlTransaction {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::mysql()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &MySql)?;
        with_tx!(self.slot, conn => fetch_rows(conn, &compiled.sql, compiled.params).await)
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        if plan.returning().is_empty() {
            let compiled = compile_write(plan, &MySql)?;
            let affected = with_tx!(self.slot, conn => execute_rows(conn, &compiled.sql, compiled.params).await)?;
            return Ok(affected_result(affected));
        }
        let prepared = prepare(plan)?;
        with_tx!(self.slot, conn => {
            let info = self.tables.table(&mut *conn, table_of(plan)).await?;
            write_returning(conn, &info, plan, prepared).await
        })
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        with_tx!(self.slot, conn => fetch_rows(conn, sql, params).await)
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        with_tx!(self.slot, conn => execute_rows(conn, sql, params).await)
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        let result =
            with_tx!(self.slot, conn => script_result(sqlx::Executor::execute(conn, sql).await));
        self.tables.clear();
        result
    }
}

#[async_trait]
impl Transaction for MySqlTransaction {
    async fn commit(&self) -> Result<(), OrmError> {
        self.slot.commit().await
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        self.slot.rollback().await
    }
}

/// Pooled connection held for a migration lock (no SQL transaction).
///
/// Like the Postgres held connection, commit and rollback close the
/// connection instead of returning it to the pool so an unreleased
/// `GET_LOCK` is never handed to another caller.
struct MySqlHeld {
    slot: ConnSlot<sqlx::MySql>,
    tables: Arc<TableCache>,
}

#[async_trait]
impl Executor for MySqlHeld {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::mysql()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &MySql)?;
        with_conn!(self.slot, conn => fetch_rows(conn, &compiled.sql, compiled.params).await)
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        if plan.returning().is_empty() {
            let compiled = compile_write(plan, &MySql)?;
            let affected = with_conn!(self.slot, conn => execute_rows(conn, &compiled.sql, compiled.params).await)?;
            return Ok(affected_result(affected));
        }
        let prepared = prepare(plan)?;
        with_conn!(self.slot, conn => {
            let info = self.tables.table(&mut *conn, table_of(plan)).await?;
            write_returning(conn, &info, plan, prepared).await
        })
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        with_conn!(self.slot, conn => fetch_rows(conn, sql, params).await)
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        with_conn!(self.slot, conn => execute_rows(conn, sql, params).await)
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        let result =
            with_conn!(self.slot, conn => script_result(sqlx::Executor::execute(conn, sql).await));
        self.tables.clear();
        result
    }
}

#[async_trait]
impl Transaction for MySqlHeld {
    async fn commit(&self) -> Result<(), OrmError> {
        let mut conn = self.slot.take().await?;
        conn.close_on_drop();
        drop(conn);
        Ok(())
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        let mut conn = self.slot.take().await?;
        conn.close_on_drop();
        drop(conn);
        Ok(())
    }
}

fn db_error(e: sqlx::Error) -> OrmError {
    map_error(e).into()
}

fn script_result(done: Result<MySqlQueryResult, sqlx::Error>) -> Result<(), OrmError> {
    done.map(|_| ()).map_err(db_error)
}

// ---- RETURNING emulation ----------------------------------------------------
