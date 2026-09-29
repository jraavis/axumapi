//! `TestDatabase`: schema creation from model metadata and rollback isolation.
#![allow(clippy::unwrap_used, dead_code)]

use axumapi::orm::{ModelOps, OrmError};
use axumapi::prelude::*;
use axumapi_testkit::TestDatabase;

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "authors", ordering = ["id"])]
struct Author {
    #[field(primary_key, auto)]
    id: i64,
    #[field(unique)]
    name: String,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "posts", ordering = ["id"], many_to_many(tags(Tag, through_table = "post_tags")))]
struct Post {
    #[field(primary_key, auto)]
    id: i64,
    title: String,
    author: ForeignKey<Author>,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "tags")]
struct Tag {
    #[field(primary_key, auto)]
    id: i64,
    label: String,
}

async fn database() -> TestDatabase {
    TestDatabase::sqlite_memory()
        .await
        .unwrap()
        .with_models(&[Author::META, Post::META, Tag::META])
        .await
        .unwrap()
}

fn author(name: &str) -> Author {
    Author {
        id: 0,
        name: name.into(),
    }
}

#[tokio::test]
async fn with_models_creates_tables_constraints_and_join_tables() {
    let test = database().await;
    let db = test.db();

    let mut ann = author("Ann");
    ann.save(db).await.unwrap();
    let mut post = Post {
        id: 0,
        title: "Hello".into(),
        author: ForeignKey::new(ann.id),
    };
    post.save(db).await.unwrap();
    assert_eq!(Post::objects(db).count().await.unwrap(), 1);

    // UNIQUE from the model is enforced by the generated DDL.
    assert!(author("Ann").save(db).await.is_err());
    // The auto many-to-many join table exists.
    let rows = db.raw_sql("SELECT * FROM post_tags", vec![]).await.unwrap();
    assert!(rows.rows.is_empty());
}

#[tokio::test]
async fn isolated_discards_writes_but_keeps_the_schema() {
    let test = database().await;
    let seen = test
        .isolated(|db| async move {
            author("Temp").save(&db).await.unwrap();
            Author::objects(&db).count().await.unwrap()
        })
        .await
        .unwrap();
    assert_eq!(seen, 1);
    assert_eq!(Author::objects(test.db()).count().await.unwrap(), 0);

    // Data committed outside `isolated` survives later isolated runs.
    author("Kept").save(test.db()).await.unwrap();
    test.isolated(|db| async move {
        author("Temp").save(&db).await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(Author::objects(test.db()).count().await.unwrap(), 1);
}

#[tokio::test]
async fn isolated_supports_nested_savepoints_and_returns_errors_as_values() {
    let test = database().await;
    test.isolated(|db| async move {
        let result: Result<(), OrmError> = db
            .transaction(|inner| async move {
                author("Inner").save(&inner).await?;
                Err(OrmError::from(axumapi::orm::QueryError::InvalidPlan(
                    "abort".into(),
                )))
            })
            .await;
        assert!(result.is_err());
        assert_eq!(Author::objects(&db).count().await.unwrap(), 0);
    })
    .await
    .unwrap();
}
