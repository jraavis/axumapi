//! MongoDB end-to-end tests.
//!
//! They run only when `MONGODB_URL` starts with `mongodb` (a replica set is
//! needed for the transaction tests, for example
//! `mongodb://127.0.0.1:27018/?replicaSet=rs0&directConnection=true`) and
//! print a note otherwise. Every test works in its own database, dropped at
//! the end, so tests run in parallel against one server.
#![cfg(feature = "mongodb")]
#![allow(clippy::unwrap_used)]

mod common;

use axumapi_backends::mongodb::{Keys, MongoBackend};
use axumapi_orm::functions::{coalesce, concat, length, lower, upper};
use axumapi_orm::{
    Avg, BackendCapabilityError, BackendError, Count, Db, Expr, Feature, IsolationLevel, Max, Min,
    Model, ModelOps, OrmError, RowNumber, StdDev, StringAgg, Sum, Value, Variance,
};
use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use common::{Author, Book, Event, Note, Seed, Tag, seed, titles};
use rust_decimal::Decimal;
use uuid::Uuid;

struct TestDb {
    db: Db,
    backend: MongoBackend,
}

impl TestDb {
    async fn open() -> Option<Self> {
        let Some(url) = std::env::var("MONGODB_URL")
            .ok()
            .filter(|u| u.starts_with("mongodb"))
        else {
            eprintln!("skipping MongoDB test: MONGODB_URL does not start with `mongodb`");
            return None;
        };
        let name = format!("axumapi_test_{}", Uuid::new_v4().simple());
        let backend = MongoBackend::connect(&url, &name)
            .await
            .unwrap()
            .with_keys(Keys::default().with("tags", "slug"));
        Some(Self {
            db: Db::new(backend.clone()),
            backend,
        })
    }

    async fn seeded() -> Option<(Self, Seed)> {
        let t = Self::open().await?;
        let seed = seed(&t.db).await;
        Some((t, seed))
    }

    async fn cleanup(self) {
        self.backend.database().drop().await.unwrap();
    }
}

fn is_capability(result: &Result<impl std::fmt::Debug, OrmError>, wanted: Feature) -> bool {
    matches!(
        result,
        Err(OrmError::Capability(BackendCapabilityError::Unsupported { feature, .. }))
            if *feature == wanted
    ) || matches!(
        (result, wanted),
        (
            Err(OrmError::Capability(
                BackendCapabilityError::RowLockingUnsupported { .. }
            )),
            Feature::RowLocking
        )
    )
}

fn ms(t: DateTime<Utc>) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(t.timestamp_millis()).unwrap()
}

#[tokio::test]
async fn keys_are_generated_in_bulk_order_and_stored_as_id() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut a = Author::new("a", Some(1));
    a.save(db).await.unwrap();
    assert!(a.id > 0);
    let bulk = Author::objects(db)
        .bulk_create(
            (0..5)
                .map(|i| Author::new(&format!("n{i}"), Some(i)))
                .collect(),
        )
        .await
        .unwrap();
    let ids: Vec<i64> = bulk.iter().map(|x| x.id).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "{ids:?}");
    assert_eq!(
        bulk.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
        ["n0", "n1", "n2", "n3", "n4"]
    );
    assert!(ids[0] > a.id);
    // The key column lives in `_id`; there is no separate `id` field.
    let raw = t
        .backend
        .database()
        .collection::<mongodb::bson::Document>("authors")
        .find_one(mongodb::bson::doc! { "_id": a.id })
        .await
        .unwrap()
        .unwrap();
    assert!(!raw.contains_key("id"));
    assert_eq!(raw.get_str("name").unwrap(), "a");
    t.cleanup().await;
}

#[tokio::test]
async fn instance_operations_and_timestamps() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut note = Note::new("hello");
    note.save(db).await.unwrap();
    assert!(note.id > 0 && note.created_at > DateTime::UNIX_EPOCH);
    assert_eq!(
        note.created_at,
        ms(note.created_at),
        "stored timestamps have millisecond precision"
    );
    note.body = "edited".into();
    note.save(db).await.unwrap();
    let mut copy = Note::new("stale");
    copy.id = note.id;
    copy.refresh(db).await.unwrap();
    assert_eq!(copy.body, "edited");
    assert_eq!(copy.created_at, note.created_at);
    assert_eq!(note.delete(db).await.unwrap(), 1);
    assert_eq!(note.delete(db).await.unwrap(), 0);
    t.cleanup().await;
}

