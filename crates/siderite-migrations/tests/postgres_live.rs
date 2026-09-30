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
use siderite_migrations::MigrationError;
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::make_migrations;
use siderite_migrations::migration::Migration;
use siderite_migrations::operation::Operation;
use siderite_migrations::state::{FieldState, ModelState, SqlType};
use siderite_orm::Value;

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

/// A text column holding digits converts to integer via `USING col::type`.
#[tokio::test]
#[ignore = "needs a PostgreSQL server: set DATABASE_URL"]
async fn postgres_alter_text_to_integer_uses_cast() {
    let Some(t) = ScratchDb::postgres().await.unwrap() else {
        return;
    };
    let db = &t.db;
    let dir = temp_dir();
    fn item(val_type: SqlType) -> ModelState {
        ModelState {
            name: "Item".into(),
            table: "items".into(),
            fields: vec![
                FieldState {
                    nullable: false,
                    primary_key: true,
                    auto: true,
                    ..FieldState::new("id", "id", SqlType::BigInt)
                },
                FieldState::new("val", "val", val_type),
            ],
            indexes: Vec::new(),
            constraints: Vec::new(),
        }
    }
    let create = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel {
            model: item(SqlType::Text),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &create).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES ('42')", vec![])
        .await
        .unwrap();

    let alter = Migration::new(
        "0002_val_to_int",
        vec![create.id.clone()],
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field: FieldState::new("val", "val", SqlType::Integer),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &alter).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].get("val"), Some(&Value::Int(42)));
    t.cleanup().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A PostgreSQL migration with `atomic: false` runs outside a transaction
/// (`CREATE INDEX CONCURRENTLY` needs that), so a failure leaves its earlier
/// statements committed. The re-run must resume at the statement that failed
/// instead of replaying the committed one.
#[tokio::test]
#[ignore = "needs a PostgreSQL server: set DATABASE_URL"]
async fn postgres_non_atomic_migration_resumes_at_failed_statement() {
    let Some(t) = ScratchDb::postgres().await.unwrap() else {
        return;
    };
    let db = &t.db;
    let dir = temp_dir();

    let migration = Migration::new(
        "0001_pg_non_atomic",
        Vec::new(),
        vec![
            Operation::RunSQL {
                sql: "CREATE TABLE pg_resume (n INTEGER)".into(),
                reverse_sql: Some("DROP TABLE pg_resume".into()),
            },
            // The same table again: this statement fails on a live server.
            Operation::RunSQL {
                sql: "CREATE TABLE pg_resume (n INTEGER)".into(),
                reverse_sql: Some("DROP TABLE pg_resume".into()),
            },
        ],
        false, // atomic: false
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &migration).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let migrator = Migrator::new(db, &graph);

    // Statement 2 of 2 fails; statement 1 autocommitted and no history row was
    // written, so the migration is still pending.
    let err = migrator.migrate(None, false).await.unwrap_err();
    assert!(
        matches!(
            err,
            MigrationError::PostgresPartial {
                index: 2,
                total: 2,
                ..
            }
        ),
        "{err:?}"
    );
    db.raw_sql("SELECT n FROM pg_resume", vec![]).await.unwrap();
    let progress = db
        .raw_sql(
            "SELECT op_index, stmt_index FROM siderite_migration_progress \
             WHERE migration_id = $1 AND direction = 'apply'",
            vec![Value::Text(migration.id.clone())],
        )
        .await
        .unwrap();
    assert_eq!(progress.rows.len(), 1, "{progress:?}");
    assert_eq!(progress.rows[0].get("op_index"), Some(&Value::Int(1)));
    assert_eq!(progress.rows[0].get("stmt_index"), Some(&Value::Int(0)));

    // Re-run with nothing repaired: it resumes at operation 2 and fails on the
    // same statement. Replaying the committed statement 1 would fail on the
    // existing table and report `index: 1` instead.
    let err = migrator.migrate(None, false).await.unwrap_err();
    assert!(
        matches!(
            err,
            MigrationError::PostgresPartial {
                index: 2,
                total: 2,
                ..
            }
        ),
        "{err:?}"
    );
    let progress = db
        .raw_sql(
            "SELECT op_index FROM siderite_migration_progress \
             WHERE migration_id = $1 AND direction = 'apply'",
            vec![Value::Text(migration.id.clone())],
        )
        .await
        .unwrap();
    assert_eq!(progress.rows[0].get("op_index"), Some(&Value::Int(1)));

    // Repair what the failed statement left behind, then the resume finishes
    // the remaining operation and clears the progress row.
    db.execute_script("DROP TABLE pg_resume").await.unwrap();
    let report = migrator.migrate(None, false).await.unwrap();
    assert_eq!(report.applied, vec![migration.id.clone()]);
    db.raw_sql("SELECT n FROM pg_resume", vec![]).await.unwrap();
    let progress = db
        .raw_sql("SELECT op_index FROM siderite_migration_progress", vec![])
        .await
        .unwrap();
    assert!(progress.rows.is_empty(), "{progress:?}");

    t.cleanup().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
