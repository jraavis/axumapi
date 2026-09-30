//! SQLite-backed todo API.

use siderite::prelude::*;
use siderite_cli::{AppCli, CliSettings};
use todo_sqlite::{Todo, User, app, open_db};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://todo.db?mode=rwc".into());
    let db = match open_db(&url).await {
        Ok(db) => db,
        Err(err) => {
            eprintln!("{err}");
            return std::process::ExitCode::from(1);
        }
    };
    AppCli::new(move || app(db.clone()))
        .models(&[User::META, Todo::META])
        .settings(CliSettings::new().database("default", url))
        .run()
        .await
}
