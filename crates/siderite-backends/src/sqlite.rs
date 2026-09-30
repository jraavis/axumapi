//! SQLite backend built on SQLx.
//!
//! Types without a native SQLite storage class are bound in their canonical
//! text form ([`canonical_text`]), so they compare and sort correctly and
//! decode through [`DbType::from_value`](siderite_orm::DbType::from_value).

use crate::shared::{TxSlot, affected_result, map_error, returning_result, with_tx};
use crate::sql::{CompiledQuery, Sqlite, compile, compile_write};
use async_trait::async_trait;
use siderite_orm::types::canonical_text;
use siderite_orm::{
    Backend, BackendCapabilities, BackendError, ExecResult, Executor, IsolationLevel, OrmError,
    QueryError, QueryPlan, QueryResult, Row, Transaction, Value, WritePlan,
};
use sqlx::sqlite::{SqliteArguments, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};

/// SQLite adapter executing compiled plans on a connection pool.
#[derive(Debug, Clone)]
pub struct SqliteBackend {
    pool: SqlitePool,
}

impl SqliteBackend {
    /// Connect to `url` (e.g. `sqlite::memory:` or `sqlite://app.db?mode=rwc`).
    ///
    /// SQLx enables foreign-key enforcement on every SQLite connection.
    ///
    /// # Errors
    /// Returns [`BackendError::Connection`] if the pool cannot be created.
    pub async fn connect(url: &str) -> Result<Self, BackendError> {
        // In-memory databases are per-connection, so a single connection keeps
        // state consistent for `sqlite::memory:`.
        let max = if url.contains(":memory:") { 1 } else { 10 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max)
            .connect(url)
            .await
            .map_err(|e| BackendError::Connection(e.to_string()))?;
        Ok(Self { pool })
    }
}

#[async_trait]
impl Executor for SqliteBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::sqlite()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &Sqlite)?;
        fetch_rows(&self.pool, &compiled.sql, compiled.params).await
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &Sqlite)?;
        run_write(&self.pool, compiled, !plan.returning().is_empty()).await
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        fetch_rows(&self.pool, sql, params).await
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        execute_rows(&self.pool, sql, params).await
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        script_result(sqlx::Executor::execute(&self.pool, sql).await)
    }
}

#[async_trait]
impl Backend for SqliteBackend {
    async fn begin(
        &self,
        isolation: Option<IsolationLevel>,
    ) -> Result<Box<dyn Transaction>, OrmError> {
        if let Some(level) = isolation {
            self.capabilities()
                .require(siderite_orm::Feature::Isolation(level))?;
        }
        let tx = self.pool.begin().await.map_err(map_error)?;
        Ok(Box::new(SqliteTransaction(TxSlot::new(tx))))
    }
}

/// An open SQLite transaction. Dropped without commit, it rolls back.
struct SqliteTransaction(TxSlot<sqlx::Sqlite>);

#[async_trait]
impl Executor for SqliteTransaction {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::sqlite()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &Sqlite)?;
        with_tx!(self.0, conn => fetch_rows(conn, &compiled.sql, compiled.params).await)
    }

    async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let compiled = compile_write(plan, &Sqlite)?;
        let returning = !plan.returning().is_empty();
        with_tx!(self.0, conn => run_write(conn, compiled, returning).await)
    }

    async fn fetch_raw(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        with_tx!(self.0, conn => fetch_rows(conn, sql, params).await)
    }

    async fn execute_raw(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        with_tx!(self.0, conn => execute_rows(conn, sql, params).await)
    }

    async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        with_tx!(self.0, conn => script_result(sqlx::Executor::execute(conn, sql).await))
    }
}

#[async_trait]
impl Transaction for SqliteTransaction {
    async fn commit(&self) -> Result<(), OrmError> {
        self.0.commit().await
    }

    async fn rollback(&self) -> Result<(), OrmError> {
        self.0.rollback().await
    }
}

fn bind_all(
    sql: &str,
    params: Vec<Value>,
) -> sqlx::query::Query<'_, sqlx::Sqlite, SqliteArguments<'_>> {
    params.into_iter().fold(sqlx::query(sql), |query, param| {
        if let Some(text) = canonical_text(&param) {
            return query.bind(text);
        }
        match param {
            Value::Bool(b) => query.bind(b),
            Value::Int(i) => query.bind(i),
            Value::Float(f) => query.bind(f),
            Value::Text(s) => query.bind(s),
            Value::Bytes(b) => query.bind(b),
            // `canonical_text` covered every other variant.
            _ => query.bind(None::<i64>),
        }
    })
}

async fn fetch_rows<'c, E>(ex: E, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::Sqlite>,
{
    let rows = bind_all(sql, params)
        .fetch_all(ex)
        .await
        .map_err(map_error)?;
    let rows = rows.iter().map(decode_row).collect::<Result<_, _>>()?;
    Ok(QueryResult { rows })
}

