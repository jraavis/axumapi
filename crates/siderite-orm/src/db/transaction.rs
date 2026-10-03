//! Scope ownership and cancellation-safe transaction orchestration.

use super::{Db, Target};
use crate::backend::{Backend, Executor, Transaction};
use crate::capabilities::{BackendKind, Feature};
use crate::capabilities::{IsolationLevel, TransactionSupport};
use crate::error::{BackendCapabilityError, OrmError, QueryError};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

fn lock<T>(cell: &Mutex<T>) -> MutexGuard<'_, T> {
    cell.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests;

type Hook = Box<dyn FnOnce() + Send>;

#[derive(Default)]
pub(super) struct Hooks(Mutex<Vec<Hook>>);

impl Hooks {
    fn push(&self, hook: Hook) {
        lock(&self.0).push(hook);
    }

    fn take(&self) -> Vec<Hook> {
        std::mem::take(&mut *lock(&self.0))
    }
}

struct Lifecycle {
    handle: Option<Arc<dyn Transaction>>,
    active: u64,
    next: u64,
    running: bool,
    aborted: bool,
}

pub(super) struct Control(Mutex<Lifecycle>);

impl Control {
    fn new(handle: Arc<dyn Transaction>) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Lifecycle {
            handle: Some(handle),
            active: 0,
            next: 0,
            running: false,
            aborted: false,
        })))
    }

    fn check(state: &Lifecycle, scope: u64) -> Result<(), OrmError> {
        if state.aborted {
            return Err(QueryError::TransactionAborted.into());
        }
        if state.handle.is_none() {
            return Err(QueryError::TransactionClosed.into());
        }
        if state.active != scope || state.running {
            return Err(QueryError::TransactionBusy.into());
        }
        Ok(())
    }

    fn ready(&self, scope: u64) -> Result<(), OrmError> {
        Self::check(&lock(&self.0), scope)
    }

    fn child(self: &Arc<Self>, parent: u64) -> Result<Scope, OrmError> {
        let mut state = lock(&self.0);
        Self::check(&state, parent)?;
        let id = state
            .next
            .checked_add(1)
            .ok_or_else(|| QueryError::InvalidPlan("scope overflow".into()))?;
        state.next = id;
        state.active = id;
        Ok(Scope {
            control: Arc::clone(self),
            id,
            parent,
            armed: true,
            hooks: Arc::default(),
        })
    }

    pub(super) fn lease(self: &Arc<Self>, id: u64) -> Result<Lease, OrmError> {
        let mut state = lock(&self.0);
        Self::check(&state, id)?;
        let handle = state
            .handle
            .as_ref()
            .ok_or(QueryError::TransactionClosed)?
            .clone();
        state.running = true;
        Ok(Lease {
            control: Arc::clone(self),
            handle,
            settled: false,
        })
    }

    fn close(&self, aborted: bool) {
        // No backend code or destructor runs while the lifecycle lock is held.
        let handle = {
            let mut state = lock(&self.0);
            state.aborted |= aborted;
            state.handle.take()
        };
        drop(handle);
    }

    fn end_root(&self) -> Result<(), OrmError> {
        let (ready, handle) = {
            let mut state = lock(&self.0);
            let ready = Self::check(&state, 0);
            state.aborted |= ready.is_err();
            (ready, state.handle.take())
        };
        drop(handle);
        ready
    }

    fn register(&self, scope: u64, hooks: &Hooks, hook: Hook) {
        let state = lock(&self.0);
        if !state.aborted && state.handle.is_some() && state.active == scope {
            hooks.push(hook);
        }
    }
}

/// An executor lease prevents statements overlapping savepoint boundaries.
pub(super) struct Lease {
    control: Arc<Control>,
    handle: Arc<dyn Transaction>,
    settled: bool,
}

impl Lease {
    async fn execute_script(&mut self, sql: &str) -> Result<(), OrmError> {
        let result = self.handle.execute_script(sql).await;
        self.settled = true;
        result
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let handle = {
            let mut state = self
                .control
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            state.running = false;
            if self.settled {
                None
            } else {
                state.aborted = true;
                state.handle.take()
            }
        };
        drop(handle);
    }
}

pub(super) enum ExecutorRef<'a> {
    Pool(&'a dyn Executor),
    Leased(Lease),
}

impl ExecutorRef<'_> {
    pub(super) fn finish(&mut self) {
        if let Self::Leased(lease) = self {
            lease.settled = true;
        }
    }

    pub(super) fn as_ref(&self) -> &dyn Executor {
        match self {
            Self::Pool(pool) => *pool,
            Self::Leased(lease) => lease.handle.as_ref(),
        }
    }
}

pub(super) struct TxState {
    pub(super) control: Arc<Control>,
    pub(super) pool: Arc<dyn Backend>,
    pub(super) hooks: Arc<Hooks>,
    pub(super) scope: u64,
}

