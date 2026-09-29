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
//! * **`UPDATE` / `DELETE`**: the affected keys are read `FOR UPDATE` in a
//!   transaction (an `UPDATE` by primary-key equality skips that), the
//!   statement runs, and the rows are read back (after an `UPDATE`) or were
//!   read beforehand (`DELETE`).
//!
//! The primary key and the auto-increment column are looked up once per table
//! in `information_schema` and cached; [`execute_script`](Executor::execute_script)
//! clears the cache. A table without a primary key cannot use the emulation.
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

use crate::shared::{TxSlot, affected_result, map_error, with_tx};
use crate::sql::{CompiledQuery, MySql, compile, compile_write};
use async_trait::async_trait;
use axumapi_orm::types::canonical_text;
use axumapi_orm::{
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
    /// # Errors
    /// [`BackendError::Connection`] if the pool cannot be created.
    pub async fn connect(url: &str) -> Result<Self, BackendError> {
        Self::connect_with(url, MySqlPoolOptions::new().max_connections(10)).await
    }

    /// Like [`connect`](Self::connect) with explicit pool options (pool size,
    /// timeouts). The session setup is added to `options`.
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
        if writes_one_row_by_key(plan, &info) {
            // One write plus a read: no transaction needed.
            let mut conn = self.pool.acquire().await.map_err(db_error)?;
            write_returning(&mut conn, &info, plan, prepared).await
        } else {
            let mut tx = self.pool.begin().await.map_err(db_error)?;
            let result = write_returning(&mut tx, &info, plan, prepared).await?;
            tx.commit().await.map_err(db_error)?;
            Ok(result)
        }
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
                    .require(axumapi_orm::Feature::Isolation(level))?;
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

/// Primary key and generated column of a table.
#[derive(Debug)]
struct TableInfo {
    table: String,
    primary_key: Vec<String>,
    auto_increment: Option<String>,
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
             CAST(EXTRA AS CHAR) AS e FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? ORDER BY ORDINAL_POSITION",
        )
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
        };
        for row in &rows {
            let column: String = row.try_get("c").map_err(db_error)?;
            let key: String = row.try_get("k").map_err(db_error)?;
            let extra: String = row.try_get("e").map_err(db_error)?;
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
    /// `UPDATE` / `DELETE`: the locking read of the affected rows.
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
        WritePlan::Insert(_) => None,
        WritePlan::Update(p) => Some(locking_read(&p.table, p.filter.as_ref(), &[])),
        WritePlan::Delete(p) => Some(locking_read(&p.table, p.filter.as_ref(), &p.returning)),
    };
    let read = read.map(|plan| compile(&plan, &MySql)).transpose()?;
    Ok(Prepared { write, read })
}

/// `SELECT columns FROM table WHERE filter FOR UPDATE`; with no `columns`
/// the projection is a placeholder that [`write_returning`] never uses (the
/// primary key is selected instead, see [`key_read`]).
fn locking_read(
    table: &str,
    filter: Option<&Expr>,
    columns: &[axumapi_orm::expr::Ident],
) -> QueryPlan {
    let mut plan = QueryPlan::from_table(table.to_owned());
    plan.filter = filter.cloned();
    plan.lock = Some(LockMode::ForUpdate);
    for column in columns {
        plan = plan.select(Expr::col(column.clone()), None);
    }
    plan
}

/// Whether the write is an `UPDATE` addressing one row by its primary key
/// (what `save` issues), or an `INSERT`: both are one statement plus a read.
fn writes_one_row_by_key(plan: &WritePlan, info: &TableInfo) -> bool {
    match plan {
        WritePlan::Insert(_) => true,
        WritePlan::Update(p) => key_equality(info, p.filter.as_ref()).is_some(),
        WritePlan::Delete(_) => false,
    }
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
                None => match read {
                    Some(read) => read_keys(conn, info, &read).await?,
                    None => Vec::new(),
                },
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

/// How the keys of the rows of an `INSERT` are known.
enum InsertKeys {
    /// The statement supplies them (one entry per row, in primary-key order).
    Supplied(Vec<Vec<Value>>),
    /// The primary key is `AUTO_INCREMENT` and omitted.
    Generated,
}

impl InsertKeys {
    fn of(info: &TableInfo, plan: &axumapi_orm::InsertPlan) -> Result<Self, QueryError> {
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

/// Primary keys of the rows `read` selects (a `FOR UPDATE` read of the
/// filter of an `UPDATE`).
async fn read_keys(
    conn: &mut MySqlConnection,
    info: &TableInfo,
    read: &CompiledQuery,
) -> Result<Vec<Vec<Value>>, OrmError> {
    // The prepared read has no projection: swap in the key columns.
    let mut sql = String::from("SELECT ");
    for (i, key) in info.primary_key.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        ident(&mut sql, key);
    }
    let from = read
        .sql
        .strip_prefix("SELECT * FROM")
        .ok_or_else(|| QueryError::Model("unexpected locking read".into()))?;
    sql.push_str(" FROM");
    sql.push_str(from);
    let rows = fetch_rows(&mut *conn, &sql, read.params.clone())
        .await?
        .rows;
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
    columns: &[axumapi_orm::expr::Ident],
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
    let type_name = raw.type_info().name().to_owned();
    match type_name.as_str() {
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