#[tokio::test]
async fn every_scalar_type_round_trips() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut e = Event {
        id: Uuid::new_v4(),
        at: Utc.timestamp_micros(1_700_000_000_123_456).unwrap(),
        day: NaiveDate::from_ymd_opt(2026, 9, 29).unwrap(),
        clock: NaiveTime::from_hms_micro_opt(13, 45, 6, 250_000).unwrap(),
        payload: serde_json::json!({"k": [1, 2, {"z": null}], "s": "x"}),
        blob: vec![0, 1, 254, 255],
        score: Some(2.5),
    };
    e.save(db).await.unwrap();
    assert_eq!(e.at, ms(e.at));
    let back = Event::objects(db).get(Event::id.eq(e.id)).await.unwrap();
    assert_eq!(back, e);
    assert_eq!(back.at.timestamp_millis(), 1_700_000_000_123);
    // Stored natively: uuid binary subtype 4, datetime, embedded document.
    let raw = t
        .backend
        .database()
        .collection::<mongodb::bson::Document>("events")
        .find_one(mongodb::bson::doc! {})
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        raw.get("_id"),
        Some(mongodb::bson::Bson::Binary(b)) if b.subtype == mongodb::bson::spec::BinarySubtype::Uuid
    ));
    assert!(matches!(
        raw.get("at"),
        Some(mongodb::bson::Bson::DateTime(_))
    ));
    assert!(matches!(
        raw.get("payload"),
        Some(mongodb::bson::Bson::Document(_))
    ));
    assert_eq!(raw.get_str("day").unwrap(), "2026-09-29");

    // Filtering on typed values.
    let n = Event::objects(db)
        .filter(Event::at.ge(e.at))
        .filter(Event::day.eq(e.day))
        .filter(Event::score.is_not_null())
        .count()
        .await
        .unwrap();
    assert_eq!(n, 1);

    let mut none = Event {
        score: None,
        id: Uuid::new_v4(),
        ..e.clone()
    };
    none.save(db).await.unwrap();
    assert_eq!(
        Event::objects(db)
            .get(Event::id.eq(none.id))
            .await
            .unwrap()
            .score,
        None
    );
    t.cleanup().await;
}

