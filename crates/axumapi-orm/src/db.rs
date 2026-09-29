//! [`Db`]: the database handle application code passes around.
//!
//! A `Db` is a cheap clone that points either at a connection pool or at an
//! open transaction. Every ORM entry point takes `&Db`, so the same code runs
//! inside or outside a transaction:
//!
//! ```ignore
//! db.transaction(|tx| async move {
//!     let user = User::objects(&tx).get(User::id.eq(1)).await?;
//!     Post::objects(&tx).filter(Post::author.eq(user.id)).delete().await?;
//!     Ok::<_, OrmError>(())
//! }).await?;
//! ```
//!
//! Inside the closure use `tx`, not the outer `db`: the outer handle would
//! take a *second* connection, which deadlocks on a one-connection pool such
//! as `sqlite::memory:`.

use crate::backend::{Backend, ExecResult, Executor, QueryResult, Transaction};
use crate::capabilities::{BackendCapabilities, Feature, IsolationLevel, TransactionSupport};
use crate::error::{BackendCapabilityError, OrmError, QueryError};
use crate::plan::QueryPlan;
use crate::value::Value;
use crate::write::WritePlan;
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

type Hook = Box<dyn FnOnce() + Send>;

/// Callbacks registered with [`Db::on_commit`] for one transaction level.
#[derive(Default)]
struct Hooks(Mutex<Vec<Hook>>);

impl Hooks {
    fn push(&self, hook: Hook) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(hook);
    }

    fn take(&self) -> Vec<Hook> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

struct TxState {
    tx: Arc<dyn Transaction>,
    /// Hooks of this level; moved to the parent on savepoint release.
    hooks: Arc<Hooks>,
    /// Shared savepoint name counter for the whole transaction.
    savepoints: Arc<AtomicU32>,
}

#[derive(Clone)]
enum Target {
    Pool(Arc<dyn Backend>),
    Tx(Arc<TxState>),
}

/// Handle to a database: a pool, or an open transaction.
#[derive(Clone)]
pub struct Db {
    target: Target,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = self.capabilities().kind;
        f.debug_struct("Db")
            .field("backend", &kind)
            .field("in_transaction", &self.in_transaction())
            .finish()
    }
}

impl Db {
    /// Wrap a backend (connection pool).
    pub fn new(backend: impl Backend) -> Self {
        Self::from_arc(Arc::new(backend))
    }

    /// Wrap a shared backend.
    pub fn from_arc(backend: Arc<dyn Backend>) -> Self {
        Self {
            target: Target::Pool(backend),
        }
    }

    fn executor(&self) -> &dyn Executor {
        match &self.target {
            Target::Pool(pool) => pool.as_ref(),
            Target::Tx(state) => state.tx.as_ref(),
        }
    }

    /// Declared capabilities of the underlying backend.
    pub fn capabilities(&self) -> BackendCapabilities {
        self.executor().capabilities()
    }

    /// Whether this handle runs inside a transaction.
    pub fn in_transaction(&self) -> bool {
        matches!(self.target, Target::Tx(_))
    }

    /// Execute a read plan.
    ///
    /// # Errors
    /// Capability, backend or decode errors.
    pub async fn fetch(&self, plan: &QueryPlan) -> Result<QueryResult, OrmError> {
        self.executor().fetch(plan).await
    }

