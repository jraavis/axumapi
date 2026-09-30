//! The OAuth2 password flow: bearer-token extraction and the token form.

use super::{bearer_token, document_scheme};
use crate::body::Body;
use crate::error::ApiError;
use crate::extract::{FromRequest, FromRequestParts, Request, invalid};
use http::StatusCode;
use http::request::Parts;
use serde_json::{Map, Value, json};
use siderite_openapi::{Operation, SchemaObject, SchemaRegistry};
use std::collections::HashMap;
use std::fmt;
use std::marker::PhantomData;

/// Describes one OAuth2 password-flow scheme; implement on a marker type.
pub trait OAuth2Spec: Send + Sync + 'static {
    /// URL of the token endpoint (usually the route taking
    /// [`OAuth2PasswordRequestForm`]).
    const TOKEN_URL: &'static str;
    /// `(scope, description)` pairs the API understands.
    const SCOPES: &'static [(&'static str, &'static str)];
    /// Name under `components.securitySchemes`.
    const SCHEME: &'static str;
}

/// Bearer token for an OAuth2 password flow described by `S`.
///
/// Reads `Authorization: Bearer <token>`; missing or malformed headers yield
/// `401` with `WWW-Authenticate: Bearer`. The token is not validated.
pub struct OAuth2PasswordBearer<S: OAuth2Spec> {
    /// The raw token, without the `Bearer` prefix.
    pub token: String,
    spec: PhantomData<fn() -> S>,
}

impl<S: OAuth2Spec> OAuth2PasswordBearer<S> {
    /// Wrap a token value.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            spec: PhantomData,
        }
    }
}

impl<S: OAuth2Spec> fmt::Debug for OAuth2PasswordBearer<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuth2PasswordBearer")
            .field("scheme", &S::SCHEME)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

impl<S: OAuth2Spec> Clone for OAuth2PasswordBearer<S> {
    fn clone(&self) -> Self {
        Self::new(self.token.clone())
    }
}

impl<S: OAuth2Spec> FromRequestParts for OAuth2PasswordBearer<S> {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        bearer_token(parts).map(Self::new)
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        let scopes: Map<String, Value> = S::SCOPES
            .iter()
            .map(|(scope, description)| ((*scope).to_owned(), json!(description)))
            .collect();
        document_scheme(
            op,
            registry,
            S::SCHEME,
            json!({
                "type": "oauth2",
                "flows": {"password": {"tokenUrl": S::TOKEN_URL, "scopes": scopes}},
            }),
            &[],
        );
    }
}

/// The token-endpoint form of the OAuth2 password flow
/// (`application/x-www-form-urlencoded`, RFC 6749 section 4.3.2).
///
/// `username` and `password` are required (`422` when absent). `scope` is a
/// space-separated list. `grant_type` is accepted but not checked. A wrong
/// `Content-Type` yields `415`.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuth2PasswordRequestForm {
    /// The user name.
    pub username: String,
    /// The password.
    pub password: String,
    /// Requested scopes.
    pub scopes: Vec<String>,
    /// Optional client identifier.
    pub client_id: Option<String>,
    /// Optional client secret.
    pub client_secret: Option<String>,
}

impl fmt::Debug for OAuth2PasswordRequestForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuth2PasswordRequestForm")
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .field("scopes", &self.scopes)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl OAuth2PasswordRequestForm {
    /// Build the form from decoded fields.
    fn from_fields(mut fields: HashMap<String, String>) -> Result<Self, ApiError> {
        let mut required = |name: &str| {
            fields
                .remove(name)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid("body", "missing", format!("missing form field `{name}`")))
        };
        let username = required("username")?;
        let password = required("password")?;
        let scopes = fields
            .remove("scope")
            .map(|scope| scope.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();
        Ok(Self {
            username,
            password,
            scopes,
            client_id: fields.remove("client_id").filter(|v| !v.is_empty()),
            client_secret: fields.remove("client_secret").filter(|v| !v.is_empty()),
        })
    }
}

impl FromRequest for OAuth2PasswordRequestForm {
    async fn from_request(req: Request) -> Result<Self, ApiError> {
        use axum::extract::FromRequest as _;
        use axum::extract::rejection::FormRejection;
        match axum::Form::<Vec<(String, String)>>::from_request(req.map(Body::into_inner), &())
            .await
        {
            // Later duplicates win; the form has no repeated fields.
            Ok(axum::Form(pairs)) => Self::from_fields(pairs.into_iter().collect()),
            Err(rejection) => {
                let message = rejection.body_text();
                Err(match rejection {
                    FormRejection::InvalidFormContentType(_) => {
                        ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, message)
                    }
                    FormRejection::FailedToDeserializeForm(_)
                    | FormRejection::FailedToDeserializeFormBody(_) => {
                        invalid("body", "form_invalid", message)
                    }
                    other => ApiError::new(other.status(), message),
                })
            }
        }
    }

    fn describe(op: &mut Operation, _registry: &mut SchemaRegistry) {
        let string = || json!({"type": "string"});
        let schema = SchemaObject::of_type("object")
            .with(
                "properties",
                json!({
                    "grant_type": {"type": "string", "pattern": "^password$"},
                    "username": string(),
                    "password": string(),
                    "scope": string(),
                    "client_id": string(),
                    "client_secret": string(),
                }),
            )
            .with("required", json!(["username", "password"]));
        op.set_request_body("application/x-www-form-urlencoded", schema, true);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn splits_scopes_on_whitespace() {
        let form = OAuth2PasswordRequestForm::from_fields(fields(&[
            ("username", "ann"),
            ("password", "pw"),
            ("scope", "read  write"),
        ]))
        .unwrap();
        assert_eq!(form.scopes, ["read", "write"]);
        assert_eq!(form.client_id, None);
    }

    #[test]
    fn requires_username_and_password() {
        let err =
            OAuth2PasswordRequestForm::from_fields(fields(&[("username", "ann")])).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn debug_output_redacts_secrets() {
        let form = OAuth2PasswordRequestForm::from_fields(fields(&[
            ("username", "ann"),
            ("password", "hunter2"),
            ("client_secret", "shh"),
        ]))
        .unwrap();
        let text = format!("{form:?}");
        assert!(!text.contains("hunter2") && !text.contains("shh"));
    }
}
