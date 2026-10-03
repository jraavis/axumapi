//! Bounded handler admission and optional response-body ownership.

use super::adapt::{BoxService, Next, from_fn, impl_layer};
use crate::{ApiError, IntoResponse, Request, Response};
use http::{StatusCode, header};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower::Layer;

mod body;

/// Invalid concurrency or queue-deadline configuration.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("invalid concurrency capacity or queue deadline")]
pub struct ConcurrencyLimitError;

/// Admission counters shared by all clones of a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConcurrencyStats {
    /// Requests or bodies currently holding execution permits.
    pub active: usize,
    /// Requests waiting for execution permits.
    pub waiting: usize,
    /// Requests admitted to execution since construction.
    pub admitted: u64,
    /// Requests rejected because the queue is full or closed.
    pub rejected: u64,
    /// Requests whose queue deadline expired.
    pub timed_out: u64,
    /// Cumulative queue time of completed admission attempts, nanoseconds.
    pub wait_nanos: u64,
}

#[derive(Debug)]
struct Shared {
    active: Arc<Semaphore>,
    waiting: Arc<Semaphore>,
    max_active: usize,
    max_waiting: usize,
    admitted: AtomicU64,
    rejected: AtomicU64,
    timed_out: AtomicU64,
    wait_nanos: AtomicU64,
}

/// Bounds execution and waiting, returning 503 with Retry-After on overload.
///
/// Defaults to one queue slot per execution slot and a five-second queue
/// deadline. Permits normally end at response production. `hold_body(true)`
/// retains them until body completion, error or drop. Upgraded connections
/// require their own lifecycle budget. Clones share admission and counters.
#[derive(Debug, Clone)]
pub struct ConcurrencyLimit {
    shared: Arc<Shared>,
    wait: Duration,
    body: bool,
    valid: bool,
}

impl ConcurrencyLimit {
    /// Configure execution capacity, treating zero as one for compatibility.
    ///
    /// Args:
    ///     max: Maximum active requests, at most Semaphore::MAX_PERMITS.
    ///
    /// Returns:
    ///     Bounded limit; invalid oversized configuration fails requests.
    #[must_use]
    pub fn new(max: usize) -> Self {
        let capacity = max.clamp(1, Semaphore::MAX_PERMITS);
        Self {
            shared: Arc::new(Shared::new(capacity, capacity)),
            wait: Duration::from_secs(5),
            body: false,
            valid: max <= Semaphore::MAX_PERMITS,
        }
    }

    /// Validate a strictly positive execution capacity immediately.
    ///
    /// Args:
    ///     max: Maximum active requests.
    ///
    /// Returns:
    ///     Configured limit or invalid-capacity error.
    pub fn try_new(max: usize) -> Result<Self, ConcurrencyLimitError> {
        if max == 0 || max > Semaphore::MAX_PERMITS {
            return Err(ConcurrencyLimitError);
        }
        Ok(Self::new(max))
    }

    /// Set bounded queue capacity and deadline, including a zero-slot queue.
    ///
    /// Args:
    ///     max: Maximum waiting requests, at most Semaphore::MAX_PERMITS.
    ///     timeout: Maximum admission wait; zero means immediate admission.
    ///
    /// Returns:
    ///     Fresh admission state; existing clones keep their previous state.
    #[must_use]
    pub fn queue(mut self, max: usize, timeout: Duration) -> Self {
        let capacity = max.min(Semaphore::MAX_PERMITS);
        let deadline = Instant::now().checked_add(timeout).is_some();
        self.valid &= max <= Semaphore::MAX_PERMITS && deadline;
        self.shared = Arc::new(Shared::new(self.shared.max_active, capacity));
        self.wait = timeout;
        self
    }

    /// Include response-body consumption in the execution limit.
    ///
    /// Args:
    ///     enabled: Retain permits until body completion, error or drop.
    ///
    /// Returns:
    ///     Layer sharing its original admission state.
    #[must_use]
    pub fn hold_body(mut self, enabled: bool) -> Self {
        self.body = enabled;
        self
    }

    /// Close admission and wake waiters; already active work can complete.
    ///
    /// Returns:
    ///     No value; all clones observe the closed state.
    pub fn close(&self) {
        self.shared.active.close();
        self.shared.waiting.close();
    }

    /// Read an approximate concurrent snapshot without locking.
    ///
    /// Returns:
    ///     Current capacities and cumulative admission counters.
    pub fn stats(&self) -> ConcurrencyStats {
        let state = &self.shared;
        ConcurrencyStats {
            active: state.max_active - state.active.available_permits(),
            waiting: state.max_waiting - state.waiting.available_permits(),
            admitted: state.admitted.load(Ordering::Relaxed),
            rejected: state.rejected.load(Ordering::Relaxed),
            timed_out: state.timed_out.load(Ordering::Relaxed),
            wait_nanos: state.wait_nanos.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), ConcurrencyLimitError> {
        if self.valid {
            Ok(())
        } else {
            Err(ConcurrencyLimitError)
        }
    }

    async fn acquire(&self) -> Option<OwnedSemaphorePermit> {
        let state = &self.shared;
        if let Ok(permit) = state.active.clone().try_acquire_owned() {
            state.admitted.fetch_add(1, Ordering::Relaxed);
            return Some(permit);
        }
        let waiting = state.waiting.clone().try_acquire_owned();
        let Ok(_waiting) = waiting else {
            state.rejected.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let started = Instant::now();
        let future = state.active.clone().acquire_owned();
        let result = tokio::time::timeout(self.wait, future).await;
        let nanos = started.elapsed().as_nanos().min(u64::MAX as u128);
        state.wait_nanos.fetch_add(nanos as u64, Ordering::Relaxed);
        match result {
            Ok(Ok(permit)) => {
                state.admitted.fetch_add(1, Ordering::Relaxed);
                Some(permit)
            }
            Ok(Err(_)) => {
                state.rejected.fetch_add(1, Ordering::Relaxed);
                None
            }
            Err(_) => {
                state.timed_out.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    fn wrap(&self, inner: BoxService) -> BoxService {
        let layer = self.clone();
        from_fn(move |req: Request, next: Next| {
            let layer = layer.clone();
            async move {
                if layer.validate().is_err() {
                    let error = ApiError::internal("Invalid admission.");
                    return error.into_response();
                }
                let Some(permit) = layer.acquire().await else {
                    return overload();
                };
                let response = next.run(req).await;
                if layer.body {
                    body::retain(response, permit)
                } else {
                    drop(permit);
                    response
                }
            }
        })
        .layer(inner)
    }
}

impl Shared {
    fn new(active: usize, waiting: usize) -> Self {
        Self {
            active: Arc::new(Semaphore::new(active)),
            waiting: Arc::new(Semaphore::new(waiting)),
            max_active: active,
            max_waiting: waiting,
            admitted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            timed_out: AtomicU64::new(0),
            wait_nanos: AtomicU64::new(0),
        }
    }
}

fn overload() -> Response {
    let mut response = ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "Request admission unavailable; retry later.",
    )
    .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, http::HeaderValue::from_static("1"));
    response
}

impl_layer!(ConcurrencyLimit);
