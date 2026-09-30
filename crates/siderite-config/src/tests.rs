//! Integration tests for layered configuration. Environment is isolated with
//! [`figment::Jail`].

#![allow(clippy::unwrap_used)]

use crate::LogSettings;
use crate::builder::{ConfigBuilder, load};
use crate::error::ConfigError;
use crate::secret::REDACTED;
use crate::secret::Secret;
use crate::settings::{
    DEFAULT_APP_NAME, DEFAULT_CACHE_MAX_ENTRIES, DEFAULT_LOG_LEVEL, DEFAULT_SERVER_ADDR, Settings,
};
use crate::tracing_init::env_filter;
use serde::Deserialize;
use std::path::Path;

#[allow(clippy::result_large_err)]
fn jail(f: impl FnOnce(&mut figment::Jail)) {
    figment::Jail::expect_with(|jail| {
        jail.clear_env();
        f(jail);
        Ok(())
    });
}

#[test]
fn defaults_are_documented_values() {
    jail(|_| {
        let settings = ConfigBuilder::new().build().unwrap();
        assert_eq!(settings.app.name, DEFAULT_APP_NAME);
        assert!(!settings.app.debug);
        assert_eq!(settings.server.addr, DEFAULT_SERVER_ADDR);
        assert!(settings.databases.is_empty());
        assert!(settings.cache.url.is_none());
        assert_eq!(settings.cache.max_entries, DEFAULT_CACHE_MAX_ENTRIES);
        assert_eq!(settings.log.level, DEFAULT_LOG_LEVEL);
        assert!(!settings.log.json);
        assert!(settings.secret_key.is_none());
    });
}

#[test]
fn file_overrides_defaults() {
    jail(|jail| {
        jail.create_file(
            "app.toml",
            r#"
            [app]
            name = "from-file"
            debug = true
            [server]
            addr = "0.0.0.0:1"
            [log]
            level = "debug"
            json = true
            "#,
        )
        .unwrap();
        let settings = ConfigBuilder::new().file("app.toml").build().unwrap();
        assert_eq!(settings.app.name, "from-file");
        assert!(settings.app.debug);
        assert_eq!(settings.server.addr, "0.0.0.0:1");
        assert_eq!(settings.log.level, "debug");
        assert!(settings.log.json);
    });
}

#[test]
fn env_overrides_file_and_nests_on_double_underscore() {
    jail(|jail| {
        jail.create_file(
            "app.toml",
            r#"
            [app]
            name = "from-file"
            [server]
            addr = "0.0.0.0:1"
            "#,
        )
        .unwrap();
        jail.set_env("SIDERITE_APP__NAME", "from-env");
        jail.set_env("SIDERITE_APP__DEBUG", "true");
        jail.set_env("SIDERITE_SERVER__ADDR", "0.0.0.0:2");
        let settings = ConfigBuilder::new()
            .file("app.toml")
            .env_prefix("SIDERITE_")
            .build()
            .unwrap();
        assert_eq!(settings.app.name, "from-env");
        assert!(settings.app.debug);
        assert_eq!(settings.server.addr, "0.0.0.0:2");
    });
}

#[test]
fn set_overrides_env() {
    jail(|jail| {
        jail.create_file(
            "app.toml",
            r#"
            [app]
            name = "from-file"
            "#,
        )
        .unwrap();
        jail.set_env("SIDERITE_APP__NAME", "from-env");
        let settings = ConfigBuilder::new()
            .file("app.toml")
            .env_prefix("SIDERITE_")
            .set("app.name", "from-set")
            .build()
            .unwrap();
        assert_eq!(settings.app.name, "from-set");
    });
}

#[test]
fn set_wins_even_when_called_before_other_sources() {
    jail(|jail| {
        jail.create_file("app.toml", "[server]\naddr = \"10.0.0.1:1\"\n")
            .unwrap();
        jail.set_env("ADDR", "127.0.0.1:1");
        let settings = ConfigBuilder::new()
            .set("server.addr", "0.0.0.0:9000")
            .file("app.toml")
            .env_prefix("SIDERITE_")
            .build()
            .unwrap();
        assert_eq!(settings.server.addr, "0.0.0.0:9000");
    });
}

