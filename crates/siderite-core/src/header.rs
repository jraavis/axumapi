//! Typed header and cookie extractors and `Set-Cookie` responses.

use crate::error::ApiError;
use crate::extract::FromRequestParts;
use crate::response::{IntoResponse, Response};
use cookie::Cookie;
use http::HeaderValue;
use http::header::{COOKIE, SET_COOKIE};
use http::request::Parts;
use serde_json::json;
use siderite_openapi::{Operation, Parameter, ParameterLocation, SchemaObject, SchemaRegistry};

/// A typed HTTP header identified by a static name.
pub trait NamedHeader: Sized + Send {
    /// Header name as sent on the wire (matched case-insensitively).
    const NAME: &'static str;

    /// Decode the header value.
    ///
    /// # Errors
    /// Returns a message describing why the value is invalid.
    fn decode(value: &str) -> Result<Self, String>;

    /// JSON Schema for this header (defaults to a string).
    fn schema(_registry: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("string")
    }
}

/// Extractor for a required typed header `H`.
///
/// Missing headers yield `422` with code `missing` and location
/// `["header", H::NAME]`. Invalid values yield code `header_invalid`.
/// Wrap in [`Option`] to make the header optional; extraction failures
/// become `None` via the blanket [`FromRequestParts`] impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header<H: NamedHeader>(pub H);

impl<H: NamedHeader> FromRequestParts for Header<H> {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        let value = parts.headers.get(H::NAME).ok_or_else(|| {
            header_error(H::NAME, "missing", format!("missing header `{}`", H::NAME))
        })?;
        let text = value.to_str().map_err(|_| {
            header_error(
                H::NAME,
                "header_invalid",
                format!("header `{}` is not valid UTF-8", H::NAME),
            )
        })?;
        H::decode(text)
            .map(Header)
            .map_err(|message| header_error(H::NAME, "header_invalid", message))
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        op.add_parameter(Parameter::new(
            H::NAME,
            ParameterLocation::Header,
            true,
            H::schema(registry),
        ));
    }
}

fn header_error(name: &str, code: &str, message: impl Into<String>) -> ApiError {
    ApiError::unprocessable(json!([{
        "location": ["header", name],
        "code": code,
        "message": message.into(),
    }]))
}

/// The `User-Agent` request header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserAgent(pub String);

impl NamedHeader for UserAgent {
    const NAME: &'static str = "User-Agent";

    fn decode(value: &str) -> Result<Self, String> {
        Ok(Self(value.to_owned()))
    }
}

/// The `Accept` request header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accept(pub String);

impl NamedHeader for Accept {
    const NAME: &'static str = "Accept";

    fn decode(value: &str) -> Result<Self, String> {
        Ok(Self(value.to_owned()))
    }
}

/// Cookies parsed from the request `Cookie` header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cookies {
    pairs: Vec<(String, String)>,
}

impl Cookies {
    /// The first cookie value for `name`, if present.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Iterate over `(name, value)` pairs in header order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.pairs.iter().map(|(n, v)| (n.as_str(), v.as_str()))
    }
}

impl FromRequestParts for Cookies {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        let mut pairs = Vec::new();
        for header in parts.headers.get_all(COOKIE) {
            let Ok(text) = header.to_str() else {
                continue;
            };
            for cookie in Cookie::split_parse(text).flatten() {
                pairs.push((cookie.name().to_owned(), cookie.value().to_owned()));
            }
        }
        Ok(Self { pairs })
    }
}

/// `SameSite` attribute for a [`SetCookie`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    /// Never send the cookie on cross-site requests.
    Strict,
    /// Send the cookie on top-level cross-site GET navigations.
    Lax,
    /// Send the cookie on all cross-site requests (requires `Secure`).
    None,
}

impl From<SameSite> for cookie::SameSite {
    fn from(value: SameSite) -> Self {
        match value {
            SameSite::Strict => Self::Strict,
            SameSite::Lax => Self::Lax,
            SameSite::None => Self::None,
        }
    }
}

/// Builder for a `Set-Cookie` header.
#[derive(Debug, Clone)]
pub struct SetCookie {
    inner: Cookie<'static>,
}

impl SetCookie {
    /// Start a cookie with `name` and `value`.
    #[must_use]
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            inner: Cookie::new(name.into(), value.into()),
        }
    }

    /// Set the `HttpOnly` flag.
    #[must_use]
    pub fn http_only(mut self, value: bool) -> Self {
        self.inner.set_http_only(value);
        self
    }

    /// Set the `Secure` flag.
    #[must_use]
    pub fn secure(mut self, value: bool) -> Self {
        self.inner.set_secure(value);
        self
    }

    /// Set the `SameSite` attribute.
    #[must_use]
    pub fn same_site(mut self, value: SameSite) -> Self {
        self.inner.set_same_site(cookie::SameSite::from(value));
        self
    }

    /// Set the cookie `Path`.
    #[must_use]
    pub fn path(mut self, value: impl Into<String>) -> Self {
        self.inner.set_path(value.into());
        self
    }

    /// Set the cookie `Domain`.
    #[must_use]
    pub fn domain(mut self, value: impl Into<String>) -> Self {
        self.inner.set_domain(value.into());
        self
    }

    /// Set `Max-Age` from a [`std::time::Duration`].
    #[must_use]
    pub fn max_age(mut self, value: std::time::Duration) -> Self {
        if let Ok(duration) = cookie::time::Duration::try_from(value) {
            self.inner.set_max_age(duration);
        }
        self
    }
}

/// Wraps a response and appends `Set-Cookie` headers.
#[derive(Debug)]
pub struct WithCookies<R> {
    inner: R,
    cookies: Vec<SetCookie>,
}

impl<R> WithCookies<R> {
    /// Wrap `inner` with no cookies.
    #[must_use]
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            cookies: Vec::new(),
        }
    }

    /// Append a `Set-Cookie` header built from `cookie`.
    #[must_use]
    pub fn cookie(mut self, cookie: SetCookie) -> Self {
        self.cookies.push(cookie);
        self
    }
}

impl<R: IntoResponse> IntoResponse for WithCookies<R> {
    fn into_response(self) -> Response {
        let mut headers = Vec::new();
        for cookie in self.cookies {
            match HeaderValue::from_str(&cookie.inner.to_string()) {
                Ok(value) => headers.push(value),
                Err(_) => {
                    return ApiError::internal("invalid Set-Cookie header value").into_response();
                }
            }
        }
        let mut response = self.inner.into_response();
        for value in headers {
            response.headers_mut().append(SET_COOKIE, value);
        }
        response
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        R::describe(op, registry);
    }
}
