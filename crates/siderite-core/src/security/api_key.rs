//! API keys carried in a header, query parameter or cookie.

use super::document_scheme;
use crate::error::ApiError;
use crate::extract::FromRequestParts;
use crate::header::Cookies;
use http::StatusCode;
use http::request::Parts;
use serde_json::json;
use siderite_openapi::{Operation, SchemaRegistry};
use std::fmt;
use std::marker::PhantomData;

/// Where an API key is carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyLocation {
    /// A request header.
    Header,
    /// A query-string parameter.
    Query,
    /// A cookie.
    Cookie,
}

impl ApiKeyLocation {
    /// The OpenAPI `in` value.
    const fn openapi(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Query => "query",
            Self::Cookie => "cookie",
        }
    }
}

/// Describes one API-key scheme; implement on a marker type.
pub trait ApiKeySpec: Send + Sync + 'static {
    /// Header, query-parameter or cookie name.
    const NAME: &'static str;
    /// Where the key is carried.
    const LOCATION: ApiKeyLocation;
    /// Name under `components.securitySchemes`.
    const SCHEME: &'static str;
}

/// An API key described by `S`.
///
/// A missing or empty key yields `401` (API keys have no standard
/// `WWW-Authenticate` challenge, so none is sent). The key is not validated;
/// compare it in constant time inside your own [`Authenticate`](super::Authenticate)
/// implementation.
pub struct ApiKey<S: ApiKeySpec> {
    /// The raw key.
    pub key: String,
    spec: PhantomData<fn() -> S>,
}

impl<S: ApiKeySpec> ApiKey<S> {
    /// Wrap a key value.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            spec: PhantomData,
        }
    }
}

impl<S: ApiKeySpec> fmt::Debug for ApiKey<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKey")
            .field("scheme", &S::SCHEME)
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl<S: ApiKeySpec> Clone for ApiKey<S> {
    fn clone(&self) -> Self {
        Self::new(self.key.clone())
    }
}

/// The first non-empty query value named `name`.
fn query_value(parts: &Parts, name: &str) -> Option<String> {
    let pairs = serde_urlencoded::from_str::<Vec<(String, String)>>(parts.uri.query()?).ok()?;
    pairs
        .into_iter()
        .find(|(key, value)| key == name && !value.is_empty())
        .map(|(_, value)| value)
}

/// The first non-empty header value named `name`.
fn header_value(parts: &Parts, name: &str) -> Option<String> {
    parts
        .headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

impl<S: ApiKeySpec> FromRequestParts for ApiKey<S> {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        let key = match S::LOCATION {
            ApiKeyLocation::Header => header_value(parts, S::NAME),
            ApiKeyLocation::Query => query_value(parts, S::NAME),
            ApiKeyLocation::Cookie => {
                let cookies = Cookies::from_request_parts(parts).await?;
                cookies
                    .get(S::NAME)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            }
        };
        key.map(Self::new)
            .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "Not authenticated.").absent())
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        document_scheme(
            op,
            registry,
            S::SCHEME,
            json!({"type": "apiKey", "in": S::LOCATION.openapi(), "name": S::NAME}),
            &[],
        );
    }
}
