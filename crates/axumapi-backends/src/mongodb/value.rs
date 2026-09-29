//! Canonical mapping between [`Value`] and BSON.

use ::mongodb::bson::spec::BinarySubtype;
use ::mongodb::bson::{Binary, Bson, DateTime, Decimal128, Document};
use axumapi_orm::types::{DATE_FORMAT, TIME_FORMAT};
use axumapi_orm::{QueryError, Value};
use rust_decimal::Decimal;
use std::str::FromStr;
use uuid::Uuid;

/// Convert a [`Value`] to BSON (see the module docs of
/// [`mongodb`](crate::mongodb) for the table).
///
/// # Errors
/// [`QueryError::InvalidPlan`] for a decimal that Decimal128 cannot hold or a
/// JSON number outside the 64-bit range.
pub fn to_bson(value: &Value) -> Result<Bson, QueryError> {
    Ok(match value {
        Value::Null => Bson::Null,
        Value::Bool(b) => Bson::Boolean(*b),
        Value::Int(i) => Bson::Int64(*i),
        Value::Float(f) => Bson::Double(*f),
        Value::Decimal(d) => Bson::Decimal128(
            Decimal128::from_str(&d.to_string())
                .map_err(|e| QueryError::InvalidPlan(format!("decimal {d}: {e}")))?,
        ),
        Value::Text(s) => Bson::String(s.clone()),
        Value::Bytes(b) => Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: b.clone(),
        }),
        Value::Uuid(u) => Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: u.as_bytes().to_vec(),
        }),
        Value::Date(d) => Bson::String(d.format(DATE_FORMAT).to_string()),
        Value::Time(t) => Bson::String(t.format(TIME_FORMAT).to_string()),
        Value::Timestamp(t) => Bson::DateTime(DateTime::from_millis(t.timestamp_millis())),
        Value::Json(j) => json_to_bson(j)?,
    })
}

/// JSON objects and arrays are embedded; JSON scalars are stored as their
/// JSON text (`5`, `"x"`, `null`), so they decode back through
/// `DbType::from_value` for `serde_json::Value`.
fn json_to_bson(json: &serde_json::Value) -> Result<Bson, QueryError> {
    Ok(match json {
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => embed_json(json)?,
        scalar => Bson::String(scalar.to_string()),
    })
}

fn embed_json(json: &serde_json::Value) -> Result<Bson, QueryError> {
    Ok(match json {
        serde_json::Value::Null => Bson::Null,
        serde_json::Value::Bool(b) => Bson::Boolean(*b),
        serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Bson::Int64(i),
            (None, Some(f)) if !n.is_u64() => Bson::Double(f),
            _ => {
                return Err(QueryError::InvalidPlan(format!(
                    "JSON number {n} does not fit a BSON int64 or double"
                )));
            }
        },
        serde_json::Value::String(s) => Bson::String(s.clone()),
        serde_json::Value::Array(items) => {
            Bson::Array(items.iter().map(embed_json).collect::<Result<_, _>>()?)
        }
        serde_json::Value::Object(map) => {
            let mut doc = Document::new();
            for (k, v) in map {
                doc.insert(k.clone(), embed_json(v)?);
            }
            Bson::Document(doc)
        }
    })
}

/// Convert BSON read from the database to a [`Value`].
///
/// Documents and arrays become [`Value::Json`]; strings stay [`Value::Text`]
/// (dates and times decode from their canonical text through
/// `DbType::from_value`).
///
/// # Errors
/// [`QueryError::Decode`] for BSON types with no [`Value`] counterpart.
pub fn from_bson(bson: Bson, column: &str) -> Result<Value, QueryError> {
    let fail = |reason: String| QueryError::Decode {
        column: column.to_owned(),
        reason,
    };
    Ok(match bson {
        Bson::Null | Bson::Undefined => Value::Null,
        Bson::Boolean(b) => Value::Bool(b),
        Bson::Int32(i) => Value::Int(i64::from(i)),
        Bson::Int64(i) => Value::Int(i),
        Bson::Double(f) => Value::Float(f),
        Bson::Decimal128(d) => {
            let text = d.to_string();
            let parsed = Decimal::from_scientific(&text).or_else(|_| Decimal::from_str(&text));
            Value::Decimal(parsed.map_err(|e| fail(format!("decimal {text}: {e}")))?)
        }
        Bson::String(s) => Value::Text(s),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes,
        }) if bytes.len() == 16 => {
            Value::Uuid(Uuid::from_slice(&bytes).map_err(|e| fail(format!("invalid uuid: {e}")))?)
        }
        Bson::Binary(b) => Value::Bytes(b.bytes),
        Bson::DateTime(dt) => Value::Timestamp(
            chrono::DateTime::from_timestamp_millis(dt.timestamp_millis())
                .ok_or_else(|| fail("timestamp out of range".into()))?,
        ),
        Bson::ObjectId(oid) => Value::Text(oid.to_hex()),
        doc @ (Bson::Document(_) | Bson::Array(_)) => Value::Json(bson_to_json(doc)),
        other => {
            return Err(fail(format!(
                "unsupported BSON type {:?}",
                other.element_type()
            )));
        }
    })
}

