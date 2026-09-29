//! The settings the command line needs, as plain values.
//!
//! [`CliSettings`] is a thin adapter: it carries only the listen address and
//! the database URLs, so this crate does not depend on the configuration
//! crate's `Settings` type. The application (or the configuration crate) maps
//! its settings into it, for example from `server.addr` and
//! `databases.<alias>.url`.

use std::collections::BTreeMap;
use std::fmt;

/// Listen address used when neither a flag, `ADDR` nor the settings give one.
pub const DEFAULT_ADDR: &str = "127.0.0.1:8000";

/// Alias of the database that holds the application's models.
pub const DEFAULT_DATABASE: &str = "default";

/// Listen address and database URLs for the command line.
///
/// `Debug` prints the database aliases only: URLs may contain passwords.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct CliSettings {
    addr: Option<String>,
    databases: BTreeMap<String, String>,
}

impl CliSettings {
    /// Empty settings: no address (so [`DEFAULT_ADDR`] applies), no databases.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the configured listen address (`server.addr`).
    #[must_use]
    pub fn addr(mut self, addr: impl Into<String>) -> Self {
        self.addr = Some(addr.into());
        self
    }

    /// Register the database `alias` at `url`.
    #[must_use]
    pub fn database(mut self, alias: impl Into<String>, url: impl Into<String>) -> Self {
        self.databases.insert(alias.into(), url.into());
        self
    }

    /// Replace all databases (`alias -> url`).
    #[must_use]
    pub fn databases(mut self, databases: BTreeMap<String, String>) -> Self {
        self.databases = databases;
        self
    }

    /// The configured listen address, if any.
    pub fn configured_addr(&self) -> Option<&str> {
        self.addr.as_deref()
    }

    /// Database URLs by alias.
    pub fn database_urls(&self) -> &BTreeMap<String, String> {
        &self.databases
    }

    /// URL of the database `alias`.
    pub fn database_url(&self, alias: &str) -> Option<&str> {
        self.databases.get(alias).map(String::as_str)
    }

    /// Resolve the listen address: `--addr` flag, then the `ADDR`
    /// environment variable, then the configured address, then
    /// [`DEFAULT_ADDR`]. Empty values count as unset.
    pub fn resolve_addr(&self, flag: Option<&str>, env: Option<&str>) -> String {
        [flag, env, self.addr.as_deref()]
            .into_iter()
            .flatten()
            .find(|addr| !addr.trim().is_empty())
            .unwrap_or(DEFAULT_ADDR)
            .to_owned()
    }
}

impl fmt::Debug for CliSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CliSettings")
            .field("addr", &self.addr)
            .field("databases", &self.databases.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_precedence_is_flag_env_settings_default() {
        let settings = CliSettings::new().addr("0.0.0.0:9000");
        assert_eq!(
            settings.resolve_addr(Some("1.1.1.1:1"), Some("2.2.2.2:2")),
            "1.1.1.1:1"
        );
        assert_eq!(settings.resolve_addr(None, Some("2.2.2.2:2")), "2.2.2.2:2");
        assert_eq!(settings.resolve_addr(None, None), "0.0.0.0:9000");
        assert_eq!(CliSettings::new().resolve_addr(None, None), DEFAULT_ADDR);
    }

    #[test]
    fn empty_values_are_unset() {
        let settings = CliSettings::new().addr("0.0.0.0:9000");
        assert_eq!(settings.resolve_addr(Some(""), Some("  ")), "0.0.0.0:9000");
        assert_eq!(
            CliSettings::new().addr("").resolve_addr(None, None),
            DEFAULT_ADDR
        );
    }

    #[test]
    fn debug_never_prints_urls() {
        let settings = CliSettings::new().database("default", "postgres://user:hunter2@db/app");
        let shown = format!("{settings:?}");
        assert!(shown.contains("default"));
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("postgres://"), "{shown}");
    }

    #[test]
    fn database_lookup() {
        let settings = CliSettings::new()
            .database("default", "sqlite::memory:")
            .database("replica", "sqlite::memory:");
        assert_eq!(settings.database_url("default"), Some("sqlite::memory:"));
        assert_eq!(settings.database_url("other"), None);
        assert_eq!(settings.database_urls().len(), 2);
    }
}
