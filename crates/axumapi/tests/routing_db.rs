//! Database routing on SQLite: router read/write selection, `using(alias)`,
//! unknown aliases and the rule that querysets never span databases.
#![allow(clippy::unwrap_used, dead_code)]

use axumapi::orm::router::DatabaseRouter;
use axumapi::orm::{Databases, Db, ModelMeta, ModelOps, OrmError, QueryError};
use axumapi::prelude::*;
use axumapi_backends::sqlite::SqliteBackend;

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "books", ordering = ["title"])]
struct Book {
    #[field(primary_key, auto)]
    id: i64,
    title: String,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "events")]
struct Event {
    #[field(primary_key, auto)]
    id: i64,
    name: String,
}

const SCHEMA: &str =
    "CREATE TABLE books (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL);
     CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL);";

async fn sqlite() -> Db {
    let db = Db::new(SqliteBackend::connect("sqlite::memory:").await.unwrap());
    db.execute_script(SCHEMA).await.unwrap();
    db
}

/// Books are read from `replica` and written to `default`; events live on
/// `analytics`; everything else falls through to `default`.
struct AppRouter;

impl DatabaseRouter for AppRouter {
    fn db_for_read(&self, model: &ModelMeta) -> Option<&str> {
        match model.table {
            "books" => Some("replica"),
            "events" => Some("analytics"),
            _ => None,
        }
    }

    fn db_for_write(&self, model: &ModelMeta) -> Option<&str> {
        (model.table == "events").then_some("analytics")
    }

    fn allow_migrate(&self, alias: &str, model: &ModelMeta) -> bool {
        (model.table == "events") == (alias == "analytics")
    }
}

struct Fixture {
    databases: Databases,
    default: Db,
    replica: Db,
    analytics: Db,
}

async fn fixture(router: impl DatabaseRouter) -> Fixture {
    let (default, replica, analytics) = (sqlite().await, sqlite().await, sqlite().await);
    let databases = Databases::new()
        .with("default", default.clone())
        .with("replica", replica.clone())
        .with("analytics", analytics.clone())
        .with_router(router);
    Fixture {
        databases,
        default,
        replica,
        analytics,
    }
}

async fn add_book(db: &Db, title: &str) {
    Book {
        id: 0,
        title: title.into(),
    }
    .save(db)
    .await
    .unwrap();
}

async fn titles(queryset: QuerySet<Book>) -> Vec<String> {
    queryset
        .all()
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.title)
        .collect()
}

#[tokio::test]
async fn the_router_picks_read_and_write_databases_per_model() {
    let f = fixture(AppRouter).await;

    assert!(
        f.databases
            .for_read::<Book>()
            .unwrap()
            .same_database(&f.replica)
    );
    assert!(
        f.databases
            .for_write::<Book>()
            .unwrap()
            .same_database(&f.default)
    );
    assert!(
        f.databases
            .for_read::<Event>()
            .unwrap()
            .same_database(&f.analytics)
    );
    assert!(
        f.databases
            .for_write::<Event>()
            .unwrap()
            .same_database(&f.analytics)
    );
}

#[tokio::test]
async fn objects_reads_from_the_routed_database() {
    let f = fixture(AppRouter).await;
    add_book(f.databases.for_write::<Book>().unwrap(), "on default").await;
    add_book(&f.replica, "on replica").await;

    assert_eq!(
        titles(f.databases.objects::<Book>().unwrap()).await,
        ["on replica"]
    );
}

#[tokio::test]
async fn using_selects_an_alias_and_bypasses_the_router() {
    let f = fixture(AppRouter).await;
    add_book(&f.default, "d").await;
    add_book(&f.replica, "r").await;

    assert_eq!(
        titles(f.databases.using::<Book>("default").unwrap()).await,
        ["d"]
    );
    assert_eq!(
        titles(f.databases.using::<Book>("replica").unwrap()).await,
        ["r"]
    );
}

#[tokio::test]
async fn without_a_router_everything_uses_default() {
    let (default, other) = (sqlite().await, sqlite().await);
    let databases = Databases::new()
        .with("default", default.clone())
        .with("other", other);

    assert!(
        databases
            .for_read::<Book>()
            .unwrap()
            .same_database(&default)
    );
    assert!(
        databases
            .for_write::<Book>()
            .unwrap()
            .same_database(&default)
    );
    assert!(databases.allow_migrate("other", Book::META));
}

#[tokio::test]
async fn a_router_returning_none_defers_to_default() {
    struct Silent;
    impl DatabaseRouter for Silent {}
    let f = fixture(Silent).await;

    assert!(
        f.databases
            .for_read::<Book>()
            .unwrap()
            .same_database(&f.default)
    );
    assert!(
        f.databases
            .for_write::<Event>()
            .unwrap()
            .same_database(&f.default)
    );
}

