//! Throw-away databases for tests: schema setup and transactional isolation.

use siderite_migrations::{
    MigrationError, MigrationGraph, Migrator, ProjectState, diff, load_dir, schema_editor,
};
use siderite_orm::signals::Signals;
use siderite_orm::{BackendError, Db, ModelMeta, OrmError};
use std::convert::Infallible;
use std::future::Future;
use std::path::Path;
use thiserror::Error;

/// Failure while preparing or using a [`TestDatabase`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TestDatabaseError {
    /// The backend could not be opened.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// A statement or transaction operation failed.
    #[error(transparent)]
    Orm(#[from] OrmError),
    /// Loading or applying migrations failed, or DDL could not be rendered.
    #[error(transparent)]
    Migration(#[from] MigrationError),
}

/// A disposable database for one test.
///
/// # Lifetime model
///
/// A [`TestDatabase`] owns one [`Db`] handle. [`TestDatabase::sqlite_memory`]
/// uses a single shared in-memory connection, so the database lives exactly as
/// long as the last clone of the handle and every clone sees the same data.
///
/// [`TestDatabase::isolated`] opens a transaction, hands the closure a `Db`
/// bound to it and **always rolls back** when the closure returns, so tests
/// never observe each other's writes. Because an in-memory database has one
/// connection, the transaction holds it: use only the `Db` given to the
/// closure inside it (querying the outer handle would wait forever), and do
/// not keep clones of the transactional `Db` after the closure finishes.
#[derive(Debug, Clone)]
pub struct TestDatabase {
    db: Db,
}

/// Internal outcome that forces `Db::transaction` to roll back while still
/// carrying the closure's value out.
enum Rollback<T> {
    Done(T),
    Failed(OrmError),
}

impl<T> From<OrmError> for Rollback<T> {
    fn from(err: OrmError) -> Self {
        Self::Failed(err)
    }
}

impl TestDatabase {
    /// Wrap an existing handle (any backend).
    pub fn from_db(db: Db) -> Self {
        Self { db }
    }

    /// Open a fresh shared in-memory SQLite database (single connection).
    ///
    /// # Errors
    /// [`TestDatabaseError::Backend`] if the pool cannot be created.
    #[cfg(feature = "sqlite")]
    pub async fn sqlite_memory() -> Result<Self, TestDatabaseError> {
        let backend = siderite_backends::sqlite::SqliteBackend::connect("sqlite::memory:").await?;
        Ok(Self::from_db(Db::new(backend)))
    }

    /// The underlying handle (cheap to clone).
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Consume the wrapper and return the handle.
    pub fn into_db(self) -> Db {
        self.db
    }

    /// Attach `signals` so model signals fire for this database. Do this
    /// before cloning the handle out; it applies to this handle and later
    /// clones only.
    #[must_use]
    pub fn with_signals(self, signals: Signals) -> Self {
        Self {
            db: self.db.with_signals(signals),
        }
    }

    /// Create the tables (and indexes, constraints, M2M join tables) for
    /// `models` from their compiled metadata, as `makemigrations` +
    /// `migrate` would.
    ///
    /// # Errors
    /// [`TestDatabaseError::Migration`] if DDL cannot be rendered for the
    /// backend, or [`TestDatabaseError::Orm`] if a statement fails.
    pub async fn with_models(self, models: &[&ModelMeta]) -> Result<Self, TestDatabaseError> {
        let target = ProjectState::from_metas(models);
        let operations = diff(&ProjectState::new(), &target)?;
        let kind = self.db.capabilities().kind;
        for sql in schema_editor::statements(kind, &ProjectState::new(), &operations)? {
            self.db.execute_script(&sql).await?;
        }
        Ok(self)
    }

    /// Apply every migration file found in `dir`.
    ///
    /// # Errors
    /// [`TestDatabaseError::Migration`] for unreadable, inconsistent or
    /// failing migrations.
    pub async fn with_migrations(self, dir: impl AsRef<Path>) -> Result<Self, TestDatabaseError> {
        let graph = MigrationGraph::build(load_dir(dir.as_ref())?)?;
        Migrator::new(&self.db, &graph).migrate(None, false).await?;
        Ok(self)
    }

    /// Run `f` inside a transaction that is **always rolled back**, returning
    /// `f`'s value. See the [`TestDatabase`] docs for the lifetime model.
    ///
    /// Panics in `f` also discard the transaction.
    ///
    /// # Errors
    /// [`TestDatabaseError::Orm`] if the transaction cannot begin or roll back.
    pub async fn isolated<F, Fut, T>(&self, f: F) -> Result<T, TestDatabaseError>
    where
        F: FnOnce(Db) -> Fut,
        Fut: Future<Output = T>,
    {
        let outcome = self
            .db
            .transaction(|tx| async move { Err::<Infallible, _>(Rollback::Done(f(tx).await)) })
            .await;
        match outcome {
            Err(Rollback::Done(value)) => Ok(value),
            Err(Rollback::Failed(err)) => Err(err.into()),
            Ok(never) => match never {},
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use siderite_migrations::{FieldState, ModelState, SqlType};
    use siderite_migrations::{Migration, Operation, write_migration};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn widget_model() -> ModelState {
        let mut id = FieldState::new("id", "id", SqlType::BigInt);
        id.primary_key = true;
        id.auto = true;
        ModelState {
            name: "Widget".into(),
            table: "widgets".into(),
            fields: vec![id, FieldState::new("label", "label", SqlType::Text)],
            indexes: Vec::new(),
            constraints: Vec::new(),
        }
    }

    #[tokio::test]
    async fn with_migrations_applies_files_from_a_directory() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir =
            std::env::temp_dir().join(format!("siderite-testkit-{}-{nanos}", std::process::id()));
        let migration = Migration::new(
            "0001_initial",
            Vec::new(),
            vec![Operation::CreateModel {
                model: widget_model(),
            }],
            true,
            Vec::new(),
        )
        .unwrap();
        write_migration(&dir, &migration).unwrap();

        let test = TestDatabase::sqlite_memory()
            .await
            .unwrap()
            .with_migrations(&dir)
            .await
            .unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        test.db()
            .raw_execute("INSERT INTO widgets (label) VALUES ('a')", vec![])
            .await
            .unwrap();
        let rows = test
            .db()
            .raw_sql("SELECT id FROM widgets", vec![])
            .await
            .unwrap();
        assert_eq!(rows.rows.len(), 1);
    }

    #[tokio::test]
    async fn with_migrations_rejects_an_unreadable_directory_entry() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "siderite-testkit-bad-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("0001_bad.json"), "not json").unwrap();
        let result = TestDatabase::sqlite_memory()
            .await
            .unwrap()
            .with_migrations(&dir)
            .await;
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(result, Err(TestDatabaseError::Migration(_))));
    }

    #[tokio::test]
    async fn memory_database_is_shared_between_clones() {
        let test = TestDatabase::sqlite_memory().await.unwrap();
        test.db()
            .execute_script("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
        let clone = test.clone().into_db();
        clone
            .raw_execute("INSERT INTO t (id) VALUES (1)", vec![])
            .await
            .unwrap();
        let rows = test.db().raw_sql("SELECT id FROM t", vec![]).await.unwrap();
        assert_eq!(rows.rows.len(), 1);
    }

    #[tokio::test]
    async fn isolated_rolls_back_and_returns_value() {
        let test = TestDatabase::sqlite_memory().await.unwrap();
        test.db()
            .execute_script("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
        let inside = test
            .isolated(|db| async move {
                db.raw_execute("INSERT INTO t (id) VALUES (1)", vec![])
                    .await
                    .unwrap();
                assert!(db.in_transaction());
                db.raw_sql("SELECT id FROM t", vec![])
                    .await
                    .unwrap()
                    .rows
                    .len()
            })
            .await
            .unwrap();
        assert_eq!(inside, 1);
        let after = test.db().raw_sql("SELECT id FROM t", vec![]).await.unwrap();
        assert!(after.rows.is_empty());
        // The connection is released: a second isolated run works.
        test.isolated(|_| async {}).await.unwrap();
    }
}
