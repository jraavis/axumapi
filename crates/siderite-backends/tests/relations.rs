//! Related traversal, `select_related`, `prefetch_related` and
//! many-to-many managers on SQLite.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Author, Book, Tag, Team, db, seed, titles};
use siderite_orm::{BackendError, ModelOps, OrmError, Prefetch, QueryError};
use siderite_orm::{ForeignKey, Model};

#[tokio::test]
async fn joined_fields_filter_and_order_through_foreign_keys() {
    let db = db().await;
    seed(&db).await;
    let anns = Book::objects(&db)
        .filter(Book::author.join(Author::name).eq("Ann"))
        .order_by([Book::id.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&anns), ["Rust", "Async"]);

    let by_author = Book::objects(&db)
        .order_by([Book::author.join(Author::name).desc(), Book::id.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&by_author), ["Go", "Zig", "SQL", "Rust", "Async"]);

    let old = Book::objects(&db)
        .filter(Book::author.join(Author::age).gt(28))
        .count()
        .await
        .unwrap();
    assert_eq!(old, 4);
}

#[tokio::test]
async fn joins_chain_across_several_hops_and_are_shared() {
    let db = db().await;
    let s = seed(&db).await;
    let team_name = Book::author.join(Author::team.join(Team::name));
    let qs = Book::objects(&db)
        .filter(team_name.clone().eq("Red"))
        .filter(Book::author.join(Author::name).ne("Dee"))
        .order_by([team_name.asc(), Book::id.asc()]);
    let joins: Vec<_> = qs
        .plan()
        .joins
        .iter()
        .map(|j| j.source.alias.clone().unwrap().to_string())
        .collect();
    assert_eq!(joins, ["author", "author__team"]);
    assert_eq!(titles(&qs.all().await.unwrap()), ["Rust", "Async"]);

    Book::new("Teamless", s.cy.id).save(&db).await.unwrap();
    let unteamed = Book::objects(&db)
        .filter(Book::author.join(Author::team.join(Team::name)).is_null())
        .count()
        .await
        .unwrap();
    assert_eq!(
        unteamed, 1,
        "LEFT JOINs keep books whose author has no team"
    );
}

#[tokio::test]
async fn first_and_paginate_work_with_joins() {
    let db = db().await;
    seed(&db).await;
    let qs = Book::objects(&db).filter(Book::author.join(Author::name).istarts_with("d"));
    assert_eq!(qs.clone().first().await.unwrap().unwrap().title, "Go");
    assert_eq!(qs.clone().last().await.unwrap().unwrap().title, "Zig");
    let page = qs.paginate(1, 1).await.unwrap();
    assert_eq!((page.total, page.items.len()), (2, 1));
}

