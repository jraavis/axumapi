//! Live MySQL round trip. Ignored by default: set `MYSQL_URL` (or
//! `DATABASE_URL`) to a `mysql://` URL of a scratch database and run
//! `cargo test -p siderite-migrations --all-features -- --ignored`.
#![allow(clippy::unwrap_used)]

mod common;

use common::{author_meta, book_meta, temp_dir};
use siderite_backends::mysql::MySqlBackend;
use siderite_migrations::executor::Migrator;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::make_migrations;
use siderite_migrations::operation::Operation;
use siderite_migrations::schema_editor;
use siderite_migrations::state::{
    ConstraintState, DbDefault, FieldState, ForeignKeyState, IndexState, ModelState, OnDelete,
    ProjectState, SqlType,
};
use siderite_orm::{BackendKind, Db, Value};

fn mysql_url() -> Option<String> {
    ["MYSQL_URL", "DATABASE_URL"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|url| url.to_ascii_lowercase().starts_with("mysql://"))
}

async fn reset(db: &Db) {
    // Drop children before parents (foreign keys).
    for table in ["books", "authors", "siderite_migrations"] {
        db.execute_script(&format!("DROP TABLE IF EXISTS `{table}`"))
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "needs a MySQL server: set MYSQL_URL"]
async fn migrate_and_rollback_on_mysql() {
    let Some(url) = mysql_url() else {
        eprintln!("MYSQL_URL not set; skipping");
        return;
    };
    let db = Db::new(MySqlBackend::connect(&url).await.unwrap());
    reset(&db).await;

    let dir = temp_dir();
    let models: &[&'static siderite_orm::ModelMeta] = &[author_meta(), book_meta()];
    let migration = make_migrations(models, &dir, None, false).unwrap().unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let migrator = Migrator::new(&db, &graph);

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
    assert!(db.raw_sql("SELECT 1 FROM authors", vec![]).await.is_err());
    reset(&db).await;
}

fn field(name: &str, ty: SqlType) -> FieldState {
    FieldState::new(name, name, ty)
}

/// Every operation kind, rendered by the editor and executed on the server.
#[tokio::test]
#[ignore = "needs a MySQL server: set MYSQL_URL"]
async fn every_operation_runs_on_mysql() {
    let Some(url) = mysql_url() else {
        eprintln!("MYSQL_URL not set; skipping");
        return;
    };
    let db = Db::new(MySqlBackend::connect(&url).await.unwrap());
    for table in ["ax_child", "ax_parent"] {
        db.execute_script(&format!("DROP TABLE IF EXISTS `{table}`"))
            .await
            .unwrap();
    }
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
}
