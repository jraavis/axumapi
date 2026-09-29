//! Dependency injection (`Depends`), inspired by FastAPI.
//!
//! A [`Dependency`] is any type that knows how to build itself from the
//! request ([`Dependency::resolve`]). Handlers ask for one with
//! [`Depends<T>`]; dependencies may in turn depend on others through
//! [`ResolveContext::resolve`].
//!
//! # Scoping and caching
//! Resolution is **request-scoped and cached**: within one request the same
//! `T` is resolved at most once, no matter how many handler arguments or other
//! dependencies ask for it. Failed resolutions are not cached.
//!
//! # Application-scoped values, overrides and global dependencies
//! * [`App::provide`] registers a shared value. It takes precedence over
//!   `T::resolve` for `Depends<T>` (when `T: Dependency`) and is the only way
//!   to inject types that are not dependencies, via [`Provided<T>`].
//! * [`App::override_dependency`] / [`App::override_value`] replace
//!   `T::resolve` (also when `T` is requested by another dependency); meant
//!   for tests.
//! * [`App::dependency`] runs a dependency before **every routed request of
//!   the app and its mounts** and short-circuits with its error (e.g. 401).
//!   Unrouted requests (404) and the generated documentation endpoints are not
//!   affected. Per-route dependencies are not provided; add a `Depends<T>`
//!   argument to the handler instead.
//!
//! A mounted child app sees its parent's registrations; its own registrations
//! take precedence inside the child.
//!
//! # Teardown
//! `Drop` cannot be async, so cleanup that must await is registered with
//! [`ResolveContext::on_teardown`] (the analogue of FastAPI's `yield`).
//! Teardown futures run **after the response has been produced**, in
//! **reverse (LIFO) registration order**, on a spawned task; errors and panics
//! are logged. Teardown is best-effort and not tied to the end of response
//! *streaming*: a streaming body may still be sent while teardown runs.
//!
//! # Documentation
//! [`Dependency::describe`] contributes OpenAPI parameters; `Depends<T>`
//! forwards to it, so headers/queries of nested dependencies show up in the
//! operation (cycles are guarded).

use crate::app::App;
use crate::body::Body;
use crate::error::ApiError;
use crate::extract::FromRequestParts;
use crate::response::IntoResponse;
use axumapi_openapi::{Operation, SchemaRegistry};
use http::request::Parts;
use std::any::{Any, TypeId, type_name};
use std::cell::RefCell;
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::Display;
use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use thiserror::Error;
use tower::{Layer, Service, ServiceExt};

type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type Erased = Arc<dyn Any + Send + Sync>;

/// Failure inside the dependency machinery. Rendered to clients as an opaque
/// 500; the details are logged.
#[derive(Debug, Clone, Error)]
#[non_exhaustive]
pub enum DependencyError {
    /// A dependency (transitively) depends on itself.
    #[error("dependency cycle detected: {}", .chain.join(" -> "))]
    Cycle {
        /// Type names along the cycle, starting and ending with the same type.
        chain: Vec<&'static str>,
    },
    /// A value was requested with [`Provided`] but never registered.
    #[error("no value of type `{type_name}` was provided with App::provide")]
    NotProvided {
        /// The requested type.
        type_name: &'static str,
    },
}

/// A type that can be built from a request and injected with [`Depends`].
pub trait Dependency: Send + Sync + Sized + 'static {
    /// Build the value. Use `ctx` to read the request or resolve other
    /// dependencies.
    fn resolve(ctx: &mut ResolveContext<'_>)
    -> impl Future<Output = Result<Self, ApiError>> + Send;

    /// Document this dependency on `op` (parameters, security, ...).
    fn describe(_op: &mut Operation, _registry: &mut SchemaRegistry) {}
}

/// Owned snapshot of the request head handed to dependency overrides.
#[derive(Debug, Clone)]
pub struct RequestHead {
    /// Request method.
    pub method: http::Method,
    /// Request URI.
    pub uri: http::Uri,
    /// Request headers.
    pub headers: http::HeaderMap,
}

