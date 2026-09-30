//! MongoDB-backed todo API.

use siderite::prelude::*;
use siderite_cli::AppCli;
use todo_mongo::{Todo, app, open_db};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let url = std::env::var("MONGODB_URL")
        .unwrap_or_else(|_| "mongodb://127.0.0.1:27017/todos?directConnection=true".into());
    let database = std::env::var("MONGODB_DATABASE").unwrap_or_else(|_| "todos".into());
    let db = match open_db(&url, &database).await {
        Ok(db) => db,
        Err(err) => {
            eprintln!("{err}");
            return std::process::ExitCode::from(1);
        }
    };
    AppCli::new(move || app(db.clone()))
        .models(&[Todo::META])
        .run()
        .await
}
