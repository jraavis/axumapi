//! MongoDB-backed todo API.

use todo_mongo::{app, open_db};

#[tokio::main]
async fn main() -> Result<(), axumapi::ServerError> {
    let url = std::env::var("MONGODB_URL")
        .unwrap_or_else(|_| "mongodb://127.0.0.1:27017/todos?directConnection=true".into());
    let database = std::env::var("MONGODB_DATABASE").unwrap_or_else(|_| "todos".into());
    let db = open_db(&url, &database)
        .await
        .map_err(|err| axumapi::ServerError::Configuration(err.to_string()))?;
    let addr = std::env::var("ADDR").unwrap_or_else(|_| "127.0.0.1:8000".to_owned());
    app(db).run(&addr).await
}
