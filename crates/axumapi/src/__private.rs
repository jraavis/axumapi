//! Runtime helpers used by `#[derive(Schema)]` expansions.
//!
//! Semver-exempt: only the macros in this workspace may rely on it.

pub use axumapi_validation::hooks::{Probe, ViaDefault, ViaHooks};
pub use regex::Regex;
pub use serde;
pub use serde_json;
pub use serde_json::json;

use axumapi_validation::SchemaObject;
use serde_json::{Map, Number, Value};

/// Add annotation/constraint keywords to `schema`.
///
/// A bare `$ref` cannot carry sibling keywords in every OpenAPI consumer, so
/// it is wrapped as `{"allOf": [{"$ref": ..}], ...keywords}`.
pub fn annotate(schema: SchemaObject, keywords: Vec<(&'static str, Value)>) -> SchemaObject {
    if keywords.is_empty() {
        return schema;
    }
    let mut target = if schema.get("$ref").is_some() {
        SchemaObject::default().with("allOf", Value::Array(vec![schema.into_value()]))
    } else {
        schema
    };
    for (key, value) in keywords {
        target.0.insert(key.to_owned(), value);
    }
    target
}

/// Accumulates the properties of an object schema.
#[derive(Debug, Default)]
pub struct ObjectBuilder {
    properties: Map<String, Value>,
    required: Vec<Value>,
}

impl ObjectBuilder {
    /// Empty object.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a property.
    pub fn property(&mut self, name: &str, required: bool, schema: SchemaObject) {
        self.properties.insert(name.to_owned(), schema.into_value());
        if required {
            self.required.push(Value::String(name.to_owned()));
        }
    }

    /// The properties added so far (computed fields add theirs here).
    pub fn properties_mut(&mut self) -> &mut Map<String, Value> {
        &mut self.properties
    }

    /// Finish the schema; `deny_unknown` sets `additionalProperties: false`.
    pub fn build(self, deny_unknown: bool) -> SchemaObject {
        let mut schema = SchemaObject::of_type("object").with("properties", self.properties);
        if !self.required.is_empty() {
            schema = schema.with("required", self.required);
        }
        if deny_unknown {
            schema = schema.with("additionalProperties", false);
        }
        schema
    }
}

/// `{"type": "string", "const": value}`.
pub fn const_string(value: &str) -> SchemaObject {
    SchemaObject::of_type("string").with("const", value)
}

/// `{"type": "string", "enum": [..]}`.
pub fn string_enum(values: &[&str]) -> SchemaObject {
    SchemaObject::of_type("string").with("enum", values.to_vec())
}

/// `{"oneOf": [..]}`; an empty list yields a schema nothing satisfies.
pub fn one_of(schemas: Vec<SchemaObject>) -> SchemaObject {
    if schemas.is_empty() {
        return SchemaObject::default().with("not", Value::Object(Map::new()));
    }
    let items: Vec<Value> = schemas.into_iter().map(SchemaObject::into_value).collect();
    SchemaObject::default().with("oneOf", items)
}

/// Fixed-length array schema (`prefixItems`).
pub fn tuple(items: Vec<SchemaObject>) -> SchemaObject {
    let len = items.len() as u64;
    let items: Vec<Value> = items.into_iter().map(SchemaObject::into_value).collect();
    SchemaObject::of_type("array")
        .with("prefixItems", items)
        .with("minItems", len)
        .with("maxItems", len)
}

/// Externally tagged variant: `{"Name": payload}`.
pub fn externally_tagged(name: &str, payload: SchemaObject) -> SchemaObject {
    let mut object = ObjectBuilder::new();
    object.property(name, true, payload);
    object.build(true)
}

/// Unit variant of an internally or adjacently tagged enum: `{tag: "Name"}`.
pub fn tagged_unit(tag: &str, name: &str) -> SchemaObject {
    let mut object = ObjectBuilder::new();
    object.property(tag, true, const_string(name));
    object.build(false)
}

/// Adjacently tagged variant: `{tag: "Name", content: payload}`.
pub fn adjacently_tagged(
    tag: &str,
    name: &str,
    content: &str,
    payload: Option<SchemaObject>,
) -> SchemaObject {
    let mut object = ObjectBuilder::new();
    object.property(tag, true, const_string(name));
    if let Some(payload) = payload {
        object.property(content, true, payload);
    }
    object.build(false)
}

/// Internally tagged newtype variant: the tag object plus the inner struct.
pub fn internally_tagged_newtype(tag: &str, name: &str, inner: SchemaObject) -> SchemaObject {
    let parts = vec![tagged_unit(tag, name).into_value(), inner.into_value()];
    SchemaObject::default().with("allOf", parts)
}

/// A finite float as a JSON number. The derives reject non-finite bounds at
/// compile time, so the fallback is never used.
pub fn number_f64(value: f64) -> Number {
    Number::from_f64(value).unwrap_or_else(|| Number::from(0_i64))
}