#[tokio::test]
async fn unknown_aliases_are_a_configuration_error() {
    let f = fixture(AppRouter).await;

    let err = f.databases.using::<Book>("nowhere").unwrap_err();
    assert!(matches!(err, OrmError::UnknownDatabase(ref a) if a == "nowhere"));
    assert!(err.to_string().contains("nowhere"));

    // A router naming an unregistered alias fails the same way.
    let databases = Databases::new()
        .with("default", sqlite().await)
        .with_router(AppRouter);
    assert!(matches!(
        databases.for_read::<Book>(),
        Err(OrmError::UnknownDatabase(ref a)) if a == "replica"
    ));
    assert!(matches!(
        databases.objects::<Book>(),
        Err(OrmError::UnknownDatabase(_))
    ));

    // Nothing registered as "default".
    let empty = Databases::new();
    assert!(matches!(
        empty.for_write::<Book>(),
        Err(OrmError::UnknownDatabase(ref a)) if a == "default"
    ));
}

#[tokio::test]
async fn unknown_alias_maps_to_an_internal_server_error() {
    let err = Databases::new().using::<Book>("x").unwrap_err();
    assert_eq!(axumapi::ApiError::from(err).status().as_u16(), 500);
}

#[tokio::test]
async fn allow_migrate_consults_the_router() {
    let f = fixture(AppRouter).await;
    assert!(f.databases.allow_migrate("analytics", Event::META));
    assert!(!f.databases.allow_migrate("default", Event::META));
    assert!(f.databases.allow_migrate("default", Book::META));
    assert!(!f.databases.allow_migrate("analytics", Book::META));
}

#[tokio::test]
async fn aliases_lists_every_registered_database_sorted() {
    let f = fixture(AppRouter).await;
    assert_eq!(
        f.databases.aliases().collect::<Vec<_>>(),
        ["analytics", "default", "replica"]
    );
}

#[tokio::test]
async fn querysets_bound_to_different_databases_cannot_be_combined() {
    let f = fixture(AppRouter).await;
    add_book(&f.default, "d").await;
    add_book(&f.replica, "r").await;
    let on_default = f.databases.using::<Book>("default").unwrap();
    let on_replica = f.databases.using::<Book>("replica").unwrap();

    for combined in [
        on_default.clone().union(on_replica.clone()),
        on_default.clone().union_all(on_replica.clone()),
        on_default.clone().intersection(on_replica.clone()),
        on_default.clone().difference(on_replica.clone()),
    ] {
        assert!(matches!(
            combined,
            Err(QueryError::InvalidPlan(ref m)) if m.contains("different databases")
        ));
    }

    // Same database: fine.
    let same = on_default
        .clone()
        .filter(Book::title.eq("d"))
        .union(on_default.filter(Book::title.eq("x")))
        .unwrap();
    assert_eq!(titles(same).await, ["d"]);
}

#[tokio::test]
async fn subqueries_from_another_database_are_rejected() {
    let f = fixture(AppRouter).await;
    add_book(&f.default, "d").await;
    add_book(&f.replica, "r").await;
    let replica_ids = Book::objects(&f.replica).subquery("id");
    let foreign = Book::objects(&f.default).filter(Book::id.in_subquery(replica_ids));
    assert!(matches!(
        foreign.all().await,
        Err(OrmError::Query(QueryError::InvalidPlan(ref m))) if m.contains("another database")
    ));
    let foreign_exists = Book::objects(&f.default).filter(Book::objects(&f.replica).exists_expr());
    assert!(foreign_exists.count().await.is_err());
    let foreign_delete = Book::objects(&f.default).filter(Book::objects(&f.replica).exists_expr());
    assert!(foreign_delete.delete().await.is_err());

    // Same database, including through a transaction handle: fine.
    let local = Book::objects(&f.default)
        .filter(Book::id.in_subquery(Book::objects(&f.default).subquery("id")));
    assert_eq!(titles(local).await, ["d"]);
    f.default
        .transaction(|tx| {
            let outer = f.default.clone();
            async move {
                let ids = Book::objects(&outer).subquery("id");
                let inside = Book::objects(&tx).filter(Book::id.in_subquery(ids));
                assert_eq!(inside.count().await?, 1);
                Ok::<_, OrmError>(())
            }
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_transaction_handle_still_counts_as_its_database() {
    let f = fixture(AppRouter).await;
    let on_default = f.databases.using::<Book>("default").unwrap();
    f.default
        .transaction(|tx| async move {
            let inside = Book::objects(&tx);
            assert!(inside.union(on_default).is_ok());
            Ok::<_, OrmError>(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn writes_go_where_the_router_says() {
    let f = fixture(AppRouter).await;
    let write_db = f.databases.for_write::<Book>().unwrap();
    add_book(write_db, "routed").await;

    assert_eq!(Book::objects(&f.default).count().await.unwrap(), 1);
    assert_eq!(Book::objects(&f.replica).count().await.unwrap(), 0);
}
