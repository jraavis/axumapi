//! HTTP Bearer and Basic authentication.

use super::{
    NOT_AUTHENTICATED, authorization_credentials, bearer_token, document_scheme, unauthorized,
};
use crate::error::ApiError;
use crate::extract::FromRequestParts;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::request::Parts;
use serde_json::json;
use siderite_openapi::{Operation, SchemaRegistry};
use std::fmt;

const BASIC_CHALLENGE: &str = r#"Basic realm="api""#;

/// Bearer token from `Authorization: Bearer <token>` (scheme `HTTPBearer`).
///
/// Missing or malformed headers yield `401` with `WWW-Authenticate: Bearer`.
/// The token is not validated; use [`Authenticate`](super::Authenticate).
/// Wrap in `Option` for optional authentication.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpBearer {
    /// The raw token, without the `Bearer` prefix.
    pub token: String,
}

impl fmt::Debug for HttpBearer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpBearer")
            .field("token", &"[REDACTED]")
            .finish()
    }
}

impl FromRequestParts for HttpBearer {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        bearer_token(parts).map(|token| Self { token })
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        document_scheme(
            op,
            registry,
            "HTTPBearer",
            json!({"type": "http", "scheme": "bearer"}),
            &[],
        );
    }
}

/// Credentials from `Authorization: Basic <base64(user:pass)>` (scheme
/// `HTTPBasic`).
///
/// Missing or malformed headers yield `401` with
/// `WWW-Authenticate: Basic realm="api"`. The password is split at the first
/// colon, so it may itself contain colons.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpBasic {
    /// The user name.
    pub username: String,
    /// The password.
    pub password: String,
}

impl fmt::Debug for HttpBasic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpBasic")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// Decode `base64(username:password)`; `None` when malformed.
fn decode_basic(encoded: &str) -> Option<HttpBasic> {
    let bytes = STANDARD.decode(encoded.trim()).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let (username, password) = text.split_once(':')?;
    Some(HttpBasic {
        username: username.to_owned(),
        password: password.to_owned(),
    })
}

impl FromRequestParts for HttpBasic {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        let encoded = authorization_credentials(parts, "Basic")
            .ok_or_else(|| unauthorized(BASIC_CHALLENGE, NOT_AUTHENTICATED).absent())?;
        decode_basic(encoded)
            .ok_or_else(|| unauthorized(BASIC_CHALLENGE, "Invalid basic credentials."))
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        document_scheme(
            op,
            registry,
            "HTTPBasic",
            json!({"type": "http", "scheme": "basic"}),
            &[],
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn decodes_basic_credentials() {
        let user = decode_basic(&STANDARD.encode("ann:s3:cret")).unwrap();
        assert_eq!(user.username, "ann");
        assert_eq!(user.password, "s3:cret");
    }

    #[test]
    fn rejects_malformed_basic_credentials() {
        assert!(decode_basic("!!!not-base64").is_none());
        assert!(decode_basic(&STANDARD.encode("no-colon")).is_none());
        assert!(decode_basic(&STANDARD.encode([0xff, b':', 0xfe])).is_none());
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let basic = HttpBasic {
            username: "ann".into(),
            password: "hunter2".into(),
        };
        let bearer = HttpBearer {
            token: "tok-123".into(),
        };
        let text = format!("{basic:?} {bearer:?}");
        assert!(!text.contains("hunter2") && !text.contains("tok-123"));
    }
}
