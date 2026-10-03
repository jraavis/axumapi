//! A scratch database per live test.
//!
//! The live Postgres/MySQL tests share one server, and migrations record
//! their history in a fixed table (`siderite_migrations`). Sharing a database
//! would make parallel tests clobber each other's history rows, so each test
//! gets its own random database and drops it afterwards.

use siderite_orm::Db;
use uuid::Uuid;

/// The env var read for `scheme`, in priority order.
fn url_for(scheme: &str) -> Option<String> {
    ["MYSQL_URL", "DATABASE_URL", "POSTGRES_URL"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|url| url.to_ascii_lowercase().starts_with(scheme))
}

/// A database handle on a private database, dropped by [`ScratchDb::cleanup`].
pub struct ScratchDb {
    /// Handle to the private database.
    pub db: Db,
    /// Connection to the server's default database, for `DROP DATABASE`.
    admin: Admin,
    name: String,
    url: String,
}

/// Pool for the server that owns the scratch database.
enum Admin {
    MySql(sqlx::MySqlPool),
    Postgres(sqlx::PgPool),
}

impl ScratchDb {
    /// A private database on the MySQL or PostgreSQL server named by
    /// `MYSQL_URL` / `DATABASE_URL`; missing configuration is an error.
    ///
    /// # Errors
    /// Returns backend errors when the server cannot create the database.
    pub async fn mysql() -> Result<Option<Self>, siderite_orm::OrmError> {
        let Some(url) = url_for("mysql") else {
            return Err(siderite_orm::BackendError::Connection(
                "live migration tests require MYSQL_URL".to_owned(),
            )
            .into());
        };
        let name = format!("siderite_mig_{}", Uuid::new_v4().simple());
        let admin = sqlx::MySqlPool::connect(&url).await.map_err(err)?;
        sqlx::query(&format!("CREATE DATABASE `{name}`"))
            .execute(&admin)
            .await
            .map_err(err)?;
        let url = database_url(&url, &name);
        let db = Db::new(
            siderite_backends::mysql::MySqlBackend::connect(&url)
                .await
                .map_err(siderite_orm::OrmError::Backend)?,
        );
        Ok(Some(Self {
            db,
            admin: Admin::MySql(admin),
            name,
            url,
        }))
    }

    /// A private database on the PostgreSQL server named by `DATABASE_URL`,
    /// with an error when it is not set.
    ///
    /// # Errors
    /// Returns backend errors when the server cannot create the database.
    pub async fn postgres() -> Result<Option<Self>, siderite_orm::OrmError> {
        let Some(url) = url_for("postgres") else {
            return Err(siderite_orm::BackendError::Connection(
                "live migration tests require DATABASE_URL".to_owned(),
            )
            .into());
        };
        let name = format!("siderite_mig_{}", Uuid::new_v4().simple());
        let admin = sqlx::PgPool::connect(&url).await.map_err(err)?;
        sqlx::query(&format!("CREATE DATABASE \"{name}\""))
            .execute(&admin)
            .await
            .map_err(err)?;
        let url = database_url(&url, &name);
        let db = Db::new(
            siderite_backends::postgres::PgBackend::connect(&url)
                .await
                .map_err(siderite_orm::OrmError::Backend)?,
        );
        Ok(Some(Self {
            db,
            admin: Admin::Postgres(admin),
            name,
            url,
        }))
    }

    /// Private URL for child-process tests; never print it.
    pub fn connection_url(&self) -> &str {
        &self.url
    }

    /// Drop the private database.
    ///
    /// # Errors
    /// Returns backend errors when the server cannot drop the database.
    pub async fn cleanup(self) -> Result<(), siderite_orm::OrmError> {
        match &self.admin {
            Admin::MySql(pool) => {
                sqlx::query(&format!("DROP DATABASE `{}`", self.name))
                    .execute(pool)
                    .await
                    .map_err(err)?;
            }
            Admin::Postgres(pool) => {
                // `WITH (FORCE)` terminates the test's own pool sessions,
                // which otherwise keep the database "in use".
                sqlx::query(&format!("DROP DATABASE \"{}\" WITH (FORCE)", self.name))
                    .execute(pool)
                    .await
                    .map_err(err)?;
            }
        }
        Ok(())
    }
}

fn err(e: sqlx::Error) -> siderite_orm::OrmError {
    siderite_orm::OrmError::Backend(siderite_orm::BackendError::Connection(e.to_string()))
}

/// `server/name`, replacing any database in `url`. The caller only reaches
/// this with a URL that named a database, so a missing path cannot happen.
fn database_url(url: &str, name: &str) -> String {
    match url.rsplit_once('/') {
        Some((server, _)) => format!("{server}/{name}"),
        None => format!("{url}/{name}"),
    }
}
