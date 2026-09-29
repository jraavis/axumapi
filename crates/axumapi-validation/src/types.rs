//! Constrained newtypes that validate on construction and on deserialize.

use std::fmt;
use std::ops::Deref;

use serde::{Deserialize, Serialize};

use crate::error::FieldError;
use crate::rules;

const REDACTED: &str = "**********";

/// UTF-8 string whose character length is in `MIN..=MAX`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ConstrainedString<const MIN: usize, const MAX: usize>(String);

impl<const MIN: usize, const MAX: usize> ConstrainedString<MIN, MAX> {
    /// Validate `value` and wrap it.
    pub fn new(value: impl Into<String>) -> Result<Self, FieldError> {
        let value = value.into();
        rules::min_length(&value, MIN)?;
        rules::max_length(&value, MAX)?;
        Ok(Self(value))
    }

    /// Unwrap the inner string.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl<const MIN: usize, const MAX: usize> TryFrom<String> for ConstrainedString<MIN, MAX> {
    type Error = FieldError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<const MIN: usize, const MAX: usize> From<ConstrainedString<MIN, MAX>> for String {
    fn from(value: ConstrainedString<MIN, MAX>) -> Self {
        value.0
    }
}

impl<const MIN: usize, const MAX: usize> AsRef<str> for ConstrainedString<MIN, MAX> {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl<const MIN: usize, const MAX: usize> Deref for ConstrainedString<MIN, MAX> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<const MIN: usize, const MAX: usize> fmt::Display for ConstrainedString<MIN, MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Signed 64-bit integer whose value is in `MIN..=MAX`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct BoundedI64<const MIN: i64, const MAX: i64>(i64);

impl<const MIN: i64, const MAX: i64> BoundedI64<MIN, MAX> {
    /// Validate `value` and wrap it.
    pub fn new(value: i64) -> Result<Self, FieldError> {
        rules::ge(&value, &MIN)?;
        rules::le(&value, &MAX)?;
        Ok(Self(value))
    }

