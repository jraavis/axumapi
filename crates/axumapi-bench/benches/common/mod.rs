//! Helpers shared by the benchmark binaries.
#![allow(dead_code, clippy::unwrap_used)]

use axumapi::Body;
use http::{Method, Request, header};

/// Build a bodiless request for `method` and `uri`.
pub fn empty_request(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

/// Build a JSON request for `uri` carrying `body`.
pub fn json_request(uri: &str, body: &[u8]) -> Request<Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_vec()))
        .unwrap()
}

/// Same as [`empty_request`] for a raw axum router.
pub fn axum_empty_request(method: Method, uri: &str) -> Request<axum::body::Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(axum::body::Body::empty())
        .unwrap()
}

/// Same as [`json_request`] for a raw axum router.
pub fn axum_json_request(uri: &str, body: &[u8]) -> Request<axum::body::Body> {
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body.to_vec()))
        .unwrap()
}
