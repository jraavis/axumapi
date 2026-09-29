//! SQLite-backed todo API.

use todo_sqlite::{app, open_db};

#[tokio::main]
async fn main() -> Result<(), axumapi::ServerError> {
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://todo.db?mode=rwc".into());
    let db = open_db(&url)
        .await
        .map_err(|err| axumapi::ServerError::Configuration(err.to_string()))?;
    let addr = std::env::var("ADDR").unwrap_or_else(|_| "127.0.0.1:8000".to_owned());
    app(db).run(&addr).await
}
