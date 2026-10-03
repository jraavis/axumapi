//! Hard-bounded token buckets with incremental conservative expiry.

use super::TrustedProxies;
use super::adapt::{BoxService, Next, from_fn, impl_layer};
use crate::error::ApiError;
use crate::extract::Request;
use crate::response::IntoResponse;
use http::{HeaderValue, StatusCode, header};
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tower::Layer;

const DEFAULT_MAX_CLIENTS: usize = 10_000;
const EXPIRY_SCAN_BUDGET: usize = 16;
type Key = Option<IpAddr>;
type Verdict = Result<(), Duration>;

struct Bucket {
    tokens: f64,
    last: Instant,
}

#[derive(Default)]
struct Buckets {
    entries: HashMap<Key, Bucket>,
    expiry: VecDeque<Key>,
}

/// Token-bucket rate limiter keyed by validated client IP.
///
/// State is process-local with a hard client bound. At saturation new
/// identities are denied; existing depleted buckets are never evicted.
/// Only fully replenished buckets expire, with bounded scanning per request.
/// Proxy headers are ignored unless explicitly trusted. Unknown peers share
/// one bucket; counters reset on process restart, so replicas multiply limits.
#[derive(Clone)]
pub struct RateLimit {
    capacity: f64,
    refill_per_sec: f64,
    max_clients: usize,
    proxies: TrustedProxies,
    buckets: Arc<Mutex<Buckets>>,
}

/// Invalid rate-limiter configuration.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("rate limits require positive capacity and finite positive refill")]
pub struct RateLimitError;

impl std::fmt::Debug for RateLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimit")
            .field("capacity", &self.capacity)
            .field("refill_per_sec", &self.refill_per_sec)
            .field("max_clients", &self.max_clients)
            .field("proxies", &self.proxies)
            .finish_non_exhaustive()
    }
}

impl RateLimit {
    /// Configure burst and refill; invalid values fail App validation.
    ///
    /// Args:
    ///     burst: Positive request-token capacity.
    ///     per_second: Finite, positive replenishment rate.
    ///
    /// Returns:
    ///     Limiter; use [`Self::try_new`] to check configuration immediately.
    pub fn new(burst: u32, per_second: f64) -> Self {
        Self {
            capacity: f64::from(burst),
            refill_per_sec: per_second,
            max_clients: DEFAULT_MAX_CLIENTS,
            proxies: TrustedProxies::default(),
            buckets: Arc::default(),
        }
    }

    /// Validate configuration before constructing a limiter.
    ///
    /// Args:
    ///     burst: Positive request-token capacity.
    ///     rate: Finite, positive replenishment rate.
    ///
    /// Returns:
    ///     A valid limiter or a configuration error.
    pub fn try_new(burst: u32, rate: f64) -> Result<Self, RateLimitError> {
        let limiter = Self::new(burst, rate);
        if limiter.valid() {
            Ok(limiter)
        } else {
            Err(RateLimitError)
        }
    }

    /// Configure a hard client-state bound with fresh counters.
    ///
    /// Args:
    ///     max: Positive tracked-client capacity; zero is invalid.
    ///
    /// Returns:
    ///     A newly configured limiter; existing clones keep their counters.
    #[must_use]
    pub fn max_clients(mut self, max: usize) -> Self {
        self.max_clients = max;
        self.buckets = Arc::default();
        self
    }

    /// Trust forwarded client IP only from configured immediate peers.
    ///
    /// Args:
    ///     proxies: Policy requiring proxies to replace client headers.
    ///
    /// Returns:
    ///     Limiter using validated, normalized client IP addresses.
    #[must_use]
    pub fn trusted_proxies(mut self, proxies: TrustedProxies) -> Self {
        self.proxies = proxies;
        self
    }

    /// Explicit compatibility trust for the first X-Forwarded-For hop.
    ///
    /// Args:
    ///     trust: Blanket ingress trust; false ignores forwarded client IP.
    ///
    /// Returns:
    ///     Limiter with the selected policy; malformed IPs are rejected.
    #[must_use]
    pub fn trust_forwarded_for(self, trust: bool) -> Self {
        self.trusted_proxies(if trust {
            TrustedProxies::dangerously_trust_all()
        } else {
            TrustedProxies::default()
        })
    }

    pub(super) fn valid(&self) -> bool {
        self.capacity > 0.0
            && self.refill_per_sec.is_finite()
            && self.refill_per_sec > 0.0
            && self.max_clients > 0
    }

    fn take(&self, key: Key, now: Instant) -> Verdict {
        let guard = self.buckets.lock();
        let mut state = guard.unwrap_or_else(PoisonError::into_inner);
        if !state.entries.contains_key(&key) {
            if state.entries.len() >= self.max_clients {
                self.expire(&mut state, now);
            }
            if state.entries.len() >= self.max_clients {
                return Err(Duration::from_secs(1));
            }
            state.expiry.push_back(key);
        }
        let bucket = state.entries.entry(key).or_insert(Bucket {
            tokens: self.capacity,
            last: now,
        });
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        let refilled = bucket.tokens + elapsed * self.refill_per_sec;
        bucket.tokens = refilled.min(self.capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let wait = (1.0 - bucket.tokens) / self.refill_per_sec;
            let maximum = Duration::from_secs(3600);
            Err(Duration::try_from_secs_f64(wait).unwrap_or(maximum))
        }
    }

    fn expire(&self, state: &mut Buckets, now: Instant) {
        for _ in 0..state.expiry.len().min(EXPIRY_SCAN_BUDGET) {
            let Some(key) = state.expiry.pop_front() else {
                break;
            };
            let expired = state.entries.get(&key).is_some_and(|bucket| {
                let elapsed = now.duration_since(bucket.last).as_secs_f64();
                bucket.tokens + elapsed * self.refill_per_sec >= self.capacity
            });
            if expired {
                state.entries.remove(&key);
            } else {
                state.expiry.push_back(key);
            }
        }
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let this = self.clone();
        from_fn(move |req: Request, next: Next| {
            let valid = this.valid();
            let verdict = if valid {
                this.proxies
                    .resolve(&req)
                    .map(|context| {
                        let now = Instant::now();
                        this.take(context.client_ip, now)
                    })
                    .map_err(|_| ())
            } else {
                Err(())
            };
            async move {
                if !valid {
                    let message = "invalid rate limiter configuration";
                    return ApiError::internal(message).into_response();
                }
                match verdict {
                    Err(()) => {
                        let message = "Invalid forwarded request context.";
                        ApiError::bad_request(message).into_response()
                    }
                    Ok(Ok(())) => next.run(req).await,
                    Ok(Err(wait)) => {
                        let status = StatusCode::TOO_MANY_REQUESTS;
                        let message = "Rate limit exceeded.";
                        let error = ApiError::new(status, message);
                        let mut response = error.into_response();
                        let round_up = u64::from(wait.subsec_nanos() > 0);
                        let secs = wait.as_secs().saturating_add(round_up);
                        let seconds = secs.max(1).to_string();
                        let value = HeaderValue::from_str(&seconds);
                        if let Ok(value) = value {
                            let headers = response.headers_mut();
                            headers.insert(header::RETRY_AFTER, value);
                        }
                        response
                    }
                }
            }
        })
        .layer(inner)
    }
}

impl_layer!(RateLimit);

#[cfg(test)]
#[path = "rate_limit_tests.rs"]
mod tests;
