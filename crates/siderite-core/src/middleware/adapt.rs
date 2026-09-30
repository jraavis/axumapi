//! Plumbing shared by all middleware: the boxed service type, `from_fn`,
//! and adapters between siderite bodies and the engine's bodies.

use crate::body::Body;
use crate::extract::Request;
use crate::response::Response;
use crate::service::RouterService;
use axum::BoxError;
use axum::body::HttpBody;
use bytes::Bytes;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::util::BoxCloneSyncService;
use tower::{Layer, Service, ServiceExt};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// The service type middleware layers wrap: the rest of the application.
pub type BoxService = BoxCloneSyncService<Request, Response, Infallible>;

/// The rest of the middleware chain, handed to [`from_fn`] closures.
#[derive(Clone)]
pub struct Next {
    inner: BoxService,
}

impl Next {
    /// Forward `req` to the inner service and await its response.
    pub async fn run(self, req: Request) -> Response {
        match self.inner.oneshot(req).await {
            Ok(response) => response,
            Err(never) => match never {},
        }
    }
}

/// Build a middleware layer from an async function `(Request, Next) -> Response`.
pub fn from_fn<F, Fut>(f: F) -> FromFnLayer<F>
where
    F: Fn(Request, Next) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    FromFnLayer { f }
}

/// Layer returned by [`from_fn`].
#[derive(Clone)]
pub struct FromFnLayer<F> {
    f: F,
}

impl<F, Fut> Layer<BoxService> for FromFnLayer<F>
where
    F: Fn(Request, Next) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    type Service = BoxService;

    fn layer(&self, inner: BoxService) -> BoxService {
        BoxService::new(FromFnService {
            inner,
            f: self.f.clone(),
        })
    }
}

#[derive(Clone)]
struct FromFnService<F> {
    inner: BoxService,
    f: F,
}

impl<F, Fut> Service<Request> for FromFnService<F>
where
    F: Fn(Request, Next) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response> + Send + 'static,
{
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<Result<Response, Infallible>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let fut = (self.f)(
            req,
            Next {
                inner: self.inner.clone(),
            },
        );
        Box::pin(async move { Ok(fut.await) })
    }
}

/// Implement `tower::Layer<BoxService>` for types with an inherent
/// `fn wrap(&self, BoxService) -> BoxService`.
macro_rules! impl_layer {
    ($($ty:ty),+ $(,)?) => {$(
        impl tower::Layer<$crate::middleware::BoxService> for $ty {
            type Service = $crate::middleware::BoxService;

            fn layer(&self, inner: $crate::middleware::BoxService) -> Self::Service {
                self.wrap(inner)
            }
        }
    )+};
}
pub(crate) use impl_layer;

/// Wrap `router` in a user layer, producing a router that delegates to it.
pub(crate) fn apply_layer<L>(layer: &L, router: axum::Router) -> axum::Router
where
    L: Layer<BoxService>,
    L::Service:
        Service<Request, Response = Response, Error = Infallible> + Clone + Send + Sync + 'static,
    <L::Service as Service<Request>>::Future: Send + 'static,
{
    let service = layer.layer(BoxService::new(RouterService::new(router)));
    axum::Router::new().fallback_service(ToEngine { inner: service })
}

/// Engine-facing adapter: engine bodies in, engine bodies out.
#[derive(Clone)]
struct ToEngine<S> {
    inner: S,
}

impl<S> Service<axum::extract::Request> for ToEngine<S>
where
    S: Service<Request, Response = Response, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = axum::response::Response;
    type Error = Infallible;
    type Future = BoxFuture<Result<Self::Response, Infallible>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: axum::extract::Request) -> Self::Future {
        let fut = self.inner.call(req.map(Body::from_inner));
        Box::pin(async move { fut.await.map(|r| r.map(Body::into_inner)) })
    }
}

/// Presents a [`BoxService`] to a `tower-http` layer.
#[derive(Clone)]
pub(crate) struct EngineIn(BoxService);

impl<B> Service<http::Request<B>> for EngineIn
where
    B: HttpBody<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    type Response = http::Response<axum::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Result<Self::Response, Infallible>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<B>) -> Self::Future {
        let fut = self
            .0
            .call(req.map(|b| Body::from_inner(axum::body::Body::new(b))));
        Box::pin(async move { fut.await.map(|r| r.map(Body::into_inner)) })
    }
}

#[derive(Clone)]
struct EngineOut<S>(S);

impl<S, B> Service<Request> for EngineOut<S>
where
    S: Service<http::Request<axum::body::Body>, Response = http::Response<B>, Error = Infallible>,
    S::Future: Send + 'static,
    B: HttpBody<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    type Response = Response;
    type Error = Infallible;
    type Future = BoxFuture<Result<Response, Infallible>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.0.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let fut = self.0.call(req.map(Body::into_inner));
        Box::pin(async move {
            fut.await
                .map(|r| r.map(|b| Body::from_inner(axum::body::Body::new(b))))
        })
    }
}

/// Apply a `tower-http` style layer (generic over body types) to `inner`.
pub(crate) fn wrap_engine<L, B>(layer: &L, inner: BoxService) -> BoxService
where
    L: Layer<EngineIn>,
    L::Service: Service<http::Request<axum::body::Body>, Response = http::Response<B>, Error = Infallible>
        + Clone
        + Send
        + Sync
        + 'static,
    <L::Service as Service<http::Request<axum::body::Body>>>::Future: Send + 'static,
    B: HttpBody<Data = Bytes> + Send + 'static,
    B::Error: Into<BoxError>,
{
    BoxService::new(EngineOut(layer.layer(EngineIn(inner))))
}
