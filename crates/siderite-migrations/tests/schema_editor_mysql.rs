//! MySQL DDL strings. No database is needed: these check the SQL text only.
#![allow(clippy::unwrap_used)]

mod common;

use common::{author_meta, book_meta, post_meta, tag_meta};
use siderite_migrations::MigrationError;
use siderite_migrations::operation::Operation;
use siderite_migrations::schema_editor;
use siderite_migrations::state::{
    ConstraintState, DbDefault, FieldState, ForeignKeyState, IndexState, ModelState, OnDelete,
    ProjectState, SqlType,
};
use siderite_orm::BackendKind;

fn render(state: &ProjectState, op: &Operation) -> Result<Vec<String>, MigrationError> {
    schema_editor::render(BackendKind::MySql, op, state)
}

fn initial(metas: &[&'static siderite_orm::ModelMeta]) -> Vec<String> {
    let to = ProjectState::from_metas(metas);
    let ops = siderite_migrations::diff(&ProjectState::new(), &to).unwrap();
    schema_editor::statements(BackendKind::MySql, &ProjectState::new(), &ops).unwrap()
}

fn state_with(model: ModelState) -> ProjectState {
    let mut state = ProjectState::new();
    Operation::CreateModel { model }
        .apply_to_state(&mut state)
        .unwrap();
    state
}

fn pk() -> FieldState {
    FieldState {
        primary_key: true,
        auto: true,
        ..FieldState::new("id", "id", SqlType::BigInt)
    }
}

fn model(fields: Vec<FieldState>) -> ModelState {
    ModelState {
        name: "Thing".into(),
        table: "things".into(),
        fields,
        indexes: Vec::new(),
        constraints: Vec::new(),
    }
}

fn err_text(result: Result<Vec<String>, MigrationError>) -> String {
    result.unwrap_err().to_string()
}

#[test]
fn author_book_snapshot() {
    let sql = initial(&[author_meta(), book_meta()]);
    assert_eq!(
        sql[0],
        "CREATE TABLE `authors` (\n  `id` BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,\n  `name` VARCHAR(100) NOT NULL\n)"
    );
    let joined = sql.join(";\n");
    assert!(joined.contains("`pages` INT\n") || joined.contains("`pages` INT,"));
    assert!(!joined.contains("`pages` INT NOT NULL"));
    assert!(joined.contains(
        "CONSTRAINT `books_author_id_fk` FOREIGN KEY (`author_id`) REFERENCES `authors` (`id`) ON DELETE CASCADE"
    ));
    assert!(joined.contains("CREATE INDEX `books_author_id_idx` ON `books` (`author_id`)"));
}

#[test]
fn inline_references_are_never_emitted() {
    let sql = initial(&[author_meta(), book_meta(), tag_meta()]).join(";\n");
    assert!(!sql.contains("BIGINT NOT NULL REFERENCES"));
    assert!(
        !sql.contains('"'),
        "MySQL DDL must not use double quotes: {sql}"
    );
}

#[test]
fn now_default_and_unique_varchar() {
    let m = model(vec![
        pk(),
        FieldState {
            default: Some(DbDefault::Now),
            ..FieldState::new("created_at", "created_at", SqlType::Timestamp)
        },
    ]);
    let sql = render(&ProjectState::new(), &Operation::CreateModel { model: m }).unwrap();
    assert!(sql[0].contains("`created_at` DATETIME(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6)"));
    let sql = initial(&[tag_meta()]).join(";\n");
    assert!(sql.contains("`slug` VARCHAR(50) NOT NULL UNIQUE"));
}

#[test]
fn indexed_text_without_length_in_a_model_is_rejected() {
    // The reference `Post` indexes `title`, a TEXT column without `max_length`.
    let to = ProjectState::from_metas(&[post_meta(), tag_meta()]);
    let ops = siderite_migrations::diff(&ProjectState::new(), &to).unwrap();
    let err =
        schema_editor::statements(BackendKind::MySql, &ProjectState::new(), &ops).unwrap_err();
    assert!(err.to_string().contains("max_length"), "{err}");
}

#[test]
fn column_types_follow_the_backend_storage_table() {
    let cases = [
        (SqlType::SmallInt, "SMALLINT"),
        (SqlType::Integer, "INT"),
        (SqlType::BigInt, "BIGINT"),
        (SqlType::Real, "FLOAT"),
        (SqlType::Double, "DOUBLE"),
        (SqlType::Decimal, "DECIMAL(38,10)"),
        (SqlType::Bool, "TINYINT(1)"),
        (SqlType::Binary, "BLOB"),
        (SqlType::Date, "DATE"),
        (SqlType::Time, "TIME(6)"),
        (SqlType::Timestamp, "DATETIME(6)"),
        (SqlType::Duration, "BIGINT"),
        (SqlType::Uuid, "CHAR(36)"),
        (SqlType::Json, "JSON"),
        (SqlType::IpAddr, "VARCHAR(45)"),
    ];
    for (ty, expected) in cases {
        let m = model(vec![pk(), FieldState::new("v", "v", ty)]);
        let sql = render(&ProjectState::new(), &Operation::CreateModel { model: m })
            .unwrap()
            .join(";");
        assert!(
            sql.contains(&format!("`v` {expected} NOT NULL")),
            "{ty:?}: {sql}"
        );
    }
}

#[test]
fn decimal_and_varchar_sizes() {
    let m = model(vec![
        pk(),
        FieldState {
            max_digits: Some(8),
            decimal_places: Some(2),
            ..FieldState::new("price", "price", SqlType::Decimal)
        },
        FieldState {
            max_length: Some(20),
            ..FieldState::new("code", "code", SqlType::Text)
        },
    ]);
    let sql = render(&ProjectState::new(), &Operation::CreateModel { model: m })
        .unwrap()
        .join(";");
    assert!(sql.contains("`price` DECIMAL(8,2) NOT NULL"));
    assert!(sql.contains("`code` VARCHAR(20) NOT NULL"));
}

#[test]
fn auto_key_widths_are_preserved() {
    for (ty, expected) in [
        (SqlType::SmallInt, "SMALLINT"),
        (SqlType::Integer, "INT"),
        (SqlType::BigInt, "BIGINT"),
    ] {
        let m = model(vec![FieldState {
            primary_key: true,
            auto: true,
            ..FieldState::new("id", "id", ty)
        }]);
        let sql = render(&ProjectState::new(), &Operation::CreateModel { model: m }).unwrap();
        assert!(
            sql[0].contains(&format!(
                "`id` {expected} NOT NULL AUTO_INCREMENT PRIMARY KEY"
            )),
            "{}",
            sql[0]
        );
    }
}

#[test]
fn defaults_are_typed_and_escaped() {
    let m = model(vec![
        pk(),
        FieldState {
            default: Some(DbDefault::Int(7)),
            ..FieldState::new("n", "n", SqlType::Integer)
        },
        FieldState {
            default: Some(DbDefault::Bool(true)),
            ..FieldState::new("on", "on", SqlType::Bool)
        },
        FieldState {
            default: Some(DbDefault::Bool(false)),
            ..FieldState::new("off", "off", SqlType::Bool)
        },
        FieldState {
            max_length: Some(10),
            default: Some(DbDefault::Text(r"it's a\b".into())),
            ..FieldState::new("label", "label", SqlType::Text)
        },
        FieldState {
            default: Some(DbDefault::Text("hello".into())),
            ..FieldState::new("note", "note", SqlType::Text)
        },
    ]);
    let sql = render(&ProjectState::new(), &Operation::CreateModel { model: m })
        .unwrap()
        .join(";");
    assert!(sql.contains("`n` INT NOT NULL DEFAULT 7"));
    assert!(sql.contains("`on` TINYINT(1) NOT NULL DEFAULT TRUE"));
    assert!(sql.contains("`off` TINYINT(1) NOT NULL DEFAULT FALSE"));
    assert!(sql.contains(r"`label` VARCHAR(10) NOT NULL DEFAULT 'it''s a\\b'"));
    // TEXT only accepts an expression default.
    assert!(sql.contains("`note` TEXT NOT NULL DEFAULT ('hello')"));
}

#[test]
fn table_constraints_and_named_indexes() {
    let mut m = model(vec![
        pk(),
        FieldState {
            max_length: Some(10),
            ..FieldState::new("a", "a", SqlType::Text)
        },
        FieldState::new("b", "b", SqlType::Integer),
    ]);
    m.constraints = vec![
        ConstraintState::Unique {
            name: "things_a_b_uniq".into(),
            columns: vec!["a".into(), "b".into()],
        },
        ConstraintState::Check {
            name: "things_b_pos".into(),
            sql: "b > 0".into(),
        },
    ];
    m.indexes = vec![IndexState {
        name: "things_b_idx".into(),
        columns: vec!["b".into()],
        unique: true,
    }];
    let sql = render(&ProjectState::new(), &Operation::CreateModel { model: m }).unwrap();
    assert!(sql[0].contains("CONSTRAINT `things_a_b_uniq` UNIQUE (`a`, `b`)"));
    assert!(sql[0].contains("CONSTRAINT `things_b_pos` CHECK (b > 0)"));
    assert_eq!(
        sql[1],
        "CREATE UNIQUE INDEX `things_b_idx` ON `things` (`b`)"
    );
}

#[test]
fn delete_model_and_run_sql() {
    let state = state_with(model(vec![pk()]));
    assert_eq!(
        render(
            &state,
            &Operation::DeleteModel {
                name: "Thing".into()
            }
        )
        .unwrap(),
        vec!["DROP TABLE `things`".to_owned()]
    );
    assert_eq!(
        render(
            &state,
            &Operation::RunSQL {
                sql: "SELECT 1".into(),
                reverse_sql: None
            }
        )
        .unwrap(),
        vec!["SELECT 1".to_owned()]
    );
    assert!(
        render(
            &state,
            &Operation::RunRust {
                name: "x".into(),
                backwards: None
            }
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn add_field_with_index_and_foreign_key() {
    let state = state_with(model(vec![pk()]));
    let field = FieldState {
        index: true,
        fk: Some(ForeignKeyState {
            target_table: "owners".into(),
            target_column: "id".into(),
            on_delete: OnDelete::SetNull,
        }),
        nullable: true,
        ..FieldState::new("owner", "owner_id", SqlType::BigInt)
    };
    let sql = render(
        &state,
        &Operation::AddField {
            model: "Thing".into(),
            field,
        },
    )
    .unwrap();
    assert_eq!(
        sql,
        vec![
            "ALTER TABLE `things` ADD COLUMN `owner_id` BIGINT".to_owned(),
            "CREATE INDEX `things_owner_id_idx` ON `things` (`owner_id`)".to_owned(),
            "ALTER TABLE `things` ADD CONSTRAINT `things_owner_id_fk` FOREIGN KEY (`owner_id`) REFERENCES `owners` (`id`) ON DELETE SET NULL".to_owned(),
        ]
    );
}

#[test]
fn remove_field_drops_the_foreign_key_first() {
    let state = state_with(model(vec![
        pk(),
        FieldState {
            fk: Some(ForeignKeyState {
                target_table: "owners".into(),
                target_column: "id".into(),
                on_delete: OnDelete::Cascade,
            }),
            ..FieldState::new("owner", "owner_id", SqlType::BigInt)
        },
        FieldState::new("plain", "plain", SqlType::Integer),
    ]));
    let fk = render(
        &state,
        &Operation::RemoveField {
            model: "Thing".into(),
            name: "owner".into(),
        },
    )
    .unwrap();
    assert_eq!(
        fk,
        vec![
            "ALTER TABLE `things` DROP FOREIGN KEY `things_owner_id_fk`".to_owned(),
            "ALTER TABLE `things` DROP COLUMN `owner_id`".to_owned(),
        ]
    );
    let plain = render(
        &state,
        &Operation::RemoveField {
            model: "Thing".into(),
            name: "plain".into(),
        },
    )
    .unwrap();
    assert_eq!(
        plain,
        vec!["ALTER TABLE `things` DROP COLUMN `plain`".to_owned()]
    );
}

#[test]
fn alter_field_restates_the_whole_definition() {
    let state = state_with(model(vec![
        pk(),
        FieldState {
            max_length: Some(10),
            default: Some(DbDefault::Text("x".into())),
            ..FieldState::new("name", "name", SqlType::Text)
        },
    ]));
    // Only nullability changes; type and default must still be restated.
    let sql = render(
        &state,
        &Operation::AlterField {
            model: "Thing".into(),
            name: "name".into(),
            field: FieldState {
                nullable: true,
                max_length: Some(10),
                default: Some(DbDefault::Text("x".into())),
                ..FieldState::new("name", "name", SqlType::Text)
            },
        },
    )
    .unwrap();
    assert_eq!(
        sql,
        vec!["ALTER TABLE `things` MODIFY COLUMN `name` VARCHAR(10) DEFAULT 'x'".to_owned()]
    );
}

#[test]
fn alter_field_renames_before_modifying_and_skips_no_ops() {
    let state = state_with(model(vec![
        pk(),
        FieldState::new("n", "n", SqlType::Integer),
    ]));
    let sql = render(
        &state,
        &Operation::AlterField {
            model: "Thing".into(),
            name: "n".into(),
            field: FieldState::new("n", "count", SqlType::BigInt),
        },
    )
    .unwrap();
    assert_eq!(
        sql,
        vec![
            "ALTER TABLE `things` RENAME COLUMN `n` TO `count`".to_owned(),
            "ALTER TABLE `things` MODIFY COLUMN `count` BIGINT NOT NULL".to_owned(),
        ]
    );
    let none = render(
        &state,
        &Operation::AlterField {
            model: "Thing".into(),
            name: "n".into(),
            field: FieldState::new("n", "n", SqlType::Integer),
        },
    )
    .unwrap();
    assert!(none.is_empty());
}

#[test]
fn modify_never_repeats_key_clauses() {
    let state = state_with(model(vec![pk()]));
    let sql = render(
        &state,
        &Operation::AlterField {
            model: "Thing".into(),
            name: "id".into(),
            field: FieldState {
                primary_key: true,
                auto: true,
                ..FieldState::new("id", "id", SqlType::Integer)
            },
        },
    )
    .unwrap();
    assert_eq!(
        sql,
        vec!["ALTER TABLE `things` MODIFY COLUMN `id` INT NOT NULL AUTO_INCREMENT".to_owned()]
    );
}

#[test]
fn rename_field_uses_rename_column() {
    let state = state_with(model(vec![
        pk(),
        FieldState::new("old", "old", SqlType::Integer),
    ]));
    let sql = render(
        &state,
        &Operation::RenameField {
            model: "Thing".into(),
            old_name: "old".into(),
            new_name: "fresh".into(),
        },
    )
    .unwrap();
    assert_eq!(
        sql,
        vec!["ALTER TABLE `things` RENAME COLUMN `old` TO `fresh`".to_owned()]
    );
}

#[test]
fn index_operations_name_their_table() {
    let state = state_with(model(vec![
        pk(),
        FieldState::new("n", "n", SqlType::Integer),
    ]));
    let create = render(
        &state,
        &Operation::CreateIndex {
            model: "Thing".into(),
            index: IndexState {
                name: "things_n_idx".into(),
                columns: vec!["n".into()],
                unique: false,
            },
        },
    )
    .unwrap();
    assert_eq!(
        create,
        vec!["CREATE INDEX `things_n_idx` ON `things` (`n`)".to_owned()]
    );
    let drop = render(
        &state,
        &Operation::DeleteIndex {
            model: "Thing".into(),
            name: "things_n_idx".into(),
        },
    )
    .unwrap();
    assert_eq!(
        drop,
        vec!["DROP INDEX `things_n_idx` ON `things`".to_owned()]
    );
}

#[test]
fn constraint_operations() {
    let mut m = model(vec![pk(), FieldState::new("n", "n", SqlType::Integer)]);
    m.constraints = vec![
        ConstraintState::Unique {
            name: "things_n_uniq".into(),
            columns: vec!["n".into()],
        },
        ConstraintState::Check {
            name: "things_n_pos".into(),
            sql: "n > 0".into(),
        },
    ];
    let state = state_with(m);
    let add = render(
        &state,
        &Operation::AddConstraint {
            model: "Thing".into(),
            constraint: ConstraintState::Check {
                name: "things_n_small".into(),
                sql: "n < 100".into(),
            },
        },
    )
    .unwrap();
    assert_eq!(
        add,
        vec!["ALTER TABLE `things` ADD CONSTRAINT `things_n_small` CHECK (n < 100)".to_owned()]
    );
    let drop_unique = render(
        &state,
        &Operation::DeleteConstraint {
            model: "Thing".into(),
            name: "things_n_uniq".into(),
        },
    )
    .unwrap();
    assert_eq!(
        drop_unique,
        vec!["DROP INDEX `things_n_uniq` ON `things`".to_owned()]
    );
    let drop_check = render(
        &state,
        &Operation::DeleteConstraint {
            model: "Thing".into(),
            name: "things_n_pos".into(),
        },
    )
    .unwrap();
    assert_eq!(
        drop_check,
        vec!["ALTER TABLE `things` DROP CHECK `things_n_pos`".to_owned()]
    );
    let missing = render(
        &state,
        &Operation::DeleteConstraint {
            model: "Thing".into(),
            name: "nope".into(),
        },
    );
    assert!(err_text(missing).contains("constraint `nope`"));
}

#[test]
fn keyed_text_without_length_is_rejected() {
    for (label, field) in [
        (
            "primary key",
            FieldState {
                primary_key: true,
                ..FieldState::new("k", "k", SqlType::Text)
            },
        ),
        (
            "UNIQUE",
            FieldState {
                unique: true,
                ..FieldState::new("k", "k", SqlType::Text)
            },
        ),
        (
            "indexed",
            FieldState {
                index: true,
                ..FieldState::new("k", "k", SqlType::Json)
            },
        ),
    ] {
        let m = model(vec![field]);
        let message = err_text(render(
            &ProjectState::new(),
            &Operation::CreateModel { model: m },
        ));
        assert!(message.contains("max_length"), "{label}: {message}");
        assert!(message.contains("`k`"), "{label}: {message}");
    }
}

#[test]
fn keyed_text_with_length_is_accepted() {
    let m = model(vec![FieldState {
        primary_key: true,
        max_length: Some(64),
        ..FieldState::new("k", "k", SqlType::Text)
    }]);
    let sql = render(&ProjectState::new(), &Operation::CreateModel { model: m }).unwrap();
    assert!(sql[0].contains("`k` VARCHAR(64) NOT NULL PRIMARY KEY"));
}

#[test]
fn keyed_text_is_rejected_in_indexes_constraints_and_new_columns() {
    let mut m = model(vec![pk(), FieldState::new("body", "body", SqlType::Text)]);
    m.indexes = vec![IndexState {
        name: "things_body_idx".into(),
        columns: vec!["body".into()],
        unique: false,
    }];
    assert!(
        err_text(render(
            &ProjectState::new(),
            &Operation::CreateModel { model: m }
        ))
        .contains("max_length")
    );

    let state = state_with(model(vec![
        pk(),
        FieldState::new("body", "body", SqlType::Text),
    ]));
    let index = render(
        &state,
        &Operation::CreateIndex {
            model: "Thing".into(),
            index: IndexState {
                name: "i".into(),
                columns: vec!["body".into()],
                unique: false,
            },
        },
    );
    assert!(err_text(index).contains("max_length"));
    let unique = render(
        &state,
        &Operation::AddConstraint {
            model: "Thing".into(),
            constraint: ConstraintState::Unique {
                name: "u".into(),
                columns: vec!["body".into()],
            },
        },
    );
    assert!(err_text(unique).contains("max_length"));
    let add = render(
        &state,
        &Operation::AddField {
            model: "Thing".into(),
            field: FieldState {
                unique: true,
                ..FieldState::new("slug", "slug", SqlType::Text)
            },
        },
    );
    assert!(err_text(add).contains("max_length"));
}

#[test]
fn on_delete_set_default_is_rejected() {
    let m = model(vec![
        pk(),
        FieldState {
            fk: Some(ForeignKeyState {
                target_table: "owners".into(),
                target_column: "id".into(),
                on_delete: OnDelete::SetDefault,
            }),
            ..FieldState::new("owner", "owner_id", SqlType::BigInt)
        },
    ]);
    let message = err_text(render(
        &ProjectState::new(),
        &Operation::CreateModel { model: m },
    ));
    assert!(message.contains("SET DEFAULT"), "{message}");
}

#[test]
fn on_delete_policies_map_to_mysql_keywords() {
    for (policy, keyword) in [
        (OnDelete::Cascade, "CASCADE"),
        (OnDelete::Protect, "RESTRICT"),
        (OnDelete::SetNull, "SET NULL"),
        (OnDelete::DoNothing, "NO ACTION"),
    ] {
        let m = model(vec![
            pk(),
            FieldState {
                fk: Some(ForeignKeyState {
                    target_table: "owners".into(),
                    target_column: "id".into(),
                    on_delete: policy,
                }),
                ..FieldState::new("owner", "owner_id", SqlType::BigInt)
            },
        ]);
        let sql = render(&ProjectState::new(), &Operation::CreateModel { model: m }).unwrap();
        assert!(
            sql[0].contains(&format!("ON DELETE {keyword}")),
            "{}",
            sql[0]
        );
    }
}

#[test]
fn unknown_model_or_field_is_a_state_error() {
    let state = ProjectState::new();
    assert!(
        render(
            &state,
            &Operation::DeleteModel {
                name: "Ghost".into()
            }
        )
        .is_err()
    );
    let state = state_with(model(vec![pk()]));
    assert!(
        render(
            &state,
            &Operation::RemoveField {
                model: "Thing".into(),
                name: "ghost".into()
            }
        )
        .is_err()
    );
}

#[test]
fn quote_identifier_selects_the_dialect() {
    assert_eq!(
        schema_editor::quote_identifier(BackendKind::MySql, "a`b"),
        "`a``b`"
    );
    assert_eq!(
        schema_editor::quote_identifier(BackendKind::Postgres, "a"),
        "\"a\""
    );
    assert_eq!(
        schema_editor::quote_identifier(BackendKind::Sqlite, "a"),
        "\"a\""
    );
}
