//! Mapping between Rust field types, SQL column types and [`Value`].
//!
//! [`DbType`] is implemented for every type a model field may have. The
//! `Model` derive uses [`DbType::SQL_TYPE`] and [`DbType::NULLABLE`] for
//! migration metadata and [`DbType::to_value`] / [`DbType::from_value`] for
//! writes and reads.
//!
//! # Canonical storage forms
//!
//! Backends without a native column type store a canonical form, and
//! `from_value` accepts both the native value and that form:
//!
//! | Rust type | Native `Value` | Canonical fallback |
//! |---|---|---|
//! | `bool` | `Bool` | `Int` 0/1 |
//! | `Decimal` | `Decimal` | `Text` (`"12.50"`), `Int`, `Float` |
//! | `Uuid` | `Uuid` | `Text` (hyphenated) or 16 `Bytes` |
//! | `NaiveDate` | `Date` | `Text` [`DATE_FORMAT`] |
//! | `NaiveTime` | `Time` | `Text` [`TIME_FORMAT`] |
//! | `DateTime<Utc>` | `Timestamp` | `Text` [`TIMESTAMP_FORMAT`] (fixed width, so text order = time order) |
//! | `TimeDelta` | `Int` (microseconds) | — |
//! | `IpAddr` | `Text` | — |
//! | `serde_json::Value` | `Json` | `Text` holding JSON |
//!
//! Integer and float types also accept `Decimal` (integral, respectively any
//! value), because PostgreSQL returns `numeric` for `SUM` and `AVG`. On
//! SQLite, declare `Decimal` columns `NUMERIC`: the canonical text form would
//! otherwise sort and aggregate as text.

use crate::error::QueryError;
use crate::value::Value;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use std::net::IpAddr;
use uuid::Uuid;

/// Text form of dates on backends without a native date type.
pub const DATE_FORMAT: &str = "%Y-%m-%d";
/// Text form of times of day.
pub const TIME_FORMAT: &str = "%H:%M:%S%.6f";
/// Text form of UTC timestamps (always 6 fractional digits and `Z`).
pub const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.6fZ";

/// Portable column type family. Backends map it to concrete DDL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SqlType {
    /// 16-bit integer.
    SmallInt,
    /// 32-bit integer.
    Integer,
    /// 64-bit integer.
    BigInt,
    /// 32-bit float.
    Real,
    /// 64-bit float.
    Double,
    /// Exact decimal; precision comes from `FieldMeta::max_digits`/`decimal_places`.
    Decimal,
    /// Boolean.
    Bool,
    /// Text; `VARCHAR(n)` when `FieldMeta::max_length` is set, else `TEXT`.
    Text,
    /// Binary data.
    Binary,
    /// Calendar date.
    Date,
    /// Time of day.
    Time,
    /// UTC timestamp.
    Timestamp,
    /// Duration stored as 64-bit microseconds.
    Duration,
    /// UUID.
    Uuid,
    /// JSON document (`jsonb` on PostgreSQL).
    Json,
    /// IP address stored as text.
    IpAddr,
}

/// A Rust type that can be stored in a model column.
pub trait DbType: Sized + Send + Sync + 'static {
    /// Column type family.
    const SQL_TYPE: SqlType;
    /// Whether the column accepts `NULL` (true only for `Option<T>`).
    const NULLABLE: bool = false;

    /// Convert to a bind value.
    fn to_value(&self) -> Value;

    /// Decode from a value read from any backend.
    ///
    /// # Errors
    /// A human-readable reason when `value` has the wrong shape; callers wrap
    /// it into [`QueryError::Decode`] with the column name.
    fn from_value(value: Value) -> Result<Self, String>;
}

/// Decode column `column` of `value` as `T`, naming the column on failure.
///
/// # Errors
/// [`QueryError::Decode`].
pub fn decode<T: DbType>(column: &str, value: Value) -> Result<T, QueryError> {
    T::from_value(value).map_err(|reason| QueryError::Decode {
        column: column.to_owned(),
        reason,
    })
}

fn mismatch(expected: &str, got: &Value) -> String {
    format!("expected {expected}, got {}", got.kind())
}

