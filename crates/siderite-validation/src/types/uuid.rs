//! UUID newtype wrapping [`uuid::Uuid`].

use serde::{Deserialize, Serialize};

use crate::context::ValidationContext;
use crate::dump::Dump;
use crate::error::FieldError;
use crate::rules;
use crate::schema::{Schema, SchemaObject, SchemaRegistry};
use crate::validate::Validate;
use serde_json::Value;

/// An RFC 4122 UUID of any version.
///
/// Unlike Pydantic `UUID4`, this does not require version 4. Input is a
/// hyphenated (`8-4-4-4-12`) or simple (32 hex digits) string; matching is
/// case-insensitive. URN and braced forms are rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Uuid(::uuid::Uuid);

impl Uuid {
    /// Parse `value` as a hyphenated or simple UUID.
    pub fn new(value: impl AsRef<str>) -> Result<Self, FieldError> {
        rules::parse_uuid(value.as_ref()).map(Self)
    }
}

crate::types::impl_wrapper!(Uuid => ::uuid::Uuid);

impl TryFrom<String> for Uuid {
    type Error = FieldError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for Uuid {
    type Error = FieldError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Uuid> for String {
    fn from(value: Uuid) -> Self {
        value.0.to_string()
    }
}

impl Validate for Uuid {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        if super::prepare_as_string(input, ctx) {
            ctx.check(Self::new(input.as_str().unwrap_or_default()).map(|_| ()));
        }
    }
}

impl Schema for Uuid {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("string").with("format", "uuid")
    }
}

impl Dump for Uuid {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::context::ValidationContext;
    use crate::dump::{Dump, DumpOptions};
    use crate::schema::schema_for;
    use crate::types::prepare_codes;
    use serde_json::json;

    const HYPHENATED: &str = "550e8400-e29b-41d4-a716-446655440000";
    const SIMPLE: &str = "550e8400e29b41d4a716446655440000";

    #[test]
    fn uuid_construction() {
        let uuid = Uuid::new(HYPHENATED).unwrap();
        assert_eq!(uuid.to_string(), HYPHENATED);
        assert_eq!(Uuid::new(SIMPLE).unwrap().to_string(), HYPHENATED);
        assert!(Uuid::new("550E8400-E29B-41D4-A716-446655440000").is_ok());
        assert_eq!(Uuid::new("not-a-uuid").unwrap_err().code, "uuid_parsing");
        assert_eq!(
            Uuid::new("urn:uuid:550e8400-e29b-41d4-a716-446655440000")
                .unwrap_err()
                .code,
            "uuid_parsing"
        );
    }

    #[test]
    fn uuid_serde_round_trip_and_reject() {
        let uuid = Uuid::new(SIMPLE).unwrap();
        let encoded = serde_json::to_string(&uuid).unwrap();
        assert_eq!(encoded, format!("\"{HYPHENATED}\""));
        let round: Uuid = serde_json::from_str(&encoded).unwrap();
        assert_eq!(round, uuid);
        assert!(serde_json::from_str::<Uuid>("\"nope\"").is_err());
    }

    #[test]
    fn uuid_prepare_lax_and_strict() {
        let (out, codes) = prepare_codes::<Uuid>(json!(HYPHENATED), ValidationContext::new());
        assert!(codes.is_empty());
        assert_eq!(out, json!(HYPHENATED));
        assert_eq!(
            prepare_codes::<Uuid>(json!(1), ValidationContext::new()).1,
            ["string_type"]
        );
        assert_eq!(
            prepare_codes::<Uuid>(json!(1), ValidationContext::new().with_strict(true)).1,
            ["string_type"]
        );
        assert_eq!(
            prepare_codes::<Uuid>(json!("nope"), ValidationContext::new()).1,
            ["uuid_parsing"]
        );
    }

    #[test]
    fn uuid_schema_dump_and_parse_value() {
        assert_eq!(
            schema_for::<Uuid>().0.into_value(),
            json!({"type": "string", "format": "uuid"})
        );
        let uuid = Uuid::new(HYPHENATED).unwrap();
        assert_eq!(uuid.dump(&DumpOptions::new()).unwrap(), json!(HYPHENATED));
        let parsed = crate::parse_value::<Uuid>(json!(SIMPLE), ValidationContext::new()).unwrap();
        assert_eq!(parsed.to_string(), HYPHENATED);
        let err = crate::parse_value::<Uuid>(json!("nope"), ValidationContext::new()).unwrap_err();
        assert_eq!(err.errors[0].code, "uuid_parsing");
    }
}