fn bson_to_json(bson: Bson) -> serde_json::Value {
    use serde_json::Value as J;
    match bson {
        Bson::Null | Bson::Undefined => J::Null,
        Bson::Boolean(b) => J::Bool(b),
        Bson::Int32(i) => J::from(i),
        Bson::Int64(i) => J::from(i),
        Bson::Double(f) => serde_json::Number::from_f64(f).map_or(J::Null, J::Number),
        Bson::String(s) => J::String(s),
        Bson::Array(items) => J::Array(items.into_iter().map(bson_to_json).collect()),
        Bson::Document(doc) => {
            J::Object(doc.into_iter().map(|(k, v)| (k, bson_to_json(v))).collect())
        }
        Bson::DateTime(dt) => J::String(dt.try_to_rfc3339_string().unwrap_or_default()),
        Bson::Decimal128(d) => J::String(d.to_string()),
        Bson::ObjectId(oid) => J::String(oid.to_hex()),
        other => other.into_relaxed_extjson(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};

    fn round(value: Value) -> Value {
        from_bson(to_bson(&value).unwrap(), "c").unwrap()
    }

    #[test]
    fn scalars_round_trip() {
        assert_eq!(round(Value::Int(7)), Value::Int(7));
        assert_eq!(round(Value::Float(1.5)), Value::Float(1.5));
        assert_eq!(round(Value::Bool(true)), Value::Bool(true));
        assert_eq!(round(Value::Null), Value::Null);
        assert_eq!(round(Value::Text("$x".into())), Value::Text("$x".into()));
        assert_eq!(round(Value::Bytes(vec![1, 2])), Value::Bytes(vec![1, 2]));
        let u = Uuid::from_u128(42);
        assert_eq!(round(Value::Uuid(u)), Value::Uuid(u));
    }

    #[test]
    fn decimal_is_decimal128() {
        let d = Decimal::new(1250, 2);
        assert!(matches!(
            to_bson(&Value::Decimal(d)),
            Ok(Bson::Decimal128(_))
        ));
        assert_eq!(round(Value::Decimal(d)), Value::Decimal(d));
        let small = Decimal::from_str("-0.000001").unwrap();
        assert_eq!(round(Value::Decimal(small)), Value::Decimal(small));
    }

    #[test]
    fn uuid_is_binary_subtype_4() {
        let Ok(Bson::Binary(b)) = to_bson(&Value::Uuid(Uuid::nil())) else {
            panic!("not binary");
        };
        assert_eq!(b.subtype, BinarySubtype::Uuid);
        assert_eq!(b.bytes.len(), 16);
    }

    #[test]
    fn timestamps_truncate_to_milliseconds() {
        let t = Utc.timestamp_micros(1_700_000_000_123_456).unwrap();
        let expected = Utc.timestamp_millis_opt(1_700_000_000_123).unwrap();
        assert_eq!(round(Value::Timestamp(t)), Value::Timestamp(expected));
    }

    #[test]
    fn dates_and_times_are_canonical_text() {
        let d = NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
        assert_eq!(
            to_bson(&Value::Date(d)).unwrap(),
            Bson::String("2026-09-29".into())
        );
        let t = NaiveTime::from_hms_micro_opt(1, 2, 3, 4).unwrap();
        assert_eq!(
            to_bson(&Value::Time(t)).unwrap(),
            Bson::String("01:02:03.000004".into())
        );
    }

    #[test]
    fn json_documents_are_embedded_and_scalars_are_text() {
        let j = serde_json::json!({"a": [1, 2.5, null], "$k": {"b": true}});
        assert!(matches!(
            to_bson(&Value::Json(j.clone())).unwrap(),
            Bson::Document(_)
        ));
        assert_eq!(round(Value::Json(j.clone())), Value::Json(j));
        assert_eq!(
            to_bson(&Value::Json(serde_json::json!("x"))).unwrap(),
            Bson::String("\"x\"".into())
        );
        assert!(to_bson(&Value::Json(serde_json::json!([u64::MAX]))).is_err());
    }
}