#[tokio::test]
async fn decimals_are_exact_and_summed_by_the_server() {
    let Some((t, _)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let go = Book::objects(db).get(Book::title.eq("Go")).await.unwrap();
    assert_eq!(go.price, Decimal::new(4599, 2));
    let row = Book::objects(db)
        .aggregate([
            ("total", Sum::of(Book::price)),
            ("mean", Avg::of(Book::price)),
        ])
        .await
        .unwrap();
    assert_eq!(
        row.get_as::<Decimal>("total").unwrap(),
        Decimal::new(11848, 2)
    );
    let dear = Book::objects(db)
        .filter(Book::price.gt(Decimal::new(2500, 2)))
        .order_by([Book::price.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&dear), ["Rust", "Go"]);
    t.cleanup().await;
}

#[tokio::test]
async fn lookups_follow_sql_semantics() {
    let Some((t, _)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let names = |qs: axumapi_orm::QuerySet<Author>| async move {
        let mut v: Vec<String> = qs
            .all()
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.name)
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        names(Author::objects(db).filter(Author::name.icontains("A"))).await,
        ["Ann"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.contains("a"))).await,
        Vec::<String>::new()
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.starts_with("B"))).await,
        ["Bob"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.ends_with("y"))).await,
        ["Cy"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.iexact("dEe"))).await,
        ["Dee"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.regex("^[AB]"))).await,
        ["Ann", "Bob"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.is_in(["Ann", "Cy", "Zed"]))).await,
        ["Ann", "Cy"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::age.range(25, 30))).await,
        ["Ann", "Bob"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::age.is_null())).await,
        ["Cy"]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::age.eq(None::<i32>))).await,
        ["Cy"]
    );
    // NULL never satisfies a comparison, negated or not: Cy has no age.
    assert_eq!(
        names(Author::objects(db).filter(Author::age.ne(30))).await,
        ["Bob", "Dee"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::age.eq(30))).await,
        ["Bob", "Dee"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::age.gt(26))).await,
        ["Bob"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::age.range(25, 30))).await,
        ["Dee"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::age.is_in([30, 25]))).await,
        ["Dee"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::name.icontains("n"))).await,
        ["Bob", "Cy", "Dee"]
    );
    assert_eq!(
        names(Author::objects(db).exclude(Author::age.gt(26).and(Author::name.starts_with("D"))))
            .await,
        ["Ann", "Bob", "Cy"],
        "NOT (NULL AND FALSE) is true in SQL"
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::age.lt(26).or(Author::age.gt(40)))).await,
        ["Bob", "Dee"]
    );
    // Special characters are literal in contains/startswith.
    let mut odd = Author::new("a.b*c[", None);
    odd.save(db).await.unwrap();
    assert_eq!(
        names(Author::objects(db).filter(Author::name.contains(".b*"))).await,
        ["a.b*c["]
    );
    assert_eq!(
        names(Author::objects(db).filter(Author::name.icontains("C["))).await,
        ["a.b*c["]
    );
    assert_eq!(
        names(
            Author::objects(db).filter(
                Author::name
                    .contains("a.b*c[")
                    .and(Author::name.ends_with("c["))
            )
        )
        .await,
        ["a.b*c["]
    );
    // Empty AND / OR.
    assert_eq!(
        Author::objects(db)
            .filter(Expr::And(vec![]))
            .count()
            .await
            .unwrap(),
        5
    );
    assert_eq!(
        Author::objects(db)
            .filter(Expr::Or(vec![]))
            .count()
            .await
            .unwrap(),
        0
    );
    t.cleanup().await;
}

#[tokio::test]
async fn expressions_compile_to_aggregation_operators() {
    let Some((t, _)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    // Column-to-column comparison and arithmetic.
    let hated = Book::objects(db)
        .filter(Book::likes.gt(Book::dislikes + 1_i64))
        .order_by([Book::title.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&hated), ["Rust", "SQL"]);
    // NOT over an $expr keeps NULL rows out: `pages` is NULL for Async.
    let not_short = Book::objects(db)
        .exclude(Book::pages.expr().lt(Book::likes.expr() * 20_i64))
        .order_by([Book::title.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&not_short), ["Go", "Rust", "Zig"]);
    let rows = Book::objects(db)
        .filter(Book::title.eq("Rust"))
        .project([Book::title.select(), ("t", Book::title.expr()).into()])
        .rows()
        .await
        .unwrap();
    assert_eq!(rows[0].get("t"), Some(&Value::Text("Rust".into())));
    let rows = Book::objects(db)
        .filter(Book::title.eq("Rust"))
        .annotate("shout", upper(Book::title.expr()))
        .annotate("size", length(Book::title.expr()))
        .annotate("pg", coalesce([Book::pages.expr(), Expr::val(0)]))
        .annotate(
            "both",
            concat([
                Book::title.expr(),
                Expr::val("-"),
                lower(Book::title.expr()),
            ]),
        )
        .annotate("net", Book::likes.expr() - Book::dislikes.expr())
        .project(["title"])
        .rows()
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    let plain = Book::objects(db)
        .order_by([lower(Book::title.expr()).desc()])
        .limit(2)
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&plain), ["Zig", "SQL"]);
    // Date parts on stored dates.
    let y2022 = Book::objects(db)
        .filter(Book::published.year().eq(2022_i64))
        .order_by([Book::title.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&y2022), ["Go", "Zig"]);
    let feb = Book::objects(db)
        .filter(Book::published.month().eq(2_i64))
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&feb), ["Zig"]);
    t.cleanup().await;
}

