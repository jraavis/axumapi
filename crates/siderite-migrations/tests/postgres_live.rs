//! Live PostgreSQL round trip. Ignored by default: set `DATABASE_URL` to a
//! `postgres://` URL of a server and run
//! `cargo test -p siderite-migrations --all-features -- --ignored`.
//!
//! Each test creates and drops its own database: they share one server, and
//! migrations record their history in a fixed table, so a shared database
//! would make parallel tests clobber each other's history.
#![allow(clippy::unwrap_used)]

mod common;

use common::scratch::ScratchDb;
use common::{author_meta, temp_dir};
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::make_migrations;

#[tokio::test]
#[ignore = "needs a PostgreSQL server: set DATABASE_URL"]
async fn migrate_and_rollback_on_postgres() {
    let Some(t) = ScratchDb::postgres().await.unwrap() else {
        return;
    };
    let db = &t.db;

    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta()];
    let migration = make_migrations(models, &dir, None, false).unwrap().unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let migrator = Migrator::new(db, &graph);

    let report = migrator.migrate(None, false).await.unwrap();
    assert_eq!(report.applied, vec![migration.id.clone()]);
    assert!(db.raw_sql("SELECT id FROM authors", vec![]).await.is_ok());

    let shown = migrator.show().await.unwrap();
    assert_eq!(shown, vec![(migration.id.clone(), true)]);

    migrator.rollback(None, Some(1), false).await.unwrap();
    assert!(db.raw_sql("SELECT id FROM authors", vec![]).await.is_err());
    t.cleanup().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The migration session lock must be released *and* its connection never
/// returned to the pool while locked: a second migrate on the same pool has
/// to succeed immediately instead of hanging on `pg_advisory_lock`.
#[tokio::test]
#[ignore = "needs a PostgreSQL server: set DATABASE_URL"]
async fn second_migrate_after_first_succeeds() {
    let Some(t) = ScratchDb::postgres().await.unwrap() else {
        return;
    };
    let db = &t.db;

    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta()];
    make_migrations(models, &dir, None, false).unwrap().unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let migrator = Migrator::new(db, &graph);

    let first = migrator.migrate(None, false).await.unwrap();
    assert_eq!(first.applied.len(), 1);
    let second = migrator.migrate(None, false).await.unwrap();
    assert!(second.applied.is_empty());
    assert_eq!(second.planned, second.applied);
    t.cleanup().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
