//! The siderite command line.
//!
//! Two entry points:
//!
//! * [`AppCli`] is the command line of an **application binary**. It needs the
//!   application's [`App`](siderite_core::App) and model metadata, so it can
//!   serve the app (`runserver`), list its routes, validate it (`check`), open
//!   a database shell and run every migration command including
//!   `makemigrations`.
//! * [`run`] is the standalone `siderite` binary. It only needs a database, so
//!   it offers `migrate`, `rollback`, `showmigrations` and `squashmigrations`
//!   on JSON migration files. Its `postgres` and `mysql` features add those
//!   backends (SQLite is always available); the database is picked by URL
//!   scheme (`sqlite:`, `postgres://`, `mysql://`).

#![forbid(unsafe_code)]

mod app_cli;
mod args;
pub mod check;
pub mod connect;
pub mod dbshell;
mod error;
#[cfg(test)]
mod fixtures;
pub mod routes;
pub mod settings;
mod standalone;

pub use app_cli::AppCli;
pub use check::{CheckIssue, CheckLevel, check};
pub use connect::connect_url;
pub use dbshell::ShellCommand;
pub use error::CliError;
pub use routes::{RouteRow, render_routes, route_table};
pub use settings::{CliSettings, DEFAULT_ADDR};
pub use standalone::run;
