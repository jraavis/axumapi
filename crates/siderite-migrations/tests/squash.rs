//! Squash concatenates, merges CreateModel+AddField, and cancels create+delete.
#![allow(clippy::unwrap_used)]

mod common;

use common::temp_dir;
use siderite_migrations::SqlType;
use siderite_migrations::loader::{self, MigrationGraph};
use siderite_migrations::migration::Migration;
use siderite_migrations::operation::Operation;
use siderite_migrations::squash::{self, optimize};
use siderite_migrations::state::{FieldState, ModelState};

fn model(name: &str, fields: Vec<FieldState>) -> ModelState {
    ModelState {
        name: name.into(),
        table: name.to_ascii_lowercase(),
        fields,
        indexes: Vec::new(),
        constraints: Vec::new(),
    }
}

#[test]
fn create_then_add_field_merges() {
    let ops = vec![
        Operation::CreateModel {
            model: model("User", vec![FieldState::new("id", "id", SqlType::BigInt)]),
        },
        Operation::AddField {
            model: "User".into(),
            field: FieldState::new("email", "email", SqlType::Text),
        },
    ];
    let out = optimize(ops);
    assert_eq!(out.len(), 1);
    match &out[0] {
        Operation::CreateModel { model } => {
            assert_eq!(model.fields.len(), 2);
            assert_eq!(model.fields[1].name, "email");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn create_then_delete_cancels() {
    let ops = vec![
        Operation::CreateModel {
            model: model("Temp", vec![FieldState::new("id", "id", SqlType::BigInt)]),
        },
        Operation::AddField {
            model: "Temp".into(),
            field: FieldState::new("x", "x", SqlType::Integer),
        },
        Operation::DeleteModel {
            name: "Temp".into(),
        },
    ];
    let out = optimize(ops);
    assert!(out.is_empty(), "{out:?}");
}

#[test]
fn squash_writes_replaces_and_loader_hides_range() {
    let dir = temp_dir();
    let m1 = Migration::new(
        "0001_initial",
        vec![],
        vec![Operation::CreateModel {
            model: model("User", vec![FieldState::new("id", "id", SqlType::BigInt)]),
        }],
        true,
        vec![],
    )
    .unwrap();
    let m2 = Migration::new(
        "0002_email",
        vec!["0001_initial".into()],
        vec![Operation::AddField {
            model: "User".into(),
            field: FieldState::new("email", "email", SqlType::Text),
        }],
        true,
        vec![],
    )
    .unwrap();
    loader::write_migration(&dir, &m1).unwrap();
    loader::write_migration(&dir, &m2).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    let squashed = squash::squash(&graph, "0001_initial", "0002_email", Some("users")).unwrap();
    assert_eq!(
        squashed.replaces,
        vec!["0001_initial".to_owned(), "0002_email".to_owned()]
    );
    assert_eq!(squashed.operations.len(), 1);
    match &squashed.operations[0] {
        Operation::CreateModel { model } => assert_eq!(model.fields.len(), 2),
        other => panic!("{other:?}"),
    }
    loader::write_migration(&dir, &squashed).unwrap();
    let graph = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap();
    assert_eq!(graph.order, vec![squashed.id.clone()]);
    assert!(graph.replaced_by.contains_key("0001_initial"));

    let mut history = std::collections::HashSet::new();
    history.insert("0001_initial".into());
    history.insert("0002_email".into());
    assert!(loader::is_applied(&squashed, &history));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn multiple_heads_error() {
    let dir = temp_dir();
    let a = Migration::new(
        "0001_a",
        vec![],
        vec![Operation::RunSQL {
            sql: "SELECT 1".into(),
            reverse_sql: Some("SELECT 1".into()),
        }],
        true,
        vec![],
    )
    .unwrap();
    let b = Migration::new(
        "0002_b",
        vec![],
        vec![Operation::RunSQL {
            sql: "SELECT 2".into(),
            reverse_sql: Some("SELECT 2".into()),
        }],
        true,
        vec![],
    )
    .unwrap();
    loader::write_migration(&dir, &a).unwrap();
    loader::write_migration(&dir, &b).unwrap();
    let err = MigrationGraph::build(loader::load_dir(&dir).unwrap()).unwrap_err();
    assert!(
        matches!(
            err,
            siderite_migrations::MigrationError::MultipleHeads { .. }
        ),
        "{err:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
