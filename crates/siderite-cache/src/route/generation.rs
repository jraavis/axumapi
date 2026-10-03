//! Random generations keep stale fills unreachable across target writes.

use super::{Policy, cache_key, push_part};
use crate::{Cache, CacheError};
use http::Method;
use siderite_core::Request;
use uuid::Uuid;

type Stamp = Result<Uuid, CacheError>;
type Outcome = Result<(), CacheError>;

pub(super) fn target(req: &Request, policy: &Policy, scheme: &str) -> String {
    let mut target = String::from("route-v2-target:");
    push_part(&mut target, policy.namespace.as_bytes());
    push_part(&mut target, scheme.as_bytes());
    let host = req
        .headers()
        .get(http::header::HOST)
        .map(http::HeaderValue::as_bytes)
        .or_else(|| req.uri().authority().map(|a| a.as_str().as_bytes()))
        .unwrap_or_default();
    push_part(&mut target, &host.to_ascii_lowercase());
    let path = req.uri().path_and_query().map_or("/", |p| p.as_str());
    push_part(&mut target, path.as_bytes());
    target
}

pub(super) fn representation_key(
    req: &Request,
    policy: &Policy,
    scheme: &str,
    generation: Uuid,
) -> String {
    let mut key = String::from("route-v2-response:");
    push_part(&mut key, policy.namespace.as_bytes());
    push_part(&mut key, generation.as_bytes());
    push_part(&mut key, cache_key(req, scheme).as_bytes());
    key
}

pub(super) async fn current<C: Cache>(cache: &C, key: &str) -> Stamp {
    if let Some(value) = cache.get(key).await? {
        return decode(&value);
    }
    let candidate = Uuid::new_v4();
    if cache
        .set_if_absent(key, candidate.as_bytes().to_vec())
        .await?
    {
        return Ok(candidate);
    }
    match cache.get(key).await? {
        Some(value) => decode(&value),
        None => Err(CacheError::Backend("generation was evicted".into())),
    }
}

fn decode(value: &[u8]) -> Stamp {
    let reason = "invalid cache generation";
    Uuid::from_slice(value).map_err(|_| CacheError::Backend(reason.into()))
}

pub(super) async fn invalidate<C: Cache>(cache: &C, key: &str) -> Outcome {
    cache
        .set(key, Uuid::new_v4().as_bytes().to_vec(), None)
        .await
}

pub(super) fn unsafe_method(method: &Method) -> bool {
    !matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::TRACE
    )
}