pub(super) struct ConnState {
    pub(super) control: Arc<Control>,
    pub(super) pool: Arc<dyn Backend>,
}

/// Armed before any scope SQL; abandoning it makes the whole handle unusable.
struct Scope {
    control: Arc<Control>,
    id: u64,
    parent: u64,
    armed: bool,
    hooks: Arc<Hooks>,
}

impl Scope {
    fn root(control: &Arc<Control>) -> Self {
        Self {
            control: Arc::clone(control),
            id: 0,
            parent: 0,
            armed: true,
            hooks: Arc::default(),
        }
    }

    fn release(
        &mut self,
        hooks: &Hooks,
        parent: Option<&Hooks>,
        keep: bool,
    ) -> Result<(), OrmError> {
        let mut state = self
            .control
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Control::check(&state, self.id)?;
        if keep && let Some(parent) = parent {
            for hook in hooks.take() {
                parent.push(hook);
            }
        }
        state.active = self.parent;
        self.armed = false;
        Ok(())
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if self.armed {
            self.control.close(true);
        }
        // Break cycles when a callback captured its own scoped Db handle.
        drop(self.hooks.take());
    }
}

async fn rollback(handle: &dyn Transaction) {
    if let Err(error) = handle.rollback().await {
        tracing::warn!(%error, "transaction rollback failed");
    }
}

impl Db {
    /// Run `f` in a transaction, or a savepoint inside a transaction.
    ///
    /// Cancelling a closure, child scope or statement makes the transaction
    /// unusable. An outer success then fails and rolls back. Concurrent
    /// statements and sibling scopes are rejected; recursive nesting through
    /// the child is supported.
    /// Use the child handle while its scope is active. Escaped handles cannot
    /// execute after their scope finishes. Cancellation during COMMIT can
    /// leave the server outcome unknown; callers must not retry blindly.
    ///
    /// Args:
    ///     f: Closure receiving the scoped transaction handle.
    ///
    /// Returns:
    ///     The closure result after commit or confirmed savepoint release.
    ///
    /// # Errors
    /// Closure, backend, capability, busy-scope or aborted-transaction errors.
    pub async fn transaction<F, Fut, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(Db) -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: From<OrmError>,
    {
        self.run_transaction(None, f).await
    }

    /// Run a scoped transaction at an explicit isolation level.
    ///
    /// Args:
    ///     isolation: A level supported by the backend; disallowed in nesting.
    ///     f: Closure receiving the scoped transaction handle.
    ///
    /// Returns:
    ///     The closure result after successful commit.
    ///
    /// # Errors
    /// As [`transaction`](Self::transaction), plus unsupported isolation.
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

    /// Run `f` on a dedicated schema connection, optionally transactional.
    ///
    /// SQLite disables foreign keys before BEGIN and restores them afterwards.
    /// A cancelled nested scope makes the connection unusable; it cannot be
    /// committed through an escaped handle.
    ///
    /// Args:
    ///     transactional: Whether to wrap schema work in a transaction.
    ///     f: Closure receiving the dedicated connection handle.
    ///
    /// Returns:
    ///     The closure result after the backend finishes schema work.
    ///
    /// # Errors
    /// Closure/backend errors; SQLite rejects an already-scoped connection.
    pub async fn schema_change<F, Fut, T, E>(
        &self,
        // SQLite prepares this connection before opening a transaction.
        transactional: bool,
        f: F,
    ) -> Result<T, E>
    where
        F: FnOnce(Db) -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: From<OrmError>,
    {
        match &self.target {
            Target::Pool(pool) => {
                let handle: Arc<dyn Transaction> =
                    Arc::from(pool.begin_schema(transactional).await?);
                self.root_scope(pool, handle, transactional, f).await
            }
            _ if self.capabilities().kind == BackendKind::Sqlite => {
                let reason = "SQLite schema requires a pool handle";
                let error = QueryError::InvalidPlan(reason.into());
                Err(OrmError::from(error).into())
            }
            _ => self.run_transaction(None, f).await,
        }
    }

