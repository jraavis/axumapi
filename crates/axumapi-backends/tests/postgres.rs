//! PostgreSQL end-to-end tests.
//!
//! They run only when `DATABASE_URL` starts with `postgres` (for example
//! `postgres://postgres:postgres@localhost/postgres`) and print a note
//! otherwise. Each test works in its own freshly created schema, so tests can
//! run in parallel against one database.
#![cfg(feature = "postgres")]
#![allow(clippy::unwrap_used)]

mod common;

use axumapi_backends::postgres::PgBackend;
use axumapi_orm::functions::case;
use axumapi_orm::{
    ArrayAgg, BackendError, Count, Db, Expr, IsolationLevel, Model, ModelOps, OrmError, Relation,
    RowNumber, StdDev, Sum, Value, params,
};
use common::{Author, Book, Event, Tag, seed, titles};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

const SCHEMA: &str = "
    CREATE TABLE teams (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE authors (
        id BIGSERIAL PRIMARY KEY, name VARCHAR(100) NOT NULL, age INTEGER,
        team_id BIGINT REFERENCES teams(id) ON DELETE SET NULL);
    CREATE TABLE tags (slug TEXT PRIMARY KEY, label TEXT NOT NULL);
    CREATE TABLE books (
        id BIGSERIAL PRIMARY KEY, title TEXT NOT NULL,
        author_id BIGINT NOT NULL REFERENCES authors(id) ON DELETE CASCADE,
        pages INTEGER, price NUMERIC(8,2) NOT NULL, likes BIGINT NOT NULL,
        dislikes BIGINT NOT NULL, published DATE NOT NULL);
    CREATE TABLE book_tags (
        book_id BIGINT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
        tag_slug TEXT NOT NULL REFERENCES tags(slug) ON DELETE CASCADE,
        PRIMARY KEY (book_id, tag_slug));
    CREATE TABLE notes (
        id BIGSERIAL PRIMARY KEY, body TEXT NOT NULL,
        created_at TIMESTAMPTZ NOT NULL, updated_at TIMESTAMPTZ NOT NULL);
    CREATE TABLE events (
        id UUID PRIMARY KEY, at TIMESTAMPTZ NOT NULL, day DATE NOT NULL, clock TIME NOT NULL,
        payload JSONB NOT NULL, blob BYTEA NOT NULL, score DOUBLE PRECISION);
    CREATE TABLE nullables (
        id BIGSERIAL PRIMARY KEY, t TEXT, u UUID, ts TIMESTAMPTZ, j JSONB, n NUMERIC, d DATE);
";

/// A database handle on a private schema, dropped by [`cleanup`](Self::cleanup).
struct TestDb {
    db: Db,
    admin: PgPool,
    schema: String,
}

impl TestDb {
    async fn open() -> Option<Self> {
        let Some(url) = std::env::var("DATABASE_URL")
            .ok()
            .filter(|u| u.starts_with("postgres"))
        else {
            eprintln!("skipping PostgreSQL test: DATABASE_URL does not start with `postgres`");
            return None;
        };
        let schema = format!("axumapi_test_{}", uuid::Uuid::new_v4().simple());
        let admin = PgPool::connect(&url).await.unwrap();
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let options = PgConnectOptions::from_str(&url)
            .unwrap()
            .options([("search_path", schema.as_str())]);
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::Executor::execute(&mut *conn, "SET TIME ZONE 'UTC'").await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .unwrap();
        let db = Db::new(PgBackend::from_pool(pool));
        db.execute_script(SCHEMA).await.unwrap();
        Some(Self { db, admin, schema })
    }

    async fn cleanup(self) {
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn instance_operations_and_timestamps() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut note = common::Note::new("hello");
    note.save(db).await.unwrap();
    assert!(note.id > 0 && note.created_at > chrono::DateTime::UNIX_EPOCH);
    note.body = "edited".into();
    note.save(db).await.unwrap();
    let mut copy = common::Note::new("stale");
    copy.id = note.id;
    copy.refresh(db).await.unwrap();
    assert_eq!(copy.body, "edited");
    assert_eq!(note.delete(db).await.unwrap(), 1);
    t.cleanup().await;
}

#[tokio::test]
async fn null_parameters_fit_any_column_type() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let inserted =
        t.db.raw_execute(
            "INSERT INTO nullables (t, u, ts, j, n, d) VALUES ($1, $2, $3, $4, $5, $6)",
            params![
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null
            ],
        )
        .await
        .unwrap();
    assert_eq!(inserted, 1);
    let rows =
        t.db.raw_sql("SELECT t, u, ts, j, n, d FROM nullables", vec![])
            .await
            .unwrap();
    assert!(rows.rows[0].iter().all(|(_, v)| v.is_null()));
    t.cleanup().await;
}

#[tokio::test]
async fn scalar_types_round_trip_natively() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut event = Event {
        id: uuid::Uuid::from_u128(0xfeed),
        at: chrono::DateTime::from_timestamp_micros(1_715_953_530_123_456).unwrap(),
        day: chrono::NaiveDate::from_ymd_opt(2024, 5, 17).unwrap(),
        clock: chrono::NaiveTime::from_hms_micro_opt(13, 45, 30, 123_456).unwrap(),
        payload: serde_json::json!({"k": [1, 2]}),
        blob: vec![0, 1, 255],
        score: None,
    };
    event.save(db).await.unwrap();
    assert_eq!(
        Event::objects(db)
            .get(Event::id.eq(event.id))
            .await
            .unwrap(),
        event
    );
    let rows = Event::objects(db)
        .project([
            ("year", Event::at.year()),
            ("week", Event::at.week()),
            ("hour", Event::at.hour()),
        ])
        .rows()
        .await
        .unwrap();
    assert_eq!(rows[0].get_as::<i64>("year").unwrap(), 2024);
    assert_eq!(rows[0].get_as::<i64>("week").unwrap(), 20);
    t.cleanup().await;
}

