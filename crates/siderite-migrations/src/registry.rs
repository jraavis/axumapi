//! Runtime registry of [`crate::operation::Operation::RunRust`] functions.

use crate::error::MigrationError;
use siderite_orm::Db;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Boxed future returned by a registered Rust operation.
pub type RustFuture = Pin<Box<dyn Future<Output = Result<(), MigrationError>> + Send>>;
/// `RunRust` callback. Receives a cheap clone of [`Db`].
pub type RustOp = Arc<dyn Fn(Db) -> RustFuture + Send + Sync>;

/// Names → forward (and optional backwards) functions.
#[derive(Clone, Default)]
pub struct MigrationRegistry {
    ops: BTreeMap<String, RustOp>,
    replay_safe: BTreeSet<String>,
}

impl std::fmt::Debug for MigrationRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigrationRegistry")
            .field("ops", &self.ops.keys().collect::<Vec<_>>())
            .field("replay_safe", &self.replay_safe)
            .finish()
    }
}

impl MigrationRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `name`. The same map is used for forwards and backwards;
    /// a reversible `RunRust` stores both names as separate entries.
    ///
    /// Args:
    ///     name: Operation identifier in the migration file.
    ///     f: Callback whose future owns completion of its work.
    ///
    /// Returns:
    ///     None after registering with ordinary, conservative retry policy.
    ///
    /// Unconfirmed non-transactional execution blocks automatic retry.
    /// Use `register_replay_safe` only with an established idempotence contract.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        f: impl Fn(Db) -> RustFuture + Send + Sync + 'static,
    ) {
        let name = name.into();
        self.replay_safe.remove(&name);
        self.ops.insert(name, Arc::new(f));
    }

    /// Register a callback whose committed effects can safely be repeated.
    ///
    /// Args:
    ///     name: Operation name used by the migration file.
    ///     f: Callback safe to retry after interruption at any await point.
    ///
    /// Returns:
    ///     None after registering the callback and its explicit retry policy.
    ///
    /// Use only after establishing idempotence for database and external
    /// effects. Registration permits retry; it does not verify idempotence.
    /// Ordinary `register` clears this declaration when replacing a name.
    pub fn register_replay_safe(
        &mut self,
        name: impl Into<String>,
        f: impl Fn(Db) -> RustFuture + Send + Sync + 'static,
    ) {
        let name = name.into();
        self.register(name.clone(), f);
        self.replay_safe.insert(name);
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.ops.contains_key(name)
    }

    pub(crate) fn is_replay_safe(&self, name: &str) -> bool {
        self.replay_safe.contains(name)
    }

    /// Run the function named `name`.
    ///
    /// On SQLite the function runs on the schema-change connection with
    /// `PRAGMA foreign_keys` off (see the migrations guide), so data code
    /// must stay FK-clean by hand; orphans fail the migration at commit.
    ///
    /// # Errors
    /// [`MigrationError::UnregisteredRust`] or the function's own error.
    pub async fn run(&self, name: &str, db: &Db) -> Result<(), MigrationError> {
        let op = self
            .ops
            .get(name)
            .ok_or_else(|| MigrationError::UnregisteredRust(name.to_owned()))?;
        op(db.clone()).await
    }
}
