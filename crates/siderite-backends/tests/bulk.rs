//! `bulk_create` / `bulk_update` on SQLite.
#![cfg(feature = "sqlite")]
#![allow(clippy::unwrap_used)]

mod common;

use common::{Author, Book, Note, Tag, db, seed};
use siderite_orm::{BackendError, Model, ModelOps, OrmError};

#[tokio::test]
async fn bulk_create_returns_rows_in_input_order_with_keys() {
    let db = db().await;
    let authors: Vec<Author> = (0..5)
        .map(|i| Author::new(&format!("a{i}"), Some(i)))
        .collect();
    let stored = Author::objects(&db).bulk_create(authors).await.unwrap();
    assert_eq!(
        stored.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        ["a0", "a1", "a2", "a3", "a4"]
    );
    let ids: Vec<i64> = stored.iter().map(|a| a.id).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]) && ids[0] > 0);
    assert_eq!(Author::objects(&db).count().await.unwrap(), 5);
}

#[tokio::test]
async fn bulk_create_chunks_to_the_parameter_limit() {
    let db = db().await;
    let mut ann = Author::new("Ann", None);
    ann.save(&db).await.unwrap();
    // 7 columns per row and 32766 parameters per statement: 4680 rows per insert.
    let books: Vec<Book> = (0..10_000)
        .map(|i| Book::new(&format!("b{i}"), ann.id))
        .collect();
    let stored = Book::objects(&db).bulk_create(books).await.unwrap();
    assert_eq!(stored.len(), 10_000);
    assert_eq!(stored[0].title, "b0");
    assert_eq!(stored[9_999].title, "b9999");
    assert_eq!(Book::objects(&db).count().await.unwrap(), 10_000);
}

#[tokio::test]
async fn bulk_create_handles_manual_keys_and_stamps_timestamps() {
    let db = db().await;
    let tags = vec![Tag::new("b", "B"), Tag::new("a", "A"), Tag::new("c", "C")];
    let stored = Tag::objects(&db).bulk_create(tags.clone()).await.unwrap();
    assert_eq!(stored, tags, "manual keys keep the input order");

    let notes = Note::objects(&db)
        .bulk_create(vec![Note::new("x"), Note::new("y")])
        .await
        .unwrap();
    assert!(
        notes
            .iter()
            .all(|n| n.created_at > chrono::DateTime::UNIX_EPOCH)
    );
}

#[tokio::test]
async fn bulk_create_mixes_saved_and_unsaved_objects() {
    let db = db().await;
    let explicit = Author {
        id: 50,
        ..Author::new("explicit", None)
    };
    let stored = Author::objects(&db)
        .bulk_create(vec![
            Author::new("x", None),
            explicit,
            Author::new("y", None),
        ])
        .await
        .unwrap();
    assert_eq!(stored.len(), 3);
    assert_eq!(stored[1].id, 50);
    assert_eq!(stored[0].name, "x");
    assert_eq!(stored[2].name, "y");
}

#[tokio::test]
async fn bulk_create_is_all_or_nothing() {
    let db = db().await;
    Tag::new("dup", "old").save(&db).await.unwrap();
    let outcome = Tag::objects(&db)
        .bulk_create(vec![Tag::new("ok", "ok"), Tag::new("dup", "new")])
        .await;
    assert!(matches!(
        outcome,
        Err(OrmError::Backend(BackendError::Constraint(_)))
    ));
    assert_eq!(Tag::objects(&db).count().await.unwrap(), 1);
    assert!(
        Tag::objects(&db)
            .bulk_create(Vec::new())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn bulk_update_writes_selected_columns() {
    let db = db().await;
    let s = seed(&db).await;
    let mut books = s.books.clone();
    for (i, book) in books.iter_mut().enumerate() {
        book.likes = 100 + i as i64;
        book.title = format!("t{i}");
        book.pages = None;
    }
    let changed = Book::objects(&db)
        .bulk_update(&books, &["likes", "title"])
        .await
        .unwrap();
    assert_eq!(changed, 5);
    let stored = Book::objects(&db)
        .order_by([Book::id.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(
        stored.iter().map(|b| b.likes).collect::<Vec<_>>(),
        [100, 101, 102, 103, 104]
    );
    assert_eq!(stored[2].title, "t2");
    assert_eq!(
        stored[0].pages, s.books[0].pages,
        "unlisted columns are untouched"
    );
}

#[tokio::test]
async fn bulk_update_validates_columns_and_skips_missing_rows() {
    let db = db().await;
    let s = seed(&db).await;
    assert!(
        Book::objects(&db)
            .bulk_update(&s.books, &["id"])
            .await
            .is_err()
    );
    assert!(
        Book::objects(&db)
            .bulk_update(&s.books, &["nope"])
            .await
            .is_err()
    );
    assert_eq!(
        Book::objects(&db)
            .bulk_update(&[], &["likes"])
            .await
            .unwrap(),
        0
    );
    let ghost = Book {
        id: 999,
        likes: 1,
        ..s.books[0].clone()
    };
    let changed = Book::objects(&db)
        .bulk_update(&[ghost, s.books[1].clone()], &["likes"])
        .await
        .unwrap();
    assert_eq!(changed, 1);
}

#[tokio::test]
async fn bulk_create_keeps_input_order_for_explicit_keys() {
    let db = db().await;
    let authors = [50, 10, 30].map(|id| Author {
        id,
        ..Author::new(&format!("a{id}"), None)
    });
    let stored = Author::objects(&db)
        .bulk_create(authors.to_vec())
        .await
        .unwrap();
    assert_eq!(
        stored.iter().map(|a| a.id).collect::<Vec<_>>(),
        [50, 10, 30]
    );
}
