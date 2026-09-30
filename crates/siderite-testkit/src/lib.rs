//! In-process testing helpers for siderite applications.
#![forbid(unsafe_code)]

mod builder;
mod database;

pub use builder::TestClientBuilder;
pub use database::{TestDatabase, TestDatabaseError};
/// Re-exported so tests can build custom [`Request`]s for [`TestClient::send`].
pub use http;

use http::{HeaderMap, Method, Request, StatusCode, header};
use serde::{Serialize, de::DeserializeOwned};
use siderite_core::lifespan::Lifespan;
use siderite_core::{App, Body, BodyError, RouterService, ServerError};
use std::sync::{Arc, Mutex, PoisonError};
use thiserror::Error;
use tower::ServiceExt;

/// Failure while building or executing a test request.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TestClientError {
    /// The request could not be constructed.
    #[error("invalid request: {0}")]
    Request(#[from] http::Error),
    /// The JSON payload could not be serialized.
    #[error("cannot serialize request body: {0}")]
    Serialize(#[from] serde_json::Error),
    /// The response body could not be read.
    #[error(transparent)]
    Body(#[from] BodyError),
}

/// A fully buffered response.
#[derive(Debug, Clone)]
pub struct TestResponse {
    /// Status code.
    pub status: StatusCode,
    /// Response headers.
    pub headers: HeaderMap,
    /// Raw body bytes.
    pub body: Vec<u8>,
}

impl TestResponse {
    /// Deserialize the body as JSON.
    ///
    /// # Errors
    /// Returns an error if the body is not valid JSON for `T`.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }

    /// The body as text (invalid UTF-8 is replaced).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The `content-type` header, if present and valid UTF-8.
    pub fn content_type(&self) -> Option<&str> {
        self.headers.get(header::CONTENT_TYPE)?.to_str().ok()
    }
}

/// Drives an [`App`] in-process, without sockets.
#[derive(Debug, Clone)]
pub struct TestClient {
    service: RouterService,
    lifespan: Arc<Mutex<Option<Lifespan>>>,
}

impl TestClient {
    /// Start a [`TestClientBuilder`] to apply dependency overrides and
    /// databases to `app` before it is built.
    pub fn builder(app: App) -> TestClientBuilder {
        TestClientBuilder::new(app)
    }

    /// Wrap `app`.
    ///
    /// # Errors
    /// Returns the app's configuration error (duplicate routes, invalid
    /// paths, OpenAPI generation failures).
    pub fn try_new(app: App) -> Result<Self, siderite_core::ServerError> {
        Ok(Self {
            service: app.into_router_service()?,
            lifespan: Arc::default(),
        })
    }

    /// Wrap `app`, for use in tests.
    ///
    /// # Panics
    /// Panics if the app is misconfigured; use [`TestClient::try_new`] to
    /// assert on configuration errors.
    pub fn new(app: App) -> Self {
        match Self::try_new(app) {
            Ok(client) => client,
            Err(err) => panic!("invalid app configuration: {err}"),
        }
    }

    /// Wrap `app` and run its startup hooks (mounted children included).
    ///
    /// Unlike [`TestClient::try_new`], which never runs hooks, this mirrors
    /// `App::run` without binding a socket. Call [`TestClient::shutdown`] to
    /// run the shutdown hooks.
    ///
    /// # Errors
    /// Returns the configuration error, or [`ServerError::Lifespan`] if a
    /// startup hook fails.
    pub async fn start(app: App) -> Result<Self, ServerError> {
        let (service, mut lifespan) = app.into_service_with_lifespan()?;
        lifespan.startup().await?;
        Ok(Self {
            service,
            lifespan: Arc::new(Mutex::new(Some(lifespan))),
        })
    }

    /// Run the shutdown hooks (reverse order). A no-op if already run or if
    /// the client was not created with [`TestClient::start`].
    ///
    /// # Errors
    /// Returns [`ServerError::Lifespan`] with the first failing hook.
    pub async fn shutdown(&self) -> Result<(), ServerError> {
        let taken = self
            .lifespan
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match taken {
            Some(mut lifespan) => lifespan.shutdown().await,
            None => Ok(()),
        }
    }

    /// Send a GET request.
    ///
    /// # Errors
    /// Returns [`TestClientError`] on a malformed request or unreadable body.
    pub async fn get(&self, path: &str) -> Result<TestResponse, TestClientError> {
        let req = Request::builder()
            .method(Method::GET)
            .uri(path)
            .body(Body::empty())?;
        self.send(req).await
    }

    /// Send a DELETE request.
    ///
    /// # Errors
    /// Returns [`TestClientError`] on a malformed request or unreadable body.
    pub async fn delete(&self, path: &str) -> Result<TestResponse, TestClientError> {
        let req = Request::builder()
            .method(Method::DELETE)
            .uri(path)
            .body(Body::empty())?;
        self.send(req).await
    }

    /// Send a POST request with a JSON body.
    ///
    /// # Errors
    /// Returns [`TestClientError`] on serialization, request or body failure.
    pub async fn post_json<T: Serialize>(
        &self,
        path: &str,
        payload: &T,
    ) -> Result<TestResponse, TestClientError> {
        self.post_raw(path, "application/json", serde_json::to_vec(payload)?)
            .await
    }

    /// Send a POST request with an arbitrary body and content type.
    ///
    /// # Errors
    /// Returns [`TestClientError`] on a malformed request or unreadable body.
    pub async fn post_raw(
        &self,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<TestResponse, TestClientError> {
        let req = Request::builder()
            .method(Method::POST)
            .uri(path)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))?;
        self.send(req).await
    }

    /// Send any request and buffer the response.
    ///
    /// # Errors
    /// Returns [`TestClientError::Body`] if the response body cannot be read.
    pub async fn send(&self, req: Request<Body>) -> Result<TestResponse, TestClientError> {
        let Ok(response) = self.service.clone().oneshot(req).await;
        let (parts, body) = response.into_parts();
        Ok(TestResponse {
            status: parts.status,
            headers: parts.headers,
            body: body.into_bytes_limited(usize::MAX).await?,
        })
    }
}
