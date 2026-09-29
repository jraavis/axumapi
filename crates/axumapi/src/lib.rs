//! axumapi: a batteries-included, type-driven web framework.
//!
//! ```no_run
//! use axumapi::prelude::*;
//!
//! async fn index() -> PlainText<&'static str> {
//!     PlainText("hello")
//! }
//!
//! # async fn run() -> Result<(), ServerError> {
//! App::new().route("/", get(index)).run("0.0.0.0:8000").await
//! # }
//! ```
#![forbid(unsafe_code)]

pub use axumapi_core::*;
pub use axumapi_openapi as openapi;
pub use axumapi_orm as orm;
pub use axumapi_validation as validation;

/// Everything needed to write a typical application.
pub mod prelude {
    pub use axumapi_core::{
        ApiError, ApiResult, App, FromRequest, FromRequestParts, Html, IntoResponse, Json,
        MethodRouter, NoContent, Path, PlainText, Query, Route, ServerError, State, WithStatus,
        delete, get, head, options, patch, post, put,
    };
    pub use axumapi_orm as orm;
    pub use axumapi_validation::{
        Schema, SchemaObject, SchemaRegistry, Validate, ValidationError, ValidationResult,
    };
    pub use serde::{Deserialize, Serialize};
}
