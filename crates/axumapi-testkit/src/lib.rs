//! In-process testing helpers for axumapi applications.
#![forbid(unsafe_code)]

use axumapi_core::{App, Body, BodyError, RouterService};
use http::{HeaderMap, Method, Request, StatusCode, header};
use serde::{Serialize, de::DeserializeOwned};
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
}

impl TestClient {
    /// Wrap `app`.
    pub fn new(app: App) -> Self {
        Self {
            service: app.into_router_service(),
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
            body: body.into_bytes().await?,
        })
    }
}
