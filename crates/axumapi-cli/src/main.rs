//! `axumapi` CLI: apply JSON migrations, squash them, and show their status.
//!
//! `makemigrations` needs compiled [`axumapi_orm::ModelMeta`] and is invoked
//! from the application binary via [`axumapi_migrations::cli::run`].

#![forbid(unsafe_code)]

use axumapi_backends::sqlite::SqliteBackend;
use axumapi_migrations::cli::{self, ExitCode};
use axumapi_orm::Db;
use std::env;
use std::path::PathBuf;
use std::process::ExitCode as ProcessExit;

#[tokio::main]
async fn main() -> ProcessExit {
    match run().await {
        Ok(code) => ProcessExit::from(code.0),
        Err(err) => {
            eprintln!("{err}");
            ProcessExit::from(1)
        }
    }
}

async fn run() -> Result<ExitCode, axumapi_migrations::MigrationError> {
    let raw: Vec<String> = env::args().skip(1).collect();
    let (global, rest) = split_global(&raw)?;
    if global.help {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }
    let command = rest.first().map(String::as_str).unwrap_or("");
    if command.is_empty() {
        print_help();
        return Ok(ExitCode::USAGE);
    }
    if command == "makemigrations" {
        print_makemigrations_hint();
        return Ok(ExitCode::USAGE);
    }
    if matches!(
        command,
        "migrate" | "rollback" | "showmigrations" | "squashmigrations"
    ) {
        let dir = global
            .migrations_dir
            .unwrap_or_else(|| PathBuf::from("migrations"));
        let db = connect(&global.database_url, command).await?;
        return cli::run(&[], &db, &dir, rest).await;
    }
    eprintln!("unknown command `{command}` (try --help)");
    Ok(ExitCode::USAGE)
}

struct Global {
    database_url: Option<String>,
    migrations_dir: Option<PathBuf>,
    help: bool,
}

fn split_global(
    args: &[String],
) -> Result<(Global, Vec<String>), axumapi_migrations::MigrationError> {
    let mut global = Global {
        database_url: env::var("DATABASE_URL").ok(),
        migrations_dir: None,
        help: false,
    };
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--help" || a == "-h" {
            global.help = rest.is_empty();
            rest.push(a.clone());
            i += 1;
            continue;
        }
        if a == "--database-url" {
            i += 1;
            let v = args.get(i).ok_or_else(|| {
                axumapi_migrations::MigrationError::usage("--database-url requires a value")
            })?;
            global.database_url = Some(v.clone());
            i += 1;
            continue;
        }
        if let Some(v) = a.strip_prefix("--database-url=") {
            global.database_url = Some(v.to_owned());
            i += 1;
            continue;
        }
        if a == "--migrations-dir" {
            i += 1;
            let v = args.get(i).ok_or_else(|| {
                axumapi_migrations::MigrationError::usage("--migrations-dir requires a value")
            })?;
            global.migrations_dir = Some(PathBuf::from(v));
            i += 1;
            continue;
        }
        if let Some(v) = a.strip_prefix("--migrations-dir=") {
            global.migrations_dir = Some(PathBuf::from(v));
            i += 1;
            continue;
        }
        rest.push(a.clone());
        i += 1;
    }
    Ok((global, rest))
}

async fn connect(
    url: &Option<String>,
    command: &str,
) -> Result<Db, axumapi_migrations::MigrationError> {
    if command == "squashmigrations" {
        if let Some(url) = url.as_deref() {
            return connect_url(url).await;
        }
        let backend = SqliteBackend::connect("sqlite::memory:")
            .await
            .map_err(axumapi_orm::OrmError::from)?;
        return Ok(Db::new(backend));
    }
    let Some(url) = url.as_deref() else {
        return Err(axumapi_migrations::MigrationError::usage(
            "missing --database-url or DATABASE_URL",
        ));
    };
    connect_url(url).await
}

async fn connect_url(url: &str) -> Result<Db, axumapi_migrations::MigrationError> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("postgres://") || lower.starts_with("postgresql://") {
        return Err(axumapi_migrations::MigrationError::usage(
            "PostgreSQL execution is not implemented yet (no PostgresBackend in axumapi-backends). \
             Use a sqlite:// URL, or generate PostgreSQL DDL through axumapi_migrations::schema_editor.",
        ));
    }
    if !(lower.starts_with("sqlite:") || lower.starts_with("sqlite://")) {
        return Err(axumapi_migrations::MigrationError::usage(format!(
            "unsupported database URL scheme (expected sqlite://): {url}"
        )));
    }
    let backend = SqliteBackend::connect(url)
        .await
        .map_err(axumapi_orm::OrmError::from)?;
    Ok(Db::new(backend))
}

fn print_help() {
    println!(
        "\
axumapi {} — schema migrations

Commands:
  makemigrations              (must be run from the app binary; see below)
  migrate [TARGET]            Apply migrations
  rollback [--steps N|TARGET] Unapply migrations
  showmigrations              List migrations and applied status
  squashmigrations FROM TO    Collapse a range into one migration

Options:
  --database-url URL          Connection string (or DATABASE_URL)
  --migrations-dir DIR        Migration JSON directory (default: migrations)
  --dry-run                   Print SQL without executing
  --name SLUG                 Slug for a new / squashed migration
  --empty                     Write an empty migration (app binary only)
  --steps N                   Rollback N applied migrations
  --help                      Show this help
",
        env!("CARGO_PKG_VERSION")
    );
    print_makemigrations_hint();
}

fn print_makemigrations_hint() {
    println!(
        "\
makemigrations needs compiled model metadata. Call it from your application binary:

    axumapi_migrations::cli::run(
        &[User::META, Post::META],
        &db,
        std::path::Path::new(\"migrations\"),
        std::env::args().skip(1),
    )
    .await?;
"
    );
}
