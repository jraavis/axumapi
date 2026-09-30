//! `ModelOps`: save / delete / refresh on SQLite.
#![allow(clippy::unwrap_used)]

mod common;

use chrono::{DateTime, TimeDelta, Utc};
use common::{Author, Book, Note, Tag, Team, db};
use siderite_orm::{BackendError, Model, ModelOps, OrmError, QueryError};

#[tokio::test]
async fn save_inserts_unsaved_and_updates_saved() {
    let db = db().await;
    let mut ann = Author::new("Ann", Some(30));
    ann.save(&db).await.unwrap();
    assert!(ann.id > 0, "generated key is stored");

    ann.name = "Anna".into();
    ann.age = None;
    ann.save(&db).await.unwrap();
    let stored = Author::objects(&db)
        .get(Author::id.eq(ann.id))
        .await
        .unwrap();
    assert_eq!(stored, ann);
    assert_eq!(Author::objects(&db).all().await.unwrap().len(), 1);
}

#[tokio::test]
async fn manual_keys_update_then_fall_back_to_insert() {
    let db = db().await;
    let mut tag = Tag::new("rust", "Rust");
    tag.save(&db).await.unwrap();
    tag.label = "Rust lang".into();
    tag.save(&db).await.unwrap();
    let all = Tag::objects(&db).all().await.unwrap();
    assert_eq!(all, [Tag::new("rust", "Rust lang")]);
}

#[tokio::test]
async fn assigned_auto_key_inserts_when_row_is_missing() {
    let db = db().await;
    let mut team = Team {
        id: 42,
        name: "Core".into(),
    };
    team.save(&db).await.unwrap();
    assert_eq!(
        Team::objects(&db).get(Team::id.eq(42_i64)).await.unwrap(),
        team
    );
}

#[tokio::test]
async fn timestamps_are_stamped_and_read_back() {
    let db = db().await;
    let before = Utc::now() - TimeDelta::seconds(1);
    let mut note = Note::new("hello");
    note.save(&db).await.unwrap();
    assert!(note.created_at > before && note.updated_at > before);
    let created = note.created_at;

    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    note.body = "edited".into();
    note.created_at = DateTime::UNIX_EPOCH;
    note.save(&db).await.unwrap();
    assert_eq!(note.created_at, created, "auto_now_add is never rewritten");
    assert!(
        note.updated_at > created,
        "auto_now refreshes on every save"
    );
}

#[tokio::test]
async fn delete_and_refresh() {
    let db = db().await;
    let mut ann = Author::new("Ann", None);
    ann.save(&db).await.unwrap();
    let mut copy = ann.clone();
    copy.name = "stale".into();
    copy.refresh(&db).await.unwrap();
    assert_eq!(copy.name, "Ann");

    assert_eq!(ann.delete(&db).await.unwrap(), 1);
    assert_eq!(ann.delete(&db).await.unwrap(), 0);
    assert!(matches!(
        copy.refresh(&db).await,
        Err(OrmError::Query(QueryError::DoesNotExist))
    ));
}

#[tokio::test]
async fn constraint_violations_are_typed_and_deletes_cascade() {
    let db = db().await;
    let mut orphan = Book::new("Orphan", 99);
    assert!(matches!(
        orphan.save(&db).await,
        Err(OrmError::Backend(BackendError::Constraint(_)))
    ));

    let mut ann = Author::new("Ann", None);
    ann.save(&db).await.unwrap();
    let mut book = Book::new("Rust", ann.id);
    book.save(&db).await.unwrap();
    assert_eq!(book.price, rust_decimal::Decimal::new(1000, 2));
    ann.delete(&db).await.unwrap();
    assert!(Book::objects(&db).all().await.unwrap().is_empty());
}

#[tokio::test]
async fn save_is_atomic_inside_a_transaction() {
    let db = db().await;
    let result: Result<(), OrmError> = db
        .transaction(|tx| async move {
            Author::new("temp", None).save(&tx).await?;
            Err(QueryError::InvalidPlan("abort".into()).into())
        })
        .await;
    assert!(result.is_err());
    assert!(Author::objects(&db).all().await.unwrap().is_empty());
}
