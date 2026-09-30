//! Database routing: pick the database alias for reads and writes
//! (Django `DATABASE_ROUTERS`).
//!
//! Implement [`DatabaseRouter`] and attach it with
//! [`Databases::with_router`](crate::Databases::with_router). Each hook may
//! return an alias registered in the [`Databases`](crate::Databases); `None`
//! defers to the default (`"default"`). A router naming an alias that is not
//! registered yields [`OrmError::UnknownDatabase`](crate::OrmError::UnknownDatabase).
//!
//! ```ignore
//! struct ReplicaRouter;
//!
//! impl DatabaseRouter for ReplicaRouter {
//!     fn db_for_read(&self, _model: &ModelMeta) -> Option<&str> {
//!         Some("replica")
//!     }
//! }
//! ```

use crate::model::ModelMeta;

/// Chooses the database alias for a model. Every hook has a neutral default.
pub trait DatabaseRouter: Send + Sync + 'static {
    /// Alias to read `model` from; `None` means the default database.
    fn db_for_read(&self, model: &ModelMeta) -> Option<&str> {
        let _ = model;
        None
    }

    /// Alias to write `model` to; `None` means the default database.
    fn db_for_write(&self, model: &ModelMeta) -> Option<&str> {
        let _ = model;
        None
    }

    /// Whether migrations may create `model`'s table on database `alias`.
    fn allow_migrate(&self, alias: &str, model: &ModelMeta) -> bool {
        let _ = (alias, model);
        true
    }
}