async fn execute_rows<'c, E>(ex: E, sql: &str, params: Vec<Value>) -> Result<u64, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::Sqlite>,
{
    let done = bind_all(sql, params).execute(ex).await.map_err(map_error)?;
    Ok(done.rows_affected())
}

async fn run_write<'c, E>(
    ex: E,
    compiled: CompiledQuery,
    returning: bool,
) -> Result<ExecResult, OrmError>
where
    E: sqlx::Executor<'c, Database = sqlx::Sqlite>,
{
    if returning {
        let rows = fetch_rows(ex, &compiled.sql, compiled.params).await?.rows;
        Ok(returning_result(rows))
    } else {
        Ok(affected_result(
            execute_rows(ex, &compiled.sql, compiled.params).await?,
        ))
    }
}

fn script_result(
    done: Result<sqlx::sqlite::SqliteQueryResult, sqlx::Error>,
) -> Result<(), OrmError> {
    done.map(|_| ()).map_err(|e| map_error(e).into())
}

fn decode_row(row: &SqliteRow) -> Result<Row, QueryError> {
    row.columns()
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let name = col.name().to_owned();
            let decode_err = |e: sqlx::Error| QueryError::Decode {
                column: name.clone(),
                reason: e.to_string(),
            };
            let raw = row.try_get_raw(i).map_err(decode_err)?;
            let value = if raw.is_null() {
                Value::Null
            } else {
                match raw.type_info().name() {
                    "INTEGER" | "BOOLEAN" => Value::Int(row.try_get(i).map_err(decode_err)?),
                    "REAL" => Value::Float(row.try_get(i).map_err(decode_err)?),
                    "BLOB" => Value::Bytes(row.try_get(i).map_err(decode_err)?),
                    _ => Value::Text(row.try_get(i).map_err(decode_err)?),
                }
            };
            Ok((name, value))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Row::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use siderite_orm::expr::Field;
    use siderite_orm::{BackendCapabilityError, Expr, LockMode, OrderDirection};

    struct User;
    #[allow(non_upper_case_globals)]
    impl User {
        const name: Field<User, String> = Field::new("name");
        const age: Field<User, i64> = Field::new("age");
    }

    async fn seeded() -> SqliteBackend {
        let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
        Executor::execute_script(&db,
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, age INTEGER);
             INSERT INTO users (name, age) VALUES ('Alice', 30), ('bob', NULL), ('ALINA', 22), ('50%', 1);",
        )
        .await
        .unwrap();
        db
    }

    fn names(r: &QueryResult) -> Vec<String> {
        r.rows
            .iter()
            .map(|row| match row.get("name") {
                Some(Value::Text(s)) => s.clone(),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[tokio::test]
    async fn executes_filters_ordering_and_limits() {
        let db = seeded().await;
        let plan = QueryPlan::from_table("users")
            .filter(User::name.icontains("al"))
            .order_by(User::age, OrderDirection::Asc);
        assert_eq!(names(&db.fetch(&plan).await.unwrap()), ["ALINA", "Alice"]);

        let cs = QueryPlan::from_table("users").filter(User::name.contains("Al"));
        assert_eq!(names(&db.fetch(&cs).await.unwrap()), ["Alice"]);

        let nulls = QueryPlan::from_table("users").filter(User::age.is_null());
        assert_eq!(names(&db.fetch(&nulls).await.unwrap()), ["bob"]);

        let wildcard = QueryPlan::from_table("users").filter(User::name.icontains("%"));
        assert_eq!(names(&db.fetch(&wildcard).await.unwrap()), ["50%"]);

        let page = QueryPlan::from_table("users")
            .order_by(Expr::col("id"), OrderDirection::Asc)
            .offset(3);
        assert_eq!(names(&db.fetch(&page).await.unwrap()), ["50%"]);
    }

    #[tokio::test]
    async fn decodes_nulls_and_integers() {
        let db = seeded().await;
        let r = db
            .fetch(&QueryPlan::from_table("users").filter(User::name.eq("bob")))
            .await
            .unwrap();
        assert_eq!(r.rows[0].get("age"), Some(&Value::Null));
        assert_eq!(r.rows[0].get("id"), Some(&Value::Int(2)));
    }

    #[tokio::test]
    async fn row_locking_is_rejected_before_io() {
        let db = seeded().await;
        let err = db
            .fetch(&QueryPlan::from_table("users").lock(LockMode::ForUpdate))
            .await;
        assert!(matches!(
            err,
            Err(OrmError::Capability(
                BackendCapabilityError::RowLockingUnsupported { .. }
            ))
        ));
    }
}
