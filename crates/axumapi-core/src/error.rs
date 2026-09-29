//! Error types: RFC 7807 problem details, server and body errors.

use crate::response::{IntoResponse, Response, with_content_type};
use axumapi_openapi::{Operation, Schema, SchemaObject, SchemaRegistry};
use axumapi_orm::{BackendError, OrmError, QueryError};
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
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
    /// Boxed so `ApiError` stays small when `serde_json` is built with
    /// `preserve_order` (pulled in by the MongoDB driver under `--all-features`).
    errors: Option<Box<Value>>,
    /// Extra response headers (for example `WWW-Authenticate`); boxed for the
    /// same reason as `errors`.
    headers: Option<Box<HeaderMap>>,
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
            headers: None,
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
        err.errors = Some(Box::new(errors));
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

    /// Attach a response header (appended if the name is already set).
    #[must_use]
    pub fn with_header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers
            .get_or_insert_with(Default::default)
            .append(name, value);
        self
    }

    /// Extra response headers attached with [`with_header`](Self::with_header).
    pub fn headers(&self) -> Option<&HeaderMap> {
        self.headers.as_deref()
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
        self.errors.as_deref()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let problem = Problem {
            type_uri: &self.type_uri,
            title: self.title,
            status: self.status.as_u16(),
            detail: self.detail.as_deref(),
            errors: self.errors.as_deref(),
        };
        let body = serde_json::to_vec(&problem).unwrap_or_else(|_| FALLBACK_BODY.to_vec());
        let mut response = with_content_type(PROBLEM_JSON, body);
        *response.status_mut() = self.status;
        if let Some(extra) = self.headers {
            response.headers_mut().extend(*extra);
        }
        response
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        let schema = registry.subschema::<ProblemSchema>();
        op.add_response(
            "default",
            "Error (RFC 7807 problem details)",
            Some((PROBLEM_JSON, schema)),
        );
    }
}

/// Schema of the RFC 7807 document produced by [`ApiError`].
struct ProblemSchema;

impl Schema for ProblemSchema {
    fn schema_name() -> Option<&'static str> {
        Some("Problem")
    }

    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("object")
            .with(
                "properties",
                serde_json::json!({
                    "type": {"type": "string"},
                    "title": {"type": "string"},
                    "status": {"type": "integer"},
                    "detail": {"type": "string"},
                    "errors": {}
                }),
            )
            .with("required", serde_json::json!(["type", "title", "status"]))
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
            OrmError::Backend(BackendError::Constraint(detail)) => {
                // The driver message may name tables/columns: log it, don't return it.
                tracing::info!(%detail, "database constraint violation");
                Self::new(
                    StatusCode::CONFLICT,
                    "The request conflicts with existing data (a uniqueness, reference or check constraint failed).",
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
    /// The application is misconfigured (detected before serving).
    #[error("invalid application configuration: {0}")]
    Configuration(String),
    /// A startup or shutdown hook failed.
    #[error("lifespan hook failed: {0}")]
    Lifespan(#[source] ApiError),
    /// The server failed while serving connections.
    #[error("server error: {0}")]
    Serve(#[source] std::io::Error),
}

/// Failure to read a request or response body.
#[derive(Debug, Clone, Error)]
#[error("failed to read body: {0}")]
pub struct BodyError(pub(crate) String);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orm_errors_map_to_http_statuses() {
        let conflict = ApiError::from(OrmError::Backend(BackendError::Constraint(
            "UNIQUE constraint failed: users.email".into(),
        )));
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        let missing = ApiError::from(OrmError::Query(QueryError::DoesNotExist));
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let broken = ApiError::from(OrmError::Backend(BackendError::Database("x".into())));
        assert_eq!(broken.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
