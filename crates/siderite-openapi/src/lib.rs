//! OpenAPI 3.1 document model and generation for siderite.
//!
//! * [`model`] – typed, serializable OpenAPI 3.1 structures.
//! * [`DocumentBuilder`] – collects operations and produces a document whose
//!   schemas are deduplicated into `components.schemas`.
//! * [`ui`] – HTML pages for Swagger UI (`/docs`) and ReDoc (`/redoc`).
//!
//! This crate knows nothing about HTTP servers; `siderite-core` feeds it route
//! metadata and serves the result.
#![forbid(unsafe_code)]

mod builder;
pub mod model;
pub mod ui;

pub use builder::{DocumentBuilder, OpenApiError};
pub use model::{
    Components, HttpMethod, Info, MediaType, OpenApi, Operation, Parameter, ParameterLocation,
    PathItem, RequestBody, Response,
};

/// Re-export of the schema types operations are described with.
pub use siderite_validation::{Schema, SchemaObject, SchemaRegistry};