impl From<&Parts> for RequestHead {
    fn from(parts: &Parts) -> Self {
        Self {
            method: parts.method.clone(),
            uri: parts.uri.clone(),
            headers: parts.headers.clone(),
        }
    }
}

/// What a teardown future may return: `()` or a `Result<(), E>` whose error is
/// logged.
pub trait TeardownOutput: Send + 'static {
    /// The failure message, if the teardown failed.
    fn failure(self) -> Option<String>;
}

impl TeardownOutput for () {
    fn failure(self) -> Option<String> {
        None
    }
}

impl<E: Display + Send + 'static> TeardownOutput for Result<(), E> {
    fn failure(self) -> Option<String> {
        self.err().map(|e| e.to_string())
    }
}

type Teardown = BoxFut<'static, ()>;

#[derive(Default)]
struct ScopeState {
    cache: HashMap<TypeId, Erased>,
    stack: Vec<(TypeId, &'static str)>,
    teardowns: Vec<Teardown>,
}

/// Per-request resolution state, stored in the request extensions.
#[derive(Clone, Default)]
pub(crate) struct RequestScope(Arc<Mutex<ScopeState>>);

impl RequestScope {
    fn with<R>(&self, f: impl FnOnce(&mut ScopeState) -> R) -> R {
        f(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn of(parts: &mut Parts) -> Self {
        if let Some(scope) = parts.extensions.get::<Self>() {
            return scope.clone();
        }
        let scope = Self::default();
        parts.extensions.insert(scope.clone());
        scope
    }

    /// Run registered teardowns (LIFO) on a spawned task.
    fn spawn_teardowns(&self) {
        let list = self.with(|s| std::mem::take(&mut s.teardowns));
        if list.is_empty() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("no tokio runtime available; dependency teardown skipped");
            return;
        };
        runtime.spawn(async move {
            for teardown in list.into_iter().rev() {
                if let Err(err) = tokio::spawn(teardown).await {
                    tracing::error!(error = %err, "dependency teardown panicked");
                }
            }
        });
    }
}

type OverrideFn =
    Arc<dyn Fn(RequestHead) -> BoxFut<'static, Result<Erased, ApiError>> + Send + Sync>;
type GlobalFn =
    Arc<dyn for<'a> Fn(&'a mut Parts) -> BoxFut<'a, Result<(), ApiError>> + Send + Sync>;

/// Application-level DI configuration collected on an [`App`].
#[derive(Default)]
pub(crate) struct DiRegistry {
    provided: HashMap<TypeId, Erased>,
    overrides: HashMap<TypeId, OverrideFn>,
    globals: Vec<GlobalFn>,
}

impl DiRegistry {
    pub(crate) fn is_empty(&self) -> bool {
        self.provided.is_empty() && self.overrides.is_empty() && self.globals.is_empty()
    }
}

/// Chain of registries active for a request (innermost first).
#[derive(Clone)]
struct Active(Arc<ActiveNode>);

struct ActiveNode {
    registry: Arc<DiRegistry>,
    parent: Option<Active>,
}

impl Active {
    fn find<R>(&self, f: impl Fn(&DiRegistry) -> Option<R>) -> Option<R> {
        let mut node = Some(&self.0);
        while let Some(n) = node {
            if let Some(found) = f(&n.registry) {
                return Some(found);
            }
            node = n.parent.as_ref().map(|p| &p.0);
        }
        None
    }
}

fn downcast<U: Send + Sync + 'static>(value: Erased) -> Result<Arc<U>, ApiError> {
    value.downcast::<U>().map_err(|_| {
        ApiError::internal(format!(
            "dependency type mismatch for `{}`",
            type_name::<U>()
        ))
    })
}

