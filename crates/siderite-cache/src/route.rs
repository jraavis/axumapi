//! [`RouteCache`]: HTTP GET/HEAD response cache as an `App` layer.

use crate::Cache;
use http::header::{
    ACCEPT, ACCEPT_ENCODING, AUTHORIZATION, CACHE_CONTROL, COOKIE, HOST, PROXY_AUTHORIZATION,
    SET_COOKIE, VARY,
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use serde::{Deserialize, Serialize};
use siderite_core::middleware::{BoxService, Next, from_fn};
use siderite_core::{ApiError, Body, IntoResponse, Request, Response};
use std::sync::Arc;
use std::time::Duration;
use tower::Layer;

const X_CACHE: HeaderName = HeaderName::from_static("x-cache");
const HIT: HeaderValue = HeaderValue::from_static("hit");
const MISS: HeaderValue = HeaderValue::from_static("miss");

/// Largest body stored by default (1 MiB).
pub const DEFAULT_MAX_BODY_BYTES: u64 = 1024 * 1024;

/// Request headers that are part of the cache key; a response may `Vary` on
/// these and still be stored.
const KEYED_HEADERS: [HeaderName; 2] = [ACCEPT, ACCEPT_ENCODING];

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
/// Caching is **opt-in**. There are two ways to opt a route in:
///
/// * **Per response.** Put the layer on the whole app; it stores only
///   responses marked `Cache-Control: public` (for example with
///   [`Cached::public`](siderite_core::Cached)). Everything else passes
///   through.
/// * **Per route.** Put the layer on one route with `MethodRouter::layer` and
///   set [`default_ttl`](RouteCache::default_ttl); that route's responses are
///   stored without a `public` marker.
///
/// ```
/// use siderite_cache::{MemoryCache, RouteCache};
/// use siderite_core::{App, Cached, get};
/// use std::time::Duration;
///
/// let cache = MemoryCache::new(1024);
/// let _app = App::new()
///     // Opted in by its response.
///     .route("/news", get(|| async {
///         Cached::public(Duration::from_secs(60), "news")
///     }))
///     // Opted in by placement.
///     .route("/stats", get(|| async { "stats" }).layer(
///         RouteCache::new(cache.clone()).default_ttl(Duration::from_secs(30)),
///     ))
///     // Never stored: no `public` marker.
///     .route("/me", get(|| async { "private" }))
///     .layer(RouteCache::new(cache));
/// ```
///
/// Do not set `default_ttl` on an app-wide layer unless every GET route in
/// the app is safe to share between users.
///
/// # Behaviour
/// * Only [`GET`](Method::GET) and [`HEAD`](Method::HEAD) are considered.
/// * The cache key is the method, scheme, `Host`, path and query (as
///   received), plus the request's `Accept` and `Accept-Encoding` values, so
///   virtual hosts sharing one cache never see each other's responses.
/// * Requests carrying credentials bypass the cache: `Authorization`,
///   `Proxy-Authorization`, `Cookie`, `X-API-Key`, any header whose name
///   contains `auth`, `token`, `session`, `jwt`, `secret`, `api-key`,
///   `apikey` or `access-key`, and any header added with
///   [`RouteCache::bypass_header`]. Such requests are neither
///   answered from nor stored in the cache.
/// * Responses are stored only when the status is 200, there is no
///   `Set-Cookie`, `Cache-Control` does not contain `no-store`, `no-cache` or
///   `private`, `Vary` names no header outside the key, and the body length
///   is known and at most [`RouteCache::max_body_bytes`]. Streaming bodies
///   (server-sent events, file streams) pass through untouched.
/// * On top of that, a response needs `Cache-Control: public` unless
///   [`default_ttl`](RouteCache::default_ttl) is set.
/// * The entry lives for `s-maxage`, else `max-age`, else `default_ttl`. A
///   `public` response without any of these, or with a lifetime of zero, is
///   not stored.
/// * Every GET/HEAD response is tagged with `x-cache: hit` or `x-cache: miss`.
///
/// Cache backend failures fail open: the request is served and treated as a
/// miss. Values, passwords and tokens are never logged.
#[derive(Clone, Debug)]
pub struct RouteCache<C> {
    cache: Arc<C>,
    default_ttl: Option<Duration>,
    max_body: u64,
    bypass: Arc<Vec<HeaderName>>,
}

impl<C: Cache> RouteCache<C> {
    /// Cache GET/HEAD 200 responses marked `Cache-Control: public` in
    /// `cache`.
    #[must_use]
    pub fn new(cache: C) -> Self {
        Self {
            cache: Arc::new(cache),
            default_ttl: None,
            max_body: DEFAULT_MAX_BODY_BYTES,
            bypass: Arc::new(vec![
                AUTHORIZATION,
                PROXY_AUTHORIZATION,
                COOKIE,
                HeaderName::from_static("x-api-key"),
            ]),
        }
    }

    /// Also store responses without `Cache-Control: public`, for `ttl` when
    /// they carry no `max-age`. Meant for a layer on a single route (see the
    /// [type docs](RouteCache)). A `ttl` of zero stores nothing.
    #[must_use]
    pub fn default_ttl(mut self, ttl: Duration) -> Self {
        self.default_ttl = Some(ttl);
        self
    }

    /// Bypass the cache for requests carrying `name` (a credential header).
    #[must_use]
    pub fn bypass_header(mut self, name: HeaderName) -> Self {
        Arc::make_mut(&mut self.bypass).push(name);
        self
    }

    /// Store only bodies of at most `bytes` (default
    /// [`DEFAULT_MAX_BODY_BYTES`]).
    #[must_use]
    pub fn max_body_bytes(mut self, bytes: u64) -> Self {
        self.max_body = bytes;
        self
    }
}

/// Per-request view of the layer configuration.
struct Policy {
    default_ttl: Option<Duration>,
    max_body: u64,
    bypass: Arc<Vec<HeaderName>>,
}

impl<C: Cache> Layer<BoxService> for RouteCache<C> {
    type Service = BoxService;

    fn layer(&self, inner: BoxService) -> Self::Service {
        let cache = Arc::clone(&self.cache);
        let (default_ttl, max_body, bypass) =
            (self.default_ttl, self.max_body, Arc::clone(&self.bypass));
        from_fn(move |req: Request, next: Next| {
            let cache = Arc::clone(&cache);
            let policy = Policy {
                default_ttl,
                max_body,
                bypass: Arc::clone(&bypass),
            };
            async move { dispatch(cache, policy, req, next).await }
        })
        .layer(inner)
    }
}

fn is_cacheable_method(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD
}

fn request_bypasses_cache(headers: &HeaderMap, bypass: &[HeaderName]) -> bool {
    bypass.iter().any(|name| headers.contains_key(name))
        || headers.keys().any(looks_like_credential)
}

/// Header names that commonly carry credentials (`X-Auth-Token`,
/// `X-Session-Id`, `X-Jwt`, `X-Access-Key`, ...). Errs toward bypassing: a
/// false positive only skips the cache, a false negative serves a private
/// response to anyone.
fn looks_like_credential(name: &HeaderName) -> bool {
    const NEEDLES: [&str; 8] = [
        "auth",
        "token",
        "session",
        "jwt",
        "secret",
        "api-key",
        "apikey",
        "access-key",
    ];
    let name = name.as_str();
    NEEDLES.iter().any(|needle| name.contains(needle))
}

/// True when `Vary` names `*` or a header that is not part of the key.
fn varies_outside_key(headers: &HeaderMap) -> bool {
    headers
        .get_all(VARY)
        .iter()
        .flat_map(|value| value.to_str().unwrap_or("*").split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .any(|name| {
            !KEYED_HEADERS
                .iter()
                .any(|keyed| keyed.as_str().eq_ignore_ascii_case(name))
        })
}

/// `Cache-Control` directives as lowercase `(name, value)` pairs.
fn cache_control(headers: &HeaderMap) -> Vec<(String, Option<String>)> {
    headers
        .get_all(CACHE_CONTROL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|text| text.split(','))
        .filter_map(|directive| {
            let (name, value) = match directive.split_once('=') {
                Some((name, value)) => (name, Some(value.trim().trim_matches('"').to_owned())),
                None => (directive, None),
            };
            let name = name.trim().trim_matches('"').to_ascii_lowercase();
            (!name.is_empty()).then_some((name, value))
        })
        .collect()
}

/// True when any `Cache-Control` directive is `no-store`, `no-cache` or
/// `private`. This layer never revalidates, so `no-cache` means "do not
/// store" here.
pub(crate) fn cache_control_forbids_store(headers: &HeaderMap) -> bool {
    cache_control(headers)
        .iter()
        .any(|(name, _)| matches!(name.as_str(), "no-store" | "no-cache" | "private"))
}

/// How long to store `response`, or `None` when it must not be stored.
fn storable_ttl(response: &Response, policy: &Policy) -> Option<Duration> {
    let headers = response.headers();
    let eligible = response.status() == StatusCode::OK
        && !headers.contains_key(SET_COOKIE)
        && !cache_control_forbids_store(headers)
        && !varies_outside_key(headers)
        && response
            .body()
            .exact_len()
            .is_some_and(|len| len <= policy.max_body);
    if !eligible {
        return None;
    }
    let directives = cache_control(headers);
    let public = directives.iter().any(|(name, _)| name == "public");
    if !public && policy.default_ttl.is_none() {
        return None;
    }
    let seconds = |wanted: &str| {
        directives
            .iter()
            .find(|(name, _)| name == wanted)
            .and_then(|(_, value)| value.as_deref()?.parse::<u64>().ok())
    };
    let ttl = seconds("s-maxage")
        .or_else(|| seconds("max-age"))
        .map(Duration::from_secs)
        .or(policy.default_ttl)?;
    (ttl > Duration::ZERO).then_some(ttl)
}

/// Build the cache key from method, scheme, host, path and query, and the
/// keyed headers. Every part is length-prefixed so different requests can
/// never produce the same key.
fn cache_key(req: &Request) -> String {
    let mut key = String::new();
    push_part(&mut key, req.method().as_str().as_bytes());
    push_part(&mut key, req.uri().scheme_str().unwrap_or("").as_bytes());
    let host = req
        .headers()
        .get(HOST)
        .map(HeaderValue::as_bytes)
        .or_else(|| req.uri().authority().map(|a| a.as_str().as_bytes()))
        .unwrap_or_default();
    push_part(&mut key, &host.to_ascii_lowercase());
    let target = req
        .uri()
        .path_and_query()
        .map_or("/", http::uri::PathAndQuery::as_str);
    push_part(&mut key, target.as_bytes());
    for name in &KEYED_HEADERS {
        let values = req.headers().get_all(name);
        key.push_str(&format!("#{}", values.iter().count()));
        for value in values {
            push_part(&mut key, value.as_bytes());
        }
    }
    key
}

/// Append `bytes` as `s{len}:{text}` (UTF-8) or `x{len}:{hex}` (anything else).
fn push_part(key: &mut String, bytes: &[u8]) {
    use std::fmt::Write as _;
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            let _ = write!(key, "s{}:{text}", text.len());
        }
        Err(_) => {
            let _ = write!(key, "x{}:", bytes.len());
            for byte in bytes {
                let _ = write!(key, "{byte:02x}");
            }
        }
    }
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

/// Rebuild a cached response. A `HEAD` keeps its stored `Content-Length`
/// (the size of the `GET` representation); a `GET` gets its body length.
fn restore(method: &Method, headers: HeaderMap, body: Vec<u8>) -> Response {
    let len = HeaderValue::from(body.len());
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    *response.headers_mut() = headers;
    if method != Method::HEAD {
        response.headers_mut().insert(header::CONTENT_LENGTH, len);
    }
    with_x_cache(response, HIT)
}

async fn dispatch<C: Cache>(cache: Arc<C>, policy: Policy, req: Request, next: Next) -> Response {
    if !is_cacheable_method(req.method()) {
        return next.run(req).await;
    }
    if request_bypasses_cache(req.headers(), &policy.bypass) {
        return with_x_cache(next.run(req).await, MISS);
    }

    let key = cache_key(&req);
    let method = req.method().clone();
    if let Ok(Some(bytes)) = cache.get(&key).await
        && let Some((headers, body)) = decode(&bytes)
    {
        return restore(&method, headers, body);
    }

    let response = next.run(req).await;
    let Some(ttl) = storable_ttl(&response, &policy) else {
        return with_x_cache(response, MISS);
    };

    let (mut parts, body) = response.into_parts();
    let limit = usize::try_from(policy.max_body).unwrap_or(usize::MAX);
    let bytes = match body.into_bytes_limited(limit).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return ApiError::internal("failed to buffer a cacheable response body")
                .into_response();
        }
    };

    if let Ok(payload) = encode(&parts.headers, bytes.clone()) {
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
    fn cache_control_detects_no_store_no_cache_and_private() {
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
        assert!(cache_control_forbids_store(&headers(&[(
            "cache-control",
            "No-Cache"
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