#[tokio::test]
async fn date_parts_functions_casts_and_case() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let mut e = Event {
        id: Uuid::new_v4(),
        at: Utc.with_ymd_and_hms(2026, 9, 29, 8, 5, 3).unwrap(),
        day: NaiveDate::from_ymd_opt(2026, 9, 29).unwrap(),
        clock: NaiveTime::from_hms_micro_opt(13, 45, 6, 250_000).unwrap(),
        payload: serde_json::json!({}),
        blob: vec![],
        score: Some(2.5),
    };
    e.save(db).await.unwrap();
    let row = Event::objects(db)
        .annotate("y", Event::day.year())
        .annotate("m", Event::day.month())
        .annotate("d", Event::day.day())
        .annotate("w", Event::day.week())
        .annotate("q", Event::day.quarter())
        .annotate("day_of", Event::at.date())
        .annotate("ah", Event::at.hour())
        .annotate("am", Event::at.minute())
        .annotate("as", Event::at.second())
        .annotate("h", Event::clock.hour())
        .annotate("mi", Event::clock.minute())
        .annotate("s", Event::clock.second())
        .annotate("trimmed", axumapi_orm::functions::trim(Expr::val("  x ")))
        .annotate(
            "part",
            axumapi_orm::functions::substr(Expr::val("abcdef"), 2, Some(3)),
        )
        .annotate(
            "rest",
            axumapi_orm::functions::substr(Expr::val("abcdef"), 4, None),
        )
        .annotate(
            "swapped",
            axumapi_orm::functions::replace(Expr::val("a-b-c"), "-", "+"),
        )
        .annotate("as_text", Event::score.cast(axumapi_orm::SqlType::Text))
        .annotate("as_int", Expr::val("42").cast(axumapi_orm::SqlType::BigInt))
        .annotate(
            "size",
            axumapi_orm::functions::case()
                .when(Event::score.gt(2.0), Expr::val("big"))
                .otherwise(Expr::val("small")),
        )
        .project([
            "y", "m", "d", "w", "q", "day_of", "ah", "am", "as", "h", "mi", "s", "trimmed", "part",
            "rest", "swapped", "as_text", "as_int", "size",
        ])
        .rows()
        .await
        .unwrap()
        .remove(0);
    let int = |c: &str| row.get_as::<i64>(c).unwrap();
    assert_eq!(
        (int("y"), int("m"), int("d"), int("w"), int("q")),
        (2026, 9, 29, 40, 3)
    );
    assert_eq!(row.get_as::<String>("day_of").unwrap(), "2026-09-29");
    assert_eq!((int("ah"), int("am"), int("as")), (8, 5, 3));
    assert_eq!((int("h"), int("mi"), int("s")), (13, 45, 6));
    assert_eq!(row.get_as::<String>("trimmed").unwrap(), "x");
    assert_eq!(row.get_as::<String>("part").unwrap(), "bcd");
    assert_eq!(row.get_as::<String>("rest").unwrap(), "def");
    assert_eq!(row.get_as::<String>("swapped").unwrap(), "a+b+c");
    assert_eq!(row.get_as::<String>("as_text").unwrap(), "2.5");
    assert_eq!(int("as_int"), 42);
    assert_eq!(row.get_as::<String>("size").unwrap(), "big");
    // Filtering on a computed part.
    assert_eq!(
        Event::objects(db)
            .filter(Event::clock.hour().eq(13_i64))
            .count()
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        Event::objects(db)
            .filter(Event::at.minute().eq(6_i64))
            .count()
            .await
            .unwrap(),
        0
    );
    t.cleanup().await;
}

