//! Raw SQL with typed decoding on SQLite.
#![allow(clippy::unwrap_used)]

mod common;

use axumapi_orm::{Model, Value, params};
use common::{Author, Book, db, seed};

#[tokio::test]
async fn raw_rows_decode_into_models() {
    let db = db().await;
    seed(&db).await;
    let result = db
        .raw_sql(
            "SELECT * FROM authors WHERE age > ? ORDER BY name",
            params![26],
        )
        .await
        .unwrap();
    let authors: Vec<Author> = result.decode().unwrap();
    assert_eq!(
        authors.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        ["Ann", "Dee"]
    );
    assert_eq!(authors[0].team.as_ref().map(|t| *t.id()), Some(1));
}

#[tokio::test]
async fn raw_rows_decode_into_tuples_and_scalars() {
    let db = db().await;
    seed(&db).await;
    let result = db
        .raw_sql(
            "SELECT title, likes FROM books WHERE likes >= ? ORDER BY likes DESC",
            params![8],
        )
        .await
        .unwrap();
    let pairs: Vec<(String, i64)> = result.decode_values().unwrap();
    assert_eq!(pairs, [("Rust".to_owned(), 10), ("SQL".to_owned(), 8)]);
    let total = db
        .raw_sql("SELECT SUM(likes) FROM books", vec![])
        .await
        .unwrap();
    assert_eq!(total.scalar(), Some(&Value::Int(27)));
    assert_eq!(total.decode_values::<i64>().unwrap(), [27]);
    let missing = result.decode::<Book>();
    assert!(
        missing.is_err(),
        "columns absent from the result are decode errors"
    );
}

#[tokio::test]
async fn raw_execute_binds_parameters() {
    let db = db().await;
    seed(&db).await;
    let changed = db
        .raw_execute(
            "UPDATE books SET likes = likes + ? WHERE title = ?",
            params![5, "Go"],
        )
        .await
        .unwrap();
    assert_eq!(changed, 1);
    let go = Book::objects(&db).get(Book::title.eq("Go")).await.unwrap();
    assert_eq!(go.likes, 6);
}
