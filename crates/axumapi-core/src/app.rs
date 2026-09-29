//! The [`App`] builder.

use crate::error::{ApiError, ServerError};
use crate::routing::MethodRouter;
use crate::service::RouterService;
use axum::response::IntoResponse;
use std::sync::Arc;

/// Application metadata, kept for later OpenAPI generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppMeta {
    /// API title.
    pub title: String,
    /// API version.
    pub version: String,
}

impl Default for AppMeta {
    fn default() -> Self {
        Self {
            title: "axumapi".to_owned(),
            version: "0.1.0".to_owned(),
        }
    }
}

/// Deferred router transformation that installs one piece of shared state.
type StateLayer = Box<dyn FnOnce(axum::Router) -> axum::Router + Send>;

/// An HTTP application: routes, shared state and metadata.
pub struct App {
    router: axum::Router,
    meta: AppMeta,
    state_layers: Vec<StateLayer>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("meta", &self.meta)
            .field("state_layers", &self.state_layers.len())
            .finish_non_exhaustive()
    }
}

async fn route_not_found() -> impl IntoResponse {
    ApiError::not_found("No route matches the requested path.")
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    /// Create an empty application; unknown routes yield a 404 problem document.
    pub fn new() -> Self {
        Self {
            router: axum::Router::new().fallback(route_not_found),
            meta: AppMeta::default(),
            state_layers: Vec::new(),
        }
    }

    /// Set the API title.
    #[must_use]
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.meta.title = title.into();
        self
    }

    /// Set the API version.
    #[must_use]
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.meta.version = version.into();
        self
    }

    /// The application metadata.
    pub fn meta(&self) -> &AppMeta {
        &self.meta
    }

    /// Register `methods` at `path` (use `{param}` for path parameters).
    ///
    /// # Panics
    /// Panics at start-up on an invalid or conflicting path (for example
    /// one not starting with `/`), as this is a programmer error.
    #[must_use]
    pub fn route(mut self, path: &str, methods: MethodRouter) -> Self {
        self.router = self.router.route(path, methods.0);
        self
    }

    /// Mount `app` under `prefix` (e.g. `/api/v1`).
    ///
    /// # Panics
    /// Panics at start-up if `prefix` is invalid or conflicts with existing routes.
    #[must_use]
    pub fn nest(mut self, prefix: &str, app: App) -> Self {
        self.router = self.router.nest(prefix, app.finalize());
        self
    }

    /// Make `state` available to handlers through [`State<T>`](crate::State).
    ///
    /// State is installed when the app is finalized, so it applies to every
    /// route regardless of call order, including mounted child apps.
    #[must_use]
    pub fn with_state<T: Send + Sync + 'static>(mut self, state: T) -> Self {
        let state = Arc::new(state);
        self.state_layers
            .push(Box::new(move |router: axum::Router| {
                router.layer(axum::Extension(state))
            }));
        self
    }

    /// Apply deferred state layers and return the finished router.
    fn finalize(self) -> axum::Router {
        self.state_layers
            .into_iter()
            .fold(self.router, |router, layer| layer(router))
    }

    /// Convert into an in-process `tower::Service` (used by the testkit).
    pub fn into_router_service(self) -> RouterService {
        RouterService::new(self.finalize())
    }

    /// Bind `addr` and serve until ctrl-c, then shut down gracefully.
    ///
    /// # Errors
    /// Returns [`ServerError`] if binding or serving fails.
    pub async fn run(self, addr: &str) -> Result<(), ServerError> {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| ServerError::Bind {
                addr: addr.to_owned(),
                source,
            })?;
        tracing::info!(%addr, title = %self.meta.title, version = %self.meta.version, "listening");
        axum::serve(listener, self.finalize())
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(ServerError::Serve)
    }
}

async fn shutdown_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        tracing::warn!(error = %err, "failed to listen for ctrl-c; graceful shutdown disabled");
        std::future::pending::<()>().await;
    }
}