#[tokio::test]
async fn ordering_paging_distinct_count_exists() {
    let Some((t, _)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let page = Book::objects(db)
        .order_by([Book::likes.desc()])
        .offset(1)
        .limit(2)
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&page), ["SQL", "Async"]);
    assert_eq!(Book::objects(db).count().await.unwrap(), 5);
    assert_eq!(Book::objects(db).limit(2).count().await.unwrap(), 2);
    assert_eq!(
        Book::objects(db)
            .filter(Book::likes.gt(100))
            .count()
            .await
            .unwrap(),
        0
    );
    assert!(Book::objects(db).exists().await.unwrap());
    assert!(
        !Book::objects(db)
            .filter(Book::likes.gt(100))
            .exists()
            .await
            .unwrap()
    );
    assert!(Book::objects(db).none().all().await.unwrap().is_empty());
    let authors: Vec<i64> = {
        let mut v = Book::objects(db)
            .distinct()
            .values_list(["author_id"])
            .await
            .unwrap();
        v.sort_unstable();
        v
    };
    assert_eq!(authors.len(), 3);
    assert_eq!(
        Book::objects(db)
            .distinct()
            .values_list::<i64, _>(["author_id"])
            .await
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        Book::objects(db)
            .project(["author_id"])
            .distinct()
            .count()
            .await
            .unwrap(),
        3
    );
    let first = Book::objects(db)
        .order_by([Book::pages.asc()])
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.title, "Async", "NULLs sort first ascending");
    let last = Book::objects(db)
        .order_by([Book::likes.asc()])
        .last()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(last.title, "Rust");
    let p = Book::objects(db)
        .order_by([Book::title.asc()])
        .paginate(2, 2)
        .await
        .unwrap();
    assert_eq!((p.total, titles(&p.items)), (5, vec!["Rust", "SQL"]));
    t.cleanup().await;
}

