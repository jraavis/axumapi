//! CORS and response compression (thin, configurable wrappers over
//! `tower-http`).

use super::adapt::{BoxService, impl_layer, wrap_engine};
use http::{HeaderName, HeaderValue, Method};
use std::time::Duration;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

#[derive(Debug, Clone, Default)]
enum Allow<T> {
    #[default]
    Nothing,
    Any,
    List(Vec<T>),
}

/// Cross-origin resource sharing.
///
/// [`Cors::new`] allows nothing; add origins/methods/headers or start from
/// [`Cors::permissive`]. With `allow_credentials(true)`, "any" is answered by
/// mirroring the request (browsers reject a literal `*` together with
/// credentials).
#[derive(Debug, Clone, Default)]
pub struct Cors {
    origins: Allow<HeaderValue>,
    methods: Allow<Method>,
    headers: Allow<HeaderName>,
    expose: Vec<HeaderName>,
    credentials: bool,
    max_age: Option<Duration>,
}

impl Cors {
    /// A policy that allows no cross-origin access until configured.
    pub fn new() -> Self {
        Self::default()
    }

    /// Any origin, method and header; no credentials.
    pub fn permissive() -> Self {
        Self::new()
            .allow_any_origin()
            .allow_any_method()
            .allow_any_header()
    }

    /// Allow one origin (e.g. `https://app.example.com`). Invalid values are
    /// ignored with a warning.
    #[must_use]
    pub fn allow_origin(mut self, origin: &str) -> Self {
        match HeaderValue::from_str(origin) {
            Ok(value) => push(&mut self.origins, value),
            Err(_) => tracing::warn!(origin, "ignoring invalid CORS origin"),
        }
        self
    }

    /// Allow every origin.
    #[must_use]
    pub fn allow_any_origin(mut self) -> Self {
        self.origins = Allow::Any;
        self
    }

    /// Allow one method.
    #[must_use]
    pub fn allow_method(mut self, method: Method) -> Self {
        push(&mut self.methods, method);
        self
    }

    /// Allow every method.
    #[must_use]
    pub fn allow_any_method(mut self) -> Self {
        self.methods = Allow::Any;
        self
    }

    /// Allow one request header. Invalid names are ignored with a warning.
    #[must_use]
    pub fn allow_header(mut self, name: &str) -> Self {
        match HeaderName::from_bytes(name.as_bytes()) {
            Ok(name) => push(&mut self.headers, name),
            Err(_) => tracing::warn!(name, "ignoring invalid CORS header name"),
        }
        self
    }

    /// Allow every request header.
    #[must_use]
    pub fn allow_any_header(mut self) -> Self {
        self.headers = Allow::Any;
        self
    }

    /// Expose a response header to browser scripts.
    #[must_use]
    pub fn expose_header(mut self, name: &str) -> Self {
        match HeaderName::from_bytes(name.as_bytes()) {
            Ok(name) => self.expose.push(name),
            Err(_) => tracing::warn!(name, "ignoring invalid CORS header name"),
        }
        self
    }

    /// Allow credentials (cookies, authorization headers).
    #[must_use]
    pub fn allow_credentials(mut self, allow: bool) -> Self {
        self.credentials = allow;
        self
    }

    /// How long browsers may cache preflight results.
    #[must_use]
    pub fn max_age(mut self, max_age: Duration) -> Self {
        self.max_age = Some(max_age);
        self
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let mirror = self.credentials;
        let origin = match &self.origins {
            Allow::Nothing => AllowOrigin::list([]),
            Allow::Any if mirror => AllowOrigin::mirror_request(),
            Allow::Any => AllowOrigin::any(),
            Allow::List(v) => AllowOrigin::list(v.clone()),
        };
        let methods = match &self.methods {
            Allow::Nothing => AllowMethods::list([]),
            Allow::Any if mirror => AllowMethods::mirror_request(),
            Allow::Any => AllowMethods::any(),
            Allow::List(v) => AllowMethods::list(v.clone()),
        };
        let headers = match &self.headers {
            Allow::Nothing => AllowHeaders::list([]),
            Allow::Any if mirror => AllowHeaders::mirror_request(),
            Allow::Any => AllowHeaders::any(),
            Allow::List(v) => AllowHeaders::list(v.clone()),
        };
        let mut layer = CorsLayer::new()
            .allow_origin(origin)
            .allow_methods(methods)
            .allow_headers(headers)
            .expose_headers(self.expose.clone())
            .allow_credentials(self.credentials);
        if let Some(age) = self.max_age {
            layer = layer.max_age(age);
        }
        wrap_engine(&layer, inner)
    }
}

fn push<T>(slot: &mut Allow<T>, item: T) {
    match slot {
        Allow::List(items) => items.push(item),
        other => *other = Allow::List(vec![item]),
    }
}

/// Response compression (gzip and brotli, negotiated via `Accept-Encoding`).
///
/// Tiny bodies, images and already-compressed types are left alone.
#[derive(Debug, Clone)]
pub struct Compression {
    gzip: bool,
    brotli: bool,
}

impl Default for Compression {
    fn default() -> Self {
        Self {
            gzip: true,
            brotli: true,
        }
    }
}

impl Compression {
    /// gzip and brotli enabled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable or disable gzip.
    #[must_use]
    pub fn gzip(mut self, enabled: bool) -> Self {
        self.gzip = enabled;
        self
    }

    /// Enable or disable brotli.
    #[must_use]
    pub fn brotli(mut self, enabled: bool) -> Self {
        self.brotli = enabled;
        self
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let layer = CompressionLayer::new().gzip(self.gzip).br(self.brotli);
        wrap_engine(&layer, inner)
    }
}

impl_layer!(Cors, Compression);
