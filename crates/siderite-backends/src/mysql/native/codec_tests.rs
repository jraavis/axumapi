//! Lossy or unsupported wire conversions fail at the value boundary.

use super::*;
use chrono::DateTime;
use std::io::Error;
type Check = Result<(), Box<dyn std::error::Error>>;

#[test]
fn unsigned_range_and_boolean_metadata() -> Result<(), Error> {
    let column = Column::new(ColumnType::MYSQL_TYPE_LONGLONG);
    assert!(decode_cell(&column, Wire::UInt(u64::MAX)).is_err());
    let largest = decode_cell(&column, Wire::UInt(i64::MAX as u64));
    assert_eq!(largest.map_err(Error::other)?, Value::Int(i64::MAX),);
    let boolean = Column::new(ColumnType::MYSQL_TYPE_TINY);
    let boolean = boolean.with_column_length(1);
    let tiny = boolean.clone().with_column_length(4);
    assert_eq!(
        decode_cell(&boolean, Wire::Int(1)).map_err(Error::other)?,
        Value::Bool(true),
    );
    assert_eq!(
        decode_cell(&tiny, Wire::Int(1)).map_err(Error::other)?,
        Value::Int(1),
    );
    Ok(())
}

#[test]
fn clock_and_date_representability() {
    let clock = Column::new(ColumnType::MYSQL_TYPE_TIME);
    for wire in [
        Wire::Time(true, 0, 1, 0, 0, 0),
        Wire::Time(false, 1, 0, 0, 0, 0),
        Wire::Time(false, 0, 24, 0, 0, 0),
        Wire::Time(false, 0, 0, 0, 0, 1_000_000),
    ] {
        assert!(decode_cell(&clock, wire).is_err());
    }
    let date = Column::new(ColumnType::MYSQL_TYPE_DATE);
    assert!(decode_cell(&date, Wire::Date(0, 0, 0, 0, 0, 0, 0)).is_err());
    assert!(encode(Value::Date(NaiveDate::MIN)).is_err());
    assert!(encode(Value::Date(NaiveDate::MAX)).is_err());
}

#[test]
fn utc_uses_mysql_microsecond_precision() -> Check {
    let timestamp = DateTime::from_timestamp(1_700_000_000, 123_456_789)
        .ok_or_else(|| Error::other("invalid test timestamp"))?;
    let wire = encode(Value::Timestamp(timestamp))?;
    let column = Column::new(ColumnType::MYSQL_TYPE_DATETIME);
    let actual = decode_cell(&column, wire).map_err(Error::other)?;
    let expected = timestamp
        .with_nanosecond(123_456_000)
        .ok_or_else(|| Error::other("invalid test microseconds"))?;
    assert_eq!(actual, Value::Timestamp(expected));
    Ok(())
}

#[test]
fn charset_distinguishes_binary_and_text() -> Result<(), Error> {
    let binary = Column::new(ColumnType::MYSQL_TYPE_BLOB);
    let binary = binary.with_character_set(63);
    let text = binary.clone().with_character_set(45);
    let bytes = Wire::Bytes(vec![0xff, 0x00]);
    assert_eq!(
        decode_cell(&binary, bytes.clone()).map_err(Error::other)?,
        Value::Bytes(vec![0xff, 0x00]),
    );
    assert!(decode_cell(&text, bytes).is_err());
    let unsupported = Column::new(ColumnType::MYSQL_TYPE_BIT);
    assert!(decode_cell(&unsupported, Wire::Bytes(vec![1])).is_err());
    assert_eq!(
        decode_cell(&unsupported, Wire::NULL).map_err(Error::other)?,
        Value::Null,
    );
    Ok(())
}