#[tokio::test]
async fn aggregates_grouping_and_having() {
    let Some((t, s)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let row = Book::objects(db)
        .aggregate([
            ("n", Count::all()),
            ("with_pages", Count::of(Book::pages)),
            ("likes", Sum::of(Book::likes)),
            ("mean", Avg::of(Book::likes)),
            ("lo", Min::of(Book::likes)),
            ("hi", Max::of(Book::likes)),
            ("sd", StdDev::sample(Book::likes)),
            ("sdp", StdDev::population(Book::likes)),
            ("var", Variance::sample(Book::likes)),
            ("authors", Count::of(Book::author).distinct()),
            ("big", Sum::of(Book::likes).filter(Book::likes.gt(4))),
            ("titles", Count::all().filter(Book::title.starts_with("S"))),
        ])
        .await
        .unwrap();
    assert_eq!(row.get_as::<i64>("n").unwrap(), 5);
    assert_eq!(row.get_as::<i64>("with_pages").unwrap(), 4);
    assert_eq!(row.get_as::<i64>("likes").unwrap(), 27);
    assert!((row.get_as::<f64>("mean").unwrap() - 5.4).abs() < 1e-9);
    assert_eq!(
        (
            row.get_as::<i64>("lo").unwrap(),
            row.get_as::<i64>("hi").unwrap()
        ),
        (1, 10)
    );
    let var = row.get_as::<f64>("var").unwrap();
    assert!((row.get_as::<f64>("sd").unwrap() - var.sqrt()).abs() < 1e-9);
    assert!((var - 13.3).abs() < 1e-9, "{var}");
    assert!((row.get_as::<f64>("sdp").unwrap().powi(2) - 10.64).abs() < 1e-9);
    assert_eq!(row.get_as::<i64>("authors").unwrap(), 3);
    assert_eq!(row.get_as::<i64>("big").unwrap(), 23);
    assert_eq!(row.get_as::<i64>("titles").unwrap(), 1);

    // Aggregates over an empty queryset still produce one row.
    let empty = Book::objects(db)
        .filter(Book::likes.gt(1000))
        .aggregate([
            ("n", Count::all()),
            ("total", Sum::of(Book::likes)),
            ("avg", Avg::of(Book::likes)),
        ])
        .await
        .unwrap();
    assert_eq!(empty.get_as::<i64>("n").unwrap(), 0);
    assert_eq!(empty.get_as::<Option<i64>>("total").unwrap(), None);
    assert_eq!(empty.get_as::<Option<f64>>("avg").unwrap(), None);
    let none = Book::objects(db)
        .none()
        .aggregate([("n", Count::all())])
        .await
        .unwrap();
    assert_eq!(none.get_as::<i64>("n").unwrap(), 0);
    // SUM of only-NULL values is NULL, not 0.
    let nulls = Author::objects(db)
        .filter(Author::age.is_null())
        .aggregate([("total", Sum::of(Author::age))])
        .await
        .unwrap();
    assert_eq!(nulls.get_as::<Option<i64>>("total").unwrap(), None);

    // Aggregate over a limited (derived) queryset.
    let limited = Book::objects(db)
        .order_by([Book::likes.desc()])
        .limit(2)
        .aggregate([("total", Sum::of(Book::likes))])
        .await
        .unwrap();
    assert_eq!(limited.get_as::<i64>("total").unwrap(), 18);

    // GROUP BY with HAVING and ordering by an aggregate.
    let grouped = Book::objects(db)
        .project([Book::author.select()])
        .annotate("n", Count::all())
        .annotate("total", Sum::of(Book::likes))
        .filter(Expr::from(Count::all()).gt(1_i64))
        .order_by([Expr::col("total").desc()])
        .rows()
        .await
        .unwrap();
    let got: Vec<(i64, i64, i64)> = grouped
        .iter()
        .map(|r| {
            (
                r.get_as("author_id").unwrap(),
                r.get_as("n").unwrap(),
                r.get_as("total").unwrap(),
            )
        })
        .collect();
    assert_eq!(got, [(s.ann.id, 2, 15), (s.dee.id, 2, 4)]);

    let joined = Book::objects(db)
        .filter(Book::author.eq(s.ann.id))
        .aggregate([("titles", StringAgg::of(Book::title, ", "))])
        .await
        .unwrap();
    let mut parts: Vec<String> = joined
        .get_as::<String>("titles")
        .unwrap()
        .split(", ")
        .map(str::to_owned)
        .collect();
    parts.sort();
    assert_eq!(parts, ["Async", "Rust"]);
    t.cleanup().await;
}

#[tokio::test]
async fn updates_deletes_and_f_expressions() {
    let Some((t, _)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let n = Book::objects(db)
        .filter(Book::likes.lt(6))
        .update([
            Book::likes.set_expr(Book::likes + 10_i64),
            Book::pages.set(7),
        ])
        .await
        .unwrap();
    assert_eq!(n, 3);
    let mut likes: Vec<i64> = Book::objects(db)
        .all()
        .await
        .unwrap()
        .iter()
        .map(|b| b.likes)
        .collect();
    likes.sort_unstable();
    assert_eq!(likes, [8, 10, 11, 13, 15]);
    // Literal-only update, NULL assignment, no match.
    let n = Author::objects(db)
        .filter(Author::name.eq("Ann"))
        .update([Author::age.set(None::<i32>)])
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        Author::objects(db)
            .filter(Author::age.is_null())
            .count()
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        Author::objects(db)
            .filter(Author::name.eq("nobody"))
            .update([Author::age.set(1)])
            .await
            .unwrap(),
        0
    );
    // A limited queryset needs `pk IN (subquery)`, which MongoDB lacks.
    let r = Book::objects(db).limit(1).delete().await;
    assert!(is_capability(&r, Feature::Subqueries), "{r:?}");
    assert_eq!(
        Book::objects(db)
            .filter(Book::likes.gt(12))
            .delete()
            .await
            .unwrap(),
        2
    );
    assert_eq!(Book::objects(db).count().await.unwrap(), 3);
    // MongoDB has no foreign keys: deleting an author keeps their books.
    Author::objects(db)
        .filter(Author::name.eq("Dee"))
        .delete()
        .await
        .unwrap();
    assert_eq!(Author::objects(db).count().await.unwrap(), 3);
    assert_eq!(Book::objects(db).count().await.unwrap(), 3);
    t.cleanup().await;
}

#[tokio::test]
async fn create_family_and_bulk_operations() {
    let Some((t, s)) = TestDb::seeded().await else {
        return;
    };
    let db = &t.db;
    let (found, created) = Author::objects(db)
        .get_or_create(Author::name.eq("Ann"), || Author::new("Ann", None))
        .await
        .unwrap();
    assert!(!created && found.id == s.ann.id);
    let (fresh, created) = Author::objects(db)
        .get_or_create(Author::name.eq("Eve"), || Author::new("Eve", Some(9)))
        .await
        .unwrap();
    assert!(created && fresh.id > s.dee.id);
    let (updated, created) = Author::objects(db)
        .update_or_create(
            Author::name.eq("Eve"),
            || Author::new("Eve", None),
            |a| a.age = Some(10),
        )
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(updated.age, Some(10));
    assert_eq!(
        Author::objects(db)
            .get(Author::name.eq("Eve"))
            .await
            .unwrap()
            .age,
        Some(10)
    );
    let (made, created) = Author::objects(db)
        .update_or_create(
            Author::name.eq("Fay"),
            || Author::new("Fay", Some(1)),
            |_| {},
        )
        .await
        .unwrap();
    assert!(created && made.id > fresh.id);

    let mut authors = Author::objects(db)
        .order_by([Author::id.asc()])
        .all()
        .await
        .unwrap();
    for a in &mut authors {
        a.age = Some(a.age.unwrap_or(0) + 1);
    }
    let changed = Author::objects(db)
        .bulk_update(&authors, &["age"])
        .await
        .unwrap();
    assert_eq!(changed, authors.len() as u64);
    assert_eq!(
        Author::objects(db)
            .get(Author::name.eq("Cy"))
            .await
            .unwrap()
            .age,
        Some(1)
    );

    let map = Author::objects(db)
        .in_bulk([s.ann.id, s.bob.id, 9999])
        .await
        .unwrap();
    assert_eq!(map.len(), 2);
    assert!(Author::objects(db).contains(&s.ann).await.unwrap());
    t.cleanup().await;
}

#[tokio::test]
async fn duplicate_keys_and_unique_indexes_are_constraint_errors() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    let tag = Tag {
        slug: "rust".into(),
        label: "Rust".into(),
    };
    Tag::objects(db).create(tag.clone()).await.unwrap();
    let err = Tag::objects(db).create(tag).await;
    assert!(
        matches!(err, Err(OrmError::Backend(BackendError::Constraint(_)))),
        "{err:?}"
    );
    assert_eq!(Tag::objects(db).count().await.unwrap(), 1);

    t.backend
        .create_unique_index("authors", "name")
        .await
        .unwrap();
    Author::new("uniq", None).save(db).await.unwrap();
    let err = Author::new("uniq", None).save(db).await;
    assert!(
        matches!(err, Err(OrmError::Backend(BackendError::Constraint(_)))),
        "{err:?}"
    );
    // A duplicate inside a bulk insert fails the whole transaction.
    let before = Author::objects(db).count().await.unwrap();
    let err = Author::objects(db)
        .bulk_create(vec![Author::new("x1", None), Author::new("uniq", None)])
        .await;
    assert!(
        matches!(err, Err(OrmError::Backend(BackendError::Constraint(_)))),
        "{err:?}"
    );
    assert_eq!(Author::objects(db).count().await.unwrap(), before);
    t.cleanup().await;
}