#[tokio::test]
async fn queries_aggregates_windows_and_sets() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let s = seed(db).await;
    let liked = Book::objects(db)
        .filter(Book::likes.ge(5_i64))
        .order_by([Book::id.asc()]);
    assert_eq!(
        titles(&liked.clone().all().await.unwrap()),
        ["Rust", "Async", "SQL"]
    );
    assert_eq!(Book::objects(db).limit(2).count().await.unwrap(), 2);
    assert!(
        Book::objects(db)
            .filter(Book::likes.gt(9_i64))
            .exists()
            .await
            .unwrap()
    );
    assert_eq!(
        Book::objects(db).paginate(2, 2).await.unwrap().items.len(),
        2
    );

    let row = Book::objects(db)
        .aggregate([("likes", Sum::of(Book::likes)), ("n", Sum::of(Book::price))])
        .await
        .unwrap();
    assert_eq!(
        row.get_as::<i64>("likes").unwrap(),
        27,
        "numeric sums decode as integers"
    );
    let sd = Book::objects(db)
        .aggregate([("sd", StdDev::population(Book::likes))])
        .await
        .unwrap();
    assert!(sd.get_as::<f64>("sd").unwrap() > 0.0);

    let grouped = Book::objects(db)
        .project([Book::author.select()])
        .annotate("books", Count::all())
        .filter(Expr::col("books").gt(1_i64))
        .order_by([Book::author.asc()])
        .values_list::<(i64, i64), _>(["author_id", "books"])
        .await
        .unwrap();
    assert_eq!(grouped, [(s.ann.id, 2), (s.dee.id, 2)]);

    let ranked = Book::objects(db)
        .annotate(
            "rn",
            RowNumber::new()
                .partition_by([Book::author])
                .order_by([Book::likes.desc()]),
        )
        .annotate(
            "heat",
            case().when(Book::likes.ge(8_i64), "hot").otherwise("cold"),
        )
        .order_by([Book::id.asc()])
        .all_annotated()
        .await
        .unwrap();
    assert_eq!(ranked[1].1.get_as::<i64>("rn").unwrap(), 2);
    assert_eq!(ranked[2].1.get_as::<String>("heat").unwrap(), "hot");

    let union = liked
        .clone()
        .union(Book::objects(db).filter(Book::title.eq("Go")))
        .unwrap();
    assert_eq!(union.count().await.unwrap(), 4);
    let per_author = Book::objects(db)
        .order_by([Book::author.asc(), Book::likes.desc()])
        .distinct_on([Book::author])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&per_author), ["Rust", "SQL", "Zig"]);
    assert_eq!(
        Book::objects(db)
            .filter(Book::title.regex("^R"))
            .count()
            .await
            .unwrap(),
        1
    );
    let agg = Book::objects(db)
        .aggregate([("titles", ArrayAgg::of(Book::title))])
        .await
        .unwrap();
    assert!(
        matches!(agg.get("titles"), Some(Value::Json(serde_json::Value::Array(a))) if a.len() == 5)
    );
    t.cleanup().await;
}