#[test]
fn precedence_defaults_file_env_set() {
    jail(|jail| {
        jail.create_file(
            "app.toml",
            r#"
            [server]
            addr = "10.0.0.1:1"
            "#,
        )
        .unwrap();
        jail.set_env("SIDERITE_SERVER__ADDR", "10.0.0.1:2");
        let from_defaults = ConfigBuilder::new().build().unwrap();
        assert_eq!(from_defaults.server.addr, DEFAULT_SERVER_ADDR);

        let from_file = ConfigBuilder::new().file("app.toml").build().unwrap();
        assert_eq!(from_file.server.addr, "10.0.0.1:1");

        let from_env = ConfigBuilder::new()
            .file("app.toml")
            .env_prefix("SIDERITE_")
            .build()
            .unwrap();
        assert_eq!(from_env.server.addr, "10.0.0.1:2");

        let from_set = ConfigBuilder::new()
            .file("app.toml")
            .env_prefix("SIDERITE_")
            .set("server.addr", "10.0.0.1:3")
            .build()
            .unwrap();
        assert_eq!(from_set.server.addr, "10.0.0.1:3");
    });
}

#[test]
fn database_url_and_addr_aliases() {
    jail(|jail| {
        jail.create_file(
            "siderite.toml",
            r#"
            [server]
            addr = "from-file:1"
            [databases.default]
            url = "postgres://file"
            "#,
        )
        .unwrap();
        jail.set_env("DATABASE_URL", "postgres://env-alias");
        jail.set_env("ADDR", "from-addr:2");
        let settings = load().unwrap();
        assert_eq!(
            settings.databases["default"].url.expose(),
            "postgres://env-alias"
        );
        assert_eq!(settings.server.addr, "from-addr:2");
    });
}

#[test]
fn prefixed_env_overrides_database_url_alias() {
    jail(|jail| {
        jail.set_env("DATABASE_URL", "postgres://alias");
        jail.set_env("SIDERITE_DATABASES__DEFAULT__URL", "postgres://prefixed");
        let settings = load().unwrap();
        assert_eq!(
            settings.databases["default"].url.expose(),
            "postgres://prefixed"
        );
    });
}

#[test]
fn set_overrides_database_url() {
    jail(|jail| {
        jail.set_env("DATABASE_URL", "postgres://alias");
        let settings = ConfigBuilder::new()
            .env_prefix("SIDERITE_")
            .set("databases.default.url", "postgres://set")
            .build()
            .unwrap();
        assert_eq!(settings.databases["default"].url.expose(), "postgres://set");
    });
}

#[test]
fn nested_database_env_and_file() {
    jail(|jail| {
        jail.create_file(
            "app.toml",
            r#"
            [databases.default]
            url = "postgres://file"
            max_connections = 4
            [databases.replica]
            url = "postgres://replica-file"
            "#,
        )
        .unwrap();
        jail.set_env("SIDERITE_DATABASES__DEFAULT__URL", "postgres://env");
        jail.set_env("SIDERITE_DATABASES__DEFAULT__MAX_CONNECTIONS", "16");
        let settings = ConfigBuilder::new()
            .file("app.toml")
            .env_prefix("SIDERITE_")
            .build()
            .unwrap();
        assert_eq!(settings.databases["default"].url.expose(), "postgres://env");
        assert_eq!(settings.databases["default"].max_connections, Some(16));
        assert_eq!(
            settings.databases["replica"].url.expose(),
            "postgres://replica-file"
        );
    });
}

#[test]
fn secret_key_and_cache_url_are_redacted_in_debug() {
    jail(|jail| {
        jail.set_env("SIDERITE_SECRET_KEY", "signing-secret-value");
        jail.set_env("SIDERITE_CACHE__URL", "redis://:cache-password@localhost/0");
        jail.set_env(
            "SIDERITE_DATABASES__DEFAULT__URL",
            "postgres://user:dbpass@h/db",
        );
        let settings = load().unwrap();
        let debug = format!("{settings:?}");
        let display_secret = format!("{}", settings.secret_key.as_ref().unwrap());
        assert_eq!(display_secret, REDACTED);
        assert!(debug.contains(REDACTED));
        assert!(!debug.contains("signing-secret-value"));
        assert!(!debug.contains("cache-password"));
        assert!(!debug.contains("dbpass"));
        assert_eq!(
            settings.secret_key.as_ref().unwrap().expose(),
            "signing-secret-value"
        );
    });
}

