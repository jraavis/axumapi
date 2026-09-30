//! Structured validation errors with Pydantic-style location paths.

use std::borrow::Cow;
use std::fmt;

use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One segment of a field location path (`loc` in Pydantic).
///
/// Serialized untagged, so a key becomes a JSON string and an index a JSON number.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LocationItem {
    /// Object key / field name.
    Key(String),
    /// Array index.
    Index(usize),
}

impl LocationItem {
    /// Object-key location segment.
    pub fn key(key: impl Into<String>) -> Self {
        Self::Key(key.into())
    }

    /// Array-index location segment.
    pub fn index(index: usize) -> Self {
        Self::Index(index)
    }
}

impl fmt::Display for LocationItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Key(key) => f.write_str(key),
            Self::Index(index) => write!(f, "{index}"),
        }
    }
}

impl From<String> for LocationItem {
    fn from(key: String) -> Self {
        Self::Key(key)
    }
}

impl From<&str> for LocationItem {
    fn from(key: &str) -> Self {
        Self::Key(key.to_owned())
    }
}

impl From<usize> for LocationItem {
    fn from(index: usize) -> Self {
        Self::Index(index)
    }
}

/// A single field-level validation failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{code}: {message}")]
pub struct FieldError {
    /// Path from the validated root to the failing value.
    pub location: Vec<LocationItem>,
    /// Stable machine-readable error code.
    pub code: Cow<'static, str>,
    /// Human-readable explanation.
    pub message: String,
}

impl FieldError {
    /// Create a field error with an empty location path.
    pub fn new(code: impl Into<Cow<'static, str>>, message: impl Into<String>) -> Self {
        Self {
            location: Vec::new(),
            code: code.into(),
            message: message.into(),
        }
    }

    /// Prepend a location segment so nested errors can bubble toward the root.
    #[must_use]
    pub fn at(mut self, item: impl Into<LocationItem>) -> Self {
        self.location.insert(0, item.into());
        self
    }
}

/// Aggregate of one or more [`FieldError`] values.
///
/// Serializes as:
///
/// ```json
/// {"type":"validation_error","errors":[...]}
/// ```
#[derive(Debug, Clone, PartialEq, Default, Error)]
#[error("validation failed with {n} error(s)", n = self.errors.len())]
pub struct ValidationError {
    /// Collected field-level violations.
    pub errors: Vec<FieldError>,
}

impl ValidationError {
    /// Create an empty error list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a field error.
    pub fn push(&mut self, error: FieldError) {
        self.errors.push(error);
    }

    /// Whether no field errors have been collected.
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Append all errors from `other`.
    pub fn merge(&mut self, other: Self) {
        self.errors.extend(other.errors);
    }

    /// Prepend `item` to the location of every contained error.
    #[must_use]
    pub fn prefixed(self, item: impl Into<LocationItem>) -> Self {
        let item = item.into();
        Self {
            errors: self
                .errors
                .into_iter()
                .map(|error| error.at(item.clone()))
                .collect(),
        }
    }

    /// Convert into `Ok(())` when empty, otherwise `Err(self)`.
    pub fn into_result(self) -> Result<(), Self> {
        if self.is_empty() { Ok(()) } else { Err(self) }
    }
}

impl From<FieldError> for ValidationError {
    fn from(error: FieldError) -> Self {
        Self {
            errors: vec![error],
        }
    }
}

impl Serialize for ValidationError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("ValidationError", 2)?;
        state.serialize_field("type", "validation_error")?;
        state.serialize_field("errors", &self.errors)?;
        state.end()
    }
}

/// Result of a validation operation.
pub type ValidationResult<T> = Result<T, ValidationError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_prepends_location_segments() {
        let error = FieldError::new("invalid_email", "invalid email address")
            .at(LocationItem::key("email"))
            .at(LocationItem::key("body"));
        assert_eq!(
            error.location,
            vec![LocationItem::key("body"), LocationItem::key("email")]
        );
    }

    #[test]
    fn serialize_matches_documented_shape() {
        let mut error = ValidationError::new();
        error.push(
            FieldError::new("invalid_email", "invalid email address")
                .at("email")
                .at("body"),
        );
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "validation_error",
                "errors": [{
                    "location": ["body", "email"],
                    "code": "invalid_email",
                    "message": "invalid email address"
                }]
            })
        );
    }

    #[test]
    fn location_index_serializes_as_number() {
        let error = FieldError::new("greater_than", "too small").at(LocationItem::index(0));
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["location"], serde_json::json!([0]));
    }

    #[test]
    fn location_item_untagged_round_trip() {
        let key: LocationItem = serde_json::from_value(serde_json::json!("email")).unwrap();
        let index: LocationItem = serde_json::from_value(serde_json::json!(3)).unwrap();
        assert_eq!(key, LocationItem::key("email"));
        assert_eq!(index, LocationItem::index(3));
    }

    #[test]
    fn into_result_ok_when_empty() {
        assert!(ValidationError::new().into_result().is_ok());
    }

    #[test]
    fn into_result_err_when_nonempty() {
        let mut error = ValidationError::new();
        error.push(FieldError::new("invalid_email", "invalid email address"));
        assert!(error.into_result().is_err());
    }

    #[test]
    fn merge_appends_errors() {
        let mut left = ValidationError::from(FieldError::new("a", "A"));
        left.merge(ValidationError::from(FieldError::new("b", "B")));
        assert_eq!(left.errors.len(), 2);
        assert_eq!(left.errors[0].code, "a");
        assert_eq!(left.errors[1].code, "b");
    }

    #[test]
    fn prefixed_prepends_to_every_error() {
        let mut error = ValidationError::new();
        error.push(FieldError::new("x", "X").at("email"));
        error.push(FieldError::new("y", "Y").at("name"));
        let error = error.prefixed("body");
        assert_eq!(
            error.errors[0].location,
            vec![LocationItem::key("body"), LocationItem::key("email")]
        );
        assert_eq!(
            error.errors[1].location,
            vec![LocationItem::key("body"), LocationItem::key("name")]
        );
    }

    #[test]
    fn is_empty_tracks_contents() {
        let mut error = ValidationError::new();
        assert!(error.is_empty());
        error.push(FieldError::new("x", "X"));
        assert!(!error.is_empty());
    }

    #[test]
    fn display_and_error_trait() {
        let error =
            ValidationError::from(FieldError::new("invalid_email", "invalid email address"));
        assert_eq!(error.to_string(), "validation failed with 1 error(s)");
        let _: &dyn std::error::Error = &error;
        assert_eq!(
            error.errors[0].to_string(),
            "invalid_email: invalid email address"
        );
    }
}
