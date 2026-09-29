//! Model lifecycle signals: `pre_save`, `post_save`, `pre_delete`,
//! `post_delete` and `m2m_changed` (Django `django.db.models.signals`).
//!
//! A [`Signals`] registry lives on the [`Db`] (see [`Db::with_signals`]);
//! clones of the handle and the transaction handles derived from it share the
//! same registry. Registration is explicit:
//!
//! ```ignore
//! #[receiver(post_save, model = User)]
//! async fn audit(user: &User, event: &SignalEvent<'_>) -> Result<(), SignalError> {
//!     // `event.db` is the handle of the operation, so this runs inside the
//!     // caller's transaction when there is one.
//!     Ok(())
//! }
//!
//! let signals = Signals::new();
//! signals.connect(audit_receiver());
//! let db = Db::new(backend).with_signals(signals);
//! ```
//!
//! # Semantics
//!
//! * Receivers are typed per model and awaited one after the other, in the
//!   order they were connected.
//! * They receive the same [`Db`] as the operation, so inside a transaction
//!   they run in it.
//! * A failing `pre_*` receiver aborts the operation (nothing is written) and
//!   the error surfaces as [`OrmError::Signal`]. Receivers connected after
//!   the failing one do not run.
//! * A failing `post_*` receiver returns its error after the statement ran;
//!   inside a transaction the caller's rollback undoes the statement. Use
//!   [`Db::on_commit`] for work that must only happen after a commit.
//! * `post_delete` fires only when a row was actually removed.
//! * Bulk [`QuerySet`](crate::QuerySet) `update` / `delete` send no signals.
//! * `m2m_changed` fires for `add`, `remove`, `clear` and `set` (which is a
//!   `remove` of the stale links followed by an `add` of the missing ones),
//!   with the [`M2mAction`] `Pre*` before and `Post*` after the join-table
//!   statements; `add` and `remove` with nothing to change send nothing. The
//!   receiver is typed on the *declaring* model and gets the source object,
//!   loaded by primary key only when such a receiver is connected.

use crate::db::Db;
use crate::error::OrmError;
use crate::model::{Model, ModelMeta};
use crate::value::Value;
use std::any::{Any, TypeId};
use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard};

/// Boxed, sendable future returned by receivers.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The signal a [`Receiver`] listens to, without event payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignalName {
    /// Before an instance is inserted or updated.
    PreSave,
    /// After an instance was inserted or updated.
    PostSave,
    /// Before an instance is deleted.
    PreDelete,
    /// After an instance was deleted.
    PostDelete,
    /// Around a many-to-many change (all [`M2mAction`]s).
    M2mChanged,
}

/// What happened, as delivered to a receiver in [`SignalEvent::kind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// Before `save`.
    PreSave,
    /// After `save`; `created` tells an INSERT from an UPDATE.
    PostSave {
        /// `true` if the row was inserted, `false` if it was updated.
        created: bool,
    },
    /// Before `delete`.
    PreDelete,
    /// After `delete` removed a row.
    PostDelete,
    /// Around a many-to-many mutation.
    M2mChanged {
        /// Which mutation, and whether it is before or after.
        action: M2mAction,
    },
}

impl SignalKind {
    /// The [`SignalName`] this event belongs to.
    pub fn name(&self) -> SignalName {
        match self {
            Self::PreSave => SignalName::PreSave,
            Self::PostSave { .. } => SignalName::PostSave,
            Self::PreDelete => SignalName::PreDelete,
            Self::PostDelete => SignalName::PostDelete,
            Self::M2mChanged { .. } => SignalName::M2mChanged,
        }
    }
}

/// The step of a many-to-many mutation an `m2m_changed` event reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum M2mAction {
    /// Before links are added.
    PreAdd,
    /// After links were added.
    PostAdd,
    /// Before links are removed.
    PreRemove,
    /// After links were removed.
    PostRemove,
    /// Before all links are removed.
    PreClear,
    /// After all links were removed.
    PostClear,
}

/// Context handed to a receiver next to the instance.
#[derive(Debug)]
#[non_exhaustive]
pub struct SignalEvent<'a> {
    /// Which signal fired, with its payload.
    pub kind: SignalKind,
    /// The handle the operation runs on (the open transaction, if any).
    pub db: &'a Db,
    /// Metadata of the instance's model.
    pub model: &'static ModelMeta,
    /// `m2m_changed` only: the relation's field name on the declaring model.
    pub relation: Option<&'static str>,
    /// `m2m_changed` only: primary keys of the targets involved (empty for
    /// `clear`).
    pub pk_set: &'a [Value],
}