/// Chain of type names if resolving `tid` now would close a cycle.
fn cycle_chain(state: &ScopeState, tid: TypeId, name: &'static str) -> Option<Vec<&'static str>> {
    let start = state.stack.iter().position(|(t, _)| *t == tid)?;
    let mut chain: Vec<&'static str> = state.stack[start..].iter().map(|(_, n)| *n).collect();
    chain.push(name);
    Some(chain)
}

async fn resolve_arc<U: Dependency>(parts: &mut Parts) -> Result<Arc<U>, ApiError> {
    let tid = TypeId::of::<U>();
    let name = type_name::<U>();
    let scope = RequestScope::of(parts);
    if let Some(hit) = scope.with(|s| s.cache.get(&tid).cloned()) {
        return downcast(hit);
    }
    if let Some(chain) = scope.with(|s| cycle_chain(s, tid, name)) {
        return Err(ApiError::internal(DependencyError::Cycle { chain }));
    }
    let active = parts.extensions.get::<Active>().cloned();
    if let Some(value) = active
        .as_ref()
        .and_then(|a| a.find(|r| r.provided.get(&tid).cloned()))
    {
        return downcast(value);
    }
    let override_fn = active
        .as_ref()
        .and_then(|a| a.find(|r| r.overrides.get(&tid).cloned()));
    scope.with(|s| s.stack.push((tid, name)));
    let outcome = match override_fn {
        Some(f) => f(RequestHead::from(&*parts)).await,
        None => {
            let mut ctx = ResolveContext { parts };
            U::resolve(&mut ctx).await.map(|v| Arc::new(v) as Erased)
        }
    };
    scope.with(|s| s.stack.pop());
    let value = outcome?;
    scope.with(|s| s.cache.insert(tid, Arc::clone(&value)));
    downcast(value)
}

/// Passed to [`Dependency::resolve`]: request access plus nested resolution.
pub struct ResolveContext<'a> {
    parts: &'a mut Parts,
}

impl<'a> ResolveContext<'a> {
    /// The request head (method, URI, headers, extensions). Other extractors
    /// can be run against it: `Header::from_request_parts(ctx.parts())`.
    pub fn parts(&mut self) -> &mut Parts {
        self.parts
    }

    /// Resolve another dependency (cached per request, cycle-checked).
    pub fn resolve<U: Dependency>(&mut self) -> BoxFut<'_, Result<Arc<U>, ApiError>> {
        Box::pin(resolve_arc::<U>(self.parts))
    }

    /// Register async cleanup to run after the response is produced (LIFO).
    /// See the [module docs](self) for the exact guarantees.
    pub fn on_teardown<F>(&mut self, teardown: F)
    where
        F: Future + Send + 'static,
        F::Output: TeardownOutput,
    {
        let wrapped: Teardown = Box::pin(async move {
            if let Some(message) = teardown.await.failure() {
                tracing::error!(error = %message, "dependency teardown failed");
            }
        });
        RequestScope::of(self.parts).with(|s| s.teardowns.push(wrapped));
    }
}

/// Extractor injecting a [`Dependency`] (request-scoped, cached).
#[derive(Debug)]
pub struct Depends<T>(pub Arc<T>);

impl<T> Clone for Depends<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Deref for Depends<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

thread_local! {
    static DESCRIBING: RefCell<Vec<TypeId>> = const { RefCell::new(Vec::new()) };
}

impl<T: Dependency> FromRequestParts for Depends<T> {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        resolve_arc::<T>(parts).await.map(Depends)
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        let tid = TypeId::of::<T>();
        let entered = DESCRIBING.with(|d| {
            let mut stack = d.borrow_mut();
            if stack.contains(&tid) {
                false
            } else {
                stack.push(tid);
                true
            }
        });
        if entered {
            T::describe(op, registry);
            DESCRIBING.with(|d| d.borrow_mut().pop());
        }
    }
}

/// Extractor for a value registered with [`App::provide`] (any
/// `Send + Sync + 'static` type, not only dependencies).
#[derive(Debug)]
pub struct Provided<T>(pub Arc<T>);

impl<T> Clone for Provided<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> Deref for Provided<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Send + Sync + 'static> FromRequestParts for Provided<T> {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        let tid = TypeId::of::<T>();
        let found = parts
            .extensions
            .get::<Active>()
            .and_then(|a| a.find(|r| r.provided.get(&tid).cloned()));
        match found {
            Some(value) => downcast(value).map(Provided),
            None => Err(ApiError::internal(DependencyError::NotProvided {
                type_name: type_name::<T>(),
            })),
        }
    }
}