#[test]
fn config_error_redacts_secret_values() {
    jail(|jail| {
        jail.set_env(
            "SIDERITE_DATABASES__DEFAULT__URL",
            "postgres://user:hunter2@db/app",
        );
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct Probe {
            databases: std::collections::BTreeMap<String, ProbeDb>,
        }
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct ProbeDb {
            url: u32,
        }
        let err = ConfigBuilder::new()
            .env_prefix("SIDERITE_")
            .extract::<Probe>()
            .unwrap_err();
        let display = err.to_string();
        let debug = format!("{err:?}");
        assert!(display.contains(REDACTED) || display.contains("url"));
        assert!(!display.contains("hunter2"));
        assert!(!debug.contains("hunter2"));
        assert!(!display.contains("postgres://user:hunter2"));
    });
}

#[test]
fn config_error_redacts_secret_key_on_type_mismatch() {
    jail(|jail| {
        jail.set_env("SIDERITE_SECRET_KEY", "top-secret-key-material");
        #[derive(Debug, Deserialize)]
        #[allow(dead_code)]
        struct Probe {
            secret_key: u32,
        }
        let err = ConfigBuilder::new()
            .env_prefix("SIDERITE_")
            .extract::<Probe>()
            .unwrap_err();
        let text = format!("{err} / {err:?}");
        assert!(!text.contains("top-secret-key-material"));
        assert!(text.contains(REDACTED) || text.contains("secret_key"));
    });
}

#[test]
fn required_file_missing_is_an_error() {
    jail(|_| {
        let err = ConfigBuilder::new()
            .file("missing.toml")
            .build()
            .unwrap_err();
        match err {
            ConfigError::MissingFile(path) => {
                assert_eq!(path, Path::new("missing.toml"));
            }
            other => panic!("expected MissingFile, got {other}"),
        }
    });
}

#[test]
fn optional_file_missing_keeps_defaults() {
    jail(|_| {
        let settings = ConfigBuilder::new()
            .file_optional("missing.toml")
            .build()
            .unwrap();
        assert_eq!(settings.app.name, DEFAULT_APP_NAME);
    });
}

#[test]
fn load_reads_siderite_toml() {
    jail(|jail| {
        jail.create_file(
            "siderite.toml",
            r#"
            [app]
            name = "loaded"
            "#,
        )
        .unwrap();
        let settings = load().unwrap();
        assert_eq!(settings.app.name, "loaded");
    });
}

#[test]
fn extract_custom_type() {
    jail(|jail| {
        jail.set_env("SIDERITE_APP__NAME", "custom");
        #[derive(Debug, Deserialize)]
        struct Slice {
            app: crate::AppSettings,
        }
        let slice: Slice = ConfigBuilder::new()
            .env_prefix("SIDERITE_")
            .extract()
            .unwrap();
        assert_eq!(slice.app.name, "custom");
    });
}

#[test]
fn rust_log_wins_over_log_level() {
    jail(|jail| {
        jail.set_env("RUST_LOG", "debug");
        let log = LogSettings {
            level: "error".into(),
            json: false,
        };
        let filter = env_filter(&log).unwrap();
        let rendered = filter.to_string();
        assert!(
            rendered.to_ascii_lowercase().contains("debug"),
            "{rendered}"
        );
    });
}

#[test]
fn log_level_used_when_rust_log_unset() {
    jail(|_| {
        let log = LogSettings {
            level: "warn".into(),
            json: false,
        };
        let filter = env_filter(&log).unwrap();
        let rendered = filter.to_string();
        assert!(rendered.to_ascii_lowercase().contains("warn"), "{rendered}");
    });
}

#[test]
fn invalid_log_level_is_an_error() {
    jail(|_| {
        let log = LogSettings {
            level: "info,module=notalevel".into(),
            json: false,
        };
        let err = env_filter(&log).unwrap_err();
        match err {
            ConfigError::InvalidFilter(_) => {}
            other => panic!("expected InvalidFilter, got {other}"),
        }
    });
}

#[test]
fn secret_debug_does_not_leak_through_settings() {
    let settings = Settings {
        secret_key: Some(Secret::new("leak-me".into())),
        ..Settings::default()
    };
    let debug = format!("{settings:?}");
    assert!(!debug.contains("leak-me"));
    assert!(debug.contains(REDACTED));
}
