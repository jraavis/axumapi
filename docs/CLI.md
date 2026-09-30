# Command line

Two entry points share the same flags and the migration commands.

| | Standalone `siderite` binary | `AppCli` in your application binary |
|---|---|---|
| Needs your `App` and models | no | yes |
| Commands | `migrate`, `rollback`, `showmigrations`, `squashmigrations` | all of those, plus `makemigrations`, `runserver`, `routes`, `check`, `dbshell` |
| Database | `--database-url` or `DATABASE_URL` | `--database-url`, settings, or `DATABASE_URL` (see below) |

The standalone binary only sees JSON migration files, so it cannot diff your models. Run `makemigrations`, `runserver`, `routes`, `check` and `dbshell` from the application binary; the standalone binary answers them with a usage hint and exit code 2.

Exit codes: `0` success, `1` failure (or `check` found an error), `2` usage error.

## Standalone binary

```bash
cargo install --path crates/siderite-cli --features postgres,mysql
siderite migrate --database-url postgres://app@localhost/app
siderite showmigrations --migrations-dir db/migrations
```

SQLite is always compiled in. PostgreSQL and MySQL are opt-in **cargo features** of `siderite-cli`: `postgres` and `mysql`. The backend is picked by URL scheme (`sqlite:`, `postgres://`, `mysql://`); a URL for a backend that was not compiled in is an error. Error messages never contain the URL, which may hold a password. `squashmigrations` only reads and writes files, so it works without a database URL.

## `AppCli`

```rust
use siderite_cli::{AppCli, CliSettings};
use siderite::prelude::*;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    AppCli::new(build_app)                       // a factory: check builds the app more than once
        .models(&[User::META, Post::META])
        .settings(
            CliSettings::new()
                .addr("0.0.0.0:8000")
                .database("default", "postgres://app@localhost/app"),
        )
        .migrations_dir("migrations")
        .run()
        .await
}
```

`CliSettings` is a thin adapter holding the listen address and the database URLs by alias. Build it from loaded configuration with `CliSettings::from(&siderite::config::load()?)`, which takes `server.addr` and every `databases.<alias>.url`, or set the fields by hand. Its `Debug` output lists aliases only, because URLs can contain passwords. `AppCli::run_from(args)` takes explicit arguments, which is handy in tests.

`runserver` connects every SQL alias and registers them as the app's `Databases`. Two hooks shape that registry:

```rust
AppCli::new(build_app)
    // Runs once per connected alias, before registration.
    .configure_db(|_alias, db| db.with_signals(receivers::signals()))
    // Routes models to aliases (see DATABASE_ROUTING.md).
    .database_router(AppRouter)
```

## Commands

| Command | What it does |
|---|---|
| `runserver [--addr ADDR]` | Connects every configured SQL database, registers each under its alias (`App::database`), and serves the app. Aliases whose URL is Redis or MongoDB are skipped: register those yourself in the factory. |
| `routes` | Prints `METHOD PATH operation_id` for every route, mounts included. |
| `check` | Validates configuration, models, migrations, routes and the backend (see below). Exits `1` when an error is found. |
| `dbshell` | Starts the database's native client. Replaces Django-style `shell`. |
| `makemigrations [--name SLUG] [--empty] [--dry-run]` | Diffs compiled models against the migration graph and writes a JSON migration. Never connects to a database. |
| `migrate [TARGET] [--dry-run]` | Applies migrations. |
| `rollback [--steps N \| TARGET] [--dry-run]` | Unapplies migrations. |
| `showmigrations` | `[X]` applied / `[ ]` pending. |
| `squashmigrations FROM TO [--name SLUG]` | Collapses a range. Never connects to a database. |

See [MIGRATIONS.md](MIGRATIONS.md) for the migration commands in detail.

### Global flags

| Flag | Meaning |
|---|---|
| `--database ALIAS` | Alias used by `migrate`, `rollback`, `showmigrations` and `dbshell` (default `default`). |
| `--database-url URL` | Database URL, overriding the settings. |
| `--migrations-dir DIR` | Directory of JSON migrations (default `migrations`). |
| `--addr ADDR` | Listen address for `runserver`. |
| `--help`, `-h` | Help. |

Flags may appear anywhere and take `--flag value` or `--flag=value`.

### Precedence

**Listen address** (`runserver`): `--addr`, then the `ADDR` environment variable, then `CliSettings::addr`, then `127.0.0.1:8000`. Empty values count as unset.

**Database** (`migrate`, `rollback`, `showmigrations`, `dbshell`): `--database-url`, then the URL configured for the selected alias, then, for the `default` alias only, `DATABASE_URL`. With none of those, the command fails with a "no database" error naming the alias.

## `check`

`check` runs without starting a server or opening a database. Each issue prints as `error: [models.E003] message` or `warning: [id] message`, followed by a summary line. Messages never contain database URLs.

| Id | Level | Meaning |
|---|---|---|
| `config.E001` | error | models are registered but no `default` database is configured |
| `config.E002` | error | a database URL has no scheme or does not parse |
| `config.E003` | error | a database URL uses an unsupported scheme |
| `models.E001` | error | two models use the same table |
| `models.E002` | error | a model name is registered more than once |
| `models.E003` | error | a model has no primary key |
| `models.E004` | error | a foreign key points to a model that is not registered |
| `models.E005` | error | a many-to-many relation (or its through model) targets an unregistered model |
| `models.E006` | error | two fields map to the same column |
| `models.E007` | error | a model has more than one primary-key field |
| `migrations.E001` | error | the migration files cannot be loaded |
| `migrations.E002` | error | the migration dependency graph is invalid |
| `migrations.E003` | error | the migrations do not replay cleanly |
| `migrations.W001` | warning | model changes are not recorded in any migration; run `makemigrations` |
| `openapi.E001` | error | the OpenAPI document cannot be generated (duplicate operations or operation ids) |
| `routes.E001` | error | the app cannot be built into a router (duplicate routes, malformed paths) |
| `backend.E001` | error | the `default` database is Redis, which cannot hold models |
| `backend.E002` | error | a model needs joins the `default` backend does not support |
| `backend.E003` | error | the schema cannot be created on the `default` backend |
| `backend.W001` | warning | the backend has no schema migrations; `migrate` will refuse to run |

Migration checks are skipped when no migrations directory is passed to the library function `siderite_cli::check`; `AppCli` always passes its own.

## `dbshell`

`dbshell` starts `sqlite3`, `psql` or `mysql` on the selected database and returns the client's exit code. Passwords never appear on the command line or in output: `psql` receives `PGPASSWORD` and `mysql` receives `MYSQL_PWD` in its environment. A userinfo password and a PostgreSQL `?password=` query parameter both move into `PGPASSWORD` and are stripped from the URL passed to `psql`, so options such as `sslmode` still apply. A URL that carries `sslpassword` is refused: there is no safe way to pass it. For MySQL only host, port, user and database are forwarded. An in-memory SQLite URL, an unknown scheme, or a client that is not on `PATH` is an error. MongoDB and Redis are not supported.
