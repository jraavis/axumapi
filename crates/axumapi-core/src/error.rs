//! Error types: RFC 7807 problem details, server and body errors.

use axum::response::{IntoResponse, Response};
use axumapi_orm::{OrmError, QueryError};
use http::{HeaderValue, StatusCode, header};
use serde::Serialize;
use serde_json::Value;
use std::fmt::Display;
use thiserror::Error;

/// Media type of RFC 7807 problem documents.
pub const PROBLEM_JSON: &str = "application/problem+json";

const GENERIC_INTERNAL_DETAIL: &str = "An internal error occurred.";
const FALLBACK_BODY: &[u8] =
    br#"{"type":"about:blank","title":"Internal Server Error","status":500}"#;

/// Convenience alias for handler results.
pub type ApiResult<T> = Result<T, ApiError>;

/// An HTTP error rendered as an RFC 7807 `application/problem+json` document.
#[derive(Debug, Clone, Error)]
#[error("{status}: {title}")]
pub struct ApiError {
    status: StatusCode,
    title: &'static str,
    type_uri: String,
    detail: Option<String>,
    errors: Option<Value>,
}

#[derive(Serialize)]
struct Problem<'a> {
    #[serde(rename = "type")]
    type_uri: &'a str,
    title: &'a str,
    status: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Option<&'a Value>,
}

impl ApiError {
    /// Create an error with an arbitrary status and human-readable detail.
    pub fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            title: status.canonical_reason().unwrap_or("Unknown"),
            type_uri: "about:blank".to_owned(),
            detail: Some(detail.into()),
            errors: None,
        }
    }

    /// 404 Not Found.
    pub fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, detail)
    }

    /// 400 Bad Request.
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, detail)
    }

    /// 422 Unprocessable Entity carrying structured validation `errors`.
    pub fn unprocessable(errors: Value) -> Self {
        let mut err = Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "The request could not be processed; see `errors` for details.",
        );
        err.errors = Some(errors);
        err
    }

    /// 500 Internal Server Error. `source` is logged but never sent to the client.
    pub fn internal(source: impl Display) -> Self {
        tracing::error!(error = %source, "internal server error");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, GENERIC_INTERNAL_DETAIL)
    }

    /// Override the problem `type` URI (defaults to `about:blank`).
    #[must_use]
    pub fn with_type(mut self, type_uri: impl Into<String>) -> Self {
        self.type_uri = type_uri.into();
        self
    }

    /// The HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The short, status-derived title.
    pub fn title(&self) -> &str {
        self.title
    }

    /// The human-readable detail, if any.
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Structured error extension, if any.
    pub fn errors(&self) -> Option<&Value> {
        self.errors.as_ref()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let problem = Problem {
            type_uri: &self.type_uri,
            title: self.title,
            status: self.status.as_u16(),
            detail: self.detail.as_deref(),
            errors: self.errors.as_ref(),
        };
        let body = serde_json::to_vec(&problem).unwrap_or_else(|_| FALLBACK_BODY.to_vec());
        let mut response = Response::new(axum::body::Body::from(body));
        *response.status_mut() = self.status;
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON));
        response
    }
}

impl From<OrmError> for ApiError {
    fn from(err: OrmError) -> Self {
        match err {
            OrmError::Query(QueryError::DoesNotExist) => Self::not_found("Resource not found."),
            OrmError::Capability(cap) => {
                tracing::error!(error = %cap, "unsupported backend capability");
                Self::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "The requested operation is not supported by the configured database backend.",
                )
            }
            other => Self::internal(other),
        }
    }
}

/// Failure to bind or run the HTTP server.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ServerError {
    /// The listen address could not be bound.
    #[error("failed to bind {addr}")]
    Bind {
        /// Address that was requested.
        addr: String,
        /// Underlying IO error.
        #[source]
        source: std::io::Error,
    },
    /// The server failed while serving connections.
    #[error("server error: {0}")]
    Serve(#[source] std::io::Error),
}

/// Failure to read a request or response body.
#[derive(Debug, Clone, Error)]
#[error("failed to read body: {0}")]
pub struct BodyError(pub(crate) String);
