//! Length-constrained vector newtype.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::context::ValidationContext;
use crate::dump::{Dump, DumpError, DumpOptions};
use crate::error::FieldError;
use crate::schema::{Schema, SchemaObject, SchemaRegistry};
use crate::validate::Validate;

/// A `Vec<T>` whose length is in `MIN..=MAX`.
///
/// Pydantic `conlist` is a factory; this is a const-generic newtype. Length
/// errors use `too_short` / `too_long` (the same codes Pydantic uses for
/// collections, not `string_too_short` / `string_too_long`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConstrainedVec<T, const MIN: usize, const MAX: usize>(Vec<T>);

fn check_len<const MIN: usize, const MAX: usize>(len: usize) -> Result<(), FieldError> {
    const { assert!(MIN <= MAX, "ConstrainedVec MIN must be <= MAX") };
    if len < MIN {
        Err(FieldError::new(
            "too_short",
            format!("ensure this value has at least {MIN} items"),
        ))
    } else if len > MAX {
        Err(FieldError::new(
            "too_long",
            format!("ensure this value has at most {MAX} items"),
        ))
    } else {
        Ok(())
    }
}

impl<T, const MIN: usize, const MAX: usize> ConstrainedVec<T, MIN, MAX> {
    /// Validate `value`'s length and wrap it.
    pub fn new(value: Vec<T>) -> Result<Self, FieldError> {
        check_len::<MIN, MAX>(value.len())?;
        Ok(Self(value))
    }
}

impl<T, const MIN: usize, const MAX: usize> ConstrainedVec<T, MIN, MAX> {
    /// Unwrap the inner vector.
    pub fn into_inner(self) -> Vec<T> {
        self.0
    }
}

impl<T, const MIN: usize, const MAX: usize> AsRef<[T]> for ConstrainedVec<T, MIN, MAX> {
    fn as_ref(&self) -> &[T] {
        &self.0
    }
}

impl<T, const MIN: usize, const MAX: usize> AsRef<Vec<T>> for ConstrainedVec<T, MIN, MAX> {
    fn as_ref(&self) -> &Vec<T> {
        &self.0
    }
}

impl<T, const MIN: usize, const MAX: usize> std::ops::Deref for ConstrainedVec<T, MIN, MAX> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, const MIN: usize, const MAX: usize> TryFrom<Vec<T>> for ConstrainedVec<T, MIN, MAX> {
    type Error = FieldError;

    fn try_from(value: Vec<T>) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<T, const MIN: usize, const MAX: usize> From<ConstrainedVec<T, MIN, MAX>> for Vec<T> {
    fn from(value: ConstrainedVec<T, MIN, MAX>) -> Self {
        value.0
    }
}

impl<T: Serialize, const MIN: usize, const MAX: usize> Serialize for ConstrainedVec<T, MIN, MAX> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>, const MIN: usize, const MAX: usize> Deserialize<'de>
    for ConstrainedVec<T, MIN, MAX>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Vec::<T>::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

impl<T: Validate, const MIN: usize, const MAX: usize> Validate for ConstrainedVec<T, MIN, MAX> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        Vec::<T>::prepare(input, ctx);
        if let Some(items) = input.as_array() {
            ctx.check(check_len::<MIN, MAX>(items.len()));
        }
    }

    fn validate(&self, ctx: &mut ValidationContext) {
        self.0.validate(ctx);
    }
}

impl<T: Schema + 'static, const MIN: usize, const MAX: usize> Schema
    for ConstrainedVec<T, MIN, MAX>
{
    fn schema(registry: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("array")
            .with("items", registry.subschema::<T>())
            .with("minItems", MIN as u64)
            .with("maxItems", MAX as u64)
    }
}

impl<T: Dump, const MIN: usize, const MAX: usize> Dump for ConstrainedVec<T, MIN, MAX> {
    fn dump(&self, opts: &DumpOptions) -> Result<Value, DumpError> {
        self.0.dump(opts)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::context::ValidationContext;
    use crate::dump::{Dump, DumpOptions};
    use crate::schema::schema_for;
    use crate::types::{SecretString, prepare_codes};
    use serde_json::json;

    type Pair = ConstrainedVec<i32, 2, 3>;

    #[test]
    fn constrained_vec_construction() {
        assert_eq!(Pair::new(vec![1, 2]).unwrap().into_inner(), vec![1, 2]);
        assert!(Pair::new(vec![1, 2, 3]).is_ok());
        assert_eq!(Pair::new(vec![1]).unwrap_err().code, "too_short");
        assert_eq!(Pair::new(vec![1, 2, 3, 4]).unwrap_err().code, "too_long");
        assert_eq!(Pair::new(vec![1, 2]).unwrap().len(), 2);
    }

    #[test]
    fn constrained_vec_serde_round_trip_and_reject() {
        let value = Pair::new(vec![1, 2]).unwrap();
        let encoded = serde_json::to_string(&value).unwrap();
        assert_eq!(encoded, "[1,2]");
        let round: Pair = serde_json::from_str(&encoded).unwrap();
        assert_eq!(&*round, &[1, 2]);
        assert!(serde_json::from_str::<Pair>("[1]").is_err());
        assert!(serde_json::from_str::<Pair>("[1,2,3,4]").is_err());
    }

    #[test]
    fn constrained_vec_prepare_lax_and_strict() {
        let (out, codes) = prepare_codes::<Pair>(json!([1, 2]), ValidationContext::new());
        assert!(codes.is_empty());
        assert_eq!(out, json!([1, 2]));
        assert_eq!(
            prepare_codes::<Pair>(json!([1]), ValidationContext::new()).1,
            ["too_short"]
        );
        assert_eq!(
            prepare_codes::<Pair>(json!(1), ValidationContext::new()).1,
            ["list_type"]
        );
        assert_eq!(
            prepare_codes::<Pair>(json!(1), ValidationContext::new().with_strict(true)).1,
            ["list_type"]
        );
        // Text input wraps a scalar into a one-item list, then length fails.
        let (out, codes) =
            prepare_codes::<Pair>(json!("3"), crate::context::ValidationContext::for_text());
        assert_eq!(out, json!([3]));
        assert_eq!(codes, ["too_short"]);
    }

    #[test]
    fn constrained_vec_schema_dump_and_parse_value() {
        assert_eq!(
            schema_for::<Pair>().0.into_value(),
            json!({"type": "array", "items": {"type": "integer", "format": "int32"}, "minItems": 2, "maxItems": 3})
        );
        let value = Pair::new(vec![1, 2, 3]).unwrap();
        assert_eq!(value.dump(&DumpOptions::new()).unwrap(), json!([1, 2, 3]));

        type Secrets = ConstrainedVec<SecretString, 1, 2>;
        let secrets = Secrets::new(vec![SecretString::new("hunter2").unwrap()]).unwrap();
        assert_eq!(
            secrets.dump(&DumpOptions::new()).unwrap(),
            json!(["**********"])
        );

        let parsed = crate::parse_value::<Pair>(json!([9, 8]), ValidationContext::new()).unwrap();
        assert_eq!(&*parsed, &[9, 8]);
        let err = crate::parse_value::<Pair>(json!([]), ValidationContext::new()).unwrap_err();
        assert_eq!(err.errors[0].code, "too_short");
    }

    #[test]
    fn constrained_vec_prepare_checks_elements_and_length() {
        type Names = ConstrainedVec<String, 1, 2>;
        let (out, codes) = prepare_codes::<Names>(json!([1, "ok", "x"]), ValidationContext::new());
        assert_eq!(out, json!([1, "ok", "x"]));
        assert_eq!(codes, ["string_type", "too_long"]);
    }
}
