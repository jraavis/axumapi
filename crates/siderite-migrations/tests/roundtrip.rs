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

/// The pre-commit check compares violating *rows*, not counts.
///
/// A migration that repairs one pre-existing orphan while adding another
/// keeps the violation count equal, so a count comparison would pass it. The
/// new orphan must still fail the commit.
#[tokio::test]
async fn sqlite_check_detects_a_new_violation_behind_a_fixed_one() {
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
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &create).unwrap();
    Migrator::new(
        &db,
        &MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap(),
    )
    .migrate(None, false)
    .await
    .unwrap();
    // Baseline orphan, created with enforcement off on the single `:memory:`
    // connection and enforcement back on so the migration takes the check.
    // The row id is fixed: the replacement must differ from it, or it is the
    // same violation and correctly passes.
    db.execute_script(
        "PRAGMA foreign_keys = OFF;
         INSERT INTO books (id, title, author_id) VALUES (1, 'Ghost', 999);
         PRAGMA foreign_keys = ON;",
    )
    .await
    .unwrap();

    let swap = Migration::new(
        "0002_swap_orphans",
        vec![create.id.clone()],
        vec![Operation::RunRust {
            name: "swap".into(),
            backwards: None,
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &swap).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let mut registry = MigrationRegistry::new();
    registry.register("swap", |db| {
        Box::pin(async move {
            db.raw_execute("DELETE FROM books WHERE id = 1", Vec::new())
                .await?;
            db.raw_execute(
                "INSERT INTO books (id, title, author_id) VALUES (2, 'Ghost', 999)",
                Vec::new(),
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
    let msg = err.to_string();
    assert!(
        msg.contains("books") && msg.contains("new violation"),
        "an equal violation count must not pass: {err:?}"
    );
    // The rollback restores the baseline orphan (id 1), not the swap's (id 2).
    let orphans = db
        .raw_sql("SELECT id FROM books", Vec::new())
        .await
        .unwrap();
    assert_eq!(orphans.rows.len(), 1, "{orphans:?}");
    assert_eq!(orphans.rows[0].get("id"), Some(&Value::Int(1)));
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
    for r in [&ra, &rb] {
        // `sql` must describe what was planned under the lock, not the
        // pre-lock preview: the loser applies nothing, so it must report
        // no SQL either.
        if r.applied.is_empty() {
            assert!(
                r.sql.is_empty(),
                "loser must not report stale pre-lock SQL: {r:?}"
            );
        } else {
            assert!(!r.sql.is_empty(), "winner must report its SQL: {r:?}");
        }
    }
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

/// SQLite rebuild must fail closed on a lossy CAST instead of truncating.
///
/// `CAST('abc' AS INTEGER)` is `0`: copying it would destroy the value, so
/// the pre-copy probe aborts with the table and column named.
#[tokio::test]
async fn sqlite_lossy_cast_aborts_migration() {
    use siderite_migrations::state::ModelState;
    fn item(val_type: SqlType) -> ModelState {
        ModelState {
            name: "Item".into(),
            table: "items".into(),
            fields: vec![
                FieldState {
                    nullable: false,
                    primary_key: true,
                    auto: true,
                    ..FieldState::new("id", "id", SqlType::Integer)
                },
                FieldState::new("val", "val", val_type),
            ],
            indexes: Vec::new(),
            constraints: Vec::new(),
        }
    }
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel {
            model: item(SqlType::Text),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES ('abc')", vec![])
        .await
        .unwrap();

    let alter = Migration::new(
        "0002_val_to_int",
        vec![first.id.clone()],
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
    let err = Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("items") && msg.contains("val"),
        "error must name table/column: {err:?}"
    );
    // All-or-nothing: the row survives with its original value.
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("val"), Some(&Value::Text("abc".into())));
    let shown = Migrator::new(&db, &graph).show().await.unwrap();
    assert!(
        shown
            .iter()
            .any(|(id, applied)| id == "0002_val_to_int" && !applied)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Lossless widenings still copy: `int -> REAL` and `anything -> TEXT`.
#[tokio::test]
async fn sqlite_lossless_casts_still_copy() {
    use siderite_migrations::state::ModelState;
    fn item(val_type: SqlType) -> ModelState {
        ModelState {
            name: "Item".into(),
            table: "items".into(),
            fields: vec![
                FieldState {
                    nullable: false,
                    primary_key: true,
                    auto: true,
                    ..FieldState::new("id", "id", SqlType::Integer)
                },
                FieldState::new("val", "val", val_type),
            ],
            indexes: Vec::new(),
            constraints: Vec::new(),
        }
    }
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel {
            model: item(SqlType::Integer),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES (5)", vec![])
        .await
        .unwrap();

    let to_real = Migration::new(
        "0002_val_to_real",
        vec![first.id.clone()],
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field: FieldState::new("val", "val", SqlType::Real),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &to_real).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);

    let to_text = Migration::new(
        "0003_val_to_text",
        vec![to_real.id.clone()],
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field: FieldState::new("val", "val", SqlType::Text),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &to_text).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// `RenameField` on SQLite must carry the column's rows into the new name.
///
/// Forward is a native `RENAME COLUMN`; rollback renames back. Either way
/// the stored value must survive instead of being dropped or nulled.
#[tokio::test]
async fn sqlite_rename_field_preserves_rows() {
    use siderite_migrations::state::ModelState;
    fn item(name: &str, column: &str) -> ModelState {
        ModelState {
            name: "Item".into(),
            table: "items".into(),
            fields: vec![
                FieldState {
                    nullable: false,
                    primary_key: true,
                    auto: true,
                    ..FieldState::new("id", "id", SqlType::Integer)
                },
                FieldState::new(name, column, SqlType::Text),
            ],
            indexes: Vec::new(),
            constraints: Vec::new(),
        }
    }
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel {
            model: item("val", "val"),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES ('hello')", vec![])
        .await
        .unwrap();

    let rename = Migration::new(
        "0002_rename_val",
        vec![first.id.clone()],
        vec![Operation::RenameField {
            model: "Item".into(),
            old_name: "val".into(),
            new_name: "label".into(),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &rename).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT label FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(
        rows.rows[0].get("label"),
        Some(&Value::Text("hello".into()))
    );

    Migrator::new(&db, &graph)
        .rollback(None, Some(1), false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].get("val"), Some(&Value::Text("hello".into())));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `AlterField` that renames the db column must copy old -> new on SQLite.
///
/// Same field name, different column: the rebuild `INSERT .. SELECT` has to
/// read the old column into the new one, and the rollback has to copy back.
#[tokio::test]
async fn sqlite_alter_field_column_rename_preserves_rows() {
    use siderite_migrations::state::ModelState;
    fn item(column: &str) -> ModelState {
        ModelState {
            name: "Item".into(),
            table: "items".into(),
            fields: vec![
                FieldState {
                    nullable: false,
                    primary_key: true,
                    auto: true,
                    ..FieldState::new("id", "id", SqlType::Integer)
                },
                FieldState::new("val", column, SqlType::Text),
            ],
            indexes: Vec::new(),
            constraints: Vec::new(),
        }
    }
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel { model: item("val") }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES ('hello')", vec![])
        .await
        .unwrap();

    let alter = Migration::new(
        "0002_rename_column",
        vec![first.id.clone()],
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field: FieldState::new("val", "renamed", SqlType::Text),
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
    let rows = db
        .raw_sql("SELECT renamed FROM items", vec![])
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(
        rows.rows[0].get("renamed"),
        Some(&Value::Text("hello".into()))
    );

    Migrator::new(&db, &graph)
        .rollback(None, Some(1), false)
        .await
        .unwrap();
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].get("val"), Some(&Value::Text("hello".into())));
    let _ = std::fs::remove_dir_all(&dir);
}

/// SQLite rebuild must preserve triggers and hand-made indexes.
///
/// A trigger and an index created outside the migrator live in
/// `sqlite_master` but not in `ProjectState`; the rebuild drops them with
/// the old table unless the executor re-creates them afterwards. The model
/// index is recreated by the rebuild itself and must not be duplicated.
#[tokio::test]
async fn sqlite_rebuild_preserves_trigger_and_hand_index() {
    use siderite_migrations::state::{IndexState, ModelState};
    fn item() -> ModelState {
        ModelState {
            name: "Item".into(),
            table: "items".into(),
            fields: vec![
                FieldState {
                    nullable: false,
                    primary_key: true,
                    auto: true,
                    ..FieldState::new("id", "id", SqlType::Integer)
                },
                FieldState::new("val", "val", SqlType::Text),
            ],
            indexes: vec![IndexState {
                name: "items_val_idx".into(),
                columns: vec!["val".into()],
                unique: false,
            }],
            constraints: Vec::new(),
        }
    }
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel { model: item() }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    db.execute_script(
        "CREATE TABLE items_audit (id INTEGER PRIMARY KEY, item_id INTEGER, val TEXT);
         CREATE TRIGGER items_val_trigger AFTER INSERT ON items BEGIN INSERT INTO items_audit (item_id, val) VALUES (NEW.id, NEW.val); END;
         CREATE INDEX items_val_hand_idx ON items (val);",
    )
    .await
    .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES ('hello')", vec![])
        .await
        .unwrap();

    let mut val = item().fields.into_iter().find(|f| f.name == "val").unwrap();
    val.max_length = Some(200);
    let alter = Migration::new(
        "0002_widen_val",
        vec![first.id.clone()],
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field: val,
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

    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].get("val"), Some(&Value::Text("hello".into())));
    for name in ["items_val_trigger", "items_val_hand_idx", "items_val_idx"] {
        let found = db
            .raw_sql(
                "SELECT name FROM sqlite_master WHERE name = ?",
                vec![Value::Text(name.into())],
            )
            .await
            .unwrap();
        assert_eq!(found.rows.len(), 1, "`{name}` must survive the rebuild");
    }
    // The trigger still fires.
    db.raw_execute("INSERT INTO items (val) VALUES ('world')", vec![])
        .await
        .unwrap();
    let audit = db
        .raw_sql("SELECT val FROM items_audit ORDER BY id", vec![])
        .await
        .unwrap();
    assert_eq!(
        audit.rows.len(),
        2,
        "trigger must fire after the rebuild: {audit:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Removing a column a trigger reads must fail closed, not drop the trigger.
///
/// The rebuild would orphan `items_val_trigger` (`NEW.val` has no `val`
/// anymore), so the executor refuses before touching the table.
#[tokio::test]
async fn sqlite_rebuild_refuses_when_trigger_column_removed() {
    use siderite_migrations::state::ModelState;
    fn item() -> ModelState {
        ModelState {
            name: "Item".into(),
            table: "items".into(),
            fields: vec![
                FieldState {
                    nullable: false,
                    primary_key: true,
                    auto: true,
                    ..FieldState::new("id", "id", SqlType::Integer)
                },
                FieldState::new("val", "val", SqlType::Text),
            ],
            indexes: Vec::new(),
            constraints: Vec::new(),
        }
    }
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel { model: item() }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    db.execute_script(
        "CREATE TABLE items_audit (id INTEGER PRIMARY KEY, item_id INTEGER, val TEXT);
         CREATE TRIGGER items_val_trigger AFTER INSERT ON items BEGIN INSERT INTO items_audit (item_id, val) VALUES (NEW.id, NEW.val); END;",
    )
    .await
    .unwrap();
    db.raw_execute("INSERT INTO items (val) VALUES ('hello')", vec![])
        .await
        .unwrap();

    let drop = Migration::new(
        "0002_drop_val",
        vec![first.id.clone()],
        vec![Operation::RemoveField {
            model: "Item".into(),
            name: "val".into(),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &drop).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let err = Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("items") && msg.contains("items_val_trigger"),
        "error must name table and trigger: {err:?}"
    );
    // All-or-nothing: the row and the trigger survive.
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows[0].get("val"), Some(&Value::Text("hello".into())));
    let found = db
        .raw_sql(
            "SELECT name FROM sqlite_master WHERE name = 'items_val_trigger'",
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(found.rows.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

fn not_null_item() -> siderite_migrations::state::ModelState {
    siderite_migrations::state::ModelState {
        name: "Item".into(),
        table: "items".into(),
        fields: vec![
            FieldState {
                nullable: false,
                primary_key: true,
                auto: true,
                ..FieldState::new("id", "id", SqlType::Integer)
            },
            FieldState {
                nullable: true,
                ..FieldState::new("val", "val", SqlType::Text)
            },
        ],
        indexes: Vec::new(),
        constraints: Vec::new(),
    }
}

/// Apply `ops` as a follow-up to `0001_item` (with one row holding `val`).
async fn not_null_followup(
    val: Option<&str>,
    ops: Vec<Operation>,
) -> (Result<(), String>, siderite_orm::Db, std::path::PathBuf) {
    let dir = temp_dir();
    let db = memory_db().await;
    let first = Migration::new(
        "0001_item",
        Vec::new(),
        vec![Operation::CreateModel {
            model: not_null_item(),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &first).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let value = val.map_or(Value::Null, |v| Value::Text(v.into()));
    db.raw_execute("INSERT INTO items (val) VALUES (?)", vec![value])
        .await
        .unwrap();
    let next = Migration::new("0002_next", vec![first.id.clone()], ops, true, Vec::new()).unwrap();
    loader::write_migration(&dir, &next).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let res = Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
    (res, db, dir)
}

#[tokio::test]
async fn add_not_null_field_without_default_refuses_populated_table() {
    let field = FieldState {
        nullable: false,
        ..FieldState::new("qty", "qty", SqlType::Integer)
    };
    let (res, db, dir) = not_null_followup(
        Some("a"),
        vec![Operation::AddField {
            model: "Item".into(),
            field,
        }],
    )
    .await;
    let msg = res.unwrap_err();
    assert!(msg.contains("items.qty"), "{msg}");
    let cols = db
        .raw_sql("SELECT name FROM pragma_table_info('items')", vec![])
        .await
        .unwrap();
    assert_eq!(cols.rows.len(), 2, "table must be unchanged");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn alter_to_not_null_refuses_existing_nulls() {
    let field = FieldState {
        nullable: false,
        ..FieldState::new("val", "val", SqlType::Text)
    };
    let (res, db, dir) = not_null_followup(
        None,
        vec![Operation::AlterField {
            model: "Item".into(),
            name: "val".into(),
            field,
        }],
    )
    .await;
    let msg = res.unwrap_err();
    assert!(msg.contains("items.val"), "{msg}");
    let rows = db.raw_sql("SELECT val FROM items", vec![]).await.unwrap();
    assert_eq!(rows.rows.len(), 1, "row must survive");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The probe reads the in-migration state: a NOT NULL field added right
/// after `CreateModel` in the same migration hits an empty table.
#[tokio::test]
async fn add_not_null_field_after_create_model_in_same_migration() {
    let dir = temp_dir();
    let db = memory_db().await;
    let mut other = not_null_item();
    other.name = "Other".into();
    other.table = "others".into();
    let m = Migration::new(
        "0001_other",
        Vec::new(),
        vec![
            Operation::CreateModel { model: other },
            Operation::AddField {
                model: "Other".into(),
                field: FieldState {
                    nullable: false,
                    ..FieldState::new("qty", "qty", SqlType::Integer)
                },
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &m).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    Migrator::new(&db, &graph)
        .migrate(None, false)
        .await
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
