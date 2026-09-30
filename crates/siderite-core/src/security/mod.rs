//! Security schemes as self-documenting extractors: HTTP Bearer, HTTP
//! Basic, API keys and the OAuth2 password flow.
//!
//! Every scheme is a [`FromRequestParts`] extractor. Besides reading the
//! credentials, it documents itself: it registers an entry under
//! `components.securitySchemes` and adds a security requirement to the
//! operation, so the OpenAPI document mirrors the handler signature.
//!
//! Missing or malformed credentials yield `401 Unauthorized` with a
//! `WWW-Authenticate` challenge (`Bearer`, or `Basic realm="api"`). API keys
//! have no standard challenge, so they answer `401` without one.
//!
//! # Verifying credentials
//!
//! The extractors only *read* credentials; they never decide whether they
//! are valid. Implement [`Authenticate`] to turn credentials into a
//! principal and use [`Security`] to run it. When comparing secrets (API
//! keys, passwords, tokens) use a constant-time comparison such as the
//! `subtle` crate: `==` on strings leaks timing information. That is the
//! application's responsibility, not this module's.
//!
//! ```
//! use siderite_core::security::{Authenticate, HttpBearer, Security, check_scopes};
//! use siderite_core::{ApiError, scopes};
//! use http::StatusCode;
//! use http::request::Parts;
//!
//! struct CurrentUser(String);
//!
//! impl Authenticate for CurrentUser {
//!     type Credentials = HttpBearer;
//!
//!     async fn authenticate(
//!         credentials: HttpBearer,
//!         required: &[&'static str],
//!         _parts: &Parts,
//!     ) -> Result<Self, ApiError> {
//!         // Look the token up in your own store here.
//!         let granted = ["read"];
//!         check_scopes(required, granted)?;
//!         Ok(CurrentUser(credentials.token))
//!     }
//! }
//!
//! scopes!(ReadScopes = ["read"]);
//!
//! async fn me(Security(user, _): Security<CurrentUser, ReadScopes>) -> String {
//!     user.0
//! }
//! # let _ = me;
//! ```

mod api_key;
mod http_auth;
mod oauth2;

pub use api_key::{ApiKey, ApiKeyLocation, ApiKeySpec};
pub use http_auth::{HttpBasic, HttpBearer};
pub use oauth2::{OAuth2PasswordBearer, OAuth2PasswordRequestForm, OAuth2Spec};

use crate::error::ApiError;
use crate::extract::FromRequestParts;
use http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use http::request::Parts;
use http::{HeaderValue, StatusCode};
use serde_json::Value;
use siderite_openapi::{Operation, SchemaRegistry};
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::marker::PhantomData;

const NOT_AUTHENTICATED: &str = "Not authenticated.";

/// `401 Unauthorized` carrying a `WWW-Authenticate` challenge.
pub(crate) fn unauthorized(challenge: &'static str, detail: &'static str) -> ApiError {
    ApiError::new(StatusCode::UNAUTHORIZED, detail)
        .with_header(WWW_AUTHENTICATE, HeaderValue::from_static(challenge))
}

/// The `Authorization` header value after `scheme` (case-insensitive) and
/// its separating spaces, when present and non-empty.
pub(crate) fn authorization_credentials<'a>(parts: &'a Parts, scheme: &str) -> Option<&'a str> {
    let value = parts.headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (name, rest) = value.split_once(' ')?;
    let credentials = rest.trim_start_matches(' ');
    (name.eq_ignore_ascii_case(scheme) && !credentials.is_empty()).then_some(credentials)
}

/// Bearer token from the `Authorization` header.
///
/// # Errors
/// `401` with a `Bearer` challenge when the header is missing, uses another
/// scheme, or the token is empty or contains whitespace.
pub(crate) fn bearer_token(parts: &Parts) -> Result<String, ApiError> {
    match authorization_credentials(parts, "Bearer") {
        Some(token) if !token.contains(char::is_whitespace) => Ok(token.to_owned()),
        Some(_) => Err(unauthorized("Bearer", "Invalid bearer token.")),
        None => Err(unauthorized("Bearer", NOT_AUTHENTICATED).absent()),
    }
}

/// Register `definition` under `name` and require it on `op` with `scopes`.
///
/// The requirement is skipped when an identical one is already present.
pub(crate) fn document_scheme(
    op: &mut Operation,
    registry: &mut SchemaRegistry,
    name: &str,
    definition: Value,
    scopes: &[&str],
) {
    registry.add_security_scheme(name, definition);
    push_requirement(op, name, scopes);
}

