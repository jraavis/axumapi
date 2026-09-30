//! Blog API command line.
//!
//! ```text
//! export DATABASE_URL=postgres://siderite:siderite@127.0.0.1:55432/siderite
//! cargo run -p blog_postgres -- migrate
//! ADDR=127.0.0.1:18080 cargo run -p blog_postgres -- run
//! ```

use blog_postgres::{DATABASE_URL_ENV, MIGRATIONS_DIR, app, models, receivers};
use siderite_cli::{AppCli, CliSettings};
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let mut settings = CliSettings::new();
    if let Ok(url) = std::env::var(DATABASE_URL_ENV) {
        settings = settings.database("default", url);
    }
    AppCli::new(app)
        .models(&models::all_models())
        .settings(settings)
        .migrations_dir(MIGRATIONS_DIR)
        .configure_db(|_, db| db.with_signals(receivers::signals()))
        .run()
        .await
}
