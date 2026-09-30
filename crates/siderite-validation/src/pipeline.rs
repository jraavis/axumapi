//! The complete validation pipeline: `prepare` → deserialize → `validate`.

use crate::context::ValidationContext;
use crate::error::{FieldError, LocationItem, ValidationError, ValidationResult};
use crate::validate::Validate;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// Validate raw input and build a `T` (Pydantic `model_validate`).
///
/// Every problem found while preparing the input is reported at once. If
/// preparation succeeds but deserialization still fails (a type whose
/// `prepare` is less strict than its `Deserialize`), that single error is
/// reported with its path.
///
/// # Errors
/// Returns all collected [`ValidationError`]s.
pub fn parse_value<T>(mut input: Value, mut ctx: ValidationContext) -> ValidationResult<T>
where
    T: DeserializeOwned + Validate,
{
    T::prepare(&mut input, &mut ctx);
    if ctx.has_errors() {
        return Err(ctx.take_errors());
    }
    let value: T = match serde_path_to_error::deserialize(input) {
        Ok(value) => value,
        Err(err) => {
            let location = err.path().iter().filter_map(segment_location).collect();
            let mut error = FieldError::new("invalid", err.inner().to_string());
            error.location = location;
            ctx.push(error);
            return Err(ctx.take_errors());
        }
    };
    value.validate(&mut ctx);
    if ctx.has_errors() {
        Err(ctx.take_errors())
    } else {
        Ok(value)
    }
}

/// Parse JSON text through the pipeline.
///
/// # Errors
/// A `json_invalid` error for malformed JSON, otherwise as [`parse_value`].
pub fn parse_json<T>(text: &str, ctx: ValidationContext) -> ValidationResult<T>
where
    T: DeserializeOwned + Validate,
{
    match serde_json::from_str::<Value>(text) {
        Ok(value) => parse_value(value, ctx),
        Err(err) => Err(FieldError::new("json_invalid", err.to_string()).into()),
    }
}

/// Run only the post-deserialization checks on an existing value.
///
/// # Errors
/// Returns every violation found.
pub fn validate<T: Validate + ?Sized>(value: &T) -> ValidationResult<()> {
    let mut ctx = ValidationContext::new();
    value.validate(&mut ctx);
    ctx.take_errors().into_result()
}

fn segment_location(segment: &serde_path_to_error::Segment) -> Option<LocationItem> {
    use serde_path_to_error::Segment;
    match segment {
        Segment::Seq { index } => Some(LocationItem::index(*index)),
        Segment::Map { key } => Some(LocationItem::key(key.clone())),
        Segment::Enum { variant } => Some(LocationItem::key(variant.clone())),
        Segment::Unknown => None,
    }
}

/// Convert text key/value pairs (query string, form) into a JSON object.
/// Repeated keys become arrays, so `Vec` fields receive every value.
pub fn text_pairs_to_value<I, K, V>(pairs: I) -> Value
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<String>,
    V: Into<String>,
{
    let mut map = Map::new();
    for (key, value) in pairs {
        let value = Value::String(value.into());
        match map.entry(key.into()) {
            serde_json::map::Entry::Vacant(slot) => {
                slot.insert(value);
            }
            serde_json::map::Entry::Occupied(mut slot) => match slot.get_mut() {
                Value::Array(items) => items.push(value),
                existing => {
                    let first = existing.take();
                    *existing = Value::Array(vec![first, value]);
                }
            },
        }
    }
    Value::Object(map)
}

/// Convenience: turn a [`ValidationError`] into its JSON body.
pub fn error_value(error: &ValidationError) -> Value {
    serde_json::to_value(error).unwrap_or(Value::Null)
}
