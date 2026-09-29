//! Core HTTP layer of axumapi: application builder, routing, extractors,
//! responses and RFC 7807 error handling.
//!
//! The underlying HTTP engine is an internal implementation detail; the
//! public API consists of axumapi-owned types.
#![forbid(unsafe_code)]

pub mod app;
pub mod body;
pub mod error;
pub mod extract;
pub mod response;
pub mod routing;
pub mod service;
pub mod state;

pub use app::{App, AppMeta};
pub use body::Body;
pub use error::{ApiError, ApiResult, BodyError, ServerError};
pub use extract::{Path, Query};
pub use response::{Html, Json, NoContent, PlainText, WithStatus};
pub use routing::{MethodRouter, delete, get, head, options, patch, post, put};
pub use service::RouterService;
pub use state::State;
