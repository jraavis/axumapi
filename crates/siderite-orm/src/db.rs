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
use crate::capabilities::{
    BackendCapabilities, BackendKind, Feature, IsolationLevel, TransactionSupport,
};
use crate::error::{BackendCapabilityError, OrmError, QueryError};
use crate::model::{Model, ModelMeta};
use crate::plan::{PlanOrigin, QueryPlan};
use crate::queryset::QuerySet;
use crate::router::DatabaseRouter;
use crate::signals::Signals;
use crate::value::Value;
use crate::write::WritePlan;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;
use tracing::Instrument;

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
    /// The pool the transaction was opened on, to tell which database a
    /// handle belongs to.
    pool: Arc<dyn Backend>,
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
    signals: Signals,
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
            signals: Signals::default(),
        }
    }

    /// Attach the [`Signals`] registry model operations dispatch to.
    ///
    /// The registry is shared by clones of this handle and by the transaction
    /// handles opened from it.
    #[must_use]
    pub fn with_signals(mut self, signals: Signals) -> Self {
        self.signals = signals;
        self
    }

    /// The signal registry of this handle.
    pub fn signals(&self) -> &Signals {
        &self.signals
    }

    /// Whether `self` and `other` talk to the same database: two handles of
    /// one pool, or a transaction and the pool it was opened on.
    pub fn same_database(&self, other: &Db) -> bool {
        std::ptr::addr_eq(Arc::as_ptr(self.pool()), Arc::as_ptr(other.pool()))
    }

    /// Identity tag for plans built against this database.
    pub(crate) fn origin(&self) -> PlanOrigin {
        PlanOrigin::new(Arc::as_ptr(self.pool()).cast::<()>() as usize)
    }

    /// Reject work carrying a subquery built against another database.
    fn ensure_local(&self, foreign: bool) -> Result<(), OrmError> {
        if foreign {
            return Err(QueryError::InvalidPlan(
                "a subquery built against another database cannot run here".into(),
            )
            .into());
        }
        Ok(())
    }

    fn pool(&self) -> &Arc<dyn Backend> {
        match &self.target {
            Target::Pool(pool) => pool,
            Target::Tx(state) => &state.pool,
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
        self.ensure_local(plan.has_foreign_origin(self.origin()))?;
        self.traced("select", &plan.source.name, self.executor().fetch(plan))
            .await
    }

    /// Execute a write plan.
    ///
    /// # Errors
    /// Capability or backend errors.
    pub async fn execute(&self, plan: &WritePlan) -> Result<ExecResult, OrmError> {
        let origin = self.origin();
        let foreign = match plan {
            WritePlan::Insert(_) => false,
            WritePlan::Update(p) => p
                .assignments
                .iter()
                .map(|(_, e)| e)
                .chain(p.filter.iter())
                .any(|e| e.has_foreign_origin(origin)),
            WritePlan::Delete(p) => p.filter.iter().any(|e| e.has_foreign_origin(origin)),
        };
        self.ensure_local(foreign)?;
        let (operation, table) = match plan {
            WritePlan::Insert(p) => ("insert", &p.table),
            WritePlan::Update(p) => ("update", &p.table),
            WritePlan::Delete(p) => ("delete", &p.table),
        };
        self.traced(operation, table, self.executor().execute(plan))
            .await
    }

    /// Raw SQL returning rows (`db.raw_sql("SELECT .. WHERE id = ?", params![id])`).
    ///
    /// Placeholders follow the backend (`?` on SQLite, `$1` on PostgreSQL).
    /// Parameters are always bound; never format untrusted input into `sql`.
    ///
    /// # Errors
    /// Backend errors.
    pub async fn raw_sql(&self, sql: &str, params: Vec<Value>) -> Result<QueryResult, OrmError> {
        tracing::trace!(sql, "raw query");
        self.traced("raw", "", self.executor().fetch_raw(sql, params))
            .await
    }

    /// Raw SQL returning the affected-row count. Parameters are bound.
    ///
    /// # Errors
    /// Backend errors.
    pub async fn raw_execute(&self, sql: &str, params: Vec<Value>) -> Result<u64, OrmError> {
        tracing::trace!(sql, "raw statement");
        self.traced("raw", "", self.executor().execute_raw(sql, params))
            .await
    }

    /// Run a parameterless multi-statement script (DDL).
    ///
    /// # Errors
    /// Backend errors.
    pub async fn execute_script(&self, sql: &str) -> Result<(), OrmError> {
        tracing::trace!(sql, "script");
        self.traced("script", "", self.executor().execute_script(sql))
            .await
    }

    /// Run `query` inside an `orm.query` span and record its duration.
    ///
    /// The span carries `db.system`, `db.operation`, `db.table` (empty for raw
    /// SQL) and `elapsed_ms`. It is emitted at `debug` level. Bind parameters
    /// are never recorded, and SQL text only by the raw entry points at
    /// `trace` level.
    async fn traced<T>(
        &self,
        operation: &'static str,
        table: &str,
        query: impl Future<Output = Result<T, OrmError>>,
    ) -> Result<T, OrmError> {
        let span = tracing::debug_span!(
            "orm.query",
            db.system = self.capabilities().kind.system_name(),
            db.operation = operation,
            db.table = table,
            elapsed_ms = tracing::field::Empty,
        );
        let recorder = span.clone();
        async move {
            let started = Instant::now();
            let result = query.await;
            recorder.record("elapsed_ms", started.elapsed().as_secs_f64() * 1000.0);
            result
        }
        .instrument(span)
        .await
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

    /// Run `f` on a dedicated connection prepared for schema changes.
    ///
    /// SQLite disables foreign-key enforcement on that connection before any
    /// `BEGIN` (and restores it afterwards) so a table rebuild does not
    /// CASCADE-delete child rows. `transactional` wraps `f` in `BEGIN` /
    /// `COMMIT` when the backend supports it.
    ///
    /// # Errors
    /// `f`'s error, or an [`OrmError`] from begin/commit/rollback. SQLite
    /// rejects this when `self` is already in a transaction, because
    /// `PRAGMA foreign_keys` cannot be changed until that transaction ends.
    pub async fn schema_change<F, Fut, T, E>(&self, transactional: bool, f: F) -> Result<T, E>
    where
        F: FnOnce(Db) -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: From<OrmError>,
    {
        match &self.target {
            Target::Pool(pool) => {
                let tx: Arc<dyn Transaction> = Arc::from(pool.begin_schema(transactional).await?);
                let state = Arc::new(TxState {
                    tx: Arc::clone(&tx),
                    pool: Arc::clone(pool),
                    hooks: Arc::default(),
                    savepoints: Arc::default(),
                });
                let hooks = Arc::clone(&state.hooks);
                let result = f(Db {
                    target: Target::Tx(state),
                    signals: self.signals.clone(),
                })
                .await;
                match result {
                    Ok(value) => {
                        tx.commit().await?;
                        hooks.take().into_iter().for_each(|hook| hook());
                        Ok(value)
                    }
                    Err(err) => {
                        drop(tx.rollback().await);
                        Err(err)
                    }
                }
            }
            Target::Tx(_) if self.capabilities().kind == BackendKind::Sqlite => {
                Err(OrmError::from(QueryError::InvalidPlan(
                    "SQLite schema migrations cannot run inside an open transaction \
                     because PRAGMA foreign_keys cannot be changed until it commits"
                        .into(),
                ))
                .into())
            }
            Target::Tx(_) => self.run_transaction(None, f).await,
        }
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
                    pool: Arc::clone(pool),
                    hooks: Arc::default(),
                    savepoints: Arc::default(),
                });
                let hooks = Arc::clone(&state.hooks);
                let result = f(Db {
                    target: Target::Tx(state),
                    signals: self.signals.clone(),
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
                let name = format!("siderite_sp_{n}");
                parent
                    .tx
                    .execute_script(&format!("SAVEPOINT {name}"))
                    .await?;
                let state = Arc::new(TxState {
                    tx: Arc::clone(&parent.tx),
                    pool: Arc::clone(&parent.pool),
                    hooks: Arc::default(),
                    savepoints: Arc::clone(&parent.savepoints),
                });
                let hooks = Arc::clone(&state.hooks);
                let result = f(Db {
                    target: Target::Tx(state),
                    signals: self.signals.clone(),
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

/// Named databases (`"default"`, `"analytics"`, ...) plus an optional
/// [`DatabaseRouter`].
///
/// A queryset holds one [`Db`] and never spans aliases: pick the database
/// with [`get`](Self::get), [`using`](Self::using) or the router-aware
/// [`for_read`](Self::for_read) / [`for_write`](Self::for_write) /
/// [`objects`](Self::objects). Combining querysets bound to different
/// databases (`union` and friends) is an error.
#[derive(Clone, Default)]
pub struct Databases {
    by_alias: BTreeMap<String, Db>,
    router: Option<Arc<dyn DatabaseRouter>>,
}

impl std::fmt::Debug for Databases {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Databases")
            .field("aliases", &self.by_alias.keys().collect::<Vec<_>>())
            .field("router", &self.router.is_some())
            .finish()
    }
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

    /// Route reads and writes through `router` (replacing a previous one).
    #[must_use]
    pub fn with_router(mut self, router: impl DatabaseRouter) -> Self {
        self.router = Some(Arc::new(router));
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

    /// Registered aliases, sorted.
    pub fn aliases(&self) -> impl Iterator<Item = &str> {
        self.by_alias.keys().map(String::as_str)
    }

    /// The database `M` is read from: the router's choice, else `"default"`.
    ///
    /// # Errors
    /// [`OrmError::UnknownDatabase`] if the chosen alias is not registered.
    pub fn for_read<M: Model>(&self) -> Result<&Db, OrmError> {
        self.resolve(self.router.as_deref().and_then(|r| r.db_for_read(M::META)))
    }

    /// The database `M` is written to: the router's choice, else `"default"`.
    ///
    /// # Errors
    /// [`OrmError::UnknownDatabase`] if the chosen alias is not registered.
    pub fn for_write<M: Model>(&self) -> Result<&Db, OrmError> {
        self.resolve(self.router.as_deref().and_then(|r| r.db_for_write(M::META)))
    }

    /// Queryset over `M` on its read database (Django `Model.objects`
    /// under a router).
    ///
    /// # Errors
    /// As [`for_read`](Self::for_read).
    pub fn objects<M: Model>(&self) -> Result<QuerySet<M>, OrmError> {
        self.for_read::<M>().map(M::objects)
    }

    /// Queryset over `M` on the database registered as `alias` (Django
    /// `Model.objects.using(alias)`), bypassing the router.
    ///
    /// # Errors
    /// [`OrmError::UnknownDatabase`] if `alias` is not registered.
    pub fn using<M: Model>(&self, alias: &str) -> Result<QuerySet<M>, OrmError> {
        self.resolve(Some(alias)).map(M::objects)
    }

    /// Whether migrations may create `model` on `alias` (`true` without a
    /// router).
    pub fn allow_migrate(&self, alias: &str, model: &ModelMeta) -> bool {
        self.router
            .as_deref()
            .is_none_or(|r| r.allow_migrate(alias, model))
    }

    fn resolve(&self, alias: Option<&str>) -> Result<&Db, OrmError> {
        let alias = alias.unwrap_or(Self::DEFAULT);
        self.get(alias)
            .ok_or_else(|| OrmError::UnknownDatabase(alias.to_owned()))
    }
}

/// Build a `Vec<Value>` of bind parameters: `params![id, "name"]`.
#[macro_export]
macro_rules! params {
    ($($value:expr),* $(,)?) => {
        ::std::vec![$($crate::Value::from($value)),*]
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{ExecResult, Executor, QueryResult, Transaction};
    use crate::capabilities::BackendCapabilities;
    use crate::plan::QueryPlan;
    use crate::write::DeletePlan;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Subscriber};
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;

    /// Backend that answers every statement with an empty result.
    struct Stub {
        _id: u8,
    }

    #[async_trait]
    impl Executor for Stub {
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::sqlite()
        }
        async fn fetch(&self, _: &QueryPlan) -> Result<QueryResult, OrmError> {
            Ok(QueryResult::default())
        }
        async fn execute(&self, _: &WritePlan) -> Result<ExecResult, OrmError> {
            Ok(ExecResult::default())
        }
        async fn fetch_raw(&self, _: &str, _: Vec<Value>) -> Result<QueryResult, OrmError> {
            Ok(QueryResult::default())
        }
        async fn execute_raw(&self, _: &str, _: Vec<Value>) -> Result<u64, OrmError> {
            Ok(0)
        }
        async fn execute_script(&self, _: &str) -> Result<(), OrmError> {
            Ok(())
        }
    }

    #[async_trait]
    impl Backend for Stub {
        async fn begin(
            &self,
            _: Option<crate::capabilities::IsolationLevel>,
        ) -> Result<Box<dyn Transaction>, OrmError> {
            Ok(Box::new(Stub { _id: 0 }))
        }
    }

    #[async_trait]
    impl Transaction for Stub {
        async fn commit(&self) -> Result<(), OrmError> {
            Ok(())
        }
        async fn rollback(&self) -> Result<(), OrmError> {
            Ok(())
        }
    }

    fn stub_db() -> Db {
        Db::new(Stub { _id: 1 })
    }

    /// Fields of one recorded span or event.
    #[derive(Debug, Default, Clone)]
    struct Captured {
        name: String,
        level: Option<tracing::Level>,
        fields: Vec<(String, String)>,
    }

    impl Captured {
        fn field(&self, name: &str) -> Option<&str> {
            self.fields
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        }
    }

    struct Collect<'a>(&'a mut Vec<(String, String)>);

    impl Visit for Collect<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push((field.name().to_owned(), format!("{value:?}")));
        }
        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.push((field.name().to_owned(), value.to_owned()));
        }
    }

    #[derive(Clone, Default)]
    struct Recorder {
        spans: Arc<Mutex<Vec<(Id, Captured)>>>,
        events: Arc<Mutex<Vec<Captured>>>,
    }

    impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Recorder {
        fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, _: Context<'_, S>) {
            let mut captured = Captured {
                name: attrs.metadata().name().to_owned(),
                level: Some(*attrs.metadata().level()),
                fields: Vec::new(),
            };
            attrs.record(&mut Collect(&mut captured.fields));
            self.spans.lock().unwrap().push((id.clone(), captured));
        }
        fn on_record(&self, id: &Id, values: &Record<'_>, _: Context<'_, S>) {
            let mut spans = self.spans.lock().unwrap();
            if let Some((_, captured)) = spans.iter_mut().find(|(i, _)| i == id) {
                values.record(&mut Collect(&mut captured.fields));
            }
        }
        fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
            let mut captured = Captured {
                name: event.metadata().name().to_owned(),
                level: Some(*event.metadata().level()),
                fields: Vec::new(),
            };
            event.record(&mut Collect(&mut captured.fields));
            self.events.lock().unwrap().push(captured);
        }
    }

    fn recording() -> (Recorder, tracing::subscriber::DefaultGuard) {
        let recorder = Recorder::default();
        let subscriber = tracing_subscriber::registry().with(recorder.clone());
        (recorder, tracing::subscriber::set_default(subscriber))
    }

    fn query_spans(recorder: &Recorder) -> Vec<Captured> {
        recorder
            .spans
            .lock()
            .unwrap()
            .iter()
            .map(|(_, c)| c.clone())
            .filter(|c| c.name == "orm.query")
            .collect()
    }

    #[tokio::test]
    async fn every_entry_point_runs_in_an_orm_query_span() {
        let (recorder, _guard) = recording();
        let db = stub_db();
        db.fetch(&QueryPlan::from_table("books")).await.unwrap();
        db.execute(&WritePlan::Delete(DeletePlan {
            table: "authors".into(),
            filter: None,
            returning: Vec::new(),
        }))
        .await
        .unwrap();
        db.raw_sql("SELECT 1", Vec::new()).await.unwrap();
        db.raw_execute("DELETE FROM t", Vec::new()).await.unwrap();
        db.execute_script("CREATE TABLE t (id INTEGER)")
            .await
            .unwrap();

        let spans = query_spans(&recorder);
        let seen: Vec<(&str, &str)> = spans
            .iter()
            .map(|s| {
                (
                    s.field("db.operation").unwrap(),
                    s.field("db.table").unwrap(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("select", "books"),
                ("delete", "authors"),
                ("raw", ""),
                ("raw", ""),
                ("script", "")
            ]
        );
        for span in &spans {
            assert_eq!(span.field("db.system"), Some("sqlite"));
            assert_eq!(span.level, Some(tracing::Level::DEBUG));
            let elapsed: f64 = span.field("elapsed_ms").unwrap().parse().unwrap();
            assert!(elapsed >= 0.0);
        }
    }

    #[tokio::test]
    async fn raw_sql_is_traced_but_bind_parameters_never_are() {
        let (recorder, _guard) = recording();
        let db = stub_db();
        db.raw_sql(
            "SELECT * FROM users WHERE token = ?",
            vec![Value::from("s3cr3t-token")],
        )
        .await
        .unwrap();

        let events = recorder.events.lock().unwrap();
        let sql_event = events
            .iter()
            .find(|e| e.field("sql").is_some())
            .expect("sql event");
        assert_eq!(sql_event.level, Some(tracing::Level::TRACE));
        assert_eq!(
            sql_event.field("sql"),
            Some("SELECT * FROM users WHERE token = ?")
        );
        let everything = format!("{events:?} {:?}", query_spans(&recorder));
        assert!(!everything.contains("s3cr3t-token"), "{everything}");
    }

    #[tokio::test]
    async fn plan_calls_never_emit_sql_or_values() {
        let (recorder, _guard) = recording();
        let db = stub_db();
        db.fetch(&QueryPlan::from_table("books")).await.unwrap();
        assert!(recorder.events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn same_database_follows_the_pool_across_clones_and_transactions() {
        let (a, b) = (stub_db(), stub_db());
        assert!(a.same_database(&a.clone()));
        assert!(!a.same_database(&b));
        let (outer, other) = (a.clone(), b.clone());
        a.transaction(|tx| async move {
            assert!(tx.same_database(&outer));
            assert!(!tx.same_database(&other));
            Ok::<_, OrmError>(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn signals_are_shared_by_clones_and_transaction_handles() {
        let signals = Signals::new();
        let db = stub_db().with_signals(signals.clone());
        assert!(db.clone().signals().is_empty());
        db.transaction(|tx| async move {
            // The registry is shared, not copied.
            let seen = tx.signals().clone();
            tx.transaction(|nested| async move {
                assert_eq!(nested.signals().len(), seen.len());
                Ok::<_, OrmError>(())
            })
            .await
        })
        .await
        .unwrap();
    }

    #[test]
    fn databases_lists_sorted_aliases_and_hides_the_router_in_debug() {
        let dbs = Databases::new()
            .with("replica", stub_db())
            .with("default", stub_db())
            .with("analytics", stub_db());
        assert_eq!(
            dbs.aliases().collect::<Vec<_>>(),
            ["analytics", "default", "replica"]
        );
        assert!(dbs.default_db().is_some());
        assert!(dbs.get("missing").is_none());
        assert!(format!("{dbs:?}").contains("analytics"));
    }
}