macro_rules! int_type {
    ($($t:ty => $sql:ident),*) => {$(
        impl DbType for $t {
            const SQL_TYPE: SqlType = SqlType::$sql;
            fn to_value(&self) -> Value {
                Value::Int(i64::from(*self))
            }
            fn from_value(value: Value) -> Result<Self, String> {
                let int = match value {
                    Value::Int(i) => i,
                    // PostgreSQL returns `numeric` for `SUM` / `AVG` of integers.
                    Value::Decimal(d) if d.is_integer() => {
                        i64::try_from(d).map_err(|_| format!("{d} is out of range"))?
                    }
                    other => return Err(mismatch("integer", &other)),
                };
                <$t>::try_from(int).map_err(|_| format!("{int} is out of range"))
            }
        }
    )*};
}
int_type!(i16 => SmallInt, i32 => Integer, i64 => BigInt);

impl DbType for f64 {
    const SQL_TYPE: SqlType = SqlType::Double;
    fn to_value(&self) -> Value {
        Value::Float(*self)
    }
    #[allow(clippy::cast_precision_loss)]
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Float(f) => Ok(f),
            Value::Int(i) => Ok(i as f64),
            Value::Decimal(d) => d
                .to_f64()
                .ok_or_else(|| format!("{d} is not a valid float")),
            other => Err(mismatch("float", &other)),
        }
    }
}

impl DbType for f32 {
    const SQL_TYPE: SqlType = SqlType::Real;
    fn to_value(&self) -> Value {
        Value::Float(f64::from(*self))
    }
    #[allow(clippy::cast_possible_truncation)]
    fn from_value(value: Value) -> Result<Self, String> {
        f64::from_value(value).map(|f| f as f32)
    }
}

impl DbType for bool {
    const SQL_TYPE: SqlType = SqlType::Bool;
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Bool(b) => Ok(b),
            Value::Int(0) => Ok(false),
            Value::Int(1) => Ok(true),
            other => Err(mismatch("boolean", &other)),
        }
    }
}

impl DbType for String {
    const SQL_TYPE: SqlType = SqlType::Text;
    fn to_value(&self) -> Value {
        Value::Text(self.clone())
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Text(s) => Ok(s),
            other => Err(mismatch("text", &other)),
        }
    }
}

impl DbType for Vec<u8> {
    const SQL_TYPE: SqlType = SqlType::Binary;
    fn to_value(&self) -> Value {
        Value::Bytes(self.clone())
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Bytes(b) => Ok(b),
            other => Err(mismatch("bytes", &other)),
        }
    }
}

impl DbType for Decimal {
    const SQL_TYPE: SqlType = SqlType::Decimal;
    fn to_value(&self) -> Value {
        Value::Decimal(*self)
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Decimal(d) => Ok(d),
            Value::Int(i) => Ok(Decimal::from(i)),
            Value::Float(f) => Decimal::try_from(f).map_err(|e| e.to_string()),
            Value::Text(s) => s.parse().map_err(|e| format!("invalid decimal {s:?}: {e}")),
            other => Err(mismatch("decimal", &other)),
        }
    }
}

impl DbType for Uuid {
    const SQL_TYPE: SqlType = SqlType::Uuid;
    fn to_value(&self) -> Value {
        Value::Uuid(*self)
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Uuid(u) => Ok(u),
            Value::Text(s) => Uuid::parse_str(&s).map_err(|e| format!("invalid uuid {s:?}: {e}")),
            Value::Bytes(b) => Uuid::from_slice(&b).map_err(|e| e.to_string()),
            other => Err(mismatch("uuid", &other)),
        }
    }
}

impl DbType for NaiveDate {
    const SQL_TYPE: SqlType = SqlType::Date;
    fn to_value(&self) -> Value {
        Value::Date(*self)
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Date(d) => Ok(d),
            Value::Text(s) => NaiveDate::parse_from_str(&s, DATE_FORMAT)
                .map_err(|e| format!("invalid date {s:?}: {e}")),
            other => Err(mismatch("date", &other)),
        }
    }
}

impl DbType for NaiveTime {
    const SQL_TYPE: SqlType = SqlType::Time;
    fn to_value(&self) -> Value {
        Value::Time(*self)
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Time(t) => Ok(t),
            Value::Text(s) => NaiveTime::parse_from_str(&s, TIME_FORMAT)
                .or_else(|_| NaiveTime::parse_from_str(&s, "%H:%M:%S"))
                .map_err(|e| format!("invalid time {s:?}: {e}")),
            other => Err(mismatch("time", &other)),
        }
    }
}

