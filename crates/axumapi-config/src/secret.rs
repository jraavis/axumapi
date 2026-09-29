//! Opaque wrapper that never prints its contents.

use serde::Deserialize;
use std::fmt;

/// Printed in place of a [`Secret`] value.
pub(crate) const REDACTED: &str = "[REDACTED]";

/// A value that is never shown by [`std::fmt::Debug`] or [`std::fmt::Display`].
///
/// Deserialize is transparent: the wrapper is invisible to serde, so a
/// `Secret<String>` reads as a JSON/TOML string. There is no [`serde::Serialize`]
/// implementation, so secrets cannot be written back out through serde.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wrap `value`.
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    /// Borrow the inner value.
    ///
    /// Callers must not log, display, or otherwise persist the returned
    /// reference.
    pub const fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[test]
    fn debug_and_display_are_redacted() {
        let secret = Secret::new(String::from("super-secret-password"));
        assert_eq!(format!("{secret:?}"), REDACTED);
        assert_eq!(format!("{secret}"), REDACTED);
        assert_eq!(secret.expose(), "super-secret-password");
        assert!(!format!("{secret:?}").contains("super-secret"));
        assert!(!format!("{secret}").contains("super-secret"));
    }

    #[test]
    fn deserialize_is_transparent() {
        #[derive(Deserialize)]
        struct Wrap {
            token: Secret<String>,
        }
        let wrap: Wrap = serde_json::from_str(r#"{"token":"abc"}"#).unwrap();
        assert_eq!(wrap.token.expose(), "abc");
        assert_eq!(format!("{:?}", wrap.token), REDACTED);
    }
}