impl App {
    /// Share `value` with every request of this app (and its mounts). See the
    /// [module docs](self).
    #[must_use]
    pub fn provide<T: Send + Sync + 'static>(self, value: T) -> Self {
        self.provide_arc(Arc::new(value))
    }

    /// Like [`App::provide`] for a value that is already shared, so the
    /// caller can keep a handle to it.
    #[must_use]
    pub fn provide_arc<T: Send + Sync + 'static>(mut self, value: Arc<T>) -> Self {
        self.di.provided.insert(TypeId::of::<T>(), value);
        self
    }

    /// Replace `T::resolve` with `f` (also for nested resolution). `f` gets an
    /// owned [`RequestHead`] because a borrowed request cannot cross the
    /// returned future.
    #[must_use]
    pub fn override_dependency<T, F, Fut>(mut self, f: F) -> Self
    where
        T: Dependency,
        F: Fn(RequestHead) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, ApiError>> + Send + 'static,
    {
        let erased: OverrideFn = Arc::new(move |head| {
            let fut = f(head);
            Box::pin(async move { fut.await.map(|v| Arc::new(v) as Erased) })
        });
        self.di.overrides.insert(TypeId::of::<T>(), erased);
        self
    }

    /// Replace `T::resolve` with a clone of `value`.
    #[must_use]
    pub fn override_value<T: Dependency + Clone>(self, value: T) -> Self {
        self.override_dependency(move |_| {
            let value = value.clone();
            async move { Ok(value) }
        })
    }

    /// Resolve `T` before every routed request; its error short-circuits.
    /// The value stays cached, so handlers asking for `Depends<T>` reuse it.
    #[must_use]
    pub fn dependency<T: Dependency>(mut self) -> Self {
        self.di.globals.push(Arc::new(|parts: &mut Parts| {
            Box::pin(async move { resolve_arc::<T>(parts).await.map(|_| ()) })
        }));
        self
    }
}

/// Wrap `router` so requests carry the DI scope, run global dependencies and
/// trigger teardown.
pub(crate) fn install(router: axum::Router, registry: DiRegistry) -> axum::Router {
    router.layer(DiLayer(Arc::new(registry)))
}

#[derive(Clone)]
struct DiLayer(Arc<DiRegistry>);

impl<S> Layer<S> for DiLayer {
    type Service = DiService<S>;

    fn layer(&self, inner: S) -> DiService<S> {
        DiService {
            inner,
            registry: Arc::clone(&self.0),
        }
    }
}

#[derive(Clone)]
struct DiService<S> {
    inner: S,
    registry: Arc<DiRegistry>,
}

impl<S> Service<axum::extract::Request> for DiService<S>
where
    S: Service<axum::extract::Request, Response = axum::response::Response, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send,
{
    type Response = axum::response::Response;
    type Error = Infallible;
    type Future = BoxFut<'static, Result<Self::Response, Infallible>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: axum::extract::Request) -> Self::Future {
        let ready = self.inner.clone();
        let inner = std::mem::replace(&mut self.inner, ready);
        let registry = Arc::clone(&self.registry);
        Box::pin(async move {
            let (mut parts, body) = req.into_parts();
            crate::middleware::note_matched_path(&parts);
            let parent = parts.extensions.get::<Active>().cloned();
            let node = ActiveNode {
                registry: Arc::clone(&registry),
                parent,
            };
            parts.extensions.insert(Active(Arc::new(node)));
            let owned = match parts.extensions.get::<RequestScope>() {
                Some(_) => None,
                None => {
                    let scope = RequestScope::default();
                    parts.extensions.insert(scope.clone());
                    Some(scope)
                }
            };
            let response = match run_globals(&registry, &mut parts).await {
                Err(err) => err.into_response().map(Body::into_inner),
                Ok(()) => {
                    inner
                        .oneshot(http::Request::from_parts(parts, body))
                        .await?
                }
            };
            if let Some(scope) = owned {
                scope.spawn_teardowns();
            }
            Ok(response)
        })
    }
}

async fn run_globals(registry: &DiRegistry, parts: &mut Parts) -> Result<(), ApiError> {
    for global in &registry.globals {
        global(parts).await?;
    }
    Ok(())
}
