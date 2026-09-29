//! JSON Schema metadata shared with OpenAPI 3.1 / JSON Schema 2020-12.
//!
//! Types implement [`Schema`]. Composite schemas obtain child schemas through
//! [`SchemaRegistry::subschema`], which returns a `$ref` for named types and
//! stores their definition exactly once. This keeps OpenAPI documents small
//! (components are reused, not duplicated) and makes recursive types safe.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::any::TypeId;
use std::collections::BTreeMap;
use thiserror::Error;

use crate::types::{BoundedI64, ConstrainedString, Email, PositiveInt, SecretString};

/// Prefix of component references in OpenAPI documents.
pub const COMPONENTS_REF_PREFIX: &str = "#/components/schemas/";

/// A JSON Schema object: a map of schema keywords.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SchemaObject(
    /// Keyword/value pairs making up the schema.
    pub Map<String, Value>,
);

impl SchemaObject {
    /// Schema object with a single `"type"` keyword.
    pub fn of_type(ty: &str) -> Self {
        Self::default().with("type", ty)
    }

    /// `{"$ref": "<target>"}`.
    pub fn reference(target: impl Into<String>) -> Self {
        Self::default().with("$ref", target.into())
    }

    /// Insert or replace a schema keyword.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.0.insert(key.into(), value.into());
        self
    }

    /// Keyword lookup.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// Convert into a JSON value.
    pub fn into_value(self) -> Value {
        Value::Object(self.0)
    }
}

impl From<SchemaObject> for Value {
    fn from(s: SchemaObject) -> Self {
        s.into_value()
    }
}

/// Types that can describe themselves as JSON Schema.
pub trait Schema {
    /// Stable component name. Named types are emitted once under
    /// `components.schemas` and referenced with `$ref`.
    fn schema_name() -> Option<&'static str> {
        None
    }

    /// The schema *definition* of this type. Use
    /// [`SchemaRegistry::subschema`] for nested types.
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject;
}

/// Two distinct Rust types claimed the same component name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("schema name `{name}` is used by both `{first}` and `{second}`")]
pub struct SchemaConflict {
    /// Conflicting component name.
    pub name: String,
    /// Type registered first.
    pub first: &'static str,
    /// Type registered second.
    pub second: &'static str,
}

#[derive(Debug, Clone)]
struct Entry {
    type_id: TypeId,
    type_name: &'static str,
    /// `None` while the definition is being generated (recursion guard).
    definition: Option<SchemaObject>,
}

/// Collects named schema definitions while schemas are generated.
#[derive(Debug, Clone, Default)]
pub struct SchemaRegistry {
    entries: BTreeMap<String, Entry>,
    conflicts: Vec<SchemaConflict>,
    security_schemes: BTreeMap<String, serde_json::Value>,
}

impl SchemaRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Schema to embed for `T`: a `$ref` if `T` is named, otherwise inline.
    pub fn subschema<T: Schema + ?Sized + 'static>(&mut self) -> SchemaObject {
        let Some(name) = T::schema_name() else {
            return T::schema(self);
        };
        let reference = SchemaObject::reference(format!("{COMPONENTS_REF_PREFIX}{name}"));
        let type_id = TypeId::of::<T>();
        let type_name = std::any::type_name::<T>();
        if let Some(existing) = self.entries.get(name) {
            if existing.type_id != type_id {
                self.conflicts.push(SchemaConflict {
                    name: name.to_owned(),
                    first: existing.type_name,
                    second: type_name,
                });
            }
            return reference;
        }
        // Placeholder first so recursive references terminate.
        self.entries.insert(
            name.to_owned(),
            Entry {
                type_id,
                type_name,
                definition: None,
            },
        );
        let definition = T::schema(self);
        if let Some(entry) = self.entries.get_mut(name) {
            entry.definition = Some(definition);
        }
        reference
    }

    /// Register an OpenAPI security scheme under `name`
    /// (`components.securitySchemes`).
    ///
    /// Registering the same definition twice is a no-op; a different
    /// definition under an existing name is recorded as a [`SchemaConflict`].
    pub fn add_security_scheme(&mut self, name: &str, scheme: serde_json::Value) {
        match self.security_schemes.get(name) {
            Some(existing) if *existing != scheme => self.conflicts.push(SchemaConflict {
                name: name.to_owned(),
                first: "security scheme",
                second: "security scheme",
            }),
            Some(_) => {}
            None => {
                self.security_schemes.insert(name.to_owned(), scheme);
            }
        }
    }

    /// Remove and return every registered security scheme.
    pub fn take_security_schemes(&mut self) -> BTreeMap<String, serde_json::Value> {
        std::mem::take(&mut self.security_schemes)
    }

    /// Consume the registry, returning `components.schemas`.
    ///
    /// # Errors
    /// Returns every [`SchemaConflict`] detected during generation.
    pub fn into_components(self) -> Result<BTreeMap<String, SchemaObject>, Vec<SchemaConflict>> {
        if !self.conflicts.is_empty() {
            return Err(self.conflicts);
        }
        Ok(self
            .entries
            .into_iter()
            .filter_map(|(name, e)| e.definition.map(|d| (name, d)))
            .collect())
    }
}

