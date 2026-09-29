//! The [`App`] builder.
//!
//! An `App` is a *description* of an application: routes, mounted child
//! apps, state, middleware and lifespan hooks. Nothing is materialised until
//! [`App::run`] or [`App::into_router_service`], which validate the whole
//! configuration first and report problems as [`ServerError::Configuration`]
//! instead of panicking.
//!
//! # Mounted applications
//! `app.mount("/api/v1", child)` keeps the child's state and middleware
//! scoped to the child's routes. Child routes are **merged** into the root
//! OpenAPI document with the prefix prepended; child docs settings are
//! ignored. Child lifespan hooks are lifted into the parent (child hooks run
//! after the parent's at startup, before them at shutdown).

use crate::body::Body;
use crate::error::{ApiError, ServerError};
use crate::response::IntoResponse;
use crate::routing::{Endpoint, MethodRouter, Route};
use axumapi_openapi::{DocumentBuilder, OpenApi, OpenApiError, ui};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Future type used by lifespan hooks.
pub(crate) type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Deferred transformation of the internal router (state, middleware,
/// services). Applied in registration order when the app is built.
pub(crate) type RouterLayer = Box<dyn FnOnce(axum::Router) -> axum::Router + Send>;

/// A startup or shutdown hook.
pub(crate) type LifespanHook = Box<dyn FnOnce() -> BoxFuture<Result<(), ApiError>> + Send>;

/// Application metadata used for OpenAPI generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppMeta {
    /// API title.
    pub title: String,
    /// API version.
    pub version: String,
    /// API description.
    pub description: Option<String>,
}

impl Default for AppMeta {
    fn default() -> Self {
        Self {
            title: "axumapi".to_owned(),
            version: "0.1.0".to_owned(),
            description: None,
        }
    }
}

/// Where the generated documentation is served. `None` disables an endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsConfig {
    /// OpenAPI JSON document (default `/openapi.json`).
    pub openapi_url: Option<String>,
    /// Swagger UI (default `/docs`); requires `openapi_url`.
    pub swagger_url: Option<String>,
    /// ReDoc (default `/redoc`); requires `openapi_url`.
    pub redoc_url: Option<String>,
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            openapi_url: Some("/openapi.json".to_owned()),
            swagger_url: Some("/docs".to_owned()),
            redoc_url: Some("/redoc".to_owned()),
        }
    }
}

/// A child app mounted under a prefix.
struct Mount {
    prefix: String,
    app: App,
}

