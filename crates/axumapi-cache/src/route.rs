//! [`RouteCache`]: HTTP GET/HEAD response cache as an `App` layer.

use crate::Cache;
use axumapi_core::middleware::{BoxService, Next, from_fn};
use axumapi_core::{ApiError, Body, IntoResponse, Request, Response};
use http::header::{AUTHORIZATION, CACHE_CONTROL, COOKIE, SET_COOKIE};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tower::Layer;

const X_CACHE: HeaderName = HeaderName::from_static("x-cache");
const HIT: HeaderValue = HeaderValue::from_static("hit");
const MISS: HeaderValue = HeaderValue::from_static("miss");

/// Hop-by-hop headers (RFC 9110) plus the marker this layer writes.
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
            | "x-cache"
    )
}

/// Cached GET/HEAD 200 response (headers + body).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedResponse {
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

/// Middleware that caches successful GET and HEAD responses.
///
/// Plug it in with `App::layer`:
///
/// ```
/// use axumapi_cache::{MemoryCache, RouteCache};
/// use axumapi_core::{App, get};
/// use std::time::Duration;
///
/// let _app = App::new()
///     .route("/ok", get(|| async { "ok" }))
///     .layer(RouteCache::new(
///         MemoryCache::new(1024),
///         Duration::from_secs(30),
///     ));
/// ```
///
/// # Behaviour
/// * Only [`GET`](Method::GET) and [`HEAD`](Method::HEAD) are considered.
/// * The cache key is `{method} {uri}` (path and query, as received).
/// * Requests with `Authorization` or `Cookie` bypass the cache.
/// * Responses are stored only when the status is 200, there is no
///   `Set-Cookie`, and `Cache-Control` does not contain `no-store` or
///   `private`.
/// * Every GET/HEAD response is tagged with `x-cache: hit` or `x-cache: miss`.
///
/// Cache backend failures fail open: the request is served and treated as a
/// miss. Values, passwords and tokens are never logged.
#[derive(Clone, Debug)]
pub struct RouteCache<C> {
    cache: Arc<C>,
    ttl: Duration,
}

impl<C: Cache> RouteCache<C> {
    /// Cache GET/HEAD 200 responses in `cache` for `ttl`.
    ///
    /// A `ttl` of zero stores nothing; GET/HEAD still receive `x-cache: miss`.
    #[must_use]
    pub fn new(cache: C, ttl: Duration) -> Self {
        Self {
            cache: Arc::new(cache),
            ttl,
        }
    }
}

impl<C: Cache> Layer<BoxService> for RouteCache<C> {
    type Service = BoxService;

    fn layer(&self, inner: BoxService) -> Self::Service {
        let cache = Arc::clone(&self.cache);
        let ttl = self.ttl;
        from_fn(move |req: Request, next: Next| {
            let cache = Arc::clone(&cache);
            async move { dispatch(cache, ttl, req, next).await }
        })
        .layer(inner)
    }
}

fn is_cacheable_method(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD
}

fn request_bypasses_cache(headers: &HeaderMap) -> bool {
    headers.contains_key(AUTHORIZATION) || headers.contains_key(COOKIE)
}

/// True when any `Cache-Control` directive is `no-store` or `private`.
pub(crate) fn cache_control_forbids_store(headers: &HeaderMap) -> bool {
    headers.get_all(CACHE_CONTROL).iter().any(|value| {
        let Ok(text) = value.to_str() else {
            return false;
        };
        text.split(',').any(|directive| {
            let name = directive
                .split('=')
                .next()
                .unwrap_or(directive)
                .trim()
                .trim_matches('"');
            name.eq_ignore_ascii_case("no-store") || name.eq_ignore_ascii_case("private")
        })
    })
}

fn is_cacheable_response(response: &Response) -> bool {
    response.status() == StatusCode::OK
        && !response.headers().contains_key(SET_COOKIE)
        && !cache_control_forbids_store(response.headers())
}

fn cache_key(req: &Request) -> String {
    format!("{} {}", req.method(), req.uri())
}

fn encode(headers: &HeaderMap, body: Vec<u8>) -> Result<Vec<u8>, serde_json::Error> {
    let headers = headers
        .iter()
        .filter(|(name, _)| !is_hop_by_hop(name))
        .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
        .collect();
    serde_json::to_vec(&CachedResponse { headers, body })
}