/// Generate a self-contained schema for `T`, with named definitions under `$defs`.
///
/// Useful outside OpenAPI; `$ref`s point into `#/components/schemas/`, so this
/// helper rewrites nothing and is intended for inspection and tests.
pub fn schema_for<T: Schema + ?Sized + 'static>() -> (SchemaObject, SchemaRegistry) {
    let mut registry = SchemaRegistry::new();
    let root = registry.subschema::<T>();
    (root, registry)
}

macro_rules! primitive_schema {
    ($($t:ty => $ty:literal $(, $k:literal = $v:expr)*);* $(;)?) => {$(
        impl Schema for $t {
            fn schema(_: &mut SchemaRegistry) -> SchemaObject {
                SchemaObject::of_type($ty)$(.with($k, $v))*
            }
        }
    )*};
}

primitive_schema! {
    String => "string";
    str => "string";
    bool => "boolean";
    i8 => "integer", "format" = "int8";
    i16 => "integer", "format" = "int16";
    i32 => "integer", "format" = "int32";
    i64 => "integer", "format" = "int64";
    u8 => "integer", "minimum" = 0_i64, "maximum" = 255_i64;
    u16 => "integer", "minimum" = 0_i64, "maximum" = i64::from(u16::MAX);
    u32 => "integer", "minimum" = 0_i64, "maximum" = i64::from(u32::MAX);
    u64 => "integer", "minimum" = 0_i64;
    f32 => "number", "format" = "float";
    f64 => "number", "format" = "double";
    serde_json::Value => "object";
}

impl<T: Schema + ?Sized + 'static> Schema for &T {
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
        registry.subschema::<T>()
    }
}

impl<T: Schema + 'static> Schema for Option<T> {
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
        let inner = registry.subschema::<T>();
        SchemaObject::default().with(
            "anyOf",
            Value::Array(vec![
                inner.into_value(),
                SchemaObject::of_type("null").into_value(),
            ]),
        )
    }
}

impl<T: Schema + 'static> Schema for Vec<T> {
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("array").with("items", registry.subschema::<T>())
    }
}

impl<T: Schema + 'static> Schema for [T] {
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("array").with("items", registry.subschema::<T>())
    }
}

impl<T: Schema + ?Sized + 'static> Schema for Box<T> {
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
        registry.subschema::<T>()
    }
}

impl<T: Schema + 'static> Schema for BTreeMap<String, T> {
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("object").with("additionalProperties", registry.subschema::<T>())
    }
}

impl<T: Schema + 'static, S: 'static> Schema for std::collections::HashMap<String, T, S> {
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("object").with("additionalProperties", registry.subschema::<T>())
    }
}

macro_rules! tuple_schema {
    ($($t:ident),+) => {
        impl<$($t: Schema + 'static),+> Schema for ($($t,)+) {
            fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
                let items = vec![$(registry.subschema::<$t>().into_value()),+];
                let len = items.len() as u64;
                SchemaObject::of_type("array")
                    .with("prefixItems", Value::Array(items))
                    .with("minItems", len)
                    .with("maxItems", len)
            }
        }
    };
}
tuple_schema!(A);
tuple_schema!(A, B);
tuple_schema!(A, B, C);
tuple_schema!(A, B, C, D);

