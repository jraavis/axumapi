//! Isolated live fixtures shared by SQLx and explicit native test runs.

use super::{Db, MySqlBackend, MySqlPool, MySqlPoolOptions, SCHEMA};
#[cfg(feature = "mysql-native")]
use siderite_backends::mysql::native::{NativeMySqlBackend, NativeMySqlOptions};

/// A private database and independent administrative connection.
pub(super) struct TestDb {
    pub(super) db: Db,
    pub(super) admin: MySqlPool,
    pub(super) name: String,
    #[cfg(feature = "mysql-native")]
    native: Option<NativeMySqlBackend>,
}

impl TestDb {
    /// Create the schema using the explicitly selected adapter.
    ///
    /// Returns:
    ///     Isolated database; missing live configuration fails explicitly.
    pub(super) async fn open() -> Option<Self> {
        let Some(url) = ["MYSQL_URL", "DATABASE_URL"]
            .into_iter()
            .filter_map(|var| std::env::var(var).ok())
            .find(|url| url.starts_with("mysql"))
        else {
            panic!("live MySQL tests require MYSQL_URL");
        };
        let name = format!("siderite_test_{}", uuid::Uuid::new_v4().simple());
        let admin = MySqlPool::connect(&url).await.unwrap();
        sqlx::query(&format!("CREATE DATABASE {name}"))
            .execute(&admin)
            .await
            .unwrap();
        let (server, _) = url.rsplit_once('/').unwrap();
        let url = format!("{server}/{name}");
        #[cfg(feature = "mysql-native")]
        let backend = native_backend(&url).await;
        #[cfg(feature = "mysql-native")]
        let db = match &backend {
            Some(backend) => Db::new(backend.clone()),
            None => sqlx_database(&url).await,
        };
        #[cfg(not(feature = "mysql-native"))]
        let db = {
            assert!(!native(), "mysql-native feature is required");
            sqlx_database(&url).await
        };
        db.execute_script(SCHEMA).await.unwrap();
        Some(Self {
            db,
            admin,
            name,
            #[cfg(feature = "mysql-native")]
            native: backend,
        })
    }

    /// Disconnect the native pool and remove only this disposable database.
    ///
    /// Returns:
    ///     Unit after the administrative DROP completes.
    pub(super) async fn cleanup(self) {
        #[cfg(feature = "mysql-native")]
        if let Some(backend) = &self.native {
            backend.close().await.unwrap();
        }
        sqlx::query(&format!("DROP DATABASE {}", self.name))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

/// Connect a constrained test user with the same adapter as the main fixture.
///
/// Args:
///     url: URL for an already-created disposable database.
///
/// Returns:
///     Database handle using the explicitly selected adapter.
pub(super) async fn database(url: &str) -> Db {
    #[cfg(feature = "mysql-native")]
    if let Some(backend) = native_backend(url).await {
        return Db::new(backend);
    }
    sqlx_database(url).await
}

async fn sqlx_database(url: &str) -> Db {
    let options = MySqlPoolOptions::new().max_connections(4);
    let backend = MySqlBackend::connect_with(url, options).await.unwrap();
    Db::new(backend)
}

fn native() -> bool {
    std::env::var_os("SIDERITE_TEST_NATIVE_MYSQL").is_some()
}

#[cfg(feature = "mysql-native")]
async fn native_backend(url: &str) -> Option<NativeMySqlBackend> {
    if !native() {
        return None;
    }
    let options = NativeMySqlOptions {
        max_connections: 4,
        ..NativeMySqlOptions::default()
    };
    Some(
        NativeMySqlBackend::connect_with(url, options)
            .await
            .unwrap(),
    )
}