/// Error a receiver returns to fail the operation.
///
/// Build one from a message ([`SignalError::new`]) or from any error
/// ([`SignalError::from_error`], or `?` on an [`OrmError`]). The text is
/// logged and never returned to HTTP clients.
#[derive(Debug, thiserror::Error)]
#[error("signal receiver failed: {message}")]
pub struct SignalError {
    message: String,
    #[source]
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl SignalError {
    /// A failure described by `message`.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// A failure caused by `error`, which is kept as the source.
    pub fn from_error(error: impl Error + Send + Sync + 'static) -> Self {
        Self {
            message: error.to_string(),
            source: Some(Box::new(error)),
        }
    }

    /// The failure message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl From<OrmError> for SignalError {
    fn from(error: OrmError) -> Self {
        Self::from_error(error)
    }
}

/// Type-erased receiver body.
type Handler = dyn for<'a> Fn(
        &'a (dyn Any + Send + Sync),
        &'a SignalEvent<'a>,
    ) -> BoxFuture<'a, Result<(), SignalError>>
    + Send
    + Sync;

/// Pins the higher-ranked signature so closures infer it.
fn erase<F>(f: F) -> F
where
    F: for<'a> Fn(
            &'a (dyn Any + Send + Sync),
            &'a SignalEvent<'a>,
        ) -> BoxFuture<'a, Result<(), SignalError>>
        + Send
        + Sync,
{
    f
}

/// One async callback for one signal of one model.
///
/// Usually produced by `#[receiver(..)]`; build one by hand with
/// [`Receiver::new`].
#[derive(Clone)]
pub struct Receiver {
    name: SignalName,
    model: TypeId,
    meta: &'static ModelMeta,
    handler: Arc<Handler>,
}

impl Receiver {
    /// A receiver for signal `name` of model `M`.
    ///
    /// `handler` gets the instance as `&M`; the downcast is checked, so a
    /// receiver never sees another model.
    ///
    /// ```ignore
    /// Receiver::new::<User, _>(SignalName::PostSave, |user, event| {
    ///     Box::pin(async move { Ok(()) })
    /// })
    /// ```
    pub fn new<M, F>(name: SignalName, handler: F) -> Self
    where
        M: Model,
        F: for<'a> Fn(&'a M, &'a SignalEvent<'a>) -> BoxFuture<'a, Result<(), SignalError>>
            + Send
            + Sync
            + 'static,
    {
        let erased = erase(move |instance: &(dyn Any + Send + Sync), event| {
            match instance.downcast_ref::<M>() {
                Some(instance) => handler(instance, event),
                None => Box::pin(async {
                    Err(SignalError::new("receiver invoked with a different model"))
                }),
            }
        });
        Self {
            name,
            model: TypeId::of::<M>(),
            meta: M::META,
            handler: Arc::new(erased),
        }
    }

    /// The signal this receiver listens to.
    pub fn name(&self) -> SignalName {
        self.name
    }

    /// Metadata of the model this receiver is typed on.
    pub fn model(&self) -> &'static ModelMeta {
        self.meta
    }

    fn accepts(&self, name: SignalName, model: TypeId) -> bool {
        self.name == name && self.model == model
    }
}

impl std::fmt::Debug for Receiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Receiver")
            .field("signal", &self.name)
            .field("model", &self.meta.name)
            .finish_non_exhaustive()
    }
}

/// Registry of [`Receiver`]s. Cheap to clone: clones share one list.
#[derive(Clone, Default)]
pub struct Signals {
    receivers: Arc<RwLock<Vec<Receiver>>>,
}

impl Signals {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `receiver`; it runs after those connected earlier.
    ///
    /// Takes `&self`: the registry is shared, so receivers connected after
    /// [`Db::with_signals`] are seen by every clone.
    pub fn connect(&self, receiver: Receiver) {
        self.receivers
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(receiver);
    }

    /// Number of connected receivers.
    pub fn len(&self) -> usize {
        self.read().len()
    }

    /// Whether no receiver is connected.
    pub fn is_empty(&self) -> bool {
        self.read().is_empty()
    }

    fn read(&self) -> RwLockReadGuard<'_, Vec<Receiver>> {
        self.receivers
            .read()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether any receiver listens to `name` for model `M`.
    pub(crate) fn has_receivers<M: Model>(&self, name: SignalName) -> bool {
        let model = TypeId::of::<M>();
        self.read().iter().any(|r| r.accepts(name, model))
    }

    /// Run the matching receivers in connection order, stopping at the first
    /// failure.
    pub(crate) async fn send<M: Model>(
        &self,
        db: &Db,
        instance: &M,
        kind: SignalKind,
        relation: Option<&'static str>,
        pk_set: &[Value],
    ) -> Result<(), OrmError> {
        let (name, model) = (kind.name(), TypeId::of::<M>());
        // Clone the matches so the lock is not held across `.await`.
        let matching: Vec<Receiver> = self
            .read()
            .iter()
            .filter(|r| r.accepts(name, model))
            .cloned()
            .collect();
        if matching.is_empty() {
            return Ok(());
        }
        let event = SignalEvent {
            kind,
            db,
            model: M::META,
            relation,
            pk_set,
        };
        for receiver in matching {
            (receiver.handler)(instance, &event).await?;
        }
        Ok(())
    }
}

impl std::fmt::Debug for Signals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signals")
            .field("receivers", &self.len())
            .finish()
    }
}
