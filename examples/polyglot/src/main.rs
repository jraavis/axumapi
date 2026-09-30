//! Users and analytics on two databases.
//!
//! `USERS_DATABASE_URL` (default a SQLite file) and `ANALYTICS_DATABASE_URL`
//! (a SQLite file, or `postgres://...`) select the databases.

use polyglot::{all_models, app, provision, registry};
use siderite::ServerError;
use siderite_cli::{AppCli, connect_url};

async fn connect(var: &str, default: &str) -> Result<siderite::orm::Db, ServerError> {
    let url = std::env::var(var).unwrap_or_else(|_| default.to_owned());
    connect_url(&url)
        .await
        .map_err(|err| ServerError::Configuration(format!("{var}: {err}")))
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let fail = |err: ServerError| {
        eprintln!("{err}");
        std::process::ExitCode::from(1)
    };
    let users = match connect("USERS_DATABASE_URL", "sqlite://polyglot_users.db?mode=rwc").await {
        Ok(db) => db,
        Err(err) => return fail(err),
    };
    let analytics = match connect(
        "ANALYTICS_DATABASE_URL",
        "sqlite://polyglot_analytics.db?mode=rwc",
    )
    .await
    {
        Ok(db) => db,
        Err(err) => return fail(err),
    };
    let databases = registry(users, analytics);
    if let Err(err) = provision(&databases).await {
        eprintln!("{err}");
        return std::process::ExitCode::from(1);
    }
    AppCli::new(move || app(databases.clone()))
        .models(&all_models())
        .run()
        .await
}