/// An HTTP application: routes, state, middleware, lifespan and metadata.
pub struct App {
    pub(crate) meta: AppMeta,
    pub(crate) docs: DocsConfig,
    pub(crate) routes: Vec<(String, MethodRouter)>,
    mounts: Vec<Mount>,
    /// State and middleware layers, applied in registration order.
    pub(crate) router_layers: Vec<RouterLayer>,
    /// Services mounted at prefixes (e.g. static files); not documented.
    pub(crate) services: Vec<RouterLayer>,
    pub(crate) startup_hooks: Vec<LifespanHook>,
    pub(crate) shutdown_hooks: Vec<LifespanHook>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("meta", &self.meta)
            .field(
                "routes",
                &self.routes.iter().map(|(p, _)| p).collect::<Vec<_>>(),
            )
            .field(
                "mounts",
                &self.mounts.iter().map(|m| &m.prefix).collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

async fn route_not_found() -> axum::response::Response {
    ApiError::not_found("No route matches the requested path.")
        .into_response()
        .map(Body::into_inner)
}

impl App {
    /// Create an empty application.
    pub fn new() -> Self {
        Self {
            meta: AppMeta::default(),
            docs: DocsConfig::default(),
            routes: Vec::new(),
            mounts: Vec::new(),
            router_layers: Vec::new(),
            services: Vec::new(),
            startup_hooks: Vec::new(),
            shutdown_hooks: Vec::new(),
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

    /// Set the API description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.meta.description = Some(description.into());
        self
    }

    /// Configure documentation endpoints.
    #[must_use]
    pub fn docs(mut self, docs: DocsConfig) -> Self {
        self.docs = docs;
        self
    }

    /// The application metadata.
    pub fn meta(&self) -> &AppMeta {
        &self.meta
    }

    /// Register `methods` at `path` (use `{param}` for path parameters).
    #[must_use]
    pub fn route(mut self, path: &str, methods: MethodRouter) -> Self {
        self.routes.push((path.to_owned(), methods));
        self
    }

    /// Register routes produced by route macros (see `routes![]`).
    #[must_use]
    pub fn routes(mut self, routes: impl IntoIterator<Item = Route>) -> Self {
        for route in routes {
            self = self.route(route.path, route.router);
        }
        self
    }

    /// Mount `app` under `prefix` (e.g. `/api/v1`). See the module docs.
    #[must_use]
    pub fn mount(mut self, prefix: &str, app: App) -> Self {
        self.mounts.push(Mount {
            prefix: prefix.trim_end_matches('/').to_owned(),
            app,
        });
        self
    }

    /// Alias of [`App::mount`].
    #[must_use]
    pub fn nest(self, prefix: &str, app: App) -> Self {
        self.mount(prefix, app)
    }

    /// Make `state` available to handlers through [`State<T>`](crate::State).
    ///
    /// Applies to every route of this app regardless of call order.
    #[must_use]
    pub fn with_state<T: Send + Sync + 'static>(mut self, state: T) -> Self {
        let state = Arc::new(state);
        self.router_layers
            .push(Box::new(move |router: axum::Router| {
                router.layer(axum::Extension(state))
            }));
        self
    }

    /// Every documented endpoint as `(full path, endpoint)`, including mounts.
    fn endpoints(&self) -> Vec<(String, &Endpoint)> {
        let mut out: Vec<(String, &Endpoint)> = self
            .routes
            .iter()
            .flat_map(|(path, r)| r.endpoints.iter().map(move |e| (path.clone(), e)))
            .collect();
        for mount in &self.mounts {
            for (path, e) in mount.app.endpoints() {
                let full = if path == "/" {
                    mount.prefix.clone()
                } else {
                    format!("{}{path}", mount.prefix)
                };
                out.push((full, e));
            }
        }
        out
    }

    /// Generate the OpenAPI 3.1 document for this app and its mounts.
    ///
    /// # Errors
    /// Returns [`OpenApiError`] on duplicate operations/ids or schema name
    /// conflicts.
    pub fn openapi(&self) -> Result<OpenApi, OpenApiError> {
        let mut builder = DocumentBuilder::new(&self.meta.title, &self.meta.version);
        if let Some(d) = &self.meta.description {
            builder = builder.description(d);
        }
        for (path, endpoint) in self.endpoints() {
            if endpoint.meta.hidden {
                continue;
            }
            if let Some(method) = endpoint.openapi_method() {
                let op = endpoint.operation(builder.registry());
                builder.add_operation(&path, method, op);
            }
        }
        builder.build()
    }

    /// Validate route paths and reject duplicate `(path, method)` pairs.
    fn validate(&self) -> Result<(), ServerError> {
        let mut seen: BTreeMap<(String, String), ()> = BTreeMap::new();
        for (path, e) in self.endpoints() {
            if !path.starts_with('/') {
                return Err(ServerError::Configuration(format!(
                    "route path `{path}` must start with `/`"
                )));
            }
            if seen
                .insert((path.clone(), e.method.to_string()), ())
                .is_some()
            {
                return Err(ServerError::Configuration(format!(
                    "duplicate route {} {path}",
                    e.method
                )));
            }
        }
        for m in &self.mounts {
            if !m.prefix.starts_with('/') {
                return Err(ServerError::Configuration(format!(
                    "mount prefix `{}` must start with `/`",
                    m.prefix
                )));
            }
        }
        Ok(())
    }

    /// Build the internal router; lifts lifespan hooks out of mounts.
    fn build_router(mut self, hooks: &mut Hooks) -> axum::Router {
        hooks.startup.append(&mut self.startup_hooks);
        let mut own_shutdown = std::mem::take(&mut self.shutdown_hooks);

        let mut grouped: BTreeMap<String, Vec<Endpoint>> = BTreeMap::new();
        for (path, r) in self.routes {
            grouped.entry(path).or_default().extend(r.endpoints);
        }
        let mut router = axum::Router::new();
        for (path, endpoints) in grouped {
            let method_router = endpoints.into_iter().filter_map(Endpoint::into_axum).fold(
                axum::routing::MethodRouter::new(),
                axum::routing::MethodRouter::merge,
            );
            router = router.route(&path, method_router);
        }
        for mount in self.mounts {
            let child = mount.app.build_router(hooks);
            router = router.nest(&mount.prefix, child);
        }
        for service in self.services {
            router = service(router);
        }
        router = self
            .router_layers
            .into_iter()
            .fold(router, |r, layer| layer(r));
        // Child shutdown hooks were appended while building mounts; ours run last.
        hooks.shutdown.append(&mut own_shutdown);
        router
    }

    /// Validate and build the root router, including documentation routes.
    pub(crate) fn build(self) -> Result<(axum::Router, Hooks), ServerError> {
        self.validate()?;
        let openapi = self
            .openapi()
            .map_err(|e| ServerError::Configuration(e.to_string()))?;
        let docs = self.docs.clone();
        let title = self.meta.title.clone();
        let mut hooks = Hooks::default();
        let mut router = self.build_router(&mut hooks);
        if let Some(spec_url) = docs.openapi_url {
            let json = serde_json::to_string(&openapi)
                .map_err(|e| ServerError::Configuration(e.to_string()))?;
            router = router.route(&spec_url, static_route("application/json", json));
            if let Some(url) = docs.swagger_url {
                let html = ui::swagger_ui_html(&title, &spec_url);
                router = router.route(&url, static_route("text/html; charset=utf-8", html));
            }
            if let Some(url) = docs.redoc_url {
                let html = ui::redoc_html(&title, &spec_url);
                router = router.route(&url, static_route("text/html; charset=utf-8", html));
            }
        }
        Ok((router.fallback(route_not_found), hooks))
    }

    /// Convert into an in-process `tower::Service` (used by the testkit).
    /// Lifespan hooks are **not** run.
    ///
    /// # Errors
    /// Returns [`ServerError::Configuration`] if the app is misconfigured.
    pub fn into_router_service(self) -> Result<crate::RouterService, ServerError> {
        self.build()
            .map(|(router, _)| crate::RouterService::new(router))
    }

    /// Run startup hooks, bind `addr`, serve until ctrl-c, then run shutdown
    /// hooks.
    ///
    /// # Errors
    /// Returns [`ServerError`] on misconfiguration, hook failure, or I/O errors.
    pub async fn run(self, addr: &str) -> Result<(), ServerError> {
        let (title, version) = (self.meta.title.clone(), self.meta.version.clone());
        let (router, hooks) = self.build()?;
        for hook in hooks.startup {
            hook().await.map_err(ServerError::Lifespan)?;
        }
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| ServerError::Bind {
                addr: addr.to_owned(),
                source,
            })?;
        tracing::info!(%addr, %title, %version, "listening");
        let served = axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await
            .map_err(ServerError::Serve);
        let mut first_error = None;
        for hook in hooks.shutdown.into_iter().rev() {
            if let Err(err) = hook().await {
                tracing::error!(error = %err, "shutdown hook failed");
                first_error.get_or_insert(err);
            }
        }
        served?;
        first_error.map_or(Ok(()), |e| Err(ServerError::Lifespan(e)))
    }
}

/// Lifespan hooks collected from an app tree.
#[derive(Default)]
pub(crate) struct Hooks {
    pub(crate) startup: Vec<LifespanHook>,
    pub(crate) shutdown: Vec<LifespanHook>,
}

fn static_route(content_type: &'static str, body: String) -> axum::routing::MethodRouter {
    let body: Arc<str> = body.into();
    axum::routing::get(move || {
        let body = Arc::clone(&body);
        async move {
            let mut resp = axum::response::Response::new(axum::body::Body::from(body.to_string()));
            resp.headers_mut().insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_static(content_type),
            );
            resp
        }
    })
}

async fn shutdown_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        tracing::warn!(error = %err, "failed to listen for ctrl-c; graceful shutdown disabled");
        std::future::pending::<()>().await;
    }
}
