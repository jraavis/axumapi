//! Live PostgreSQL round trip. Ignored by default: set `DATABASE_URL` to a
//! `postgres://` URL of a scratch database and run
//! `cargo test -p siderite-migrations --all-features -- --ignored`.
#![allow(clippy::unwrap_used)]

mod common;

use common::{author_meta, temp_dir};
use siderite_backends::postgres::PgBackend;
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::make_migrations;
use siderite_orm::Db;

fn pg_url() -> Option<String> {
    std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| url.to_ascii_lowercase().starts_with("postgres"))
}

async fn reset(db: &Db) {
    for table in ["books", "authors", "siderite_migrations"] {
        db.execute_script(&format!("DROP TABLE IF EXISTS \"{table}\""))
            .await
            .unwrap();
    }
}

/// The migration session lock must be released *and* its connection never
/// returned to the pool while locked: a second migrate on the same pool has
/// to succeed immediately instead of hanging on `pg_advisory_lock`.
#[tokio::test]
#[ignore = "needs a PostgreSQL server: set DATABASE_URL"]
async fn second_migrate_after_first_succeeds() {
    let Some(url) = pg_url() else {
        eprintln!("DATABASE_URL not set; skipping");
        return;
    };
    let db = Db::new(PgBackend::connect(&url).await.unwrap());
    reset(&db).await;

    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta()];
    make_migrations(models, &dir, None, false).unwrap().unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let migrator = Migrator::new(&db, &graph);

    let first = migrator.migrate(None, false).await.unwrap();
    assert_eq!(first.applied.len(), 1);
    let second = migrator.migrate(None, false).await.unwrap();
    assert!(second.applied.is_empty());
    assert_eq!(second.planned, second.applied);
    reset(&db).await;
    let _ = std::fs::remove_dir_all(&dir);
}
