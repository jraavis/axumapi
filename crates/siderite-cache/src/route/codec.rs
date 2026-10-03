//! Versioned response envelopes with compact bytes and retained freshness.

use super::is_hop_by_hop;
use base64::{Engine, engine::general_purpose::STANDARD};
use http::{HeaderMap, HeaderName, HeaderValue, header};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type Encoded = Result<Vec<u8>, serde_json::Error>;
type Decoded = Option<(HeaderMap, Vec<u8>)>;

#[derive(Serialize, Deserialize)]
struct CachedResponse {
    version: u8,
    stored_at: u64,
    initial_age: u64,
    headers: Vec<(String, String)>,
    body: String,
}

pub(super) fn initial_age(headers: &HeaderMap) -> Option<Duration> {
    let age = match headers.get(header::AGE) {
        Some(age) => age.to_str().ok()?.parse::<u64>().ok()?,
        None => 0,
    };
    let apparent = match headers.get(header::DATE) {
        Some(date) => {
            let date = httpdate::parse_http_date(date.to_str().ok()?).ok()?;
            SystemTime::now().duration_since(date).unwrap_or_default()
        }
        None => Duration::ZERO,
    };
    Some(apparent.max(Duration::from_secs(age)))
}

fn connection_headers(headers: &HeaderMap) -> Vec<HeaderName> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::try_from(name.trim()).ok())
        .collect()
}

pub(super) fn encode(headers: &HeaderMap, body: &[u8]) -> Encoded {
    let now = SystemTime::now();
    let elapsed = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let stored_at = elapsed.as_secs();
    let age = initial_age(headers).unwrap_or(Duration::MAX).as_secs();
    let connection = connection_headers(headers);
    let mut fields: Vec<_> = headers
        .iter()
        .filter(|(name, _)| !is_hop_by_hop(name) && !connection.contains(name))
        .map(|(name, value)| {
            let name = name.as_str().to_owned();
            let value = STANDARD.encode(value.as_bytes());
            (name, value)
        })
        .collect();
    if !headers.contains_key(header::DATE) {
        fields.push((
            "date".into(),
            STANDARD.encode(httpdate::fmt_http_date(now).as_bytes()),
        ));
    }
    serde_json::to_vec(&CachedResponse {
        version: 2,
        stored_at,
        initial_age: age,
        headers: fields,
        body: STANDARD.encode(body),
    })
}

pub(super) fn decode(bytes: &[u8], fields: usize, limit: u64) -> Decoded {
    let stored: CachedResponse = serde_json::from_slice(bytes).ok()?;
    if stored.version != 2 {
        return None;
    }
    if stored.headers.len() > fields {
        return None;
    }
    let mut headers = HeaderMap::new();
    let mut size = 0usize;
    for (name, value) in stored.headers {
        let name = HeaderName::try_from(name).ok()?;
        let bytes = STANDARD.decode(value).ok()?;
        size = size
            .saturating_add(name.as_str().len())
            .saturating_add(bytes.len());
        if size > fields {
            return None;
        }
        let value = HeaderValue::from_bytes(&bytes).ok()?;
        headers.append(name, value);
    }
    let connection = connection_headers(&headers);
    let fixed: Vec<_> = headers
        .keys()
        .filter(|name| is_hop_by_hop(name))
        .cloned()
        .collect();
    for name in connection.into_iter().chain(fixed) {
        headers.remove(name);
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let resident = now.saturating_sub(stored.stored_at);
    let age = stored.initial_age.saturating_add(resident);
    headers.insert(header::AGE, HeaderValue::from(age));
    let body = STANDARD.decode(stored.body).ok()?;
    if body.len() as u64 > limit {
        return None;
    }
    Some((headers, body))
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
