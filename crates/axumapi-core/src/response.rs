//! Response types. [`Json`] doubles as a request-body extractor (see `extract`).

use crate::error::ApiError;
use axum::response::{IntoResponse, Response};
use http::{HeaderValue, StatusCode, header};
use serde::Serialize;

/// JSON body (response) or JSON request extractor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Json<T>(pub T);

/// HTML response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Html<T>(pub T);

/// `text/plain` response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlainText<T>(pub T);

/// `204 No Content` response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoContent;

/// Overrides the status code of an inner response.
#[derive(Debug, Clone, Copy)]
pub struct WithStatus<R>(pub StatusCode, pub R);

fn text_response(content_type: &'static str, body: String) -> Response {
    let mut response = Response::new(axum::body::Body::from(body));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        match serde_json::to_vec(&self.0) {
            Ok(bytes) => {
                let mut response = Response::new(axum::body::Body::from(bytes));
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                response
            }
            Err(err) => ApiError::internal(err).into_response(),
        }
    }
}

impl<T: Into<String>> IntoResponse for Html<T> {
    fn into_response(self) -> Response {
        text_response("text/html; charset=utf-8", self.0.into())
    }
}

impl<T: Into<String>> IntoResponse for PlainText<T> {
    fn into_response(self) -> Response {
        text_response("text/plain; charset=utf-8", self.0.into())
    }
}

impl IntoResponse for NoContent {
    fn into_response(self) -> Response {
        StatusCode::NO_CONTENT.into_response()
    }
}

impl<R: IntoResponse> IntoResponse for WithStatus<R> {
    fn into_response(self) -> Response {
        let mut response = self.1.into_response();
        *response.status_mut() = self.0;
        response
    }
}
