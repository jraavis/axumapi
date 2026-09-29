//! SQLite backend built on SQLx.

use crate::sql::{Sqlite, compile};
use async_trait::async_trait;
use axumapi_orm::{
    Backend, BackendCapabilities, BackendError, OrmError, QueryError, QueryPlan, QueryResult, Row,
    Value,
};
use sqlx::sqlite::{SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{Column as _, Row as _, TypeInfo as _, ValueRef as _};

/// SQLite adapter executing compiled plans on a connection pool.
#[derive(Debug, Clone)]
pub struct SqliteBackend {
    pool: SqlitePool,
}

impl SqliteBackend {
    /// Connect to `url` (e.g. `sqlite::memory:` or `sqlite://app.db`).
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

    /// Execute raw DDL/DML with **no** parameters (schema setup, migrations).
    ///
    /// # Errors
    /// Returns [`BackendError::Database`] on failure.
    pub async fn execute_script(&self, sql: &str) -> Result<(), BackendError> {
        sqlx::raw_sql(sql)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(map_err)
    }
}

#[async_trait]
impl Backend for SqliteBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::sqlite()
    }

    async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        let compiled = compile(plan, &Sqlite)?;
        let mut query = sqlx::query(&compiled.sql);
        for param in compiled.params {
            query = match param {
                Value::Null => query.bind(None::<i64>),
                Value::Bool(b) => query.bind(b),
                Value::Int(i) => query.bind(i),
                Value::Float(f) => query.bind(f),
                Value::Text(s) => query.bind(s),
                Value::Bytes(b) => query.bind(b),
                Value::Json(j) => query.bind(j.to_string()),
            };
        }
        let rows = query.fetch_all(&self.pool).await.map_err(map_err)?;
        let rows = rows.iter().map(decode_row).collect::<Result<_, _>>()?;
        Ok(QueryResult { rows })
    }
}

fn map_err(e: sqlx::Error) -> BackendError {
    match &e {
        sqlx::Error::Database(db) if db.constraint().is_some() || db.is_unique_violation() => {
            BackendError::Constraint(db.message().to_owned())
        }
        _ => BackendError::Database(e.to_string()),
    }
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
    use axumapi_orm::expr::Field;
    use axumapi_orm::{BackendCapabilityError, Expr, LockMode, OrderDirection};

    struct User;
    #[allow(non_upper_case_globals)]
    impl User {
        const name: Field<User, String> = Field::new("name");
        const age: Field<User, i64> = Field::new("age");
    }

    async fn seeded() -> SqliteBackend {
        let db = SqliteBackend::connect("sqlite::memory:").await.unwrap();
        db.execute_script(
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