impl DbType for DateTime<Utc> {
    const SQL_TYPE: SqlType = SqlType::Timestamp;
    fn to_value(&self) -> Value {
        Value::Timestamp(*self)
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Timestamp(t) => Ok(t),
            Value::Text(s) => DateTime::parse_from_rfc3339(&s)
                .map(|t| t.with_timezone(&Utc))
                .or_else(|_| {
                    NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S%.f").map(|t| t.and_utc())
                })
                .map_err(|e| format!("invalid timestamp {s:?}: {e}")),
            other => Err(mismatch("timestamp", &other)),
        }
    }
}

impl DbType for TimeDelta {
    const SQL_TYPE: SqlType = SqlType::Duration;
    fn to_value(&self) -> Value {
        // Durations beyond ±292k years do not fit; saturate rather than panic.
        Value::Int(self.num_microseconds().unwrap_or(i64::MAX))
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Int(us) => Ok(TimeDelta::microseconds(us)),
            other => Err(mismatch("duration (microseconds)", &other)),
        }
    }
}

impl DbType for IpAddr {
    const SQL_TYPE: SqlType = SqlType::IpAddr;
    fn to_value(&self) -> Value {
        Value::Text(self.to_string())
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Text(s) => s.parse().map_err(|e| format!("invalid ip {s:?}: {e}")),
            other => Err(mismatch("ip address", &other)),
        }
    }
}

impl DbType for serde_json::Value {
    const SQL_TYPE: SqlType = SqlType::Json;
    fn to_value(&self) -> Value {
        Value::Json(self.clone())
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Json(j) => Ok(j),
            Value::Text(s) => serde_json::from_str(&s).map_err(|e| format!("invalid json: {e}")),
            other => Err(mismatch("json", &other)),
        }
    }
}

impl<T: DbType> DbType for Option<T> {
    const SQL_TYPE: SqlType = T::SQL_TYPE;
    const NULLABLE: bool = true;
    fn to_value(&self) -> Value {
        self.as_ref().map_or(Value::Null, DbType::to_value)
    }
    fn from_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Null => Ok(None),
            other => T::from_value(other).map(Some),
        }
    }
}

/// Canonical text for values a backend cannot store natively (see module docs).
///
/// Returns `None` for variants that are always bound natively.
pub fn canonical_text(value: &Value) -> Option<String> {
    Some(match value {
        Value::Decimal(d) => d.to_string(),
        Value::Uuid(u) => u.hyphenated().to_string(),
        Value::Date(d) => d.format(DATE_FORMAT).to_string(),
        Value::Time(t) => t.format(TIME_FORMAT).to_string(),
        Value::Timestamp(t) => t.format(TIMESTAMP_FORMAT).to_string(),
        Value::Json(j) => j.to_string(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: DbType + PartialEq + std::fmt::Debug>(v: T) {
        let native = v.to_value();
        let text = canonical_text(&native).map_or(native.clone(), Value::Text);
        assert_eq!(T::from_value(native).as_ref(), Ok(&v));
        assert_eq!(T::from_value(text).as_ref(), Ok(&v));
    }

    #[test]
    fn native_and_canonical_forms_round_trip() {
        round_trip(42_i64);
        round_trip(true);
        round_trip("x".to_owned());
        round_trip(Decimal::new(1250, 2));
        round_trip(Uuid::from_u128(7));
        round_trip(NaiveDate::from_ymd_opt(2026, 9, 29).unwrap_or_default());
        round_trip(NaiveTime::from_hms_micro_opt(1, 2, 3, 4).unwrap_or_default());
        round_trip(DateTime::from_timestamp_micros(1_700_000_000_123_456).unwrap_or_default());
        round_trip(TimeDelta::milliseconds(1500));
        round_trip(serde_json::json!({"a": [1]}));
        round_trip(Some(3_i32));
        round_trip(None::<String>);
    }

    #[test]
    fn sqlite_bool_and_range_errors() {
        assert_eq!(bool::from_value(Value::Int(1)), Ok(true));
        assert!(i16::from_value(Value::Int(1 << 20)).is_err());
        assert!(matches!(
            decode::<i64>("age", Value::Text("x".into())),
            Err(QueryError::Decode { column, .. }) if column == "age"
        ));
    }

    #[test]
    fn timestamp_text_orders_like_time() {
        let a = canonical_text(&Value::Timestamp(
            DateTime::from_timestamp(9, 0).unwrap_or_default(),
        ));
        let b = canonical_text(&Value::Timestamp(
            DateTime::from_timestamp(10, 5).unwrap_or_default(),
        ));
        assert!(a < b);
    }
}
