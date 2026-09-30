//! Full SQLite round trip: makemigrations → migrate → DML → add field →
//! rollback → showmigrations, plus dry-run, checksum mismatch, irreversible.
#![allow(clippy::unwrap_used)]

mod common;

use common::{author_meta, book_meta, temp_dir};
use siderite_backends::sqlite::SqliteBackend;
use siderite_migrations::cli::{self, ExitCode};
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::migration::Migration;
use siderite_migrations::operation::Operation;
use siderite_migrations::registry::MigrationRegistry;
use siderite_migrations::state::{FieldState, ProjectState};
use siderite_migrations::{MigrationError, SqlType, make_migrations};
use siderite_orm::{Db, Value};

async fn memory_db() -> Db {
    Db::new(SqliteBackend::connect("sqlite::memory:").await.unwrap())
}

#[tokio::test]
async fn sqlite_round_trip() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];

    let first = make_migrations(models, &dir, None, false).unwrap().unwrap();
    assert!(first.id.starts_with("0001_"));
    assert!(dir.join(format!("{}.json", first.id)).exists());

    let db = memory_db().await;
    let code = cli::run(
        models,
        &db,
        &dir,
        ["migrate".to_owned(), "--dry-run".to_owned()],
    )
    .await
    .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    // Dry-run must not create tables.
    let err = db.raw_sql("SELECT 1 FROM authors", vec![]).await;
    assert!(err.is_err());

    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();

    db.raw_execute(
        "INSERT INTO authors (name) VALUES (?)",
        vec![Value::Text("Ann".into())],
    )
    .await
    .unwrap();
    db.raw_execute(
        "INSERT INTO books (title, author_id, pages) VALUES (?, ?, ?)",
        vec![Value::Text("Pigs".into()), Value::Int(1), Value::Int(120)],
    )
    .await
    .unwrap();
    let rows = db.raw_sql("SELECT title FROM books", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("title"), Some(&Value::Text("Pigs".into())));

    // Second migration: add isbn.
    let mut second = Migration::new(
        "0002_add_book_isbn",
        vec![first.id.clone()],
        vec![Operation::AddField {
            model: "Book".into(),
            field: FieldState {
                nullable: true,
                ..FieldState::new("isbn", "isbn", SqlType::Text)
            },
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    second.refresh_checksum().unwrap();
    loader::write_migration(&dir, &second).unwrap();

    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();
    db.raw_execute(
        "UPDATE books SET isbn = ? WHERE id = ?",
        vec![Value::Text("978-0".into()), Value::Int(1)],
    )
    .await
    .unwrap();
    let rows = db.raw_sql("SELECT isbn FROM books", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("isbn"), Some(&Value::Text("978-0".into())));

    cli::run(
        models,
        &db,
        &dir,
        ["rollback".to_owned(), "--steps".to_owned(), "1".to_owned()],
    )
    .await
    .unwrap();
    let err = db.raw_sql("SELECT isbn FROM books", vec![]).await;
    assert!(err.is_err(), "isbn column should be gone after rollback");
    let rows = db.raw_sql("SELECT title FROM books", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("title"), Some(&Value::Text("Pigs".into())));

    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let shown = Migrator::new(&db, &graph).show().await.unwrap();
    assert_eq!(shown[0].0, first.id);
    assert!(shown[0].1);
    assert_eq!(shown[1].0, second.id);
    assert!(!shown[1].1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn checksum_mismatch_of_applied_migration() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta()];
    make_migrations(models, &dir, Some("initial"), false)
        .unwrap()
        .unwrap();
    let db = memory_db().await;
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();

    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    let path = &files[0];
    let mut migration: Migration =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    migration.operations.push(Operation::RunSQL {
        sql: "SELECT 1".into(),
        reverse_sql: None,
    });
    migration.refresh_checksum().unwrap();
    std::fs::write(path, serde_json::to_string_pretty(&migration).unwrap()).unwrap();

    let err = cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap_err();
    assert!(
        matches!(err, MigrationError::ChecksumMismatch { .. }),
        "{err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn irreversible_rollback_is_refused() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta()];
    let first = make_migrations(models, &dir, Some("initial"), false)
        .unwrap()
        .unwrap();
    let irr = Migration::new(
        "0002_data",
        vec![first.id],
        vec![Operation::RunSQL {
            sql: "SELECT 1".into(),
            reverse_sql: None,
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &irr).unwrap();

    let db = memory_db().await;
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();
    let err = cli::run(
        models,
        &db,
        &dir,
        ["rollback".to_owned(), "--steps".to_owned(), "1".to_owned()],
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, MigrationError::Irreversible { .. }),
        "{err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn dry_run_executes_nothing() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    make_migrations(models, &dir, None, false).unwrap();
    let db = memory_db().await;
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let report = Migrator::new(&db, &graph)
        .migrate(None, true)
        .await
        .unwrap();
    assert!(report.dry_run);
    assert!(!report.planned.is_empty());
    assert!(report.applied.is_empty());
    assert!(db.raw_sql("SELECT 1 FROM authors", vec![]).await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn sqlite_rebuild_preserves_rows() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    let first = make_migrations(models, &dir, None, false).unwrap().unwrap();
    let db = memory_db().await;
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();
    db.raw_execute(
        "INSERT INTO authors (name) VALUES (?)",
        vec![Value::Text("Bo".into())],
    )
    .await
    .unwrap();
    db.raw_execute(
        "INSERT INTO books (title, author_id) VALUES (?, ?)",
        vec![Value::Text("Go".into()), Value::Int(1)],
    )
    .await
    .unwrap();

    let drop_pages = Migration::new(
        "0002_drop_pages",
        vec![first.id],
        vec![Operation::RemoveField {
            model: "Book".into(),
            name: "pages".into(),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &drop_pages).unwrap();
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT title FROM books", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("title"), Some(&Value::Text("Go".into())));
    assert!(db.raw_sql("SELECT pages FROM books", vec![]).await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn unreadable_history_is_an_error_not_a_fresh_db() {
    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    make_migrations(models, &dir, None, false).unwrap().unwrap();
    let db = memory_db().await;
    // A history table that exists but cannot be read as history.
    db.raw_execute(
        "CREATE TABLE siderite_migrations (id INTEGER, checksum TEXT)",
        vec![],
    )
    .await
    .unwrap();
    db.raw_execute("INSERT INTO siderite_migrations VALUES (1, 'x')", vec![])
        .await
        .unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let err = Migrator::new(&db, &graph).show().await;
    assert!(err.is_err(), "expected an error, got {err:?}");
    // Nothing was replayed.
    assert!(db.raw_sql("SELECT 1 FROM authors", vec![]).await.is_err());
}

/// Rebuild of a parent table must not CASCADE-delete child rows.
///
/// SQLite ignores `PRAGMA foreign_keys` inside a transaction, and the
/// migrator wraps atomic SQLite migrations in one. The rebuild has to turn
/// FKs off on the *same* connection *before* `BEGIN`.
#[tokio::test]
async fn sqlite_rebuild_of_parent_keeps_cascade_children() {
    let dir = temp_dir();
    let db_path = dir.join("app.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let db = Db::new(SqliteBackend::connect(&url).await.unwrap());
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    let first = make_migrations(models, &dir, None, false).unwrap().unwrap();
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();

    db.raw_execute(
        "INSERT INTO authors (name) VALUES (?)",
        vec![Value::Text("Ann".into())],
    )
    .await
    .unwrap();
    db.raw_execute(
        "INSERT INTO books (title, author_id) VALUES (?, ?)",
        vec![Value::Text("Pigs".into()), Value::Int(1)],
    )
    .await
    .unwrap();

    let mut name = ProjectState::from_metas(&[author_meta()])
        .model("Author")
        .unwrap()
        .field("name")
        .unwrap()
        .clone();
    name.max_length = Some(200);
    let alter = Migration::new(
        "0002_widen_author_name",
        vec![first.id],
        vec![Operation::AlterField {
            model: "Author".into(),
            name: "name".into(),
            field: name,
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &alter).unwrap();
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();

    let books = db.raw_sql("SELECT title FROM books", vec![]).await.unwrap();
    assert_eq!(
        books.rows.len(),
        1,
        "ON DELETE CASCADE child rows must survive AlterField on the parent"
    );
    assert_eq!(
        books.rows[0].get("title"),
        Some(&Value::Text("Pigs".into()))
    );
    let authors = db
        .raw_sql("SELECT name FROM authors", vec![])
        .await
        .unwrap();
    assert_eq!(
        authors.rows[0].get("name"),
        Some(&Value::Text("Ann".into()))
    );
    let orphan = db
        .raw_execute(
            "INSERT INTO books (title, author_id) VALUES (?, ?)",
            vec![Value::Text("Ghost".into()), Value::Int(999)],
        )
        .await;
    assert!(
        orphan.is_err(),
        "foreign keys must still be enforced after the rebuild"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A pre-existing FK violation in an unrelated row must not block a rebuild.
///
/// The pre-commit `foreign_key_check` is scoped to *new* violations: the
/// orphan below predates the migration, so widening `authors.name` must
/// succeed and leave the orphan untouched.
#[tokio::test]
async fn sqlite_rebuild_ignores_preexisting_violations() {
    let dir = temp_dir();
    let db_path = dir.join("app.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let db = Db::new(SqliteBackend::connect(&url).await.unwrap());
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    let first = make_migrations(models, &dir, None, false).unwrap().unwrap();
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();

    db.raw_execute(
        "INSERT INTO authors (name) VALUES (?)",
        vec![Value::Text("Ann".into())],
    )
    .await
    .unwrap();
    // `PRAGMA foreign_keys` is per-connection, so the OFF/ON pair must wrap
    // the orphan insert in one script on one pooled connection.
    db.execute_script(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO books (title, author_id) VALUES ('Ghost', 999);
         PRAGMA foreign_keys = ON;",
    )
    .await
    .unwrap();

    let mut name = ProjectState::from_metas(&[author_meta()])
        .model("Author")
        .unwrap()
        .field("name")
        .unwrap()
        .clone();
    name.max_length = Some(200);
    let alter = Migration::new(
        "0002_widen_author_name",
        vec![first.id],
        vec![Operation::AlterField {
            model: "Author".into(),
            name: "name".into(),
            field: name,
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &alter).unwrap();
    cli::run(models, &db, &dir, ["migrate".to_owned()])
        .await
        .unwrap();

    let ghosts = db
        .raw_sql("SELECT title FROM books WHERE title = 'Ghost'", vec![])
        .await
        .unwrap();
    assert_eq!(ghosts.rows.len(), 1, "pre-existing orphan must survive");
    let _ = std::fs::remove_dir_all(&dir);
}

/// When foreign keys were already off, the pre-commit check is skipped.
#[tokio::test]
async fn sqlite_rebuild_skips_check_when_fks_were_off() {
    let db = memory_db().await;
    db.execute_script(
        "CREATE TABLE authors (id INTEGER PRIMARY KEY AUTOINCREMENT, name VARCHAR(100) NOT NULL);
         CREATE TABLE books (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, author_id BIGINT NOT NULL REFERENCES authors (id) ON DELETE CASCADE, pages INTEGER);",
    )
    .await
    .unwrap();
    // Single-connection `:memory:` pool: this OFF persists for the migration.
    db.execute_script(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO authors (name) VALUES ('Ann');
         INSERT INTO books (title, author_id) VALUES ('Ghost', 999);",
    )
    .await
    .unwrap();

    let dir = temp_dir();
    let alter = Migration::new(
        "0001_widen",
        Vec::new(),
        vec![Operation::RunSQL {
            sql: "CREATE TABLE t (n INTEGER)".into(),
            reverse_sql: Some("DROP TABLE t".into()),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &alter).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let ghosts = db
        .raw_sql("SELECT title FROM books WHERE title = 'Ghost'", vec![])
        .await
        .unwrap();
    assert_eq!(ghosts.rows.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `RunRust` on SQLite runs with foreign keys off, and the pre-commit
/// check rejects orphans created mid-migration.
///
/// The data-migration closure gets the rebuild connection, where `PRAGMA
/// foreign_keys` is off, so the orphan insert below succeeds. Commit then
/// fails with the offending table in the message instead of silently
/// cementing the orphan.
#[tokio::test]
async fn run_rust_sees_no_fk_enforcement_on_sqlite() {
    let dir = temp_dir();
    let db = memory_db().await;
    let create = Migration::new(
        "0001_t",
        Vec::new(),
        vec![
            Operation::RunSQL {
                sql: "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT NOT NULL)"
                    .into(),
                reverse_sql: Some("DROP TABLE authors".into()),
            },
            Operation::RunSQL {
                sql: "CREATE TABLE books (id INTEGER PRIMARY KEY, title TEXT NOT NULL, author_id BIGINT NOT NULL REFERENCES authors (id) ON DELETE CASCADE)"
                    .into(),
                reverse_sql: Some("DROP TABLE books".into()),
            },
            Operation::RunRust {
                name: "orphan".into(),
                backwards: None,
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &create).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let mut registry = MigrationRegistry::new();
    registry.register("orphan", |db| {
        Box::pin(async move {
            db.raw_execute(
                "INSERT INTO books (title, author_id) VALUES ('Ghost', 999)",
                vec![],
            )
            .await?;
            Ok(())
        })
    });
    let err = Migrator::new(&db, &graph)
        .with_registry(registry)
        .migrate(None, false)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("books"),
        "commit must name the offending table: {err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A failing migration rolls the whole SQLite run back.
///
/// `with_lock` holds a single `BEGIN IMMEDIATE` transaction for the entire
/// run (the lock), so a later failure undoes earlier migrations too — unlike
/// PostgreSQL, where each migration commits on its own.
#[tokio::test]
async fn sqlite_migrate_run_is_all_or_nothing() {
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_t",
        Vec::new(),
        vec![Operation::RunSQL {
            sql: "CREATE TABLE t (n INTEGER)".into(),
            reverse_sql: Some("DROP TABLE t".into()),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    let second = Migration::new(
        "0002_bad",
        vec![first.id.clone()],
        vec![
            Operation::RunSQL {
                sql: "CREATE TABLE u (n INTEGER)".into(),
                reverse_sql: Some("DROP TABLE u".into()),
            },
            Operation::RunSQL {
                sql: "CREATE TABLE u (n INTEGER)".into(),
                reverse_sql: Some("DROP TABLE u".into()),
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    loader::write_migration(&dir, &second).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap_err();
    assert!(
        db.raw_sql("SELECT n FROM t", vec![]).await.is_err(),
        "the first migration must roll back with the failed run"
    );
    let shown = Migrator::new(&db, &graph).show().await.unwrap();
    assert!(shown.iter().all(|(_, applied)| !applied));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two concurrent migrators must apply each migration once.
#[tokio::test]
async fn concurrent_sqlite_migrate_applies_once() {
    let dir = temp_dir();
    let db_path = dir.join("app.db");
    let url = format!("sqlite://{}?mode=rwc", db_path.display());
    let db = Db::new(SqliteBackend::connect(&url).await.unwrap());
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta()];
    make_migrations(models, &dir, None, false).unwrap().unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();

    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let run = |db: Db, graph: MigrationGraph, barrier: std::sync::Arc<tokio::sync::Barrier>| async move {
        barrier.wait().await;
        Migrator::new(&db, &graph).migrate(None, false).await
    };
    let a = tokio::spawn(run(db.clone(), graph.clone(), barrier.clone()));
    let b = tokio::spawn(run(db.clone(), graph.clone(), barrier));
    let ra = a.await.unwrap().unwrap();
    let rb = b.await.unwrap().unwrap();
    assert_eq!(
        ra.applied.len() + rb.applied.len(),
        1,
        "one replica applies, the other sees history: {ra:?} {rb:?}"
    );
    let shown = Migrator::new(&db, &graph).show().await.unwrap();
    assert_eq!(shown.len(), 1);
    assert!(shown[0].1);
    db.raw_sql("SELECT COUNT(*) AS n FROM authors", vec![])
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// RunRust must run between the SQL operations that surround it.
#[tokio::test]
async fn run_rust_interleaves_with_sql() {
    let dir = temp_dir();
    let db = memory_db().await;
    let create = Migration::new(
        "0001_t",
        Vec::new(),
        vec![
            Operation::RunSQL {
                sql: "CREATE TABLE t (n INTEGER)".into(),
                reverse_sql: Some("DROP TABLE t".into()),
            },
            Operation::RunRust {
                name: "seed".into(),
                backwards: Some("unseed".into()),
            },
            Operation::RunSQL {
                sql: "CREATE TABLE u AS SELECT * FROM t WHERE n = 1".into(),
                reverse_sql: Some("DROP TABLE u".into()),
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &create).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let mut registry = MigrationRegistry::new();
    registry.register("seed", |db| {
        Box::pin(async move {
            db.raw_execute("INSERT INTO t (n) VALUES (1)", vec![])
                .await?;
            Ok(())
        })
    });
    registry.register("unseed", |_db| Box::pin(async { Ok(()) }));
    Migrator::new(&db, &graph)
        .with_registry(registry)
        .migrate(None, false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT n FROM u", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1, "seed must run before the second SQL");
    let _ = std::fs::remove_dir_all(&dir);
}