fn decode(bytes: &[u8]) -> Option<(HeaderMap, Vec<u8>)> {
    let stored: CachedResponse = serde_json::from_slice(bytes).ok()?;
    let mut headers = HeaderMap::new();
    for (name, value) in stored.headers {
        let name = HeaderName::try_from(name).ok()?;
        if is_hop_by_hop(&name) {
            continue;
        }
        let value = HeaderValue::from_bytes(&value).ok()?;
        headers.append(name, value);
    }
    Some((headers, stored.body))
}

fn with_x_cache(mut response: Response, value: HeaderValue) -> Response {
    response.headers_mut().insert(X_CACHE, value);
    response
}

fn restore(headers: HeaderMap, body: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(body.clone()));
    *response.status_mut() = StatusCode::OK;
    *response.headers_mut() = headers;
    if let Ok(len) = HeaderValue::from_str(&body.len().to_string()) {
        response.headers_mut().insert(header::CONTENT_LENGTH, len);
    }
    with_x_cache(response, HIT)
}

async fn dispatch<C: Cache>(cache: Arc<C>, ttl: Duration, req: Request, next: Next) -> Response {
    if !is_cacheable_method(req.method()) {
        return next.run(req).await;
    }
    if request_bypasses_cache(req.headers()) {
        return with_x_cache(next.run(req).await, MISS);
    }

    let key = cache_key(&req);
    if let Ok(Some(bytes)) = cache.get(&key).await
        && let Some((headers, body)) = decode(&bytes)
    {
        return restore(headers, body);
    }

    let response = next.run(req).await;
    if !is_cacheable_response(&response) {
        return with_x_cache(response, MISS);
    }

    let (mut parts, body) = response.into_parts();
    let bytes = match body.into_bytes().await {
        Ok(bytes) => bytes,
        Err(_) => {
            return ApiError::internal("failed to buffer a cacheable response body")
                .into_response();
        }
    };

    if ttl > Duration::ZERO
        && let Ok(payload) = encode(&parts.headers, bytes.clone())
    {
        let _ = cache.set(&key, payload, Some(ttl)).await;
    }

    parts.headers.insert(X_CACHE, MISS);
    Response::from_parts(parts, Body::from(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::try_from(*name).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn cache_control_detects_no_store_and_private() {
        assert!(cache_control_forbids_store(&headers(&[(
            "cache-control",
            "no-store"
        )])));
        assert!(cache_control_forbids_store(&headers(&[(
            "cache-control",
            "max-age=0, Private"
        )])));
        assert!(cache_control_forbids_store(&headers(&[(
            "cache-control",
            "public, no-store, max-age=60"
        )])));
        assert!(cache_control_forbids_store(&headers(&[
            ("cache-control", "max-age=60"),
            ("cache-control", "private=\"Set-Cookie\""),
        ])));
        assert!(!cache_control_forbids_store(&headers(&[(
            "cache-control",
            "public, max-age=60"
        )])));
        assert!(!cache_control_forbids_store(&headers(&[(
            "cache-control",
            "no-cache"
        )])));
        assert!(!cache_control_forbids_store(&HeaderMap::new()));
    }

    #[test]
    fn encode_decode_roundtrip_strips_hop_by_hop() {
        let mut map = headers(&[
            ("content-type", "text/plain"),
            ("x-foo", "bar"),
            ("transfer-encoding", "chunked"),
            ("x-cache", "miss"),
        ]);
        map.append("x-foo", HeaderValue::from_static("baz"));
        let encoded = encode(&map, b"hello".to_vec()).unwrap();
        let (decoded, body) = decode(&encoded).unwrap();
        assert_eq!(body, b"hello");
        assert_eq!(
            decoded
                .get_all("x-foo")
                .iter()
                .map(|v| v.as_bytes())
                .collect::<Vec<_>>(),
            [b"bar".as_slice(), b"baz".as_slice()]
        );
        assert!(decoded.get("transfer-encoding").is_none());
        assert!(decoded.get("x-cache").is_none());
        assert_eq!(
            decoded.get("content-type").unwrap().as_bytes(),
            b"text/plain"
        );
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(b"not-json").is_none());
        assert!(decode(b"{}").is_none());
    }
}