    /// Unwrap the inner integer.
    pub fn into_inner(self) -> i64 {
        self.0
    }
}

impl<const MIN: i64, const MAX: i64> TryFrom<i64> for BoundedI64<MIN, MAX> {
    type Error = FieldError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl<const MIN: i64, const MAX: i64> From<BoundedI64<MIN, MAX>> for i64 {
    fn from(value: BoundedI64<MIN, MAX>) -> Self {
        value.0
    }
}

impl<const MIN: i64, const MAX: i64> AsRef<i64> for BoundedI64<MIN, MAX> {
    fn as_ref(&self) -> &i64 {
        &self.0
    }
}

impl<const MIN: i64, const MAX: i64> Deref for BoundedI64<MIN, MAX> {
    type Target = i64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<const MIN: i64, const MAX: i64> fmt::Display for BoundedI64<MIN, MAX> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Strictly positive `i64` (`value > 0`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub struct PositiveInt(i64);

impl PositiveInt {
    /// Validate `value > 0` and wrap it.
    pub fn new(value: i64) -> Result<Self, FieldError> {
        rules::gt(&value, &0)?;
        Ok(Self(value))
    }

    /// Unwrap the inner integer.
    pub fn into_inner(self) -> i64 {
        self.0
    }
}

impl TryFrom<i64> for PositiveInt {
    type Error = FieldError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<PositiveInt> for i64 {
    fn from(value: PositiveInt) -> Self {
        value.0
    }
}

impl AsRef<i64> for PositiveInt {
    fn as_ref(&self) -> &i64 {
        &self.0
    }
}

impl Deref for PositiveInt {
    type Target = i64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for PositiveInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Email address validated by the pragmatic [`rules::email`] check.
///
/// Construction does **not** implement full RFC 5322.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Email(String);

impl Email {
    /// Validate `value` as an email and wrap it.
    pub fn new(value: impl Into<String>) -> Result<Self, FieldError> {
        let value = value.into();
        rules::email(&value)?;
        Ok(Self(value))
    }

    /// Unwrap the inner string.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl TryFrom<String> for Email {
    type Error = FieldError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Email> for String {
    fn from(value: Email) -> Self {
        value.0
    }
}

impl AsRef<str> for Email {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Deref for Email {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Opaque string that redacts its contents in `Debug`, `Display`, and `Serialize`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct SecretString(String);

impl SecretString {
    /// Wrap `value` without further validation.
    pub fn new(value: impl Into<String>) -> Result<Self, FieldError> {
        Ok(Self(value.into()))
    }

    /// Unwrap the inner string.
    pub fn into_inner(self) -> String {
        self.0
    }

    /// Borrow the secret in plaintext.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SecretString {
    type Error = FieldError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for SecretString {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(REDACTED)
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<const MIN: usize, const MAX: usize> crate::dump::Dump for ConstrainedString<MIN, MAX> {}
impl<const MIN: i64, const MAX: i64> crate::dump::Dump for BoundedI64<MIN, MAX> {}
impl crate::dump::Dump for PositiveInt {}
impl crate::dump::Dump for Email {}
impl crate::dump::Dump for SecretString {}

#[cfg(test)]
mod tests {
    use super::*;

    type Username = ConstrainedString<2, 8>;
    type Percent = BoundedI64<0, 100>;

    #[test]
    fn constrained_string_success_and_char_length() {
        let value = Username::new("éé").unwrap();
        assert_eq!(value.as_ref(), "éé");
        assert_eq!(value.into_inner(), "éé");
        assert_eq!(Username::new("é").unwrap_err().code, "string_too_short");
        assert_eq!(
            Username::new("abcdefghij").unwrap_err().code,
            "string_too_long"
        );
    }

    #[test]
    fn constrained_string_serde_validates() {
        let value: Username = serde_json::from_str("\"abcd\"").unwrap();
        assert_eq!(&*value, "abcd");
        assert_eq!(serde_json::to_string(&value).unwrap(), "\"abcd\"");
        assert!(serde_json::from_str::<Username>("\"x\"").is_err());
        assert!(serde_json::from_str::<Username>("\"toolongname\"").is_err());
    }

    #[test]
    fn bounded_i64_success_and_bounds() {
        let value = Percent::new(50).unwrap();
        assert_eq!(value.into_inner(), 50);
        assert_eq!(*Percent::new(0).unwrap(), 0);
        assert_eq!(*Percent::new(100).unwrap(), 100);
        assert_eq!(Percent::new(-1).unwrap_err().code, "greater_than_equal");
        assert_eq!(Percent::new(101).unwrap_err().code, "less_than_equal");
    }

    #[test]
    fn bounded_i64_serde_validates() {
        let value: Percent = serde_json::from_str("42").unwrap();
        assert_eq!(i64::from(value), 42);
        assert_eq!(serde_json::to_string(&value).unwrap(), "42");
        assert!(serde_json::from_str::<Percent>("-1").is_err());
        assert!(serde_json::from_str::<Percent>("101").is_err());
    }

    #[test]
    fn positive_int_rejects_zero_and_negative() {
        assert_eq!(PositiveInt::new(1).unwrap().into_inner(), 1);
        assert_eq!(PositiveInt::new(0).unwrap_err().code, "greater_than");
        assert_eq!(PositiveInt::new(-3).unwrap_err().code, "greater_than");
        assert!(serde_json::from_str::<PositiveInt>("0").is_err());
        let value: PositiveInt = serde_json::from_str("7").unwrap();
        assert_eq!(serde_json::to_string(&value).unwrap(), "7");
    }

    #[test]
    fn email_construction_and_serde() {
        let email = Email::new("user@example.com").unwrap();
        assert_eq!(email.as_ref(), "user@example.com");
        assert_eq!(email.to_string(), "user@example.com");
        assert_eq!(Email::new("nope").unwrap_err().code, "invalid_email");
        let round: Email = serde_json::from_str("\"a@b.c\"").unwrap();
        assert_eq!(&*round, "a@b.c");
        assert_eq!(serde_json::to_string(&round).unwrap(), "\"a@b.c\"");
        assert!(serde_json::from_str::<Email>("\"not-an-email\"").is_err());
    }

    #[test]
    fn secret_string_redacts_everywhere() {
        let secret = SecretString::new("hunter2").unwrap();
        assert_eq!(secret.expose_secret(), "hunter2");
        assert_eq!(format!("{secret}"), REDACTED);
        assert_eq!(format!("{secret:?}"), REDACTED);
        assert_eq!(serde_json::to_string(&secret).unwrap(), "\"**********\"");
        let loaded: SecretString = serde_json::from_str("\"hunter2\"").unwrap();
        assert_eq!(loaded.expose_secret(), "hunter2");
        assert_eq!(loaded.into_inner(), "hunter2");
    }
}
