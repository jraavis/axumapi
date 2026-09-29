//! Autodetector: add / remove / alter / rename-hint / index / constraint / M2M.
#![allow(clippy::unwrap_used)]

mod common;

use axumapi_migrations::{
    ConstraintState, FieldState, IndexState, Operation, ProjectState, RenameHints, SqlType, diff,
    diff_with,
};
use common::{author_meta, book_meta, post_meta, tag_meta};

fn names(ops: &[Operation]) -> Vec<String> {
    ops.iter().map(Operation::summary).collect()
}

#[test]
fn initial_creates_author_before_book() {
    let to = ProjectState::from_metas(&[author_meta(), book_meta()]);
    let ops = diff(&ProjectState::new(), &to);
    let summaries = names(&ops);
    let author = summaries
        .iter()
        .position(|s| s == "CreateModel Author")
        .unwrap();
    let book = summaries
        .iter()
        .position(|s| s == "CreateModel Book")
        .unwrap();
    assert!(author < book, "{summaries:?}");
}

#[test]
fn add_and_remove_field() {
    let from = ProjectState::from_metas(&[author_meta(), book_meta()]);
    let mut to = from.clone();
    to.model_mut("Book")
        .unwrap()
        .fields
        .push(FieldState::new("isbn", "isbn", SqlType::Text));
    let ops = diff(&from, &to);
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Operation::AddField { model, field } if model == "Book" && field.name == "isbn"
        )),
        "{ops:?}"
    );

    let ops = diff(&to, &from);
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Operation::RemoveField { model, name } if model == "Book" && name == "isbn"
        )),
        "{ops:?}"
    );
}

#[test]
fn alter_field_nullability() {
    let from = ProjectState::from_metas(&[author_meta(), book_meta()]);
    let mut to = from.clone();
    to.model_mut("Book")
        .unwrap()
        .field_mut("pages")
        .unwrap()
        .nullable = false;
    let ops = diff(&from, &to);
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Operation::AlterField { model, name, .. } if model == "Book" && name == "pages"
        )),
        "{ops:?}"
    );
}

#[test]
fn rename_without_hint_is_remove_and_add() {
    let from = ProjectState::from_metas(&[author_meta()]);
    let mut to = from.clone();
    to.model_mut("Author")
        .unwrap()
        .field_mut("name")
        .unwrap()
        .name = "full_name".into();
    to.model_mut("Author")
        .unwrap()
        .field_mut("full_name")
        .unwrap()
        .column = "full_name".into();
    let ops = diff(&from, &to);
    assert!(
        ops.iter()
            .any(|op| matches!(op, Operation::RemoveField { name, .. } if name == "name"))
    );
    assert!(
        ops.iter()
            .any(|op| matches!(op, Operation::AddField { field, .. } if field.name == "full_name"))
    );
    assert!(
        !ops.iter()
            .any(|op| matches!(op, Operation::RenameField { .. }))
    );
}

#[test]
fn rename_hint_emits_rename_field() {
    let from = ProjectState::from_metas(&[author_meta()]);
    let mut to = from.clone();
    to.model_mut("Author")
        .unwrap()
        .field_mut("name")
        .unwrap()
        .name = "full_name".into();
    to.model_mut("Author")
        .unwrap()
        .field_mut("full_name")
        .unwrap()
        .column = "full_name".into();
    let hints = RenameHints::new().rename_field("Author", "name", "full_name");
    let ops = diff_with(&from, &to, &hints);
    assert!(
        ops.iter().any(|op| matches!(
            op,
            Operation::RenameField {
                model,
                old_name,
                new_name,
            } if model == "Author" && old_name == "name" && new_name == "full_name"
        )),
        "{ops:?}"
    );
}

#[test]
fn index_and_constraint_add_remove() {
    let from = ProjectState::from_metas(&[author_meta()]);
    let mut to = from.clone();
    to.model_mut("Author").unwrap().indexes.push(IndexState {
        name: "author_name_idx".into(),
        columns: vec!["name".into()],
        unique: false,
    });
    to.model_mut("Author")
        .unwrap()
        .constraints
        .push(ConstraintState::Check {
            name: "name_present".into(),
            sql: "length(name) > 0".into(),
        });
    let ops = diff(&from, &to);
    assert!(ops.iter().any(
        |op| matches!(op, Operation::CreateIndex { index, .. } if index.name == "author_name_idx")
    ));
    assert!(ops.iter().any(|op| matches!(
        op,
        Operation::AddConstraint { constraint, .. } if constraint.name() == "name_present"
    )));

    let ops = diff(&to, &from);
    assert!(
        ops.iter().any(
            |op| matches!(op, Operation::DeleteIndex { name, .. } if name == "author_name_idx")
        )
    );
    assert!(ops.iter().any(|op| matches!(
        op,
        Operation::DeleteConstraint { name, .. } if name == "name_present"
    )));
}

#[test]
fn m2m_creates_join_table_after_both_sides() {
    let to = ProjectState::from_metas(&[post_meta(), tag_meta()]);
    let ops = diff(&ProjectState::new(), &to);
    let summaries = names(&ops);
    let post = summaries
        .iter()
        .position(|s| s == "CreateModel Post")
        .unwrap();
    let tag = summaries
        .iter()
        .position(|s| s == "CreateModel Tag")
        .unwrap();
    let join = summaries
        .iter()
        .position(|s| s == "CreateModel post_tags")
        .unwrap();
    assert!(post < join && tag < join, "{summaries:?}");
}

#[test]
fn delete_book_before_author() {
    let from = ProjectState::from_metas(&[author_meta(), book_meta()]);
    let ops = diff(&from, &ProjectState::new());
    let summaries = names(&ops);
    let book = summaries
        .iter()
        .position(|s| s == "DeleteModel Book")
        .unwrap();
    let author = summaries
        .iter()
        .position(|s| s == "DeleteModel Author")
        .unwrap();
    assert!(book < author, "{summaries:?}");
}
