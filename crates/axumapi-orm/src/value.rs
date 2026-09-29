//! Backend-neutral scalar values used for bind parameters and decoded rows.

use serde::{Deserialize, Serialize};

/// A bindable, backend-neutral value.
///
/// Values are **always** sent to the database as bind parameters, never
/// interpolated into query text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// UTF-8 text.
    Text(String),
    /// Raw bytes.
    Bytes(Vec<u8>),
    /// Structured JSON document.
    Json(serde_json::Value),
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
    }
}
