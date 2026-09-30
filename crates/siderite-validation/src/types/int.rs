//! Signed integer newtypes that complement [`super::PositiveInt`].

use serde::{Deserialize, Serialize};

use crate::context::ValidationContext;
use crate::dump::Dump;
use crate::error::FieldError;
use crate::rules;
use crate::schema::{Schema, SchemaObject, SchemaRegistry};
use crate::validate::Validate;
use serde_json::Value;

/// Non-negative `i64` (`value >= 0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct NonNegativeInt(i64);

impl NonNegativeInt {
    /// Validate `value >= 0` and wrap it.
    pub fn new(value: i64) -> Result<Self, FieldError> {
        rules::ge(&value, &0)?;
        Ok(Self(value))
    }
}

crate::types::impl_wrapper!(NonNegativeInt => i64);

impl TryFrom<i64> for NonNegativeInt {
    type Error = FieldError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Validate for NonNegativeInt {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        let before = ctx.error_count();
        i64::prepare(input, ctx);
        if ctx.error_count() == before {
            ctx.check(rules::ge(&input.as_i64().unwrap_or_default(), &0));
        }
    }
}

impl Schema for NonNegativeInt {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("integer").with("minimum", 0_i64)
    }
}

impl Dump for NonNegativeInt {}

/// Strictly negative `i64` (`value < 0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct NegativeInt(i64);

impl NegativeInt {
    /// Validate `value < 0` and wrap it.
    pub fn new(value: i64) -> Result<Self, FieldError> {
        rules::lt(&value, &0)?;
        Ok(Self(value))
    }
}

crate::types::impl_wrapper!(NegativeInt => i64);

impl TryFrom<i64> for NegativeInt {
    type Error = FieldError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Validate for NegativeInt {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        let before = ctx.error_count();
        i64::prepare(input, ctx);
        if ctx.error_count() == before {
            ctx.check(rules::lt(&input.as_i64().unwrap_or_default(), &0));
        }
    }
}

impl Schema for NegativeInt {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("integer").with("exclusiveMaximum", 0_i64)
    }
}

impl Dump for NegativeInt {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::context::ValidationContext;
    use crate::dump::{Dump, DumpOptions};
    use crate::schema::schema_for;
    use crate::types::prepare_codes;
    use serde_json::json;

    #[test]
    fn non_negative_int_construction() {
        assert_eq!(NonNegativeInt::new(0).unwrap().into_inner(), 0);
        assert_eq!(NonNegativeInt::new(3).unwrap().into_inner(), 3);
        assert_eq!(
            NonNegativeInt::new(-1).unwrap_err().code,
            "greater_than_equal"
        );
    }

    #[test]
    fn negative_int_construction() {
        assert_eq!(NegativeInt::new(-1).unwrap().into_inner(), -1);
        assert_eq!(NegativeInt::new(0).unwrap_err().code, "less_than");
        assert_eq!(NegativeInt::new(2).unwrap_err().code, "less_than");
    }

    #[test]
    fn int_serde_round_trip_and_reject() {
        let value: NonNegativeInt = serde_json::from_str("0").unwrap();
        assert_eq!(serde_json::to_string(&value).unwrap(), "0");
        assert!(serde_json::from_str::<NonNegativeInt>("-1").is_err());
        let value: NegativeInt = serde_json::from_str("-4").unwrap();
        assert_eq!(serde_json::to_string(&value).unwrap(), "-4");
        assert!(serde_json::from_str::<NegativeInt>("0").is_err());
    }

    #[test]
    fn int_prepare_lax_and_strict() {
        assert_eq!(
            prepare_codes::<NonNegativeInt>(json!("7"), ValidationContext::new()),
            (json!(7), vec![])
        );
        assert_eq!(
            prepare_codes::<NonNegativeInt>(json!("7"), ValidationContext::new().with_strict(true))
                .1,
            ["int_type"]
        );
        assert_eq!(
            prepare_codes::<NonNegativeInt>(json!(-1), ValidationContext::new()).1,
            ["greater_than_equal"]
        );
        assert_eq!(
            prepare_codes::<NegativeInt>(json!("-2"), ValidationContext::new()),
            (json!(-2), vec![])
        );
        assert_eq!(
            prepare_codes::<NegativeInt>(json!(0), ValidationContext::new()).1,
            ["less_than"]
        );
    }

    #[test]
    fn int_schema_dump_and_parse_value() {
        assert_eq!(
            schema_for::<NonNegativeInt>().0.into_value(),
            json!({"type": "integer", "minimum": 0})
        );
        assert_eq!(
            schema_for::<NegativeInt>().0.into_value(),
            json!({"type": "integer", "exclusiveMaximum": 0})
        );
        let n = NonNegativeInt::new(4).unwrap();
        assert_eq!(n.dump(&DumpOptions::new()).unwrap(), json!(4));
        let parsed =
            crate::parse_value::<NegativeInt>(json!("-9"), ValidationContext::new()).unwrap();
        assert_eq!(parsed.into_inner(), -9);
        let err =
            crate::parse_value::<NonNegativeInt>(json!(-3), ValidationContext::new()).unwrap_err();
        assert_eq!(err.errors[0].code, "greater_than_equal");
    }
}
