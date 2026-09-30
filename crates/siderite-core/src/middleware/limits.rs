//! Timeouts, concurrency, body-size and rate limits.

use super::adapt::{BoxService, Next, from_fn, impl_layer, wrap_engine};
use crate::error::ApiError;
use crate::extract::Request;
use crate::response::IntoResponse;
use http::{HeaderValue, StatusCode, header};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tower::Layer;
use tower_http::limit::RequestBodyLimitLayer;

/// Fails requests that take longer than a deadline (default status 504).
///
/// The deadline covers producing the response, not streaming its body. The
/// status is 504 Gateway Timeout by default (the server, not the client, was
/// too slow); [`Timeout::status`] changes it, e.g. to 408.
#[derive(Debug, Clone)]
pub struct Timeout {
    limit: Duration,
    status: StatusCode,
}

impl Timeout {
    /// Time out after `limit`.
    pub fn new(limit: Duration) -> Self {
        Self {
            limit,
            status: StatusCode::GATEWAY_TIMEOUT,
        }
    }

    /// Status code of the problem returned on timeout.
    #[must_use]
    pub fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let (limit, status) = (self.limit, self.status);
        from_fn(move |req: Request, next: Next| async move {
            match tokio::time::timeout(limit, next.run(req)).await {
                Ok(response) => response,
                Err(_) => ApiError::new(status, "The request timed out.").into_response(),
            }
        })
        .layer(inner)
    }
}

/// Bounds the number of requests handled concurrently; excess requests wait
/// for a slot (the permit is held until the response is produced).
#[derive(Debug, Clone)]
pub struct ConcurrencyLimit {
    permits: Arc<Semaphore>,
}

impl ConcurrencyLimit {
    /// Allow at most `max` (minimum 1) concurrent requests.
    pub fn new(max: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max.max(1))),
        }
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let permits = Arc::clone(&self.permits);
        from_fn(move |req: Request, next: Next| {
            let permits = Arc::clone(&permits);
            async move {
                let Ok(_permit) = permits.acquire_owned().await else {
                    return ApiError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Server is shutting down.",
                    )
                    .into_response();
                };
                next.run(req).await
            }
        })
        .layer(inner)
    }
}

/// Rejects request bodies larger than a limit with a 413 problem.
///
/// Checked eagerly via `Content-Length`; bodies without it (chunked) are
/// capped while being read, which surfaces as a body error in the extractor.
#[derive(Debug, Clone)]
pub struct BodyLimit {
    max_bytes: usize,
}

impl BodyLimit {
    /// Allow bodies up to `max_bytes`.
    pub fn new(max_bytes: usize) -> Self {
        Self { max_bytes }
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let max = self.max_bytes;
        let capped = wrap_engine(&RequestBodyLimitLayer::new(max), inner);
        from_fn(move |req: Request, next: Next| async move {
            let declared = req
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<usize>().ok());
            if declared.is_some_and(|len| len > max) {
                return ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!("Request body exceeds the limit of {max} bytes."),
                )
                .into_response();
            }
            next.run(req).await
        })
        .layer(capped)
    }
}

const MAX_TRACKED_CLIENTS: usize = 10_000;

struct Bucket {
    tokens: f64,
    last: Instant,
}

/// Token-bucket rate limiter keyed by client IP (429 problem with
/// `Retry-After`).
///
/// **Process-local**: counters live in this process's memory, so with N
/// replicas the effective limit is N times higher and restarts reset it. Use
/// an edge/gateway limiter for distributed limits.
///
/// The client is the socket peer address when served by `App::run`; behind a
/// proxy enable [`trust_forwarded_for`](Self::trust_forwarded_for) to use the
/// first `X-Forwarded-For` entry (only do so if a trusted proxy sets it, as
/// clients can forge it). When no address is known (e.g. in-process tests)
/// all requests share one bucket.
#[derive(Clone)]
pub struct RateLimit {
    capacity: f64,
    refill_per_sec: f64,
    trust_forwarded_for: bool,
    buckets: Arc<Mutex<HashMap<String, Bucket>>>,
}

impl std::fmt::Debug for RateLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimit")
            .field("capacity", &self.capacity)
            .field("refill_per_sec", &self.refill_per_sec)
            .field("trust_forwarded_for", &self.trust_forwarded_for)
            .finish_non_exhaustive()
    }
}

impl RateLimit {
    /// Bursts of up to `burst` requests, refilled at `per_second` tokens/s.
    pub fn new(burst: u32, per_second: f64) -> Self {
        Self {
            capacity: f64::from(burst.max(1)),
            refill_per_sec: per_second.max(f64::MIN_POSITIVE),
            trust_forwarded_for: false,
            buckets: Arc::default(),
        }
    }

    /// Key clients by the first `X-Forwarded-For` entry.
    #[must_use]
    pub fn trust_forwarded_for(mut self, trust: bool) -> Self {
        self.trust_forwarded_for = trust;
        self
    }

    fn client_key(&self, req: &Request) -> String {
        let forwarded = self
            .trust_forwarded_for
            .then(|| req.headers().get("x-forwarded-for"))
            .flatten()
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        forwarded
            .or_else(|| {
                req.extensions()
                    .get::<axum::extract::ConnectInfo<SocketAddr>>()
                    .map(|info| info.0.ip().to_string())
            })
            .unwrap_or_else(|| "unknown".to_owned())
    }

    /// Take a token for `key`; on refusal, the time until one is available.
    fn take(&self, key: &str) -> Result<(), Duration> {
        let now = Instant::now();
        let mut map = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if map.len() >= MAX_TRACKED_CLIENTS && !map.contains_key(key) {
            let (cap, rate) = (self.capacity, self.refill_per_sec);
            map.retain(|_, b| b.tokens + now.duration_since(b.last).as_secs_f64() * rate < cap);
        }
        let bucket = map.entry(key.to_owned()).or_insert(Bucket {
            tokens: self.capacity,
            last: now,
        });
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let wait = (1.0 - bucket.tokens) / self.refill_per_sec;
            Err(Duration::try_from_secs_f64(wait).unwrap_or(Duration::from_secs(3600)))
        }
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let this = self.clone();
        from_fn(move |req: Request, next: Next| {
            let verdict = this.take(&this.client_key(&req));
            async move {
                match verdict {
                    Ok(()) => next.run(req).await,
                    Err(wait) => {
                        let mut response =
                            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "Rate limit exceeded.")
                                .into_response();
                        let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
                        if let Ok(value) = HeaderValue::from_str(&secs.max(1).to_string()) {
                            response.headers_mut().insert(header::RETRY_AFTER, value);
                        }
                        response
                    }
                }
            }
        })
        .layer(inner)
    }
}

impl_layer!(Timeout, ConcurrencyLimit, BodyLimit, RateLimit);
