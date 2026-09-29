//! Strongly typed configuration sections.

use crate::secret::Secret;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Default bind address used when no file, env, or override sets `server.addr`.
pub const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:8000";

/// Default application name.
pub const DEFAULT_APP_NAME: &str = "axumapi";

/// Default tracing filter when `RUST_LOG` is unset.
pub const DEFAULT_LOG_LEVEL: &str = "info";

/// Default in-memory cache capacity.
pub const DEFAULT_CACHE_MAX_ENTRIES: usize = 1024;

/// Complete application settings, produced by [`crate::ConfigBuilder`] or
/// [`crate::load`].
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Settings {
    /// Application identity.
    #[serde(default)]
    pub app: AppSettings,
    /// HTTP bind address.
    #[serde(default)]
    pub server: ServerSettings,
    /// Named database connections. The `"default"` alias is the conventional
    /// primary database.
    #[serde(default)]
    pub databases: BTreeMap<String, DatabaseSettings>,
    /// Cache backend.
    #[serde(default)]
    pub cache: CacheSettings,
    /// Tracing / logging.
    #[serde(default)]
    pub log: LogSettings,
    /// Signing / session key, if the application uses one.
    #[serde(default)]
    pub secret_key: Option<Secret<String>>,
}

/// Application identity and debug flag.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AppSettings {
    /// Application name. Defaults to [`DEFAULT_APP_NAME`].
    #[serde(default = "default_app_name")]
    pub name: String,
    /// When `true`, applications may enable extra diagnostics. Defaults to
    /// `false`.
    #[serde(default)]
    pub debug: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            name: default_app_name(),
            debug: false,
        }
    }
}

fn default_app_name() -> String {
    DEFAULT_APP_NAME.to_owned()
}

/// HTTP server bind settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerSettings {
    /// Address passed to the listener, for example `"127.0.0.1:8000"`.
    #[serde(default = "default_server_addr")]
    pub addr: String,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            addr: default_server_addr(),
        }
    }
}

fn default_server_addr() -> String {
    DEFAULT_SERVER_ADDR.to_owned()
}

/// Connection settings for one database alias.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct DatabaseSettings {
    /// Connection URL. Never printed by [`std::fmt::Debug`].
    pub url: Secret<String>,
    /// Optional pool size. `None` leaves the backend default in place.
    #[serde(default)]
    pub max_connections: Option<u32>,
}

/// Cache backend settings.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CacheSettings {
    /// Optional cache URL (for example a Redis URL). Never printed by [`std::fmt::Debug`].
    #[serde(default)]
    pub url: Option<Secret<String>>,
    /// Maximum in-memory entries. Defaults to [`DEFAULT_CACHE_MAX_ENTRIES`].
    #[serde(default = "default_cache_max_entries")]
    pub max_entries: usize,
}

impl Default for CacheSettings {
    fn default() -> Self {
        Self {
            url: None,
            max_entries: DEFAULT_CACHE_MAX_ENTRIES,
        }
    }
}

fn default_cache_max_entries() -> usize {
    DEFAULT_CACHE_MAX_ENTRIES
}

/// Tracing subscriber settings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LogSettings {
    /// Filter directive used when `RUST_LOG` is unset. Defaults to
    /// [`DEFAULT_LOG_LEVEL`].
    #[serde(default = "default_log_level")]
    pub level: String,
    /// When `true`, events are formatted as JSON. Defaults to `false`.
    #[serde(default)]
    pub json: bool,
}

impl Default for LogSettings {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            json: false,
        }
    }
}

fn default_log_level() -> String {
    DEFAULT_LOG_LEVEL.to_owned()
}

/// JSON object merged as the lowest-precedence provider.
pub(crate) fn default_values() -> serde_json::Value {
    serde_json::json!({
        "app": {
            "name": DEFAULT_APP_NAME,
            "debug": false,
        },
        "server": {
            "addr": DEFAULT_SERVER_ADDR,
        },
        "databases": {},
        "cache": {
            "max_entries": DEFAULT_CACHE_MAX_ENTRIES,
        },
        "log": {
            "level": DEFAULT_LOG_LEVEL,
            "json": false,
        },
    })
}
