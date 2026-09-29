//! Request ids and structured request logging.

use super::adapt::{BoxService, Next, from_fn, impl_layer};
use crate::error::ApiError;
use crate::extract::{FromRequestParts, Request};
use crate::response::Response;
use http::request::Parts;
use http::{HeaderName, HeaderValue};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;
use tower::Layer;
use tracing::Instrument;

const DEFAULT_HEADER: &str = "x-request-id";
const MAX_ID_LEN: usize = 128;

/// The id of the current request, set by the [`RequestIdLayer`] middleware.
///
/// Usable as a handler argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromRequestParts for RequestId {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<RequestId>()
            .cloned()
            .ok_or_else(|| ApiError::internal("RequestId used without the request_id middleware"))
    }
}

/// Assigns every request an id: an incoming `x-request-id` is propagated when
/// it is 1..=128 visible ASCII characters, otherwise a UUID v4 is generated.
/// The id is exposed as [`RequestId`], forwarded to the handler as a request
/// header and echoed on the response.
#[derive(Debug, Clone)]
pub struct RequestIdLayer {
    header: HeaderName,
}

impl Default for RequestIdLayer {
    fn default() -> Self {
        Self {
            header: HeaderName::from_static(DEFAULT_HEADER),
        }
    }
}

impl RequestIdLayer {
    /// Use the default `x-request-id` header.
    pub fn new() -> Self {
        Self::default()
    }

    /// Use another header name.
    #[must_use]
    pub fn header(mut self, name: HeaderName) -> Self {
        self.header = name;
        self
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let header = self.header.clone();
        from_fn(move |mut req: Request, next: Next| {
            let header = header.clone();
            async move {
                let id = req
                    .headers()
                    .get(&header)
                    .and_then(|v| v.to_str().ok())
                    .filter(|v| valid_id(v))
                    .map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
                let value = HeaderValue::from_str(&id).ok();
                if let Some(value) = &value {
                    req.headers_mut().insert(header.clone(), value.clone());
                }
                req.extensions_mut().insert(RequestId(id));
                let mut response = next.run(req).await;
                if let Some(value) = value {
                    response.headers_mut().insert(header, value);
                }
                response
            }
        })
        .layer(inner)
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && id.bytes().all(|b| b.is_ascii_graphic())
}

/// Filled with the matched route template by the DI layer (which runs after
/// routing) so outer middleware can read it once the response is ready.
#[derive(Clone, Default)]
pub(crate) struct RouteSlot(Arc<Mutex<Option<String>>>);

impl RouteSlot {
    fn set(&self, route: String) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(route);
    }

    fn get(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Record the matched route in the request's [`RouteSlot`], if any.
pub(crate) fn note_matched_path(parts: &Parts) {
    if let (Some(slot), Some(path)) = (
        parts.extensions.get::<RouteSlot>(),
        parts.extensions.get::<axum::extract::MatchedPath>(),
    ) {
        slot.set(path.as_str().to_owned());
    }
}

/// Emits a tracing span and a completion event per request with
/// `request_id`, `method`, `path`, `route` (matched template), `status` and
/// `latency_ms`.
///
/// **Privacy:** only those fields are recorded. Headers (Authorization,
/// Cookie, API keys, ...), the query string and bodies are never logged.
/// Register [`RequestIdLayer`] first so the id is available.
#[derive(Debug, Clone, Default)]
pub struct RequestLogging;

impl RequestLogging {
    /// Create the middleware.
    pub fn new() -> Self {
        Self
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        from_fn(|mut req: Request, next: Next| async move {
            let start = Instant::now();
            let slot = RouteSlot::default();
            req.extensions_mut().insert(slot.clone());
            let request_id = req
                .extensions()
                .get::<RequestId>()
                .map_or_else(|| "-".to_owned(), |id| id.0.clone());
            let span = tracing::info_span!(
                "http_request",
                %request_id,
                method = %req.method(),
                path = req.uri().path(),
            );
            let response: Response = next.run(req).instrument(span.clone()).await;
            let status = response.status().as_u16();
            let latency_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
            let route = slot.get().unwrap_or_else(|| "<unmatched>".to_owned());
            tracing::info!(parent: &span, %route, status, latency_ms, "request completed");
            response
        })
        .layer(inner)
    }
}

impl_layer!(RequestIdLayer, RequestLogging);