#[tokio::test]
async fn transactions_commit_and_roll_back() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let db = &t.db;
    Author::new("outside", None).save(db).await.unwrap();
    db.transaction(|tx| async move {
        Author::new("committed", Some(1)).save(&tx).await?;
        // Reads inside the transaction see its own writes; others do not.
        assert_eq!(Author::objects(&tx).count().await?, 2);
        Ok::<_, OrmError>(())
    })
    .await
    .unwrap();
    assert_eq!(Author::objects(db).count().await.unwrap(), 2);

    let result: Result<(), OrmError> = db
        .transaction(|tx| async move {
            Author::new("rolled back", Some(2)).save(&tx).await?;
            Author::objects(&tx)
                .filter(Author::name.eq("outside"))
                .delete()
                .await?;
            assert_eq!(Author::objects(&tx).count().await?, 2);
            Err(axumapi_orm::QueryError::InvalidPlan("boom".into()).into())
        })
        .await;
    assert!(result.is_err());
    assert_eq!(Author::objects(db).count().await.unwrap(), 2);
    assert!(
        Author::objects(db)
            .filter(Author::name.eq("outside"))
            .exists()
            .await
            .unwrap()
    );

    // Isolation is not requestable; nested transactions need savepoints.
    let r: Result<(), OrmError> = db
        .transaction_with(IsolationLevel::Serializable, |_| async { Ok(()) })
        .await;
    assert!(
        is_capability(&r, Feature::Isolation(IsolationLevel::Serializable)),
        "{r:?}"
    );
    let r: Result<(), OrmError> = db
        .transaction(|tx| async move { tx.transaction(|_| async { Ok::<_, OrmError>(()) }).await })
        .await;
    assert!(is_capability(&r, Feature::Savepoints), "{r:?}");
    // bulk_create inside a transaction is a nested transaction.
    assert_eq!(Author::objects(db).count().await.unwrap(), 2);
    t.cleanup().await;
}