impl<const MIN: usize, const MAX: usize> Schema for ConstrainedString<MIN, MAX> {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("string")
            .with("minLength", MIN as u64)
            .with("maxLength", MAX as u64)
    }
}

impl<const MIN: i64, const MAX: i64> Schema for BoundedI64<MIN, MAX> {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("integer")
            .with("minimum", MIN)
            .with("maximum", MAX)
    }
}

impl Schema for PositiveInt {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("integer").with("exclusiveMinimum", 0_i64)
    }
}

impl Schema for Email {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("string").with("format", "email")
    }
}

impl Schema for SecretString {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("string").with("format", "password")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn inline<T: Schema + ?Sized + 'static>() -> Value {
        schema_for::<T>().0.into_value()
    }

    #[test]
    fn primitive_schemas() {
        assert_eq!(inline::<String>(), json!({"type": "string"}));
        assert_eq!(inline::<bool>(), json!({"type": "boolean"}));
        assert_eq!(
            inline::<i32>(),
            json!({"type": "integer", "format": "int32"})
        );
        assert_eq!(
            inline::<u8>(),
            json!({"type": "integer", "minimum": 0, "maximum": 255})
        );
        assert_eq!(
            inline::<u32>(),
            json!({"type": "integer", "minimum": 0, "maximum": 4294967295_u64})
        );
        assert_eq!(
            inline::<f64>(),
            json!({"type": "number", "format": "double"})
        );
    }

    #[test]
    fn containers() {
        assert_eq!(
            inline::<Option<String>>(),
            json!({"anyOf": [{"type": "string"}, {"type": "null"}]})
        );
        assert_eq!(
            inline::<Vec<bool>>(),
            json!({"type": "array", "items": {"type": "boolean"}})
        );
    }

    #[test]
    fn constrained_type_keywords() {
        assert_eq!(
            inline::<ConstrainedString<1, 16>>(),
            json!({"type": "string", "minLength": 1, "maxLength": 16})
        );
        assert_eq!(
            inline::<BoundedI64<0, 100>>(),
            json!({"type": "integer", "minimum": 0, "maximum": 100})
        );
        assert_eq!(
            inline::<PositiveInt>(),
            json!({"type": "integer", "exclusiveMinimum": 0})
        );
        assert_eq!(
            inline::<Email>(),
            json!({"type": "string", "format": "email"})
        );
        assert_eq!(
            inline::<SecretString>(),
            json!({"type": "string", "format": "password"})
        );
    }

    /// A named, recursive type: `Node { children: Vec<Node> }`.
    struct Node;
    impl Schema for Node {
        fn schema_name() -> Option<&'static str> {
            Some("Node")
        }
        fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
            SchemaObject::of_type("object").with(
                "properties",
                json!({ "children": registry.subschema::<Vec<Node>>() }),
            )
        }
    }

    struct OtherNode;
    impl Schema for OtherNode {
        fn schema_name() -> Option<&'static str> {
            Some("Node")
        }
        fn schema(_: &mut SchemaRegistry) -> SchemaObject {
            SchemaObject::of_type("string")
        }
    }

    #[test]
    fn named_types_are_referenced_once_and_recursion_terminates() {
        let mut registry = SchemaRegistry::new();
        let a = registry.subschema::<Node>();
        let b = registry.subschema::<Vec<Node>>();
        assert_eq!(a.into_value(), json!({"$ref": "#/components/schemas/Node"}));
        assert_eq!(
            b.into_value(),
            json!({"type": "array", "items": {"$ref": "#/components/schemas/Node"}})
        );
        let components = registry.into_components().unwrap();
        assert_eq!(components.len(), 1);
        assert_eq!(
            components["Node"].clone().into_value(),
            json!({"type": "object", "properties": {"children": {
                "type": "array", "items": {"$ref": "#/components/schemas/Node"}}}})
        );
    }

    #[test]
    fn name_conflicts_are_reported() {
        let mut registry = SchemaRegistry::new();
        registry.subschema::<Node>();
        registry.subschema::<OtherNode>();
        let err = registry.into_components().unwrap_err();
        assert_eq!(err.len(), 1);
        assert_eq!(err[0].name, "Node");
    }
}
