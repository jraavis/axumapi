//! A complete blog API on PostgreSQL.
//!
//! Users register, exchange their password for a bearer token (OAuth2 password
//! flow) and write posts and comments; readers browse paginated, tagged posts.
//! Model signals keep an audit trail of post changes.
//!
//! | Module | Content |
//! |---|---|
//! | [`models`] | `User`, `Post`, `Tag`, `Comment`, `AccessToken`, `AuditEntry` |
//! | [`auth`] | token endpoint, [`auth::CurrentUser`], scopes |
//! | [`users`], [`posts`], [`comments`] | the HTTP routes |
//! | [`receivers`] | `post_save` / `post_delete` audit receivers |
//! | [`pagination`] | `?page=&per_page=` and the page envelope |
//!
//! The binary (`src/main.rs`) drives everything through `AppCli`:
//! `makemigrations`, `migrate`, `check`, `routes` and `run`.

pub mod auth;
pub mod comments;
pub mod db;
pub mod models;
pub mod pagination;
pub mod posts;
pub mod receivers;
pub mod users;

use siderite::prelude::*;

/// Environment variable holding the PostgreSQL URL.
pub const DATABASE_URL_ENV: &str = "DATABASE_URL";

/// Directory of the generated migrations (next to this crate's manifest).
pub const MIGRATIONS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/migrations");

/// The application. The databases are registered by the caller
/// (`AppCli` from settings, tests through the test client builder).
pub fn app() -> App {
    App::new()
        .title("Blog")
        .version("1.0.0")
        .description("Posts, tags and comments with OAuth2 password-flow authentication.")
        .routes(auth::routes())
        .routes(users::routes())
        .routes(posts::routes())
        .routes(comments::routes())
}
