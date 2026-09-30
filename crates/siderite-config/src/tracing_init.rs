//! Tracing subscriber installation from [`crate::LogSettings`].

use crate::error::ConfigError;
use crate::settings::LogSettings;
use tracing_subscriber::EnvFilter;

/// Install a `tracing-subscriber` using `log`.
///
/// When `RUST_LOG` is set, it is used as the filter. Otherwise `log.level` is
/// used. `log.json` selects JSON versus the default text formatter.
///
/// # Errors
/// Returns [`ConfigError::InvalidFilter`] if the chosen filter cannot be
/// parsed, or [`ConfigError::Tracing`] if a global subscriber is already
/// installed.
pub fn init_tracing(log: &LogSettings) -> Result<(), ConfigError> {
    let filter = env_filter(log)?;
    if log.json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .try_init()
            .map_err(|err| ConfigError::Tracing(err.to_string()))
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .try_init()
            .map_err(|err| ConfigError::Tracing(err.to_string()))
    }
}

pub(crate) fn env_filter(log: &LogSettings) -> Result<EnvFilter, ConfigError> {
    if std::env::var_os("RUST_LOG").is_some() {
        EnvFilter::try_from_default_env().map_err(|err| ConfigError::InvalidFilter(err.to_string()))
    } else {
        EnvFilter::try_new(log.level.as_str())
            .map_err(|err| ConfigError::InvalidFilter(err.to_string()))
    }
}
