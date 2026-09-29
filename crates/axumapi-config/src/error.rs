//! Configuration errors. Messages never include secret values.

use crate::secret::REDACTED;
use figment::error::Kind;
use std::path::PathBuf;
use thiserror::Error;

/// Failure while loading settings or installing tracing.
///
/// [`std::fmt::Display`] and [`std::fmt::Debug`] never include values of secret fields (`url`,
/// `secret_key`, passwords, tokens, and similar keys).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// Figment could not merge or extract configuration.
    #[error("{0}")]
    Extract(String),
    /// A file passed to [`crate::ConfigBuilder::file`] does not exist.
    #[error("required configuration file not found: {}", .0.display())]
    MissingFile(PathBuf),
    /// `log.level` or `RUST_LOG` is not a valid tracing filter.
    #[error("invalid tracing filter: {0}")]
    InvalidFilter(String),
    /// Installing the global tracing subscriber failed.
    #[error("tracing subscriber could not be initialized: {0}")]
    Tracing(String),
}

impl From<figment::Error> for ConfigError {
    fn from(err: figment::Error) -> Self {
        let message = err
            .into_iter()
            .map(format_figment_error)
            .collect::<Vec<_>>()
            .join("; ");
        Self::Extract(message)
    }
}

fn format_figment_error(err: figment::Error) -> String {
    let path = err.path.join(".");
    let kind = format_kind(&err.kind, path_is_secret(&err.path));
    if path.is_empty() {
        kind
    } else {
        format!("{kind} at `{path}`")
    }
}

fn path_is_secret(path: &[String]) -> bool {
    path.iter().any(|segment| is_secret_key(segment))
}

fn is_secret_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "url"
            | "secret_key"
            | "password"
            | "passwd"
            | "token"
            | "secret"
            | "api_key"
            | "apikey"
            | "authorization"
            | "cookie"
            | "credential"
            | "credentials"
            | "private_key"
    )
}

fn format_kind(kind: &Kind, redact: bool) -> String {
    if !redact {
        return kind.to_string();
    }
    match kind {
        Kind::InvalidType(_, expected) => {
            format!("invalid type: expected {expected}, value {REDACTED}")
        }
        Kind::InvalidValue(_, expected) => {
            format!("invalid value: expected {expected}, value {REDACTED}")
        }
        Kind::Message(_) => format!("invalid value {REDACTED}"),
        Kind::Unsupported(_) => format!("unsupported value {REDACTED}"),
        Kind::UnsupportedKey(_, expected) => {
            format!("unsupported key, expected {expected}")
        }
        Kind::InvalidLength(len, expected) => {
            format!("invalid length {len}, expected {expected}")
        }
        Kind::UnknownVariant(_, expected) => {
            format!("unknown variant, expected one of {expected:?}")
        }
        Kind::UnknownField(field, expected) => {
            format!("unknown field `{field}`, expected one of {expected:?}")
        }
        Kind::MissingField(field) => format!("missing field `{field}`"),
        Kind::DuplicateField(field) => format!("duplicate field `{field}`"),
        Kind::ISizeOutOfRange(_) | Kind::USizeOutOfRange(_) => kind.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use figment::error::Actual;

    #[test]
    fn secret_paths_are_detected() {
        assert!(path_is_secret(&[
            "databases".into(),
            "default".into(),
            "url".into()
        ]));
        assert!(path_is_secret(&["secret_key".into()]));
        assert!(path_is_secret(&["cache".into(), "url".into()]));
        assert!(!path_is_secret(&["server".into(), "addr".into()]));
        assert!(!path_is_secret(&["app".into(), "name".into()]));
    }

    #[test]
    fn redacted_kind_omits_string_payload() {
        let kind = Kind::InvalidType(
            Actual::Str("postgres://user:hunter2@db/app".into()),
            "u32".into(),
        );
        let rendered = format_kind(&kind, true);
        assert!(rendered.contains(REDACTED));
        assert!(!rendered.contains("hunter2"));
        assert!(!rendered.contains("postgres://"));
    }
}