#[tokio::test]
async fn writes_bulk_operations_and_relations() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let s = seed(db).await;
    let many: Vec<Book> = (0..10_000)
        .map(|i| Book::new(&format!("b{i}"), s.ann.id))
        .collect();
    let stored = Book::objects(db).bulk_create(many).await.unwrap();
    assert_eq!(
        stored[9_999].title, "b9999",
        "65535 parameters allow two statements"
    );
    let changed = Book::objects(db)
        .bulk_update(&stored[..3], &["likes"])
        .await
        .unwrap();
    assert_eq!(changed, 3);
    Book::objects(db)
        .filter(Book::title.starts_with("b"))
        .delete()
        .await
        .unwrap();

    let books = Book::objects(db)
        .select_related(Relation::new(Book::author, |b| &mut b.author))
        .filter(Book::author.join(Author::name).eq("Ann"))
        .all()
        .await
        .unwrap();
    assert!(books.iter().all(|b| b.author.cached().is_some()));

    Tag::new("rust", "Rust").save(db).await.unwrap();
    let book = s.book("Rust");
    book.tags(db).add_pks(["rust".to_owned()]).await.unwrap();
    assert_eq!(book.tags(db).count().await.unwrap(), 1);
    let (_, created) = Tag::objects(db)
        .get_or_create(Tag::slug.eq("db"), || Tag::new("db", "DB"))
        .await
        .unwrap();
    assert!(created);
    let orphan = Book::new("Orphan", 999_999).save(db).await;
    assert!(matches!(
        orphan,
        Err(OrmError::Backend(BackendError::Constraint(_)))
    ));
    t.cleanup().await;
}

#[tokio::test]
async fn transactions_isolation_and_row_locks() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let s = seed(db).await;
    for (level, name) in [
        (IsolationLevel::ReadCommitted, "read committed"),
        (IsolationLevel::RepeatableRead, "repeatable read"),
        (IsolationLevel::Serializable, "serializable"),
    ] {
        let seen = db
            .transaction_with(level, |tx| async move {
                let rows = tx.raw_sql("SHOW transaction_isolation", vec![]).await?;
                Ok::<_, OrmError>(
                    rows.rows[0]
                        .get_as::<String>("transaction_isolation")
                        .unwrap(),
                )
            })
            .await
            .unwrap();
        assert_eq!(seen, name);
    }

    let locked = db
        .transaction(|tx| async move {
            let rows = Author::objects(&tx)
                .filter(Author::id.eq(s.ann.id))
                .select_for_update()
                .all()
                .await?;
            // Another connection cannot lock the same row without waiting.
            let contender = Author::objects(db)
                .filter(Author::id.eq(s.ann.id))
                .select_for_update()
                .nowait()
                .all()
                .await;
            let skipped = Author::objects(db)
                .filter(Author::id.eq(s.ann.id))
                .select_for_update()
                .skip_locked()
                .all()
                .await?;
            Ok::<_, OrmError>((rows.len(), contender.is_err(), skipped.len()))
        })
        .await
        .unwrap();
    assert_eq!(locked, (1, true, 0));

    let failed: Result<(), OrmError> = db
        .transaction(|tx| async move {
            Author::new("temp", None).save(&tx).await?;
            Err(axumapi_orm::QueryError::InvalidPlan("abort".into()).into())
        })
        .await;
    assert!(failed.is_err());
    assert_eq!(
        Author::objects(db)
            .filter(Author::name.eq("temp"))
            .count()
            .await
            .unwrap(),
        0
    );
    t.cleanup().await;
}
