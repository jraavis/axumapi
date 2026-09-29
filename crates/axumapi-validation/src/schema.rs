//! Minimal JSON Schema metadata shared with OpenAPI 3.1 / JSON Schema 2020-12.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::types::{BoundedI64, ConstrainedString, Email, PositiveInt, SecretString};

/// A JSON Schema object: a map of schema keywords.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SchemaObject(
    /// Keyword/value pairs making up the schema.
    pub Map<String, Value>,
);

impl SchemaObject {
    /// Schema object with a single `"type"` keyword.
    pub fn of_type(ty: &str) -> Self {
        let mut map = Map::new();
        map.insert("type".to_owned(), Value::String(ty.to_owned()));
        Self(map)
    }

    /// Insert or replace a schema keyword.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.insert(key.into(), value.into());
        self
    }

    /// Convert into a JSON value.
    pub fn into_value(self) -> Value {
        Value::Object(self.0)
    }
}

/// Types that can describe themselves as JSON Schema.
pub trait Schema {
    /// JSON Schema document for this type.
    fn schema() -> SchemaObject;

    /// Optional stable name used for OpenAPI `$ref`s.
    fn schema_name() -> Option<&'static str> {
        None
    }
}

impl Schema for String {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("string")
    }
}

impl Schema for bool {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("boolean")
    }
}

impl Schema for i32 {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("integer").with("format", "int32")
    }
}

impl Schema for i64 {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("integer").with("format", "int64")
    }
}

impl Schema for u8 {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("integer")
            .with("minimum", 0_i64)
            .with("maximum", 255_i64)
    }
}

impl Schema for u32 {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("integer")
            .with("minimum", 0_i64)
            .with("maximum", i64::from(u32::MAX))
    }
}

impl Schema for f64 {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("number").with("format", "double")
    }
}

impl<T: Schema> Schema for Option<T> {
    fn schema() -> SchemaObject {
        SchemaObject(Map::new()).with(
            "anyOf",
            Value::Array(vec![
                T::schema().into_value(),
                SchemaObject::of_type("null").into_value(),
            ]),
        )
    }
}

impl<T: Schema> Schema for Vec<T> {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("array").with("items", T::schema().into_value())
    }
}

impl<const MIN: usize, const MAX: usize> Schema for ConstrainedString<MIN, MAX> {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("string")
            .with("minLength", MIN as u64)
            .with("maxLength", MAX as u64)
    }
}

impl<const MIN: i64, const MAX: i64> Schema for BoundedI64<MIN, MAX> {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("integer")
            .with("minimum", MIN)
            .with("maximum", MAX)
    }
}

impl Schema for PositiveInt {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("integer").with("exclusiveMinimum", 0_i64)
    }

    fn schema_name() -> Option<&'static str> {
        Some("PositiveInt")
    }
}

impl Schema for Email {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("string").with("format", "email")
    }

    fn schema_name() -> Option<&'static str> {
        Some("Email")
    }
}

impl Schema for SecretString {
    fn schema() -> SchemaObject {
        SchemaObject::of_type("string").with("format", "password")
    }

    fn schema_name() -> Option<&'static str> {
        Some("SecretString")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_schemas() {
        assert_eq!(
            String::schema().into_value(),
            serde_json::json!({"type": "string"})
        );
        assert_eq!(
            bool::schema().into_value(),
            serde_json::json!({"type": "boolean"})
        );
        assert_eq!(
            i32::schema().into_value(),
            serde_json::json!({"type": "integer", "format": "int32"})
        );
        assert_eq!(
            i64::schema().into_value(),
            serde_json::json!({"type": "integer", "format": "int64"})
        );
        assert_eq!(
            u8::schema().into_value(),
            serde_json::json!({"type": "integer", "minimum": 0, "maximum": 255})
        );
        assert_eq!(
            u32::schema().into_value(),
            serde_json::json!({"type": "integer", "minimum": 0, "maximum": 4294967295_u64})
        );
        assert_eq!(
            f64::schema().into_value(),
            serde_json::json!({"type": "number", "format": "double"})
        );
        assert_eq!(String::schema_name(), None);
    }

    #[test]
    fn option_uses_any_of_null() {
        assert_eq!(
            Option::<String>::schema().into_value(),
            serde_json::json!({
                "anyOf": [
                    {"type": "string"},
                    {"type": "null"}
                ]
            })
        );
    }

    #[test]
    fn vec_uses_array_items() {
        assert_eq!(
            Vec::<bool>::schema().into_value(),
            serde_json::json!({
                "type": "array",
                "items": {"type": "boolean"}
            })
        );
    }

    #[test]
    fn constrained_type_keywords() {
        assert_eq!(
            ConstrainedString::<1, 16>::schema().into_value(),
            serde_json::json!({"type": "string", "minLength": 1, "maxLength": 16})
        );
        assert_eq!(
            BoundedI64::<0, 100>::schema().into_value(),
            serde_json::json!({"type": "integer", "minimum": 0, "maximum": 100})
        );
        assert_eq!(
            PositiveInt::schema().into_value(),
            serde_json::json!({"type": "integer", "exclusiveMinimum": 0})
        );
        assert_eq!(
            Email::schema().into_value(),
            serde_json::json!({"type": "string", "format": "email"})
        );
        assert_eq!(
            SecretString::schema().into_value(),
            serde_json::json!({"type": "string", "format": "password"})
        );
        assert_eq!(Email::schema_name(), Some("Email"));
        assert_eq!(SecretString::schema_name(), Some("SecretString"));
        assert_eq!(PositiveInt::schema_name(), Some("PositiveInt"));
    }

    #[test]
    fn with_overwrites_and_into_value() {
        let schema = SchemaObject::of_type("string")
            .with("format", "email")
            .with("format", "idn-email");
        assert_eq!(
            schema.into_value(),
            serde_json::json!({"type": "string", "format": "idn-email"})
        );
    }
}
