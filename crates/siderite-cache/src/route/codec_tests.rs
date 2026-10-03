//! Retained freshness and bounded versioned encoding checks.

use super::{CachedResponse, decode, encode};
use http::{HeaderMap, HeaderValue, header};
use std::time::{Duration, SystemTime};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn origin_date_age_and_residence_are_preserved() -> TestResult {
    let mut headers = HeaderMap::new();
    let origin_time = SystemTime::now() - Duration::from_secs(30);
    let date = httpdate::fmt_http_date(origin_time);
    headers.insert(header::DATE, HeaderValue::from_str(&date)?);
    headers.insert(header::AGE, HeaderValue::from_static("10"));
    let encoded = encode(&headers, b"hello")?;
    let mut stored: CachedResponse = serde_json::from_slice(&encoded)?;
    stored.stored_at = stored.stored_at.saturating_sub(7);
    let bytes = serde_json::to_vec(&stored)?;
    let restored = decode(&bytes, 8192, 1024).ok_or("decode failed")?;
    let (restored, body) = restored;
    assert_eq!(body, b"hello");
    assert_eq!(restored.get(header::DATE), headers.get(header::DATE));
    let age = restored
        .get(header::AGE)
        .ok_or("missing age")?
        .to_str()?
        .parse::<u64>()?;
    assert!(age >= 37);
    assert!(age <= 39);
    Ok(())
}

#[test]
fn binary_encoding_is_compact_and_versioned() -> TestResult {
    let bytes = (0..1_048_576).map(|index| (index % 256) as u8);
    let body: Vec<u8> = bytes.collect();
    let encoded = encode(&HeaderMap::new(), &body)?;
    assert!(encoded.len() < 1_400_000);
    let restored = decode(&encoded, 8192, 1_048_576).ok_or("decode failed")?;
    let (_, decoded) = restored;
    assert_eq!(decoded, body);
    let mut stored: CachedResponse = serde_json::from_slice(&encoded)?;
    stored.version = 1;
    assert!(decode(&serde_json::to_vec(&stored)?, 8192, 1_048_576).is_none());
    Ok(())
}

#[test]
fn decode_enforces_body_and_header_admission() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert("x-long", HeaderValue::from_static("value"));
    let encoded = encode(&headers, b"body")?;
    assert!(decode(&encoded, 1, 1024).is_none());
    assert!(decode(&encoded, 8192, 1).is_none());
    Ok(())
}
