//! Users and analytics on two databases.
//!
//! `USERS_DATABASE_URL` (default a SQLite file) and `ANALYTICS_DATABASE_URL`
//! (a SQLite file, or `postgres://...`) select the databases.

use polyglot::{app, provision, registry};
use siderite::ServerError;
use siderite_cli::connect_url;

async fn connect(var: &str, default: &str) -> Result<siderite::orm::Db, ServerError> {
    let url = std::env::var(var).unwrap_or_else(|_| default.to_owned());
    connect_url(&url)
        .await
        .map_err(|err| ServerError::Configuration(format!("{var}: {err}")))
}

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    let users = connect("USERS_DATABASE_URL", "sqlite://polyglot_users.db?mode=rwc").await?;
    let analytics = connect(
        "ANALYTICS_DATABASE_URL",
        "sqlite://polyglot_analytics.db?mode=rwc",
    )
    .await?;
    let databases = registry(users, analytics);
    provision(&databases)
        .await
        .map_err(|err| ServerError::Configuration(err.to_string()))?;
    let addr = std::env::var("ADDR").unwrap_or_else(|_| "127.0.0.1:8000".to_owned());
    app(databases).run(&addr).await
}