/// Require `{name: scopes}` on `op`.
///
/// OpenAPI lists security requirement objects as *alternatives* and the
/// schemes inside one object as *all required*. Extractors in one handler
/// are all required, so the scheme is added to every existing alternative
/// (or becomes the first one).
pub(crate) fn push_requirement(op: &mut Operation, name: &str, scopes: &[&str]) {
    let scopes: Vec<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
    if op.security.is_empty() {
        op.security.push(BTreeMap::new());
    }
    for alternative in &mut op.security {
        alternative.insert(name.to_owned(), scopes.clone());
    }
}

/// Build the `403 Forbidden` error for a principal lacking scopes.
///
/// Returns `Ok(())` when every scope in `required` appears in `granted`.
///
/// # Errors
/// `403` naming the missing scopes (scope names are not secret).
///
/// # Examples
/// ```
/// use siderite_core::security::check_scopes;
/// assert!(check_scopes(&["read"], ["read", "write"]).is_ok());
/// assert_eq!(check_scopes(&["admin"], ["read"]).unwrap_err().status(), 403);
/// ```
pub fn check_scopes<'a>(
    required: &[&str],
    granted: impl IntoIterator<Item = &'a str>,
) -> Result<(), ApiError> {
    let granted: Vec<&str> = granted.into_iter().collect();
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|scope| !granted.contains(scope))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            format!("Missing required scopes: {}.", missing.join(", ")),
        ))
    }
}

/// Required scopes, as a marker type (const `&str` generics are unstable).
///
/// Declare one with the [`scopes!`](crate::scopes) macro.
pub trait Scopes: Send + Sync + 'static {
    /// The scopes a request must hold.
    const SCOPES: &'static [&'static str];
}

/// No required scopes (the default for [`Security`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NoScopes;

impl Scopes for NoScopes {
    const SCOPES: &'static [&'static str] = &[];
}

/// Declare a [`Scopes`] marker type.
///
/// ```
/// use siderite_core::security::Scopes;
/// siderite_core::scopes!(pub AdminScopes = ["users:read", "users:write"]);
/// assert_eq!(AdminScopes::SCOPES, ["users:read", "users:write"]);
/// ```
#[macro_export]
macro_rules! scopes {
    ($(#[$meta:meta])* $vis:vis $name:ident = [$($scope:literal),* $(,)?]) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        $vis struct $name;

        impl $crate::security::Scopes for $name {
            const SCOPES: &'static [&'static str] = &[$($scope),*];
        }
    };
}

/// User-defined principal built from a scheme's credentials.
///
/// `authenticate` receives the scopes the endpoint requires and the request
/// [`Parts`]; application state set with `App::with_state` is reachable
/// through `parts.extensions`. Return `401` for invalid credentials and
/// `403` (see [`check_scopes`]) for missing scopes.
pub trait Authenticate: Sized + Send + 'static {
    /// The scheme that supplies the credentials.
    type Credentials: FromRequestParts;

    /// Validate `credentials` and build the principal.
    ///
    /// # Errors
    /// An [`ApiError`], typically `401` or `403`.
    fn authenticate(
        credentials: Self::Credentials,
        required: &[&'static str],
        parts: &Parts,
    ) -> impl Future<Output = Result<Self, ApiError>> + Send;
}

/// `Security<CurrentUser, AdminScopes>`: extracts credentials, runs
/// [`Authenticate::authenticate`] (`403` on missing scopes), and documents
/// the scheme together with the required scopes.
///
/// The second field is a type marker; destructure with
/// `Security(user, _)`.
pub struct Security<T: Authenticate, S: Scopes = NoScopes>(pub T, pub PhantomData<fn() -> S>);

impl<T: Authenticate + fmt::Debug, S: Scopes> fmt::Debug for Security<T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Security").field(&self.0).finish()
    }
}

impl<T: Authenticate, S: Scopes> FromRequestParts for Security<T, S> {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        let credentials = T::Credentials::from_request_parts(parts).await?;
        let principal = T::authenticate(credentials, S::SCOPES, parts).await?;
        Ok(Self(principal, PhantomData))
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        // Let the scheme document itself, then re-emit what it added with
        // this endpoint's scopes.
        let existing = std::mem::take(&mut op.security);
        <T::Credentials as FromRequestParts>::describe(op, registry);
        let added = std::mem::replace(&mut op.security, existing);
        let names: std::collections::BTreeSet<String> =
            added.into_iter().flat_map(BTreeMap::into_keys).collect();
        for name in names {
            push_requirement(op, &name, S::SCOPES);
        }
    }
}