#[tokio::test]
async fn unsupported_features_fail_before_any_io() {
    // Nothing listens here: any I/O would fail with a connection error.
    let client = mongodb::Client::with_uri_str(
        "mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=100&directConnection=true",
    )
    .await
    .unwrap();
    let db = Db::new(MongoBackend::from_client(client, "nowhere"));
    let books = || Book::objects(&db);

    let r = books().select_for_update().all().await;
    assert!(is_capability(&r, Feature::RowLocking), "{r:?}");
    let r = books().skip_locked().all().await;
    assert!(
        is_capability(&r, Feature::RowLocking) || is_capability(&r, Feature::LockModifiers),
        "{r:?}"
    );
    let r = books()
        .filter(Book::author.join(Author::name).eq("Ann"))
        .all()
        .await;
    assert!(is_capability(&r, Feature::Joins), "{r:?}");
    let r = books().select_related(Book::author_relation()).all().await;
    assert!(is_capability(&r, Feature::Joins), "{r:?}");
    let r = books().annotate("rn", RowNumber::new()).all().await;
    assert!(is_capability(&r, Feature::WindowFunctions), "{r:?}");
    let r = books().distinct_on([Book::author.expr()]).all().await;
    assert!(is_capability(&r, Feature::DistinctOn), "{r:?}");
    let r = books()
        .filter(Book::author.in_subquery(Author::objects(&db).subquery(Author::id.select())))
        .all()
        .await;
    assert!(is_capability(&r, Feature::Subqueries), "{r:?}");
    let r = books()
        .filter(Author::objects(&db).exists_expr())
        .count()
        .await;
    assert!(is_capability(&r, Feature::Subqueries), "{r:?}");
    let r = books().union(books()).unwrap().all().await;
    assert!(is_capability(&r, Feature::SetOperations), "{r:?}");
    let r = books()
        .annotate("arr", axumapi_orm::ArrayAgg::of(Book::title))
        .all()
        .await;
    assert!(r.is_err());
    let r = books()
        .aggregate([("a", axumapi_orm::ArrayAgg::of(Book::title))])
        .await;
    assert!(is_capability(&r, Feature::Arrays), "{r:?}");
    for r in [
        db.raw_sql("SELECT 1", vec![]).await.map(|_| ()),
        db.raw_execute("DELETE FROM x", vec![]).await.map(|_| ()),
        db.execute_script("CREATE TABLE x (id INT)").await,
    ] {
        assert!(is_capability(&r, Feature::RawSql), "{r:?}");
    }
    // Isolation levels are rejected before a session starts.
    let r: Result<(), OrmError> = db
        .transaction_with(IsolationLevel::ReadCommitted, |_| async { Ok(()) })
        .await;
    assert!(
        is_capability(&r, Feature::Isolation(IsolationLevel::ReadCommitted)),
        "{r:?}"
    );
    // Whereas a supported plan does reach the network.
    let r = books().count().await;
    assert!(
        matches!(r, Err(OrmError::Backend(BackendError::Connection(_)))),
        "{r:?}"
    );
}

#[tokio::test]
async fn raw_commands_reach_the_server() {
    let Some(t) = TestDb::open().await else {
        return;
    };
    let reply = t
        .backend
        .raw_command(mongodb::bson::doc! { "ping": 1 })
        .await
        .unwrap();
    assert_eq!(reply.get_f64("ok").unwrap(), 1.0);
    t.cleanup().await;
}
