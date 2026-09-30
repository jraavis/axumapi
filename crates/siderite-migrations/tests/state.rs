//! [`ProjectState`] / [`ModelState`] built from hand-written `ModelMeta`.
#![allow(clippy::unwrap_used)]

mod common;

use common::{author_meta, book_meta, post_meta, tag_meta, unmanaged_meta};
use siderite_migrations::{DbDefault, OnDelete, ProjectState, SqlType};

#[test]
fn author_and_book_from_reference_meta() {
    let state = ProjectState::from_metas(&[author_meta(), book_meta()]);
    assert_eq!(state.models.len(), 2);

    let author = state.model("Author").unwrap();
    assert_eq!(author.table, "authors");
    let id = author.field("id").unwrap();
    assert!(id.primary_key && id.auto);
    assert_eq!(id.sql_type, SqlType::BigInt);
    let name = author.field("name").unwrap();
    assert_eq!(name.max_length, Some(100));
    assert!(!name.nullable);

    let book = state.model("Book").unwrap();
    let author_fk = book.field("author").unwrap();
    assert_eq!(author_fk.column, "author_id");
    assert!(author_fk.index);
    let fk = author_fk.fk.as_ref().unwrap();
    assert_eq!(fk.target_table, "authors");
    assert_eq!(fk.target_column, "id");
    assert_eq!(fk.on_delete, OnDelete::Cascade);
    assert!(book.field("pages").unwrap().nullable);
}

#[test]
fn skips_unmanaged_and_synthesizes_m2m_join_table() {
    let state = ProjectState::from_metas(&[post_meta(), tag_meta(), unmanaged_meta()]);
    assert!(state.model("LegacyView").is_none());
    assert!(state.model("Post").is_some());
    assert!(state.model("Tag").is_some());

    let join = state.model("post_tags").unwrap();
    assert_eq!(join.table, "post_tags");
    assert_eq!(join.fields.len(), 2);
    let post_id = join.field("post_id").unwrap();
    assert_eq!(post_id.fk.as_ref().unwrap().target_table, "posts");
    assert_eq!(post_id.fk.as_ref().unwrap().on_delete, OnDelete::Cascade);
    let tag_id = join.field("tag_id").unwrap();
    assert_eq!(tag_id.fk.as_ref().unwrap().target_table, "tags");
    match &join.constraints[0] {
        siderite_migrations::ConstraintState::Unique { columns, .. } => {
            assert_eq!(columns, &["post_id".to_owned(), "tag_id".to_owned()]);
        }
        other => panic!("expected unique pair, got {other:?}"),
    }

    let created = state.model("Post").unwrap().field("created_at").unwrap();
    assert!(matches!(created.default, Some(DbDefault::Now)));
    assert_eq!(created.sql_type, SqlType::Timestamp);
}

#[test]
fn serde_round_trip_is_stable() {
    let state = ProjectState::from_metas(&[author_meta(), book_meta()]);
    let json = serde_json::to_string(&state).unwrap();
    let back: ProjectState = serde_json::from_str(&json).unwrap();
    assert_eq!(state, back);
}