    async fn root_scope<F, Fut, T, E>(
        &self,
        pool: &Arc<dyn Backend>,
        handle: Arc<dyn Transaction>,
        transactional: bool,
        f: F,
    ) -> Result<T, E>
    where
        F: FnOnce(Db) -> Fut,
        Fut: Future<Output = Result<T, E>>,
        E: From<OrmError>,
    {
        let control = Control::new(Arc::clone(&handle));
        let mut guard = Scope::root(&control);
        let hooks = Arc::clone(&guard.hooks);
        let target = if transactional {
            Target::Tx(Arc::new(TxState {
                control: Arc::clone(&control),
                pool: Arc::clone(pool),
                hooks: Arc::clone(&hooks),
                scope: 0,
            }))
        } else {
            Target::Conn(Arc::new(ConnState {
                control: Arc::clone(&control),
                pool: Arc::clone(pool),
            }))
        };
        let result = f(Db {
            target,
            signals: self.signals.clone(),
        })
        .await;
        let ready = control.end_root();
        match result {
            Ok(value) => {
                if let Err(error) = ready {
                    rollback(handle.as_ref()).await;
                    return Err(error.into());
                }
                handle.commit().await?;
                guard.armed = false;
                hooks.take().into_iter().for_each(|hook| hook());
                Ok(value)
            }
            Err(error) => {
                rollback(handle.as_ref()).await;
                guard.armed = false;
                Err(error)
            }
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
        if let Target::Pool(pool) = &self.target {
            if let Some(level) = isolation {
                pool.capabilities()
                    .require(Feature::Isolation(level))
                    .map_err(OrmError::from)?;
            }
            let handle = Arc::from(pool.begin(isolation).await?);
            return self.root_scope(pool, handle, true, f).await;
        }
        if isolation.is_some() {
            return Err(OrmError::from(QueryError::InvalidPlan(
                "isolation level cannot be set on a nested transaction".into(),
            ))
            .into());
        }
        let (control, pool, parent_scope, parent_hooks) = match &self.target {
            Target::Tx(parent) => {
                let caps = parent.pool.capabilities();
                if caps.transactions < TransactionSupport::Savepoints {
                    let error = BackendCapabilityError::Unsupported {
                        backend: caps.kind,
                        feature: Feature::Savepoints,
                    };
                    return Err(OrmError::from(error).into());
                }
                (
                    &parent.control,
                    &parent.pool,
                    parent.scope,
                    Some(&parent.hooks),
                )
            }
            Target::Conn(parent) => (&parent.control, &parent.pool, 0, None),
            Target::Pool(_) => unreachable!("pool handled above"),
        };
        let mut guard = control.child(parent_scope)?;
        let name = format!("siderite_sp_{}", guard.id);
        let begin = if parent_hooks.is_some() {
            format!("SAVEPOINT {name}")
        } else {
            "BEGIN".into()
        };
        control.lease(guard.id)?.execute_script(&begin).await?;
        let hooks = Arc::clone(&guard.hooks);
        let child = Db {
            target: Target::Tx(Arc::new(TxState {
                control: Arc::clone(control),
                pool: Arc::clone(pool),
                hooks: Arc::clone(&hooks),
                scope: guard.id,
            })),
            signals: self.signals.clone(),
        };
        let result = f(child).await;
        if let Err(error) = control.ready(guard.id) {
            tracing::warn!(%error, "transaction scope became unusable");
            return match result {
                Err(original) => Err(original),
                Ok(_) => Err(error.into()),
            };
        }
        let end = match (&result, parent_hooks) {
            (Ok(_), Some(_)) => format!("RELEASE SAVEPOINT {name}"),
            (Err(_), Some(_)) => format!("ROLLBACK TO SAVEPOINT {name}"),
            (Ok(_), None) => "COMMIT".into(),
            (Err(_), None) => "ROLLBACK".into(),
        };
        let cleanup = async {
            control.lease(guard.id)?.execute_script(&end).await?;
            if result.is_err() && parent_hooks.is_some() {
                control
                    .lease(guard.id)?
                    .execute_script(&format!("RELEASE SAVEPOINT {name}"))
                    .await?;
            }
            Ok::<_, OrmError>(())
        }
        .await;
        if let Err(error) = cleanup {
            tracing::warn!(%error, "transaction scope cleanup failed");
            return match result {
                Err(original) => Err(original),
                Ok(_) => Err(error.into()),
            };
        }
        guard.release(&hooks, parent_hooks.map(Arc::as_ref), result.is_ok())?;
        if result.is_ok() && parent_hooks.is_none() {
            hooks.take().into_iter().for_each(|hook| hook());
        }
        result
    }

    /// Register work after the outermost transaction successfully commits.
    ///
    /// Hooks of failed or abandoned scopes are discarded. Registration on
    /// closed, aborted or temporarily inactive handles is discarded as well.
    /// Outside a transaction the callback runs immediately.
    ///
    /// Args:
    ///     hook: Callback invoked after commit.
    ///
    /// Returns:
    ///     None.
    pub fn on_commit(&self, hook: impl FnOnce() + Send + 'static) {
        match &self.target {
            Target::Pool(_) => hook(),
            Target::Conn(state) => {
                if state.control.ready(0).is_ok() {
                    hook();
                }
            }
            Target::Tx(state) => {
                state
                    .control
                    .register(state.scope, &state.hooks, Box::new(hook));
            }
        }
    }
}