    /// Execute a write plan.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        self.executor().execute(plan).await
    }

    /// Raw SQL returning rows (`db.raw_sql("SELECT .. WHERE id = ?", params![id])`).
    ///
    /// Placeholders follow the backend (`?` on SQLite, `$1` on PostgreSQL).
    /// Parameters are always bound; never format untrusted input into `sql`.
    ///
    /// # Errors
    /// Backend errors.
    pub async fn raw_sql(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        self.executor().fetch_raw(sql, params).await
    }

    /// Raw SQL returning the affected-row count. Parameters are bound.
    ///
    /// # Errors
    /// Backend errors.
    pub async fn raw_execute(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        self.executor().execute_raw(sql, params).await
    }

    /// Run a parameterless multi-statement script (DDL).
    ///
    /// # Errors
    /// Backend errors.
    pub async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        self.executor().execute_script(sql).await
    }

    /// Run `f` in a transaction: commit on `Ok`, roll back on `Err`.
    ///
    /// Called on a handle that is already in a transaction, this creates a
    /// savepoint instead (requires [`TransactionSupport::Savepoints`]). If `f`
    /// panics, the transaction is dropped and therefore rolled back.
    ///
    /// # Errors
    /// `f`'s error, or an [`OrmError`] from begin/commit/rollback.
    pub async fn transaction<F, Fut, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(Db) -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: From<OrmError>,
    {
        self.run_transaction(None, f).await
    }

    /// [`transaction`](Self::transaction) with an explicit isolation level.
    ///
    /// The level must be listed in [`BackendCapabilities::isolation_levels`]
    /// and cannot be set on a nested (savepoint) transaction.
    ///
    /// # Errors
    /// As [`transaction`](Self::transaction), plus a capability error for an
    /// unsupported level.
    pub async fn transaction_with<F, Fut, T, E>(
        &self,
        isolation: IsolationLevel,
        f: F,
    ) -> Result<T, E>
    where
        F: FnOnce(Db) -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: From<OrmError>,
    {
        self.run_transaction(Some(isolation), f).await
    }

    async fn run_transaction<F, Fut, T, E>(
        &self,
        isolation: Option<IsolationLevel>,
        f: F,
    ) -> Result<T, E>
    where
        F: FnOnce(Db) -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: From<OrmError>,
    {
        match &self.target {
            Target::Pool(pool) => {
                if let Some(level) = isolation {
                    pool.capabilities()
                        .require(Feature::Isolation(level))
                        .map_err(OrmError::from)?;
                }
                let tx: Arc<dyn Transaction> = Arc::from(pool.begin(isolation).await?);
                let state = Arc::new(TxState {
                    tx: Arc::clone(&tx),
                    hooks: Arc::default(),
                    savepoints: Arc::default(),
                });
                let hooks = Arc::clone(&state.hooks);
                let result = f(Db {
                    target: Target::Tx(state),
                })
                .await;
                match result {
                    Ok(value) => {
                        tx.commit().await?;
                        hooks.take().into_iter().for_each(|hook| hook());
                        Ok(value)
                    }
                    Err(err) => {
                        // Keep the caller's error: a failed rollback means the
                        // connection is broken, and dropping `tx` discards it.
                        drop(tx.rollback().await);
                        Err(err)
                    }
                }
            }
            Target::Tx(parent) => {
                if isolation.is_some() {
                    return Err(OrmError::from(QueryError::InvalidPlan(
                        "isolation level cannot be set on a nested transaction".into(),
                    ))
                    .into());
                }
                let caps = parent.tx.capabilities();
                if caps.transactions < TransactionSupport::Savepoints {
                    return Err(OrmError::from(BackendCapabilityError::Unsupported {
                        backend: caps.kind,
                        feature: Feature::Savepoints,
                    })
                    .into());
                }
                let n = parent.savepoints.fetch_add(1, Ordering::Relaxed) + 1;
                let name = format!("axumapi_sp_{n}");
                parent
                    .tx
                    .execute_script(&format!("SAVEPOINT {name}"))
                    .await?;
                let state = Arc::new(TxState {
                    tx: Arc::clone(&parent.tx),
                    hooks: Arc::default(),
                    savepoints: Arc::clone(&parent.savepoints),
                });
                let hooks = Arc::clone(&state.hooks);
                let result = f(Db {
                    target: Target::Tx(state),
                })
                .await;
                match result {
                    Ok(value) => {
                        parent
                            .tx
                            .execute_script(&format!("RELEASE SAVEPOINT {name}"))
                            .await?;
                        hooks.take().into_iter().for_each(|h| parent.hooks.push(h));
                        Ok(value)
                    }
                    Err(err) => {
                        parent
                            .tx
                            .execute_script(&format!("ROLLBACK TO SAVEPOINT {name}"))
                            .await?;
                        Err(err)
                    }
                }
            }
        }
    }

    /// Run `hook` after the outermost transaction commits (Django
    /// `transaction.on_commit`). Outside a transaction it runs immediately.
    /// Hooks of a rolled-back savepoint or transaction are discarded.
    pub fn on_commit(&self, hook: impl FnOnce() + Send + 'static) {
        match &self.target {
            Target::Pool(_) => hook(),
            Target::Tx(state) => state.hooks.push(Box::new(hook)),
        }
    }
}

/// Named databases (`"default"`, `"analytics"`, ...).
///
/// Querysets never mix aliases: pick the handle explicitly with
/// [`get`](Self::get), so cross-database joins cannot be expressed.
#[derive(Debug, Clone, Default)]
pub struct Databases {
    by_alias: HashMap<String, Db>,
}

impl Databases {
    /// Alias used by [`default_db`](Self::default_db).
    pub const DEFAULT: &'static str = "default";

    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `db` under `alias` (replacing an existing entry).
    #[must_use]
    pub fn with(mut self, alias: impl Into<String>, db: Db) -> Self {
        self.by_alias.insert(alias.into(), db);
        self
    }

    /// Handle registered under `alias`.
    pub fn get(&self, alias: &str) -> Option<&Db> {
        self.by_alias.get(alias)
    }

    /// Handle registered as `"default"`.
    pub fn default_db(&self) -> Option<&Db> {
        self.get(Self::DEFAULT)
    }
}

/// Build a `Vec<Value>` of bind parameters: `params![id, "name"]`.
#[macro_export]
macro_rules! params {
    ($($value:expr),* $(,)?) => {
        ::std::vec![$($crate::Value::from($value)),*]
    };
}
