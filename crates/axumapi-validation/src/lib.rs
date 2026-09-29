//! Pydantic v2-inspired validation and JSON Schema metadata for axumapi.
//!
//! This crate provides:
//!
//! - Structured [`ValidationError`] values with location / code / message
//! - The [`Validate`] trait, including blanket impls for [`Option`] and [`Vec`]
//! - Reusable field [`rules`]
//! - Constrained newtypes in [`types`] that validate on construction and deserialize
//! - JSON Schema / OpenAPI 3.1 metadata via [`Schema`]
#![forbid(unsafe_code)]

pub mod error;
pub mod rules;
pub mod schema;
pub mod types;
pub mod validate;

pub use error::{FieldError, LocationItem, ValidationError, ValidationResult};
pub use rules::{email, ge, gt, le, lt, max_length, min_length, multiple_of, pattern};
pub use schema::{Schema, SchemaConflict, SchemaObject, SchemaRegistry, schema_for};
pub use types::{BoundedI64, ConstrainedString, Email, PositiveInt, SecretString};
pub use validate::Validate;
