//! Core HTTP layer of axumapi: application builder, routing, extraction,
//! responses, dependency injection, middleware and RFC 7807 errors.
//!
//! The underlying HTTP engine is an internal implementation detail; the
//! public API consists of axumapi-owned types and traits. Every extractor
//! and response type can document itself, so OpenAPI operations are derived
//! from handler signatures.
#![forbid(unsafe_code)]

pub mod app;
pub mod background;
pub mod body;
pub mod di;
pub mod error;
pub mod extract;
pub mod form;
pub mod handler;
pub mod header;
pub mod lifespan;
pub mod middleware;
pub mod response;
pub mod responses;
pub mod routing;
pub mod service;
pub mod state;
pub mod static_files;
pub mod ws;

pub use app::{App, AppMeta, DocsConfig};
pub use body::Body;
pub use error::{ApiError, ApiResult, BodyError, ServerError};
pub use extract::{FromRequest, FromRequestParts, Path, Query, RawRequest, Request};
pub use handler::Handler;
pub use response::{Html, IntoResponse, Json, NoContent, PlainText, Response, WithStatus};
pub use routing::{
    MethodRouter, OperationMeta, Route, delete, get, head, options, patch, post, put,
};
pub use service::RouterService;
pub use state::State;

/// Re-exports of standard HTTP types used in the public API.
pub mod http {
    pub use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
}
