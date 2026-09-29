//! Arbitrary-precision decimal newtype wrapping [`rust_decimal::Decimal`].

use std::str::FromStr;

use rust_decimal::Decimal as RawDecimal;
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

use crate::context::ValidationContext;
use crate::dump::Dump;
use crate::error::FieldError;
use crate::rules;
use crate::schema::{Schema, SchemaObject, SchemaRegistry};
use crate::validate::{Validate, json_type};

/// Decimal number with at most `MAX_DIGITS` significant digits and at most
/// `DECIMAL_PLACES` digits after the decimal point.
///
/// **Why strings in strict mode.** JSON numbers are IEEE-754 floats in
/// `serde_json` (this crate does not enable `arbitrary_precision`), so values
/// such as `1.1` cannot be represented exactly. Strict mode therefore accepts
/// only JSON strings; lax mode also accepts numbers and converts them through
/// `f64`, which may round.
///
/// Trailing zeros are ignored when checking digits, matching Pydantic
/// (`1.20` satisfies `DECIMAL_PLACES = 1`). Unlike Pydantic, integer digits
/// are not additionally capped at `MAX_DIGITS - DECIMAL_PLACES`.
///
/// The JSON Schema is `{"type":"string","format":"decimal"}`. `format` is an
/// application hint (not a JSON Schema 2020-12 built-in); a `pattern` is
/// omitted because [`rust_decimal`] also accepts scientific notation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Decimal<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32>(RawDecimal);