#[tokio::test]
async fn select_related_fills_the_cache_in_one_query() {
    let db = db().await;
    let s = seed(&db).await;
    let books = Book::objects(&db)
        .select_related(Book::author_relation())
        .filter(Book::author.join(Author::name).ne("Bob"))
        .order_by([Book::id.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(books.len(), 4);
    let author = books[0].author.cached().expect("author is loaded");
    assert_eq!(author.name, "Ann");
    assert_eq!(author.id, s.ann.id);
    assert!(books.iter().all(|b| b.author.cached().is_some()));
    // Without select_related nothing is cached.
    let plain = Book::objects(&db).first().await.unwrap().unwrap();
    assert!(plain.author.cached().is_none());
    assert_eq!(plain.author.get(&db).await.unwrap().name, "Ann");
}

#[tokio::test]
async fn select_related_chains_through_nullable_relations() {
    let db = db().await;
    let s = seed(&db).await;
    Book::new("Orphan", s.cy.id).save(&db).await.unwrap();
    let books = Book::objects(&db)
        .select_related(Book::author_relation().then(Author::team_relation()))
        .select_related(Book::author_relation())
        .order_by([Book::id.asc()])
        .all()
        .await
        .unwrap();
    let plan_joins = Book::objects(&db)
        .select_related(Book::author_relation().then(Author::team_relation()))
        .select_related(Book::author_relation())
        .plan()
        .joins
        .len();
    assert_eq!(plan_joins, 2, "the author join is shared");

    let first = &books[0];
    let author = first.author.cached().unwrap();
    assert_eq!(author.team.as_ref().unwrap().cached().unwrap().name, "Red");
    let orphan = books.last().unwrap();
    let cy = orphan.author.cached().unwrap();
    assert_eq!(cy.name, "Cy");
    assert!(cy.team.is_none(), "no team, so nothing to attach");
}

#[tokio::test]
async fn prefetch_related_loads_targets_with_one_extra_query() {
    let db = db().await;
    let s = seed(&db).await;
    let books = Book::objects(&db)
        .prefetch_related(Book::author_relation())
        .order_by([Book::id.asc()])
        .all()
        .await
        .unwrap();
    let names: Vec<_> = books
        .iter()
        .map(|b| b.author.cached().unwrap().name.as_str())
        .collect();
    assert_eq!(names, ["Ann", "Ann", "Bob", "Dee", "Dee"]);
    assert!(std::ptr::eq(
        books[0].author.cached().unwrap(),
        books[0].author.cached().unwrap()
    ));

    let nullable = Author::objects(&db)
        .prefetch_related(Author::team_relation())
        .all()
        .await
        .unwrap();
    let teams: Vec<_> = nullable
        .iter()
        .map(|a| {
            a.team
                .as_ref()
                .and_then(ForeignKey::cached)
                .map(|t| t.name.clone())
        })
        .collect();
    assert_eq!(
        teams,
        [
            Some("Red".into()),
            Some("Blue".into()),
            None,
            Some("Red".into())
        ]
    );

    let only_ann = Book::objects(&db)
        .prefetch_related(
            Prefetch::new(Book::author_relation())
                .queryset(Author::objects(&db).filter(Author::id.eq(s.ann.id))),
        )
        .order_by([Book::id.asc()])
        .all()
        .await
        .unwrap();
    let loaded: Vec<_> = only_ann
        .iter()
        .map(|b| b.author.cached().is_some())
        .collect();
    assert_eq!(loaded, [true, true, false, false, false]);
}

#[tokio::test]
async fn prefetch_rejects_multi_hop_paths() {
    let db = db().await;
    let outcome = Book::objects(&db)
        .prefetch_related(Book::author_relation().then(Author::team_relation()))
        .all()
        .await;
    assert!(matches!(
        outcome,
        Err(OrmError::Query(QueryError::InvalidPlan(_)))
    ));
}

#[tokio::test]
async fn many_to_many_add_remove_set_and_clear() {
    let db = db().await;
    let s = seed(&db).await;
    for slug in ["rust", "db", "web", "cli"] {
        Tag::new(slug, slug).save(&db).await.unwrap();
    }
    let book = s.book("Rust").clone();
    let tags = book.tags(&db);
    assert_eq!(tags.count().await.unwrap(), 0);

    let tag = |slug: &str| Tag::new(slug, slug);
    tags.add([&tag("rust"), &tag("db")]).await.unwrap();
    tags.add([&tag("db"), &tag("web")]).await.unwrap();
    let slugs = |all: Vec<Tag>| all.into_iter().map(|t| t.slug).collect::<Vec<_>>();
    assert_eq!(slugs(tags.all().await.unwrap()), ["db", "rust", "web"]);
    assert!(tags.contains(&tag("web")).await.unwrap());
    assert!(!tags.contains(&tag("cli")).await.unwrap());

    assert_eq!(tags.remove([&tag("db"), &tag("cli")]).await.unwrap(), 1);
    tags.set([&tag("cli"), &tag("rust")]).await.unwrap();
    assert_eq!(slugs(tags.all().await.unwrap()), ["cli", "rust"]);
    tags.set_pks(["web".to_owned()]).await.unwrap();
    assert_eq!(slugs(tags.all().await.unwrap()), ["web"]);

    let other = s.book("Go").tags(&db);
    other.add_pks(["web".to_owned()]).await.unwrap();
    let filtered = tags
        .queryset()
        .filter(Tag::slug.eq("web"))
        .count()
        .await
        .unwrap();
    assert_eq!(filtered, 1);
    assert_eq!(tags.clear().await.unwrap(), 1);
    assert_eq!(
        other.count().await.unwrap(),
        1,
        "other objects keep their links"
    );
}

#[tokio::test]
async fn many_to_many_rolls_back_on_constraint_errors() {
    let db = db().await;
    let s = seed(&db).await;
    Tag::new("real", "real").save(&db).await.unwrap();
    let tags = s.book("SQL").tags(&db);
    let outcome = tags.add_pks(["real".to_owned(), "ghost".to_owned()]).await;
    assert!(matches!(
        outcome,
        Err(OrmError::Backend(BackendError::Constraint(_)))
    ));
    assert_eq!(
        tags.count().await.unwrap(),
        0,
        "the valid link was rolled back too"
    );
}
