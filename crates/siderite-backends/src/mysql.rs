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

use crate::shared::{ConnSlot, TxSlot, affected_result, map_error, with_conn, with_tx};
use crate::sql::{CompiledQuery, MySql, compile, compile_write};
use async_trait::async_trait;
use siderite_orm::types::canonical_text;
use siderite_orm::{
    Backend, BackendCapabilities, BackendError, BinaryOp, ExecResult, Executor, Expr,
    IsolationLevel, LockMode, OrmError, QueryError, QueryPlan, QueryResult, Row, Transaction,
    Value, WritePlan,
};
use sqlx::mysql::{
    MySqlArguments, MySqlConnection, MySqlPool, MySqlPoolOptions, MySqlQueryResult, MySqlRow,
    MySqlValueRef,
};
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::types::{Decimal, JsonValue};
use sqlx::{Column as _, Executor as _, Row as _, TypeInfo as _, ValueRef as _};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

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

    /// Like [`connect`](Self::connect) with explicit pool options (pool size,
    /// timeouts, `test_before_acquire`), used as given: a plain
    /// `MySqlPoolOptions::new()` checks connections on acquire. The session
    /// setup is added to `options`.
    ///
    /// # Errors
    /// [`BackendError::Connection`] if the pool cannot be created.
    pub async fn connect_with(url: &str, options: MySqlPoolOptions) -> Result<Self, BackendError> {
        let pool = options
            .after_connect(|conn, _meta| {
                Box::pin(async move {
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
        let info = self.tables.table(&self.pool, table_of(plan)).await?;
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
        let result = write_returning(&mut tx, &info, plan, prepared).await?;
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

fn bind_all(sql: &str, params: Vec<Value>) -> sqlx::query::Query<'_, sqlx::MySql, MySqlArguments> {
    params
        .into_iter()
        .fold(sqlx::query(sql), |query, param| match param {
            Value::Null => query.bind(None::<i64>),
            Value::Bool(v) => query.bind(v),
            Value::Int(v) => query.bind(v),
            Value::Float(v) => query.bind(v),
            Value::Decimal(v) => query.bind(v),
            Value::Text(v) => query.bind(v),
            Value::Bytes(v) => query.bind(v),
            // SQLx would send 16 raw bytes; the canonical storage is CHAR(36).
            Value::Uuid(v) => query.bind(v.hyphenated().to_string()),
            Value::Date(v) => query.bind(v),
            Value::Time(v) => query.bind(v),
            Value::Timestamp(v) => query.bind(v),
            Value::Json(v) => query.bind(v),
        })
}

async fn fetch_rows<'c, E>(ex: E, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::MySql>,
{
    let rows = bind_all(sql, params)
        .fetch_all(ex)
        .await
        .map_err(db_error)?;
    let rows = rows.iter().map(decode_row).collect::<Result<_, _>>()?;
    Ok(QueryResult { rows })
}

async fn execute_rows<'c, E>(ex: E, sql: &str, params: Vec<Value>) -> Result<u64, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::MySql>,
{
    Ok(run(ex, sql, params).await?.rows_affected())
}

async fn run<'c, E>(ex: E, sql: &str, params: Vec<Value>) -> Result<MySqlQueryResult, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::MySql>,
{
    bind_all(sql, params).execute(ex).await.map_err(db_error)
}

// ---- RETURNING emulation ----------------------------------------------------

/// Keys, columns and triggers of a table.
#[derive(Debug)]
struct TableInfo {
    table: String,
    primary_key: Vec<String>,
    auto_increment: Option<String>,
    columns: Vec<ColumnInfo>,
    /// Whether an `INSERT` trigger exists (it may change the stored row), or
    /// the user cannot see the table's triggers.
    insert_triggers: bool,
}

/// What [`synthesize`] needs to know about a column.
#[derive(Debug)]
struct ColumnInfo {
    name: String,
    /// `DATA_TYPE`, e.g. `bigint`, `varchar`.
    data_type: String,
    /// `COLUMN_TYPE`, e.g. `tinyint(1)`, `int unsigned`.
    column_type: String,
    /// `CHARACTER_MAXIMUM_LENGTH` of a string column.
    max_length: Option<u64>,
    utf8mb4: bool,
    nullable: bool,
}

impl ColumnInfo {
    /// Range of a signed integer column that is not `TINYINT(1)` (which reads
    /// back as a boolean).
    fn integer_range(&self) -> Option<(i64, i64)> {
        if self.column_type.contains("unsigned") || self.column_type.starts_with("tinyint(1)") {
            return None;
        }
        match self.data_type.as_str() {
            "tinyint" => Some((i64::from(i8::MIN), i64::from(i8::MAX))),
            "smallint" => Some((i64::from(i16::MIN), i64::from(i16::MAX))),
            "mediumint" => Some((-(1 << 23), (1 << 23) - 1)),
            "int" => Some((i64::from(i32::MIN), i64::from(i32::MAX))),
            "bigint" => Some((i64::MIN, i64::MAX)),
            _ => None,
        }
    }

    /// Whether `text` is stored as it is: no truncation, padding or character
    /// set conversion.
    fn stores_text(&self, text: &str) -> bool {
        let Some(max) = self.max_length else {
            return false;
        };
        let fits = match self.data_type.as_str() {
            // Counted in characters.
            "varchar" => text.chars().count() as u64 <= max,
            // Counted in bytes.
            "text" | "mediumtext" | "longtext" => text.len() as u64 <= max,
            _ => false,
        };
        fits && (self.utf8mb4 || text.is_ascii())
    }

    /// Whether reading the column after writing `value` gives `value` back.
    fn stores_exactly(&self, value: &Value) -> bool {
        match value {
            Value::Null => {
                self.nullable
                    && (self.integer_range().is_some()
                        || self.column_type == "tinyint(1)"
                        || self.stores_text(""))
            }
            Value::Bool(_) => self.column_type == "tinyint(1)",
            Value::Int(v) => self
                .integer_range()
                .is_some_and(|(min, max)| (min..=max).contains(v)),
            Value::Text(text) => self.stores_text(text),
            _ => false,
        }
    }
}

/// Per-backend cache of [`TableInfo`].
#[derive(Debug, Default)]
struct TableCache {
    tables: Mutex<HashMap<String, Arc<TableInfo>>>,
}

impl TableCache {
    fn clear(&self) {
        self.tables
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    async fn table<'c, E>(&self, ex: E, name: &str) -> Result<Arc<TableInfo>, OrmError>
    where
        E: sqlx::Executor<'c, Database = sqlx::MySql>,
    {
        let cached = self
            .tables
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .cloned();
        if let Some(info) = cached {
            return Ok(info);
        }
        let rows = sqlx::query(
            "SELECT CAST(COLUMN_NAME AS CHAR) AS c, CAST(COLUMN_KEY AS CHAR) AS k, \
             CAST(EXTRA AS CHAR) AS e, CAST(DATA_TYPE AS CHAR) AS d, \
             CAST(COLUMN_TYPE AS CHAR) AS t, CAST(CHARACTER_MAXIMUM_LENGTH AS UNSIGNED) AS m, \
             CAST(CHARACTER_SET_NAME AS CHAR) AS s, CAST(IS_NULLABLE AS CHAR) AS n, \
             (SELECT COUNT(*) FROM information_schema.TRIGGERS \
              WHERE EVENT_OBJECT_SCHEMA = DATABASE() AND EVENT_OBJECT_TABLE = ? \
              AND EVENT_MANIPULATION = 'INSERT') AS g, \
             (SELECT COUNT(*) FROM information_schema.USER_PRIVILEGES p \
              WHERE p.PRIVILEGE_TYPE = 'TRIGGER' AND p.GRANTEE = me.grantee) + \
             (SELECT COUNT(*) FROM information_schema.SCHEMA_PRIVILEGES p \
              WHERE p.PRIVILEGE_TYPE = 'TRIGGER' AND p.GRANTEE = me.grantee \
              AND DATABASE() LIKE p.TABLE_SCHEMA) + \
             (SELECT COUNT(*) FROM information_schema.TABLE_PRIVILEGES p \
              WHERE p.PRIVILEGE_TYPE = 'TRIGGER' AND p.GRANTEE = me.grantee \
              AND p.TABLE_SCHEMA = DATABASE() AND p.TABLE_NAME = ?) AS v \
             FROM information_schema.COLUMNS, \
             (SELECT CONCAT('''', SUBSTRING_INDEX(CURRENT_USER(), '@', 1), '''@''', \
              SUBSTRING_INDEX(CURRENT_USER(), '@', -1), '''') AS grantee) me \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
        )
        .bind(name)
        .bind(name)
        .bind(name)
        .fetch_all(ex)
        .await
        .map_err(db_error)?;
        if rows.is_empty() {
            return Err(QueryError::Model(format!("table `{name}` does not exist")).into());
        }
        let mut info = TableInfo {
            table: name.to_owned(),
            primary_key: Vec::new(),
            auto_increment: None,
            columns: Vec::with_capacity(rows.len()),
            insert_triggers: false,
        };
        for row in &rows {
            let column: String = row.try_get("c").map_err(db_error)?;
            let key: String = row.try_get("k").map_err(db_error)?;
            let extra: String = row.try_get("e").map_err(db_error)?;
            let data_type: String = row.try_get("d").map_err(db_error)?;
            let column_type: String = row.try_get("t").map_err(db_error)?;
            let charset: Option<String> = row.try_get("s").map_err(db_error)?;
            let nullable: String = row.try_get("n").map_err(db_error)?;
            // `information_schema.TRIGGERS` only lists the triggers of tables
            // the user has the `TRIGGER` privilege on. Without it (or with it
            // only through a role), assume there is one.
            let visible = row.try_get::<i64, _>("v").map_err(db_error)? > 0;
            info.insert_triggers = !visible || row.try_get::<i64, _>("g").map_err(db_error)? > 0;
            info.columns.push(ColumnInfo {
                name: column.clone(),
                data_type: data_type.to_ascii_lowercase(),
                column_type: column_type.to_ascii_lowercase(),
                max_length: row.try_get("m").map_err(db_error)?,
                utf8mb4: charset.is_some_and(|c| c.eq_ignore_ascii_case("utf8mb4")),
                nullable: nullable.eq_ignore_ascii_case("YES"),
            });
            if extra.to_ascii_lowercase().contains("auto_increment") {
                info.auto_increment = Some(column.clone());
            }
            if key == "PRI" {
                info.primary_key.push(column);
            }
        }
        if info.primary_key.is_empty() {
            return Err(QueryError::Model(format!(
                "table `{name}` has no primary key, so MySQL cannot return the rows it writes"
            ))
            .into());
        }
        let info = Arc::new(info);
        self.tables
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.to_owned(), Arc::clone(&info));
        Ok(info)
    }
}

/// The statements a write with emulated `RETURNING` needs, compiled up front
/// so plan errors (capabilities, structure) surface before any I/O.
struct Prepared {
    /// The write itself, without `RETURNING`.
    write: CompiledQuery,
    /// `DELETE`: the locking read of the affected rows. (An `UPDATE`'s read
    /// selects the primary key, known only once the table is looked up; see
    /// [`read_keys`].)
    read: Option<CompiledQuery>,
}

fn table_of(plan: &WritePlan) -> &str {
    match plan {
        WritePlan::Insert(p) => &p.table,
        WritePlan::Update(p) => &p.table,
        WritePlan::Delete(p) => &p.table,
    }
}

fn without_returning(plan: &WritePlan) -> WritePlan {
    let mut bare = plan.clone();
    match &mut bare {
        WritePlan::Insert(p) => p.returning.clear(),
        WritePlan::Update(p) => p.returning.clear(),
        WritePlan::Delete(p) => p.returning.clear(),
    }
    bare
}

fn prepare(plan: &WritePlan) -> Result<Prepared, OrmError> {
    let write = compile_write(&without_returning(plan), &MySql)?;
    let read = match plan {
        WritePlan::Insert(_) | WritePlan::Update(_) => None,
        WritePlan::Delete(p) => Some(locking_read(&p.table, p.filter.as_ref(), &p.returning)),
    };
    let read = read.map(|plan| compile(&plan, &MySql)).transpose()?;
    Ok(Prepared { write, read })
}

/// `SELECT columns FROM table WHERE filter FOR UPDATE`.
fn locking_read<C: Clone + Into<siderite_orm::expr::Ident>>(
    table: &str,
    filter: Option<&Expr>,
    columns: &[C],
) -> QueryPlan {
    let mut plan = QueryPlan::from_table(table.to_owned());
    plan.filter = filter.cloned();
    plan.lock = Some(LockMode::ForUpdate);
    for column in columns {
        plan = plan.select(Expr::col(column.clone()), None);
    }
    plan
}

/// The key of a filter of the form `pk = value` on a single-column key.
fn key_equality(info: &TableInfo, filter: Option<&Expr>) -> Option<Vec<Value>> {
    let [pk] = info.primary_key.as_slice() else {
        return None;
    };
    let Some(Expr::Binary {
        op: BinaryOp::Eq,
        lhs,
        rhs,
    }) = filter
    else {
        return None;
    };
    match (&**lhs, &**rhs) {
        (Expr::Column(c), Expr::Value(v)) | (Expr::Value(v), Expr::Column(c))
            if c.source.is_none() && c.name == pk.as_str() && !v.is_null() =>
        {
            Some(vec![v.clone()])
        }
        _ => None,
    }
}

async fn write_returning(
    conn: &mut MySqlConnection,
    info: &TableInfo,
    plan: &WritePlan,
    prepared: Prepared,
) -> Result<ExecResult, OrmError> {
    let columns = plan.returning();
    let Prepared { write, read } = prepared;
    match plan {
        WritePlan::Insert(p) => {
            if let Some(row) = synthesize(info, p) {
                let done = run(&mut *conn, &write.sql, write.params).await?;
                return synthesized_result(info, row, &done);
            }
            let keys = InsertKeys::of(info, p)?;
            let done = run(&mut *conn, &write.sql, write.params).await?;
            let inserted = done.rows_affected();
            let keys = match keys {
                InsertKeys::Supplied(keys) => keys,
                InsertKeys::Generated => {
                    generated_keys(conn, done.last_insert_id(), inserted).await?
                }
            };
            let rows = read_by_keys(conn, info, columns, &keys).await?;
            if rows.len() as u64 != inserted {
                return Err(QueryError::Model(format!(
                    "inserted {inserted} rows into `{}` but read back {}",
                    info.table,
                    rows.len()
                ))
                .into());
            }
            Ok(ExecResult {
                rows_affected: inserted,
                returning: rows,
            })
        }
        WritePlan::Update(p) => {
            if p.assignments
                .iter()
                .any(|(column, _)| info.primary_key.iter().any(|k| column == k.as_str()))
            {
                return Err(QueryError::InvalidPlan(
                    "MySQL cannot return the rows of an update that changes the primary key".into(),
                )
                .into());
            }
            let keys = match key_equality(info, p.filter.as_ref()) {
                Some(keys) => vec![keys],
                None => read_keys(conn, info, p.filter.as_ref()).await?,
            };
            if keys.is_empty() {
                return Ok(affected_result(0));
            }
            let done = run(&mut *conn, &write.sql, write.params).await?;
            let rows = if done.rows_affected() == 0 {
                Vec::new()
            } else {
                read_by_keys(conn, info, columns, &keys).await?
            };
            Ok(ExecResult {
                rows_affected: done.rows_affected(),
                returning: rows,
            })
        }
        WritePlan::Delete(_) => {
            // The rows are gone afterwards, so they are read (and locked) first.
            let read = read.ok_or_else(|| QueryError::Model("missing delete read".into()))?;
            let rows = fetch_rows(&mut *conn, &read.sql, read.params).await?.rows;
            let done = run(&mut *conn, &write.sql, write.params).await?;
            Ok(ExecResult {
                rows_affected: done.rows_affected(),
                returning: rows,
            })
        }
    }
}

/// A returned column of a single-row `INSERT` whose stored value is known
/// without reading the row back.
enum Known {
    /// The supplied value, stored unchanged.
    Supplied(Value),
    /// The omitted `AUTO_INCREMENT` column: `LAST_INSERT_ID()`.
    Generated,
}

/// The row a single-row `INSERT` returns, if every returned column is known
/// without a read-back (see the module docs). `None` means: read it back.
fn synthesize(info: &TableInfo, plan: &siderite_orm::InsertPlan) -> Option<Vec<(String, Known)>> {
    let [row] = plan.rows.as_slice() else {
        return None;
    };
    if info.insert_triggers {
        return None;
    }
    plan.returning
        .iter()
        .map(|column| {
            let name: &str = column;
            let auto = info.auto_increment.as_deref() == Some(name);
            let known = match plan.columns.iter().position(|c| c == column) {
                // A supplied `AUTO_INCREMENT` value of 0 or NULL is generated.
                Some(_) if auto => return None,
                Some(i) => {
                    let value = row.get(i)?;
                    let meta = info.columns.iter().find(|c| c.name == name)?;
                    if !meta.stores_exactly(value) {
                        return None;
                    }
                    Known::Supplied(value.clone())
                }
                None if auto => Known::Generated,
                // Omitted: a default or a generated column.
                None => return None,
            };
            Some((name.to_owned(), known))
        })
        .collect()
}

/// The result of a single-row `INSERT` whose row is [`synthesize`]d.
fn synthesized_result(
    info: &TableInfo,
    row: Vec<(String, Known)>,
    done: &MySqlQueryResult,
) -> Result<ExecResult, OrmError> {
    if done.rows_affected() != 1 {
        return Err(QueryError::Model(format!(
            "inserted {} rows into `{}` instead of 1",
            done.rows_affected(),
            info.table
        ))
        .into());
    }
    let columns = row
        .into_iter()
        .map(|(name, known)| {
            let value = match known {
                Known::Supplied(value) => value,
                Known::Generated => match i64::try_from(done.last_insert_id()) {
                    Ok(id) if id != 0 => Value::Int(id),
                    _ => {
                        return Err(QueryError::Model(format!(
                            "no usable generated key for `{}`",
                            info.table
                        )));
                    }
                },
            };
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, QueryError>>()?;
    Ok(ExecResult {
        rows_affected: 1,
        returning: vec![Row::new(columns)],
    })
}

/// How the keys of the rows of an `INSERT` are known.
enum InsertKeys {
    /// The statement supplies them (one entry per row, in primary-key order).
    Supplied(Vec<Vec<Value>>),
    /// The primary key is `AUTO_INCREMENT` and omitted.
    Generated,
}

impl InsertKeys {
    fn of(info: &TableInfo, plan: &siderite_orm::InsertPlan) -> Result<Self, QueryError> {
        let positions: Option<Vec<usize>> = info
            .primary_key
            .iter()
            .map(|key| plan.columns.iter().position(|c| c == key.as_str()))
            .collect();
        if let Some(positions) = positions {
            let keys = plan
                .rows
                .iter()
                .map(|row| positions.iter().map(|&i| row[i].clone()).collect())
                .collect();
            return Ok(Self::Supplied(keys));
        }
        match (info.primary_key.as_slice(), &info.auto_increment) {
            ([key], Some(auto)) if key == auto => Ok(Self::Generated),
            _ => Err(QueryError::Model(format!(
                "cannot read back rows inserted into `{}`: its primary key is neither supplied \
                 nor AUTO_INCREMENT",
                info.table
            ))),
        }
    }
}

/// Keys `first, first + step, ...` of a multi-row insert of `count` rows.
///
/// InnoDB allocates the block of values of a plain `INSERT .. VALUES` in one
/// go under every `innodb_autoinc_lock_mode`, and `LAST_INSERT_ID()` is the
/// first of them.
async fn generated_keys(
    conn: &mut MySqlConnection,
    first: u64,
    count: u64,
) -> Result<Vec<Vec<Value>>, OrmError> {
    let overflow = || QueryError::Model("generated key out of range".into());
    let step: i64 = if count > 1 {
        sqlx::query_scalar::<_, i64>("SELECT CAST(@@auto_increment_increment AS SIGNED)")
            .fetch_one(&mut *conn)
            .await
            .map_err(db_error)?
    } else {
        1
    };
    let first = i64::try_from(first).map_err(|_| overflow())?;
    (0..i64::try_from(count).map_err(|_| overflow())?)
        .map(|i| {
            i.checked_mul(step)
                .and_then(|offset| first.checked_add(offset))
                .map(|id| vec![Value::Int(id)])
                .ok_or_else(|| overflow().into())
        })
        .collect()
}

/// Primary keys of the rows `filter` selects, read with `FOR UPDATE` (the
/// filter of an `UPDATE`).
async fn read_keys(
    conn: &mut MySqlConnection,
    info: &TableInfo,
    filter: Option<&Expr>,
) -> Result<Vec<Vec<Value>>, OrmError> {
    let read = compile(
        &locking_read(&info.table, filter, &info.primary_key),
        &MySql,
    )?;
    let rows = fetch_rows(&mut *conn, &read.sql, read.params).await?.rows;
    rows.iter()
        .map(|row| {
            info.primary_key
                .iter()
                .map(|key| {
                    row.get(key).cloned().ok_or_else(|| {
                        QueryError::Model(format!("key column `{key}` missing from read")).into()
                    })
                })
                .collect()
        })
        .collect()
}

fn ident(out: &mut String, name: &str) {
    use crate::sql::Dialect as _;
    MySql.write_ident(out, name);
}

/// Read `columns` of the rows with the given primary keys, in key order.
async fn read_by_keys(
    conn: &mut MySqlConnection,
    info: &TableInfo,
    columns: &[siderite_orm::expr::Ident],
    keys: &[Vec<Value>],
) -> Result<Vec<Row>, OrmError> {
    let mut sql = String::from("SELECT ");
    for (i, column) in columns.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        ident(&mut sql, column);
    }
    sql.push_str(" FROM ");
    ident(&mut sql, &info.table);
    sql.push_str(" WHERE ");
    let width = info.primary_key.len();
    let tuple = |sql: &mut String| {
        sql.push('(');
        for i in 0..width {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
        }
        sql.push(')');
    };
    if width == 1 {
        ident(&mut sql, &info.primary_key[0]);
    } else {
        sql.push('(');
        for (i, key) in info.primary_key.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            ident(&mut sql, key);
        }
        sql.push(')');
    }
    sql.push_str(" IN (");
    for i in 0..keys.len() {
        if i > 0 {
            sql.push_str(", ");
        }
        if width == 1 {
            sql.push('?');
        } else {
            tuple(&mut sql);
        }
    }
    sql.push_str(") ORDER BY ");
    for (i, key) in info.primary_key.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        ident(&mut sql, key);
    }
    let params: Vec<Value> = keys.iter().flatten().cloned().collect();
    let mut rows = fetch_rows(&mut *conn, &sql, params).await?.rows;
    // Supplied keys keep their statement order (`ORDER BY` above is the key
    // order, which is insertion order for generated keys).
    let position: HashMap<String, usize> = keys
        .iter()
        .enumerate()
        .map(|(i, key)| (key_text(key.iter()), i))
        .collect();
    let in_row = |row: &Row| -> Option<usize> {
        let key: Option<Vec<&Value>> = info.primary_key.iter().map(|k| row.get(k)).collect();
        position.get(&key_text(key?.into_iter())).copied()
    };
    if rows.iter().all(|row| in_row(row).is_some()) {
        rows.sort_by_key(|row| in_row(row));
    }
    Ok(rows)
}

/// Comparable text of a key, alike for a bound value and its decoded form
/// (a `Uuid` is bound as text and read back as `Text`).
fn key_text<'a>(key: impl Iterator<Item = &'a Value>) -> String {
    key.map(|value| {
        canonical_text(value).unwrap_or_else(|| match value {
            Value::Text(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => i64::from(*b).to_string(),
            other => format!("{other:?}"),
        })
    })
    .collect::<Vec<_>>()
    .join("\u{1f}")
}

// ---- decoding ---------------------------------------------------------------

fn decode_row(row: &MySqlRow) -> Result<Row, QueryError> {
    row.columns()
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let name = col.name().to_owned();
            let raw = row.try_get_raw(i).map_err(|e| QueryError::Decode {
                column: name.clone(),
                reason: e.to_string(),
            })?;
            let value = if raw.is_null() {
                Value::Null
            } else {
                decode_value(row, i, raw).map_err(|reason| QueryError::Decode {
                    column: name.clone(),
                    reason,
                })?
            };
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Row::new)
}

/// Decode a non-null column by its MySQL type name.
fn decode_value(row: &MySqlRow, i: usize, raw: MySqlValueRef<'_>) -> Result<Value, String> {
    /// `try_get` as `$t`, converted with `$into`.
    macro_rules! get {
        ($t:ty => $into:expr) => {
            row.try_get::<$t, _>(i)
                .map($into)
                .map_err(|e| e.to_string())
        };
    }
    let type_info = raw.type_info();
    match type_info.name() {
        "BOOLEAN" => get!(bool => Value::Bool),
        "TINYINT" => get!(i8 => |v| Value::Int(i64::from(v))),
        "SMALLINT" => get!(i16 => |v| Value::Int(i64::from(v))),
        "INT" | "MEDIUMINT" => get!(i32 => |v| Value::Int(i64::from(v))),
        "BIGINT" => get!(i64 => Value::Int),
        "TINYINT UNSIGNED" => get!(u8 => |v| Value::Int(i64::from(v))),
        "SMALLINT UNSIGNED" | "YEAR" => get!(u16 => |v| Value::Int(i64::from(v))),
        "INT UNSIGNED" | "MEDIUMINT UNSIGNED" => get!(u32 => |v| Value::Int(i64::from(v))),
        "BIGINT UNSIGNED" => row
            .try_get::<u64, _>(i)
            .map_err(|e| e.to_string())
            .and_then(|v| {
                i64::try_from(v)
                    .map(Value::Int)
                    .map_err(|_| format!("{v} does not fit a 64-bit signed integer"))
            }),
        "FLOAT" => get!(f32 => |v| Value::Float(f64::from(v))),
        "DOUBLE" => get!(f64 => Value::Float),
        "DECIMAL" => get!(Decimal => Value::Decimal),
        "CHAR" | "VARCHAR" | "TINYTEXT" | "TEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET" => {
            get!(String => Value::Text)
        }
        "BINARY" | "VARBINARY" | "TINYBLOB" | "BLOB" | "MEDIUMBLOB" | "LONGBLOB" => {
            get!(Vec<u8> => Value::Bytes)
        }
        "DATE" => get!(NaiveDate => Value::Date),
        "TIME" => get!(NaiveTime => Value::Time),
        "DATETIME" => get!(NaiveDateTime => |v| Value::Timestamp(v.and_utc())),
        "TIMESTAMP" => get!(DateTime<Utc> => Value::Timestamp),
        "JSON" => get!(JsonValue => Value::Json),
        other => Err(format!("unsupported MySQL type {other}")),
    }
}
