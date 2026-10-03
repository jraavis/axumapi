//! Canonical ORM values on mysql_async's prepared-statement protocol.

use chrono::{Datelike, NaiveDate, NaiveTime, Timelike};
use mysql_async::consts::ColumnType;
use mysql_async::{Column, Value as Wire};
use rust_decimal::Decimal;
use siderite_orm::{QueryError, Row, Value};

/// Bind an owned value without changing canonical storage.
///
/// Args:
///     value: ORM parameter in placeholder order.
///
/// Returns:
///     Native parameter, or an invalid representable date error.
pub(super) fn encode(value: Value) -> Result<Wire, QueryError> {
    Ok(match value {
        Value::Null => Wire::NULL,
        Value::Bool(value) => Wire::Int(i64::from(value)),
        Value::Int(value) => Wire::Int(value),
        Value::Float(value) => Wire::Double(value),
        Value::Decimal(value) => Wire::Bytes(value.to_string().into_bytes()),
        Value::Text(value) => Wire::Bytes(value.into_bytes()),
        Value::Bytes(value) => Wire::Bytes(value),
        Value::Uuid(value) => {
            let text = value.hyphenated().to_string();
            Wire::Bytes(text.into_bytes())
        }
        Value::Json(value) => Wire::Bytes(value.to_string().into_bytes()),
        Value::Date(value) => date(value, NaiveTime::MIN)?,
        Value::Timestamp(value) => date(value.date_naive(), value.time())?,
        Value::Time(value) => Wire::Time(
            false,
            0,
            value.hour() as u8,
            value.minute() as u8,
            value.second() as u8,
            value.nanosecond() / 1_000,
        ),
    })
}

fn date(date: NaiveDate, time: NaiveTime) -> Result<Wire, QueryError> {
    let year = u16::try_from(date.year()).map_err(|_| date_range())?;
    if year > 9999 {
        return Err(date_range());
    }
    Ok(Wire::Date(
        year,
        date.month() as u8,
        date.day() as u8,
        time.hour() as u8,
        time.minute() as u8,
        time.second() as u8,
        time.nanosecond() / 1_000,
    ))
}

/// Move native row cells into canonical values using column metadata.
///
/// Args:
///     row: Fully received native row.
///
/// Returns:
///     Decoded row, or an explicit unsupported/invalid value error.
pub(super) fn decode(mut row: mysql_async::Row) -> Result<Row, QueryError> {
    let columns = row.columns();
    let mut values = Vec::with_capacity(columns.len());
    for (index, column) in columns.iter().enumerate() {
        let name = column.name_str().into_owned();
        let cell = row
            .take::<Wire, _>(index)
            .ok_or_else(|| QueryError::Decode {
                column: name.clone(),
                reason: "missing native result cell".into(),
            })?;
        let decoded = decode_cell(column, cell);
        let value = decoded.map_err(|reason| decode_error(&name, reason))?;
        values.push((name, value));
    }
    Ok(Row::new(values))
}

fn decode_cell(column: &Column, cell: Wire) -> Result<Value, String> {
    use ColumnType::*;
    if cell == Wire::NULL {
        return Ok(Value::Null);
    }
    match column.column_type() {
        MYSQL_TYPE_TINY if column.column_length() == 1 => {
            integer(cell).map(|value| Value::Bool(value != 0))
        }
        kind if numeric(kind) => integer(cell).map(Value::Int),
        MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => match cell {
            Wire::Float(value) => Ok(Value::Float(f64::from(value))),
            Wire::Double(value) => Ok(Value::Float(value)),
            _ => Err("invalid native floating-point cell".into()),
        },
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => {
            let value = text(cell)?;
            value.parse::<Decimal>().map(Value::Decimal).map_err(reason)
        }
        MYSQL_TYPE_JSON => {
            let value = text(cell)?;
            serde_json::from_str(&value)
                .map(Value::Json)
                .map_err(reason)
        }
        MYSQL_TYPE_STRING
        | MYSQL_TYPE_VAR_STRING
        | MYSQL_TYPE_VARCHAR
        | MYSQL_TYPE_TINY_BLOB
        | MYSQL_TYPE_BLOB
        | MYSQL_TYPE_MEDIUM_BLOB
        | MYSQL_TYPE_LONG_BLOB
        | MYSQL_TYPE_ENUM
        | MYSQL_TYPE_SET => {
            if column.character_set() == 63 {
                match cell {
                    Wire::Bytes(value) => Ok(Value::Bytes(value)),
                    _ => Err("invalid native binary cell".into()),
                }
            } else {
                text(cell).map(Value::Text)
            }
        }
        MYSQL_TYPE_DATE => datetime(cell).map(|value| Value::Date(value.0)),
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_TIMESTAMP => {
            let (date, time) = datetime(cell)?;
            Ok(Value::Timestamp(date.and_time(time).and_utc()))
        }
        MYSQL_TYPE_TIME => match cell {
            Wire::Time(false, 0, h, m, s, micros) => {
                let clock = time(h, m, s, micros)?;
                Ok(Value::Time(clock))
            }
            _ => Err("MySQL duration is not a clock time".into()),
        },
        other => Err(format!("unsupported MySQL type {other:?}")),
    }
}

fn integer(cell: Wire) -> Result<i64, String> {
    match cell {
        Wire::Int(value) => Ok(value),
        Wire::UInt(value) => i64::try_from(value).map_err(|_| {
            let message = format!("{value} does not fit a signed integer");
            message
        }),
        _ => Err("invalid native integer cell".into()),
    }
}

fn numeric(kind: ColumnType) -> bool {
    use ColumnType::*;
    [
        MYSQL_TYPE_TINY,
        MYSQL_TYPE_SHORT,
        MYSQL_TYPE_LONG,
        MYSQL_TYPE_LONGLONG,
        MYSQL_TYPE_INT24,
        MYSQL_TYPE_YEAR,
    ]
    .contains(&kind)
}

fn text(cell: Wire) -> Result<String, String> {
    match cell {
        Wire::Bytes(bytes) => String::from_utf8(bytes).map_err(reason),
        _ => Err("invalid native text cell".into()),
    }
}

fn datetime(cell: Wire) -> Result<(NaiveDate, NaiveTime), String> {
    let Wire::Date(year, month, day, h, m, s, micros) = cell else {
        return Err("invalid native date cell".into());
    };
    let year = i32::from(year);
    let (month, day) = (u32::from(month), u32::from(day));
    let date = NaiveDate::from_ymd_opt(year, month, day)
        .ok_or_else(|| "invalid or zero MySQL date".to_owned())?;
    Ok((date, time(h, m, s, micros)?))
}

fn time(h: u8, m: u8, s: u8, micros: u32) -> Result<NaiveTime, String> {
    let (h, m, s) = (u32::from(h), u32::from(m), u32::from(s));
    NaiveTime::from_hms_micro_opt(h, m, s, micros).ok_or_else(invalid_time)
}

fn reason(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn invalid_time() -> String {
    "invalid MySQL clock time".into()
}

fn date_range() -> QueryError {
    QueryError::InvalidPlan("date year is outside MySQL range".into())
}

fn decode_error(column: &str, reason: String) -> QueryError {
    QueryError::Decode {
        column: column.into(),
        reason,
    }
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