/// Decimal constrained only by rust_decimal's maximum scale of 28.
pub type UnboundedDecimal = Decimal<28, 28>;

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> Decimal<MAX_DIGITS, DECIMAL_PLACES> {
    /// Validate digit constraints and wrap `value`.
    pub fn new(value: RawDecimal) -> Result<Self, FieldError> {
        rules::decimal_digits(value, MAX_DIGITS, DECIMAL_PLACES)?;
        Ok(Self(value))
    }

    /// Parse a decimal string and validate digit constraints.
    pub fn parse(value: &str) -> Result<Self, FieldError> {
        let parsed = RawDecimal::from_str(value)
            .map_err(|_| FieldError::new("decimal_parsing", "input is not a valid decimal"))?;
        Self::new(parsed)
    }
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> Decimal<MAX_DIGITS, DECIMAL_PLACES> {
    /// Unwrap the inner decimal.
    pub fn into_inner(self) -> RawDecimal {
        self.0
    }
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> AsRef<RawDecimal>
    for Decimal<MAX_DIGITS, DECIMAL_PLACES>
{
    fn as_ref(&self) -> &RawDecimal {
        &self.0
    }
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> std::ops::Deref
    for Decimal<MAX_DIGITS, DECIMAL_PLACES>
{
    type Target = RawDecimal;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> std::fmt::Display
    for Decimal<MAX_DIGITS, DECIMAL_PLACES>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> From<Decimal<MAX_DIGITS, DECIMAL_PLACES>>
    for RawDecimal
{
    fn from(value: Decimal<MAX_DIGITS, DECIMAL_PLACES>) -> Self {
        value.0
    }
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> TryFrom<RawDecimal>
    for Decimal<MAX_DIGITS, DECIMAL_PLACES>
{
    type Error = FieldError;

    fn try_from(value: RawDecimal) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> TryFrom<String>
    for Decimal<MAX_DIGITS, DECIMAL_PLACES>
{
    type Error = FieldError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> TryFrom<&str>
    for Decimal<MAX_DIGITS, DECIMAL_PLACES>
{
    type Error = FieldError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> From<Decimal<MAX_DIGITS, DECIMAL_PLACES>>
    for String
{
    fn from(value: Decimal<MAX_DIGITS, DECIMAL_PLACES>) -> Self {
        value.0.to_string()
    }
}

fn number_to_decimal(n: &Number) -> Result<RawDecimal, FieldError> {
    if let Some(i) = n.as_i64() {
        return Ok(RawDecimal::from(i));
    }
    if let Some(u) = n.as_u64() {
        return Ok(RawDecimal::from(u));
    }
    if let Some(f) = n.as_f64() {
        return RawDecimal::from_f64_retain(f)
            .ok_or_else(|| FieldError::new("decimal_parsing", "input is not a valid decimal"));
    }
    Err(FieldError::new(
        "decimal_parsing",
        "input is not a valid decimal",
    ))
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> Validate
    for Decimal<MAX_DIGITS, DECIMAL_PLACES>
{
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        if input.is_string() {
            if super::prepare_as_string(input, ctx) {
                ctx.check(Self::parse(input.as_str().unwrap_or_default()).map(|_| ()));
            }
            return;
        }
        if let Some(n) = input.as_number().cloned() {
            if ctx.is_strict() {
                let other = input.clone();
                ctx.error(
                    "decimal_type",
                    format!("expected a decimal string, got {}", json_type(&other)),
                );
                return;
            }
            match number_to_decimal(&n) {
                Ok(parsed) => {
                    ctx.check(Self::new(parsed).map(|_| ()));
                    *input = Value::String(parsed.to_string());
                }
                Err(error) => ctx.push(error),
            }
            return;
        }
        let other = input.clone();
        let expected = if ctx.is_strict() {
            "a decimal string"
        } else {
            "a decimal string or number"
        };
        ctx.error(
            "decimal_type",
            format!("expected {expected}, got {}", json_type(&other)),
        );
    }
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> Schema
    for Decimal<MAX_DIGITS, DECIMAL_PLACES>
{
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("string").with("format", "decimal")
    }
}

impl<const MAX_DIGITS: u32, const DECIMAL_PLACES: u32> Dump
    for Decimal<MAX_DIGITS, DECIMAL_PLACES>
{
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::context::ValidationContext;
    use crate::dump::{Dump, DumpOptions};
    use crate::schema::schema_for;
    use crate::types::prepare_codes;
    use serde_json::json;

    type Money = Decimal<5, 2>;

    #[test]
    fn decimal_construction() {
        let value = Money::parse("123.45").unwrap();
        assert_eq!(value.to_string(), "123.45");
        assert_eq!(Money::parse("1.20").unwrap().to_string(), "1.20");
        assert_eq!(
            Money::parse("1.234").unwrap_err().code,
            "decimal_max_places"
        );
        assert_eq!(
            Money::parse("12345.6").unwrap_err().code,
            "decimal_max_digits"
        );
        assert_eq!(Money::parse("nope").unwrap_err().code, "decimal_parsing");
        assert!(UnboundedDecimal::parse("123").is_ok());
    }

    #[test]
    fn decimal_serde_round_trip_and_reject() {
        let value = Money::parse("12.5").unwrap();
        let encoded = serde_json::to_string(&value).unwrap();
        assert_eq!(encoded, "\"12.5\"");
        let round: Money = serde_json::from_str(&encoded).unwrap();
        assert_eq!(round, value);
        assert!(serde_json::from_str::<Money>("\"1.234\"").is_err());
        assert!(serde_json::from_str::<Money>("1.25").is_err());
    }

    #[test]
    fn decimal_prepare_lax_and_strict() {
        let (out, codes) = prepare_codes::<Money>(json!("1.5"), ValidationContext::new());
        assert!(codes.is_empty());
        assert_eq!(out, json!("1.5"));

        let (out, codes) = prepare_codes::<Money>(json!(2), ValidationContext::new());
        assert!(codes.is_empty());
        assert_eq!(out, json!("2"));

        assert_eq!(
            prepare_codes::<Money>(json!(1.5), ValidationContext::new().with_strict(true)).1,
            ["decimal_type"]
        );
        assert_eq!(
            prepare_codes::<Money>(json!(true), ValidationContext::new()).1,
            ["decimal_type"]
        );
        assert_eq!(
            prepare_codes::<Money>(json!("nope"), ValidationContext::new()).1,
            ["decimal_parsing"]
        );
        assert_eq!(
            prepare_codes::<Money>(json!("1.234"), ValidationContext::new()).1,
            ["decimal_max_places"]
        );
    }

    #[test]
    fn decimal_schema_dump_and_parse_value() {
        assert_eq!(
            schema_for::<Money>().0.into_value(),
            json!({"type": "string", "format": "decimal"})
        );
        let value = Money::parse("3.14").unwrap();
        assert_eq!(value.dump(&DumpOptions::new()).unwrap(), json!("3.14"));
        let parsed = crate::parse_value::<Money>(json!(12), ValidationContext::new()).unwrap();
        assert_eq!(parsed.to_string(), "12");
        let parsed = crate::parse_value::<Money>(json!("9.9"), ValidationContext::new()).unwrap();
        assert_eq!(parsed.to_string(), "9.9");
        let err =
            crate::parse_value::<Money>(json!(1.5), ValidationContext::new().with_strict(true))
                .unwrap_err();
        assert_eq!(err.errors[0].code, "decimal_type");
    }
}
