//! Runtime registry of [`crate::operation::Operation::RunRust`] functions.

use crate::error::MigrationError;
use axumapi_orm::Db;
use std::collections::BTreeMap;
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
}

impl std::fmt::Debug for MigrationRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigrationRegistry")
            .field("ops", &self.ops.keys().collect::<Vec<_>>())
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
    pub fn register(
        &mut self,
        name: impl Into<String>,
        f: impl Fn(Db) -> RustFuture + Send + Sync + 'static,
    ) {
        self.ops.insert(name.into(), Arc::new(f));
    }

    /// Run the function named `name`.
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
