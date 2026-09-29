//! Application lifespan: startup/shutdown hooks and scoped resources.
//!
//! Hooks run in registration order at startup and in **reverse** order at
//! shutdown; hooks of mounted child apps run after the parent's at startup
//! and before them at shutdown. A failing startup hook aborts startup with
//! [`ServerError::Lifespan`] (shutdown hooks do not run). At shutdown every
//! hook runs even if one fails; the first error is reported.
//!
//! [`App::lifespan_resource`] is the equivalent of FastAPI's lifespan
//! context manager: a value created at startup, injected with
//! [`Resource<T>`], and cleaned up at shutdown.

use crate::app::{App, BoxFuture, LifespanHook};
use crate::error::{ApiError, ServerError};
use crate::extract::FromRequestParts;
use http::StatusCode;
use http::request::Parts;
use std::future::Future;
use std::ops::Deref;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

/// The startup and shutdown hooks collected from an app tree.
#[derive(Default)]
pub struct Lifespan {
    pub(crate) startup: Vec<LifespanHook>,
    pub(crate) shutdown: Vec<LifespanHook>,
}

impl std::fmt::Debug for Lifespan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lifespan")
            .field("startup_hooks", &self.startup.len())
            .field("shutdown_hooks", &self.shutdown.len())
            .finish()
    }
}

impl Lifespan {
    /// Run the startup hooks in order, stopping at the first failure.
    ///
    /// # Errors
    /// [`ServerError::Lifespan`] with the failing hook's error.
    pub async fn startup(&mut self) -> Result<(), ServerError> {
        for hook in std::mem::take(&mut self.startup) {
            hook().await.map_err(ServerError::Lifespan)?;
        }
        Ok(())
    }

    /// Run the shutdown hooks in reverse order. All hooks run; failures are
    /// logged and the first one is returned.
    ///
    /// # Errors
    /// [`ServerError::Lifespan`] with the first hook error.
    pub async fn shutdown(&mut self) -> Result<(), ServerError> {
        let mut first_error = None;
        for hook in std::mem::take(&mut self.shutdown).into_iter().rev() {
            if let Err(err) = hook().await {
                tracing::error!(error = %err, "shutdown hook failed");
                first_error.get_or_insert(err);
            }
        }
        first_error.map_or(Ok(()), |e| Err(ServerError::Lifespan(e)))
    }
}

/// Handler argument giving access to a value created by
/// [`App::lifespan_resource`]. Responds `503` until startup has completed
/// (and again after shutdown).
#[derive(Debug)]
pub struct Resource<T>(pub Arc<T>);

impl<T> Clone for Resource<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Deref for Resource<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// Holds the teardown future between startup and shutdown.
type TeardownCell = Mutex<Option<BoxFuture<Result<(), ApiError>>>>;

/// Shared cell installed as a request extension; filled at startup.
struct Slot<T>(Arc<RwLock<Option<Arc<T>>>>);

impl<T> Clone for Slot<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: Send + Sync + 'static> FromRequestParts for Resource<T> {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        let slot = parts.extensions.get::<Slot<T>>().ok_or_else(|| {
            ApiError::internal(format!(
                "no lifespan resource of type `{}` registered",
                std::any::type_name::<T>()
            ))
        })?;
        let value = slot
            .0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        value.map(Resource).ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "The service is not ready yet.",
            )
        })
    }
}

impl App {
    /// Run `hook` at startup (before the socket is bound).
    #[must_use]
    pub fn on_startup<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), ApiError>> + Send + 'static,
    {
        self.startup_hooks.push(boxed(hook));
        self
    }

    /// Run `hook` at shutdown (hooks run in reverse registration order).
    #[must_use]
    pub fn on_shutdown<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), ApiError>> + Send + 'static,
    {
        self.shutdown_hooks.push(boxed(hook));
        self
    }

    /// Create a resource at startup and tear it down at shutdown.
    ///
    /// `init` returns the value and a *teardown future* (which may capture
    /// clones of whatever it needs to close). Handlers receive the value via
    /// [`Resource<T>`]. Resources are torn down in reverse registration order.
    #[must_use]
    pub fn lifespan_resource<T, F, Fut, C>(mut self, init: F) -> Self
    where
        T: Send + Sync + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<(T, C), ApiError>> + Send + 'static,
        C: Future<Output = Result<(), ApiError>> + Send + 'static,
    {
        let slot: Slot<T> = Slot(Arc::default());
        let teardown: Arc<TeardownCell> = Arc::default();
        let (start_slot, start_teardown) = (slot.clone(), Arc::clone(&teardown));
        self.startup_hooks.push(boxed(move || async move {
            let (value, close) = init().await?;
            *start_slot.0.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(value));
            let close: BoxFuture<Result<(), ApiError>> = Box::pin(close);
            *start_teardown
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(close);
            Ok(())
        }));
        let stop_slot = slot.clone();
        self.shutdown_hooks.push(boxed(move || async move {
            stop_slot
                .0
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            let close = teardown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            match close {
                Some(close) => close.await,
                None => Ok(()),
            }
        }));
        self.router_layers
            .push(Box::new(move |router| router.layer(axum::Extension(slot))));
        self
    }
}

fn boxed<F, Fut>(hook: F) -> LifespanHook
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), ApiError>> + Send + 'static,
{
    Box::new(move || Box::pin(hook()))
}
