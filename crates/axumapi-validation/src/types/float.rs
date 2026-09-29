//! Bounded floating-point newtype. Bounds are a type, not const generics,
//! because `f64` cannot be a const generic parameter on stable Rust.

use std::marker::PhantomData;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Number, Value};

use crate::context::ValidationContext;
use crate::dump::Dump;
use crate::error::FieldError;
use crate::rules;
use crate::schema::{Schema, SchemaObject, SchemaRegistry};
use crate::validate::Validate;

/// Inclusive/exclusive bounds for [`BoundedFloat`].
pub trait FloatBounds {
    /// Lower bound; `None` means unbounded below.
    const MIN: Option<f64>;
    /// Upper bound; `None` means unbounded above.
    const MAX: Option<f64>;
    /// When `true`, values equal to [`MIN`](Self::MIN) are rejected.
    const EXCLUSIVE_MIN: bool = false;
    /// When `true`, values equal to [`MAX`](Self::MAX) are rejected.
    const EXCLUSIVE_MAX: bool = false;
}

/// Closed unit interval `0.0..=1.0`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UnitInterval;

impl FloatBounds for UnitInterval {
    const MIN: Option<f64> = Some(0.0);
    const MAX: Option<f64> = Some(1.0);
}

/// Non-negative floats (`>= 0.0`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NonNegative;

impl FloatBounds for NonNegative {
    const MIN: Option<f64> = Some(0.0);
    const MAX: Option<f64> = None;
}

/// Strictly positive floats (`> 0.0`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Positive;

impl FloatBounds for Positive {
    const MIN: Option<f64> = Some(0.0);
    const MAX: Option<f64> = None;
    const EXCLUSIVE_MIN: bool = true;
}

/// Finite `f64` constrained by `B`.
///
/// NaN and infinities are rejected (`finite_number`). Bound errors reuse
/// `greater_than` / `greater_than_equal` / `less_than` / `less_than_equal`.
///
/// Pydantic `confloat` takes numeric bounds as arguments; here they live on
/// a [`FloatBounds`] type because stable Rust has no `f64` const generics.
#[derive(Debug, PartialEq, PartialOrd)]
pub struct BoundedFloat<B>(f64, PhantomData<fn() -> B>);

impl<B> Clone for BoundedFloat<B> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B> Copy for BoundedFloat<B> {}

impl<B: FloatBounds> BoundedFloat<B> {
    /// Validate finiteness and bounds, then wrap `value`.
    pub fn new(value: f64) -> Result<Self, FieldError> {
        if !value.is_finite() {
            return Err(FieldError::new("finite_number", "expected a finite number"));
        }
        if let Some(min) = B::MIN {
            if B::EXCLUSIVE_MIN {
                rules::gt(&value, &min)?;
            } else {
                rules::ge(&value, &min)?;
            }
        }
        if let Some(max) = B::MAX {
            if B::EXCLUSIVE_MAX {
                rules::lt(&value, &max)?;
            } else {
                rules::le(&value, &max)?;
            }
        }
        Ok(Self(value, PhantomData))
    }
}

impl<B> BoundedFloat<B> {
    /// Unwrap the inner float.
    pub fn into_inner(self) -> f64 {
        self.0
    }
}

impl<B> AsRef<f64> for BoundedFloat<B> {
    fn as_ref(&self) -> &f64 {
        &self.0
    }
}

impl<B> std::ops::Deref for BoundedFloat<B> {
    type Target = f64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<B> std::fmt::Display for BoundedFloat<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<B> From<BoundedFloat<B>> for f64 {
    fn from(value: BoundedFloat<B>) -> Self {
        value.0
    }
}

impl<B: FloatBounds> TryFrom<f64> for BoundedFloat<B> {
    type Error = FieldError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<B> Serialize for BoundedFloat<B> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, B: FloatBounds> Deserialize<'de> for BoundedFloat<B> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl<B: FloatBounds> Validate for BoundedFloat<B> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        let before = ctx.error_count();
        f64::prepare(input, ctx);
        if ctx.error_count() != before {
            return;
        }
        let Some(value) = input.as_f64() else {
            return;
        };
        ctx.check(Self::new(value).map(|_| ()));
    }
}

