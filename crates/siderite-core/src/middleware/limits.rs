//! Timeouts, concurrency, body-size and rate limits.

use super::adapt::{BoxService, Next, from_fn, impl_layer, wrap_engine};
use crate::error::ApiError;
use crate::extract::Request;
use crate::response::IntoResponse;
use http::{StatusCode, header};
use std::time::Duration;
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

impl_layer!(Timeout, BodyLimit);
