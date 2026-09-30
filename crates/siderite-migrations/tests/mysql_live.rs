//! Live MySQL round trip. Ignored by default: set `MYSQL_URL` (or
//! `DATABASE_URL`) to a `mysql://` URL of a server and run
//! `cargo test -p siderite-migrations --all-features -- --ignored`.
//!
//! Each test creates and drops its own database: they share one server, and
//! migrations record their history in a fixed table, so a shared database
//! would make parallel tests clobber each other's history.
#![allow(clippy::unwrap_used)]

mod common;

use common::scratch::ScratchDb;
use common::{author_meta, book_meta, temp_dir};
use siderite_migrations::MigrationError;
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::make_migrations;
use siderite_migrations::migration::Migration;
use siderite_migrations::operation::Operation;
use siderite_migrations::schema_editor;
use siderite_migrations::state::{
    ConstraintState, DbDefault, FieldState, ForeignKeyState, IndexState, ModelState, OnDelete,
    ProjectState, SqlType,
};
use siderite_orm::{BackendKind, Value};

#[tokio::test]
#[ignore = "needs a MySQL server: set MYSQL_URL"]
async fn migrate_and_rollback_on_mysql() {
    let Some(t) = ScratchDb::mysql().await.unwrap() else {
        return;
    };
    let db = &t.db;

    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    let migration = make_migrations(models, &dir, None, false).unwrap().unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let migrator = Migrator::new(db, &graph);

    let report = migrator.migrate(None, false).await.unwrap();
    assert_eq!(report.applied, vec![migration.id.clone()]);

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
    // The foreign key is enforced (an inline REFERENCES would be ignored).
    let orphan = db
        .raw_execute(
            "INSERT INTO books (title, author_id, pages) VALUES (?, ?, ?)",
            vec![Value::Text("Orphan".into()), Value::Int(999), Value::Int(1)],
        )
        .await;
    assert!(orphan.is_err());

    let shown = migrator.show().await.unwrap();
    assert_eq!(shown, vec![(migration.id.clone(), true)]);

    migrator.rollback(None, Some(1), false).await.unwrap();
    assert!(db.raw_sql("SELECT id FROM authors", vec![]).await.is_err());
    t.cleanup().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The migration session lock must be released *and* its connection never
/// returned to the pool while locked: a second migrate on the same pool has
/// to succeed immediately instead of hanging on `GET_LOCK`.
#[tokio::test]
#[ignore = "needs a MySQL server: set MYSQL_URL"]
async fn second_migrate_after_first_succeeds() {
    let Some(t) = ScratchDb::mysql().await.unwrap() else {
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

/// A failed MySQL migration resumes after hand-repair instead of replaying
/// committed statements: the third statement fails, the repair drops its
/// half-applied table, and the re-run skips the first two operations.
#[tokio::test]
#[ignore = "needs a MySQL server: set MYSQL_URL"]
async fn mysql_failed_migration_resumes_after_repair() {
    let Some(t) = ScratchDb::mysql().await.unwrap() else {
        return;
    };
    let db = &t.db;
    let dir = temp_dir();

    let migration = Migration::new(
        "0001_rs_resume",
        Vec::new(),
        vec![
            Operation::RunSQL {
                sql: "CREATE TABLE `rs_t` (`n` INTEGER)".into(),
                reverse_sql: Some("DROP TABLE `rs_t`".into()),
            },
            Operation::RunSQL {
                sql: "CREATE TABLE `rs_u` (`n` INTEGER)".into(),
                reverse_sql: Some("DROP TABLE `rs_u`".into()),
            },
            Operation::RunSQL {
                sql: "CREATE TABLE `rs_u` (`n` INTEGER)".into(),
                reverse_sql: Some("DROP TABLE `rs_u`".into()),
            },
        ],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &migration).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let migrator = Migrator::new(db, &graph);

    let err = migrator.migrate(None, false).await.unwrap_err();
    assert!(
        matches!(
            err,
            MigrationError::MysqlPartial {
                index: 3,
                total: 3,
                ..
            }
        ),
        "{err:?}"
    );
    db.execute_script("DROP TABLE `rs_u`").await.unwrap();
    let report = migrator.migrate(None, false).await.unwrap();
    assert_eq!(report.applied, vec![migration.id.clone()]);
    db.raw_sql("SELECT `n` FROM `rs_t`", Vec::new())
        .await
        .unwrap();
    db.raw_sql("SELECT `n` FROM `rs_u`", Vec::new())
        .await
        .unwrap();

    t.cleanup().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Resume granularity is per statement, so one operation that renders several
/// statements does not replay its committed first statement on re-run.
///
/// An `AlterField` that renames a column *and* narrows its type renders two
/// statements. `ALTER TABLE ... MODIFY COLUMN` fails on a value MySQL strict
/// mode cannot cast, after the rename has already committed. The repair fixes
/// the data, and the re-run must skip the committed `RENAME COLUMN` (replaying
/// it would fail on the column's new name) and only re-issue the `MODIFY`.
#[tokio::test]
#[ignore = "needs a MySQL server: set MYSQL_URL"]
async fn mysql_resume_continues_inside_one_operation() {
    let Some(t) = ScratchDb::mysql().await.unwrap() else {
        return;
    };
    let db = &t.db;
    let dir = temp_dir();

    let create = Migration::new(
        "0001_rs_multi",
        Vec::new(),
        vec![Operation::CreateModel {
            model: ModelState {
                name: "Row".into(),
                table: "rs_row".into(),
                fields: vec![
                    FieldState {
                        primary_key: true,
                        auto: true,
                        ..field("id", SqlType::BigInt)
                    },
                    FieldState {
                        max_length: Some(20),
                        ..field("label", SqlType::Text)
                    },
                ],
                indexes: Vec::new(),
                constraints: Vec::new(),
            },
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
    db.raw_execute(
        "INSERT INTO `rs_row` (`label`) VALUES ('not a number')",
        Vec::new(),
    )
    .await
    .unwrap();

    let alter = Migration::new(
        "0002_rs_alter",
        vec![create.id.clone()],
        vec![Operation::AlterField {
            model: "Row".into(),
            // The field's new name/column is `amount`, so the editor renders
            // a `RENAME COLUMN` plus the `MODIFY COLUMN`.
            name: "label".into(),
            field: FieldState::new("amount", "amount", SqlType::BigInt),
        }],
        true,
        Vec::new(),
    )
    .unwrap();
    loader::write_migration(&dir, &alter).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let migrator = Migrator::new(db, &graph);

    // Statement 2 of 2 (`MODIFY COLUMN amount BIGINT NOT NULL`) fails on the
    // value; statement 1 (`RENAME COLUMN label TO amount`) committed.
    let err = migrator.migrate(None, false).await.unwrap_err();
    assert!(
        matches!(
            err,
            MigrationError::MysqlPartial {
                index: 2,
                total: 2,
                ..
            }
        ),
        "{err:?}"
    );
    let rows = db
        .raw_sql("SELECT `amount` FROM `rs_row`", Vec::new())
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 1, "the rename committed: {rows:?}");

    // Repair only the failing statement's cause.
    db.raw_execute("UPDATE `rs_row` SET `amount` = '7'", Vec::new())
        .await
        .unwrap();
    let report = migrator.migrate(None, false).await.unwrap();
    assert_eq!(report.applied, vec![alter.id.clone()]);
    let rows = db
        .raw_sql("SELECT `amount` FROM `rs_row`", Vec::new())
        .await
        .unwrap();
    assert_eq!(rows.rows[0].get("amount"), Some(&Value::Int(7)), "{rows:?}");

    t.cleanup().await.unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

fn field(name: &str, ty: SqlType) -> FieldState {
    FieldState::new(name, name, ty)
}

/// Every operation kind, rendered by the editor and executed on the server.
#[tokio::test]
#[ignore = "needs a MySQL server: set MYSQL_URL"]
async fn every_operation_runs_on_mysql() {
    let Some(t) = ScratchDb::mysql().await.unwrap() else {
        return;
    };
    let db = &t.db;
    let pk = || FieldState {
        primary_key: true,
        auto: true,
        ..field("id", SqlType::BigInt)
    };
    let parent = ModelState {
        name: "Parent".into(),
        table: "ax_parent".into(),
        fields: vec![
            pk(),
            FieldState {
                max_length: Some(50),
                unique: true,
                ..field("code", SqlType::Text)
            },
        ],
        indexes: Vec::new(),
        constraints: Vec::new(),
    };
    let child = ModelState {
        name: "Child".into(),
        table: "ax_child".into(),
        fields: vec![
            pk(),
            FieldState {
                index: true,
                fk: Some(ForeignKeyState {
                    target_table: "ax_parent".into(),
                    target_column: "id".into(),
                    on_delete: OnDelete::Cascade,
                }),
                ..field("parent_id", SqlType::BigInt)
            },
            FieldState {
                default: Some(DbDefault::Text("it's".into())),
                ..field("note", SqlType::Text)
            },
            FieldState {
                default: Some(DbDefault::Now),
                ..field("created", SqlType::Timestamp)
            },
        ],
        indexes: Vec::new(),
        constraints: vec![ConstraintState::Check {
            name: "ax_child_parent_pos".into(),
            sql: "parent_id > 0".into(),
        }],
    };
    let operations = vec![
        Operation::CreateModel { model: parent },
        Operation::CreateModel { model: child },
        Operation::AddField {
            model: "Child".into(),
            field: FieldState {
                nullable: true,
                ..field("score", SqlType::Integer)
            },
        },
        Operation::AlterField {
            model: "Child".into(),
            name: "score".into(),
            field: FieldState {
                nullable: false,
                default: Some(DbDefault::Int(3)),
                ..field("score", SqlType::BigInt)
            },
        },
        Operation::RenameField {
            model: "Child".into(),
            old_name: "score".into(),
            new_name: "points".into(),
        },
        Operation::CreateIndex {
            model: "Child".into(),
            index: IndexState {
                name: "ax_child_points_idx".into(),
                columns: vec!["points".into()],
                unique: false,
            },
        },
        Operation::AddConstraint {
            model: "Child".into(),
            constraint: ConstraintState::Unique {
                name: "ax_child_note_points_uniq".into(),
                columns: vec!["parent_id".into(), "points".into()],
            },
        },
        Operation::DeleteConstraint {
            model: "Child".into(),
            name: "ax_child_note_points_uniq".into(),
        },
        Operation::DeleteConstraint {
            model: "Child".into(),
            name: "ax_child_parent_pos".into(),
        },
        Operation::DeleteIndex {
            model: "Child".into(),
            name: "ax_child_points_idx".into(),
        },
        Operation::RemoveField {
            model: "Child".into(),
            name: "parent_id".into(),
        },
        Operation::DeleteModel {
            name: "Child".into(),
        },
        Operation::DeleteModel {
            name: "Parent".into(),
        },
    ];
    let mut state = ProjectState::new();
    for op in &operations {
        for sql in schema_editor::render(BackendKind::MySql, op, &state).unwrap() {
            db.execute_script(&sql)
                .await
                .unwrap_or_else(|err| panic!("{sql}: {err}"));
        }
        op.apply_to_state(&mut state).unwrap();
    }
    t.cleanup().await.unwrap();
}
