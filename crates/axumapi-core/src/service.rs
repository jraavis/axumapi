//! Opaque tower service produced from an [`App`](crate::App).

use crate::body::Body;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::Service;

/// A cloneable `tower::Service` handling `http::Request<Body>`; used for
/// in-process testing without opening a socket.
#[derive(Debug, Clone)]
pub struct RouterService {
    inner: axum::Router,
}

impl RouterService {
    pub(crate) fn new(inner: axum::Router) -> Self {
        Self { inner }
    }
}

impl Service<http::Request<Body>> for RouterService {
    type Response = http::Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Service::<http::Request<axum::body::Body>>::poll_ready(&mut self.inner, cx)
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        let fut = self.inner.call(req.map(Body::into_inner));
        Box::pin(async move {
            match fut.await {
                Ok(response) => Ok(response.map(Body::from_inner)),
                Err(never) => match never {},
            }
        })
    }
}
