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
pub use axumapi_macros::{
    Model, Schema, Validate, delete, get, head, model_hooks, options, patch, post, put, routes, ws,
};
pub use axumapi_openapi as openapi;

/// The ORM, plus the column-type crates (`chrono`, `uuid`, `rust_decimal`) its
/// field types come from.
pub mod orm {
    pub use axumapi_orm::*;
    pub use {chrono, rust_decimal, uuid};
}
pub use axumapi_validation as validation;

/// Everything needed to write a typical application.
pub mod prelude {
    pub use crate::orm;
    pub use axumapi_core::{
        ApiError, ApiResult, App, BackgroundTasks, Cookies, Dependency, Depends, Form, FromRequest,
        FromRequestParts, Header, Html, IntoResponse, Json, Message, MethodRouter, NoContent, Path,
        PlainText, Provided, Query, Redirect, ResolveContext, Resource, Route, ServerError, State,
        WebSocket, WebSocketUpgrade, WithStatus, delete, get, head, options, patch, post, put,
    };
    pub use axumapi_macros::{
        Model, Schema, Validate, delete, get, head, model_hooks, options, patch, post, put, routes,
        ws,
    };
    pub use axumapi_orm::{
        Db, DbType, Expr, Field, ForeignKey, Model, OneToOne, OrmError, QuerySet,
    };
    pub use axumapi_validation::{
        Schema, SchemaObject, SchemaRegistry, Validate, ValidationError, ValidationResult,
    };
    pub use chrono::{DateTime, Utc};
    pub use serde::{Deserialize, Serialize};
}

/// Support code for macro expansions. Not part of the public API.
#[doc(hidden)]
pub mod __private;
