//! URL newtypes: any-scheme [`Url`] and http(s)-only [`HttpUrl`].

use serde::{Deserialize, Serialize};

use crate::context::ValidationContext;
use crate::dump::Dump;
use crate::error::FieldError;
use crate::rules;
use crate::schema::{Schema, SchemaObject, SchemaRegistry};
use crate::validate::Validate;
use serde_json::Value;

/// An absolute URL of any scheme.
///
/// Unlike Pydantic `AnyUrl`, this does not restrict allowed schemes, host
/// presence, or TLD — it accepts any value [`url::Url::parse`] accepts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Url(::url::Url);

impl Url {
    /// Parse `value` as an absolute URL.
    pub fn new(value: impl AsRef<str>) -> Result<Self, FieldError> {
        rules::parse_url(value.as_ref()).map(Self)
    }

    /// The URL as a string slice.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

crate::types::impl_wrapper!(Url => ::url::Url);

impl TryFrom<String> for Url {
    type Error = FieldError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for Url {
    type Error = FieldError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Url> for String {
    fn from(value: Url) -> Self {
        value.0.to_string()
    }
}

impl AsRef<str> for Url {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl Validate for Url {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        if super::prepare_as_string(input, ctx) {
            ctx.check(Self::new(input.as_str().unwrap_or_default()).map(|_| ()));
        }
    }
}

impl Schema for Url {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("string").with("format", "uri")
    }
}

impl Dump for Url {}

/// An absolute `http` or `https` URL.
///
/// Unlike Pydantic `HttpUrl`, this only checks the scheme. Host, TLD, and
/// maximum length are not enforced.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HttpUrl(::url::Url);

fn is_http_scheme(url: &::url::Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

impl HttpUrl {
    /// Parse `value` as an `http` or `https` URL.
    pub fn new(value: impl AsRef<str>) -> Result<Self, FieldError> {
        let url = rules::parse_url(value.as_ref())?;
        if !is_http_scheme(&url) {
            return Err(FieldError::new(
                "url_scheme",
                "URL scheme must be http or https",
            ));
        }
        Ok(Self(url))
    }

    /// The URL as a string slice.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

crate::types::impl_wrapper!(HttpUrl => ::url::Url);

impl TryFrom<String> for HttpUrl {
    type Error = FieldError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for HttpUrl {
    type Error = FieldError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<HttpUrl> for String {
    fn from(value: HttpUrl) -> Self {
        value.0.to_string()
    }
}

impl AsRef<str> for HttpUrl {
    fn as_ref(&self) -> &str {
        self.0.as_str()
    }
}

impl Validate for HttpUrl {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        if super::prepare_as_string(input, ctx) {
            ctx.check(Self::new(input.as_str().unwrap_or_default()).map(|_| ()));
        }
    }
}

impl Schema for HttpUrl {
    fn schema(_: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("string").with("format", "uri")
    }
}

impl Dump for HttpUrl {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::context::ValidationContext;
    use crate::dump::{Dump, DumpOptions};
    use crate::schema::schema_for;
    use crate::types::prepare_codes;
    use serde_json::json;

    #[test]
    fn url_construction() {
        let url = Url::new("https://example.com/x").unwrap();
        assert_eq!(url.as_str(), "https://example.com/x");
        assert_eq!(url.scheme(), "https");
        assert!(Url::new("ftp://files.example.com").is_ok());
        assert_eq!(Url::new("not a url").unwrap_err().code, "url_parsing");
        assert_eq!(Url::new("example.com").unwrap_err().code, "url_parsing");
    }

    #[test]
    fn http_url_rejects_other_schemes() {
        assert!(HttpUrl::new("http://example.com").is_ok());
        assert!(HttpUrl::new("https://example.com").is_ok());
        assert_eq!(
            HttpUrl::new("ftp://example.com").unwrap_err().code,
            "url_scheme"
        );
        assert_eq!(HttpUrl::new("nope").unwrap_err().code, "url_parsing");
    }

    #[test]
    fn url_serde_round_trip_and_reject() {
        let url = Url::new("https://example.com").unwrap();
        let encoded = serde_json::to_string(&url).unwrap();
        assert_eq!(encoded, "\"https://example.com/\"");
        let round: Url = serde_json::from_str(&encoded).unwrap();
        assert_eq!(round.as_str(), "https://example.com/");
        assert!(serde_json::from_str::<Url>("\"not a url\"").is_err());
        assert!(serde_json::from_str::<HttpUrl>("\"ftp://example.com\"").is_err());
    }

    #[test]
    fn url_prepare_lax_and_strict() {
        let (out, codes) =
            prepare_codes::<Url>(json!("https://example.com"), ValidationContext::new());
        assert_eq!(codes, Vec::<String>::new());
        assert_eq!(out, json!("https://example.com"));
        assert_eq!(
            prepare_codes::<Url>(json!(1), ValidationContext::new()).1,
            ["string_type"]
        );
        assert_eq!(
            prepare_codes::<Url>(json!(1), ValidationContext::new().with_strict(true)).1,
            ["string_type"]
        );
        assert_eq!(
            prepare_codes::<Url>(json!("nope"), ValidationContext::new()).1,
            ["url_parsing"]
        );
        assert_eq!(
            prepare_codes::<HttpUrl>(json!("ftp://x.com"), ValidationContext::new()).1,
            ["url_scheme"]
        );
    }

    #[test]
    fn url_schema_dump_and_parse_value() {
        assert_eq!(
            schema_for::<Url>().0.into_value(),
            json!({"type": "string", "format": "uri"})
        );
        assert_eq!(
            schema_for::<HttpUrl>().0.into_value(),
            json!({"type": "string", "format": "uri"})
        );
        let url = Url::new("https://example.com").unwrap();
        assert_eq!(
            url.dump(&DumpOptions::new()).unwrap(),
            json!("https://example.com/")
        );
        let parsed =
            crate::parse_value::<HttpUrl>(json!("https://example.com/a"), ValidationContext::new())
                .unwrap();
        assert_eq!(parsed.as_str(), "https://example.com/a");
        let err = crate::parse_value::<Url>(json!("nope"), ValidationContext::new()).unwrap_err();
        assert_eq!(err.errors[0].code, "url_parsing");
    }
}
