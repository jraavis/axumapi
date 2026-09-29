//! Backend-neutral scalar values used for bind parameters and decoded rows.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use uuid::Uuid;

/// A bindable, backend-neutral value.
///
/// Values are **always** sent to the database as bind parameters, never
/// interpolated into query text.
///
/// Backends without a native type for a variant store a canonical text form
/// (see `BACKENDS.md`); [`DbType::from_value`](crate::DbType::from_value)
/// accepts both the native and the text form, so decoding does not depend on
/// the backend.
///
/// `Value` serializes (untagged) for JSON output of dynamic rows. It does not
/// deserialize: an untagged `Text` and `Timestamp` would be ambiguous.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Value {
    /// SQL `NULL` / missing value.
    Null,
    /// Boolean.
    Bool(bool),
    /// 64-bit signed integer.
    Int(i64),
    /// 64-bit float.
    Float(f64),
    /// Exact decimal.
    Decimal(Decimal),
    /// UTF-8 text.
    Text(String),
    /// Raw bytes.
    Bytes(Vec<u8>),
    /// UUID.
    Uuid(Uuid),
    /// Calendar date.
    Date(NaiveDate),
    /// Time of day.
    Time(NaiveTime),
    /// Instant in UTC.
    Timestamp(DateTime<Utc>),
    /// Structured JSON document.
    Json(serde_json::Value),
}

impl Value {
    /// Whether this is [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Short name of the variant, for decode error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Decimal(_) => "decimal",
            Value::Text(_) => "text",
            Value::Bytes(_) => "bytes",
            Value::Uuid(_) => "uuid",
            Value::Date(_) => "date",
            Value::Time(_) => "time",
            Value::Timestamp(_) => "timestamp",
            Value::Json(_) => "json",
        }
    }
}

macro_rules! impl_from {
    ($($t:ty => $variant:ident $(as $cast:ty)?),* $(,)?) => {$(
        impl From<$t> for Value {
            fn from(v: $t) -> Self { Value::$variant(v $(as $cast)?) }
        }
    )*};
}

impl_from!(
    bool => Bool,
    i8 => Int as i64, i16 => Int as i64, i32 => Int as i64, i64 => Int,
    u8 => Int as i64, u16 => Int as i64, u32 => Int as i64,
    f32 => Float as f64, f64 => Float,
    String => Text, Vec<u8> => Bytes, serde_json::Value => Json,
    Decimal => Decimal, Uuid => Uuid, NaiveDate => Date, NaiveTime => Time,
    DateTime<Utc> => Timestamp,
);

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_owned())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Value::Null, Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert_eq!(Value::from(3_i32), Value::Int(3));
        assert_eq!(Value::from("a"), Value::Text("a".into()));
        assert_eq!(Value::from(None::<i64>), Value::Null);
        assert_eq!(Value::from(Some(true)), Value::Bool(true));
        assert_eq!(Value::from(Uuid::nil()).kind(), "uuid");
    }
}