fn f64_schema_value(value: f64) -> Value {
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

impl<B: FloatBounds> Schema for BoundedFloat<B> {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        let mut schema = SchemaObject::of_type("number").with("format", "double");
        if let Some(min) = B::MIN {
            let key = if B::EXCLUSIVE_MIN {
                "exclusiveMinimum"
            } else {
                "minimum"
            };
            schema = schema.with(key, f64_schema_value(min));
        }
        if let Some(max) = B::MAX {
            let key = if B::EXCLUSIVE_MAX {
                "exclusiveMaximum"
            } else {
                "maximum"
            };
            schema = schema.with(key, f64_schema_value(max));
        }
        schema
    }
}

impl<B: FloatBounds> Dump for BoundedFloat<B> {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::context::ValidationContext;
    use crate::dump::{Dump, DumpOptions};
    use crate::schema::schema_for;
    use crate::types::prepare_codes;
    use serde_json::json;

    type Unit = BoundedFloat<UnitInterval>;
    type Pos = BoundedFloat<Positive>;
    type NonNeg = BoundedFloat<NonNegative>;

    #[test]
    fn bounded_float_construction() {
        assert_eq!(Unit::new(0.0).unwrap().into_inner(), 0.0);
        assert_eq!(Unit::new(1.0).unwrap().into_inner(), 1.0);
        assert_eq!(Unit::new(-0.1).unwrap_err().code, "greater_than_equal");
        assert_eq!(Unit::new(1.1).unwrap_err().code, "less_than_equal");
        assert_eq!(Pos::new(0.0).unwrap_err().code, "greater_than");
        assert!(Pos::new(0.1).is_ok());
        assert!(NonNeg::new(0.0).is_ok());
        assert_eq!(Unit::new(f64::NAN).unwrap_err().code, "finite_number");
        assert_eq!(Unit::new(f64::INFINITY).unwrap_err().code, "finite_number");
    }

    #[test]
    fn bounded_float_serde_round_trip_and_reject() {
        let value = Unit::new(0.5).unwrap();
        let encoded = serde_json::to_string(&value).unwrap();
        assert_eq!(encoded, "0.5");
        let round: Unit = serde_json::from_str(&encoded).unwrap();
        assert_eq!(*round, 0.5);
        assert!(serde_json::from_str::<Unit>("1.5").is_err());
        assert!(serde_json::from_str::<Pos>("0").is_err());
    }

    #[test]
    fn bounded_float_prepare_lax_and_strict() {
        assert_eq!(
            prepare_codes::<Unit>(json!("0.25"), ValidationContext::new()),
            (json!(0.25), vec![])
        );
        assert_eq!(
            prepare_codes::<Unit>(json!("0.25"), ValidationContext::new().with_strict(true)).1,
            ["float_type"]
        );
        assert_eq!(
            prepare_codes::<Unit>(json!(1.5), ValidationContext::new()).1,
            ["less_than_equal"]
        );
        assert_eq!(
            prepare_codes::<Pos>(json!(0.0), ValidationContext::new()).1,
            ["greater_than"]
        );
        assert_eq!(
            prepare_codes::<Unit>(json!("nan"), ValidationContext::new()).1,
            ["float_parsing"]
        );
    }

    #[test]
    fn bounded_float_schema_dump_and_parse_value() {
        let unit = schema_for::<Unit>().0.into_value();
        assert_eq!(unit["type"], json!("number"));
        assert_eq!(unit["format"], json!("double"));
        assert_eq!(unit["minimum"], json!(0.0));
        assert_eq!(unit["maximum"], json!(1.0));

        let pos = schema_for::<Pos>().0.into_value();
        assert_eq!(pos["exclusiveMinimum"], json!(0.0));
        assert!(pos.get("minimum").is_none());

        let value = Unit::new(0.5).unwrap();
        assert_eq!(value.dump(&DumpOptions::new()).unwrap(), json!(0.5));
        let parsed = crate::parse_value::<Unit>(json!("0.5"), ValidationContext::new()).unwrap();
        assert_eq!(*parsed, 0.5);
        let err = crate::parse_value::<Unit>(json!(2.0), ValidationContext::new()).unwrap_err();
        assert_eq!(err.errors[0].code, "less_than_equal");
    }
}
