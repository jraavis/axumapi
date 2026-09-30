//! Typed operands, functions, `CASE`, subqueries, date parts and set
//! operations on SQLite.
#![allow(clippy::unwrap_used)]

mod common;

use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};
use common::{Author, Book, Event, db, seed, titles};
use rust_decimal::Decimal;
use siderite_orm::functions::*;
use siderite_orm::{Count, Expr, Model, ModelOps, OrmError, QueryError, SqlType};
use uuid::Uuid;

#[tokio::test]
async fn optional_fields_take_plain_values_and_null_means_is_null() {
    let db = db().await;
    seed(&db).await;
    let thirty = Author::objects(&db)
        .filter(Author::age.eq(30))
        .all()
        .await
        .unwrap();
    assert_eq!(thirty[0].name, "Ann");
    let unknown = Author::objects(&db)
        .filter(Author::age.eq(None::<i32>))
        .all()
        .await
        .unwrap();
    assert_eq!(unknown[0].name, "Cy");
    let known = Author::objects(&db)
        .filter(Author::age.ne(None::<i32>))
        .count()
        .await
        .unwrap();
    assert_eq!(known, 3);
    assert_eq!(
        Author::objects(&db)
            .filter(Author::age.is_not_null())
            .count()
            .await
            .unwrap(),
        3
    );
    assert_eq!(
        Author::objects(&db)
            .filter(Author::age.is_null())
            .count()
            .await
            .unwrap(),
        1
    );
    let ranged = Author::objects(&db)
        .filter(Author::age.range(26, 40))
        .all()
        .await
        .unwrap();
    assert_eq!(ranged.len(), 1);
    let listed = Author::objects(&db)
        .filter(Author::age.is_in([25, 41]))
        .count()
        .await
        .unwrap();
    assert_eq!(listed, 2);
}

#[tokio::test]
async fn foreign_keys_compare_with_keys_models_and_lists() {
    let db = db().await;
    let s = seed(&db).await;
    let by_key = Book::objects(&db)
        .filter(Book::author.eq(s.dee.id))
        .count()
        .await
        .unwrap();
    let by_model = Book::objects(&db)
        .filter(Book::author.eq(&s.dee))
        .count()
        .await
        .unwrap();
    let by_fk = Book::objects(&db)
        .filter(Book::author.eq(siderite_orm::ForeignKey::<Author>::new(s.dee.id)))
        .count()
        .await
        .unwrap();
    assert_eq!((by_key, by_model, by_fk), (2, 2, 2));
    let listed = Book::objects(&db)
        .filter(Book::author.is_in([s.ann.id, s.bob.id]))
        .count()
        .await
        .unwrap();
    assert_eq!(listed, 3);
    let nullable = Author::objects(&db)
        .filter(Author::team.eq(&s.red))
        .count()
        .await
        .unwrap();
    assert_eq!(nullable, 2);
    assert_eq!(
        Author::objects(&db)
            .filter(Author::team.is_null())
            .count()
            .await
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn scalar_types_round_trip_and_compare() {
    let db = db().await;
    let s = seed(&db).await;
    let cheap = Book::objects(&db)
        .filter(Book::price.lt(Decimal::new(1500, 2)))
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&cheap), ["Async", "Zig"]);
    let dated = Book::objects(&db)
        .filter(Book::published.range(
            NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2021, 12, 31).unwrap(),
        ))
        .order_by([Book::published.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&dated), ["Rust", "Async"]);
    assert_eq!(s.book("Go").price, Decimal::new(4599, 2));

    let id = Uuid::from_u128(0xfeed);
    let at = Utc.with_ymd_and_hms(2024, 5, 17, 13, 45, 30).unwrap()
        + chrono::TimeDelta::microseconds(123_456);
    let mut event = Event {
        id,
        at,
        day: NaiveDate::from_ymd_opt(2024, 5, 17).unwrap(),
        clock: NaiveTime::from_hms_micro_opt(13, 45, 30, 123_456).unwrap(),
        payload: serde_json::json!({"k": [1, 2]}),
        blob: vec![0, 1, 255],
        score: Some(2.5),
    };
    event.save(&db).await.unwrap();
    let stored = Event::objects(&db).get(Event::id.eq(id)).await.unwrap();
    assert_eq!(stored, event);
    let later = Event::objects(&db)
        .filter(Event::at.gt(at - chrono::TimeDelta::seconds(1)))
        .count()
        .await
        .unwrap();
    assert_eq!(later, 1);
    let scored = Event::objects(&db)
        .filter(Event::score.ge(2.5))
        .count()
        .await
        .unwrap();
    assert_eq!(scored, 1);
}

#[tokio::test]
async fn f_arithmetic_is_typed_and_usable_everywhere() {
    let db = db().await;
    seed(&db).await;
    let net_positive = Book::objects(&db)
        .filter((Book::likes - Book::dislikes).gt(4_i64))
        .order_by([Book::id.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&net_positive), ["Rust", "SQL"]);
    let same = Book::objects(&db)
        .filter(Book::likes.eq(Book::dislikes + 0_i64))
        .count()
        .await
        .unwrap();
    assert_eq!(same, 2, "Async and Zig have likes == dislikes");
    let best = Book::objects(&db)
        .order_by([(Book::likes * 2_i64 - Book::dislikes).desc()])
        .first()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(best.title, "Rust");
    let bumped = Book::objects(&db)
        .filter(Book::title.eq("Go"))
        .update([
            Book::likes.set_expr(Book::likes * 10_i64 + 1_i64),
            Book::pages.set_expr(-Book::likes),
        ])
        .await
        .unwrap();
    assert_eq!(bumped, 1);
    let go = Book::objects(&db).get(Book::title.eq("Go")).await.unwrap();
    assert_eq!((go.likes, go.pages), (11, Some(-1)));
}

#[tokio::test]
async fn functions_case_and_annotations() {
    let db = db().await;
    seed(&db).await;
    let rows = Book::objects(&db)
        .order_by([Book::id.asc()])
        .project([
            ("upper", upper(Book::title.expr())),
            ("len", length(Book::title.expr())),
            ("sub", substr(Book::title.expr(), 2, Some(2))),
            ("swapped", replace(Book::title.expr(), "o", "0")),
            ("pages", coalesce([Book::pages.expr(), Expr::val(-1)])),
            (
                "joined",
                concat([Book::title.expr(), Expr::val("-"), Expr::col("likes")]),
            ),
            (
                "trimmed",
                trim(concat([
                    Expr::val("  "),
                    Book::title.expr(),
                    Expr::val(" "),
                ])),
            ),
            ("as_text", cast(Book::likes.expr(), SqlType::Text)),
        ])
        .rows()
        .await
        .unwrap();
    let first = &rows[0];
    assert_eq!(first.get_as::<String>("upper").unwrap(), "RUST");
    assert_eq!(first.get_as::<i64>("len").unwrap(), 4);
    assert_eq!(first.get_as::<String>("sub").unwrap(), "us");
    assert_eq!(first.get_as::<String>("joined").unwrap(), "Rust-10");
    assert_eq!(first.get_as::<String>("trimmed").unwrap(), "Rust");
    assert_eq!(first.get_as::<String>("as_text").unwrap(), "10");
    assert_eq!(rows[1].get_as::<i64>("pages").unwrap(), -1);
    assert_eq!(rows[3].get_as::<String>("swapped").unwrap(), "G0");

    let labelled = Book::objects(&db)
        .annotate(
            "heat",
            case()
                .when(Book::likes.ge(8_i64), "hot")
                .when(Book::likes.ge(3_i64), "warm")
                .otherwise("cold"),
        )
        .annotate("net", Book::likes - Book::dislikes)
        .filter(Expr::col("net").gt(0_i64))
        .order_by([Expr::col("net").desc(), Book::id.asc()])
        .all_annotated()
        .await
        .unwrap();
    let summary: Vec<_> = labelled
        .iter()
        .map(|(b, row)| (b.title.as_str(), row.get_as::<String>("heat").unwrap()))
        .collect();
    assert_eq!(
        summary,
        [
            ("Rust", "hot".to_owned()),
            ("SQL", "hot".to_owned()),
            ("Go", "cold".to_owned())
        ]
    );

    let hidden = Book::objects(&db)
        .alias("net", Book::likes - Book::dislikes)
        .filter(Expr::col("net").eq(0_i64))
        .order_by([Book::id.asc()])
        .all_annotated()
        .await
        .unwrap();
    assert_eq!(
        titles(&hidden.iter().map(|(b, _)| b.clone()).collect::<Vec<_>>()),
        ["Async", "Zig"]
    );
    assert!(hidden[0].1.get("net").is_none(), "aliases are not selected");
}

#[tokio::test]
async fn text_lookups_and_iexact() {
    let db = db().await;
    seed(&db).await;
    let exact = Book::objects(&db)
        .filter(Book::title.iexact("RUST"))
        .count()
        .await
        .unwrap();
    assert_eq!(exact, 1);
    let upper = Book::objects(&db)
        .filter(upper(Book::title.expr()).eq("SQL"))
        .count()
        .await
        .unwrap();
    assert_eq!(upper, 1);
    let starts = Book::objects(&db)
        .filter(Book::title.istarts_with("z"))
        .count()
        .await
        .unwrap();
    assert_eq!(starts, 1);
}

#[tokio::test]
async fn subqueries_in_exists_and_scalar_forms() {
    let db = db().await;
    seed(&db).await;
    let popular = Book::objects(&db)
        .filter(Book::likes.gt(7_i64))
        .subquery(Book::author);
    let authors = Author::objects(&db)
        .filter(Author::id.in_subquery(popular))
        .all()
        .await
        .unwrap();
    assert_eq!(
        authors.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        ["Ann", "Bob"]
    );

    let with_long_books = Author::objects(&db)
        .filter(
            Book::objects(&db)
                .filter(Book::author.expr().eq(Author::id.outer_ref()))
                .filter(Book::pages.gt(190))
                .exists_expr(),
        )
        .all()
        .await
        .unwrap();
    assert_eq!(
        with_long_books
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>(),
        ["Ann", "Dee"]
    );
    let without = Author::objects(&db)
        .exclude(
            Book::objects(&db)
                .filter(Book::author.expr().eq(Author::id.outer_ref()))
                .exists_expr(),
        )
        .all()
        .await
        .unwrap();
    assert_eq!(without[0].name, "Cy");

    let per_author = Book::objects(&db)
        .filter(Book::author.expr().eq(Author::id.outer_ref()))
        .subquery(("n", Count::all()));
    let counted = Author::objects(&db)
        .annotate("books", Expr::subquery(per_author))
        .filter(Expr::col("books").ge(2_i64))
        .all_annotated()
        .await
        .unwrap();
    let counts: Vec<_> = counted
        .iter()
        .map(|(a, row)| (a.name.clone(), row.get_as::<i64>("books").unwrap()))
        .collect();
    assert_eq!(counts, [("Ann".to_owned(), 2), ("Dee".to_owned(), 2)]);
}

#[tokio::test]
async fn self_referencing_subquery_needs_an_alias() {
    let db = db().await;
    seed(&db).await;
    // Books that have a more liked book by the same author.
    let beaten = Book::objects(&db)
        .filter(
            Book::objects(&db)
                .aliased("rival")
                .filter(Book::author.expr().eq(Expr::outer("author_id")))
                .filter(Book::likes.expr().gt(Expr::outer("likes")))
                .exists_expr(),
        )
        .order_by([Book::id.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(titles(&beaten), ["Async", "Go"]);
}

#[tokio::test]
async fn date_and_time_parts_match_their_sql_definitions() {
    let db = db().await;
    for (i, (y, m, d, h, mi, sec)) in [
        (2024, 1, 1, 0, 0, 0),
        (2024, 5, 17, 13, 45, 30),
        (2024, 12, 30, 23, 59, 59),
        (2021, 1, 3, 6, 7, 8),
    ]
    .into_iter()
    .enumerate()
    {
        Event {
            id: Uuid::from_u128(i as u128 + 1),
            at: Utc.with_ymd_and_hms(y, m, d, h, mi, sec).unwrap(),
            day: NaiveDate::from_ymd_opt(y, m, d).unwrap(),
            clock: NaiveTime::from_hms_opt(h, mi, sec).unwrap(),
            payload: serde_json::json!(null),
            blob: vec![],
            score: None,
        }
        .save(&db)
        .await
        .unwrap();
    }
    let rows = Event::objects(&db)
        .order_by([Event::at.asc()])
        .project([
            ("year", Event::at.year()),
            ("month", Event::at.month()),
            ("day", Event::at.day()),
            ("week", Event::at.week()),
            ("quarter", Event::at.quarter()),
            ("hour", Event::at.hour()),
            ("minute", Event::at.minute()),
            ("second", Event::at.second()),
            ("date", Event::at.date()),
            ("clock_hour", Event::clock.hour()),
            ("day_week", Event::day.week()),
        ])
        .rows()
        .await
        .unwrap();
    let get = |row: usize, name: &str| rows[row].get_as::<i64>(name).unwrap();
    // Order: 2021-01-03, 2024-01-01, 2024-05-17, 2024-12-30.
    assert_eq!(
        (get(1, "year"), get(1, "month"), get(1, "day")),
        (2024, 1, 1)
    );
    assert_eq!(
        (get(2, "hour"), get(2, "minute"), get(2, "second")),
        (13, 45, 30)
    );
    assert_eq!(get(2, "quarter"), 2);
    assert_eq!(get(3, "quarter"), 4);
    assert_eq!(get(2, "clock_hour"), 13);
    // ISO weeks: 2021-01-03 is a Sunday of week 53, 2024-01-01 starts week 1,
    // 2024-05-17 is in week 20, 2024-12-30 already belongs to week 1 of 2025.
    assert_eq!(
        [
            get(0, "week"),
            get(1, "week"),
            get(2, "week"),
            get(3, "week")
        ],
        [53, 1, 20, 1]
    );
    assert_eq!(get(0, "day_week"), 53);
    assert_eq!(
        rows[2].get_as::<NaiveDate>("date").unwrap(),
        NaiveDate::from_ymd_opt(2024, 5, 17).unwrap()
    );

    let in_may = Event::objects(&db)
        .filter(
            Event::at
                .year()
                .eq(2024_i64)
                .and(Event::at.month().eq(5_i64)),
        )
        .count()
        .await
        .unwrap();
    assert_eq!(in_may, 1);
    let books_2022 = {
        seed(&db).await;
        Book::objects(&db)
            .filter(Book::published.year().eq(2022_i64))
            .count()
            .await
            .unwrap()
    };
    assert_eq!(books_2022, 2);
}

#[tokio::test]
async fn set_operations_combine_and_validate_shapes() {
    let db = db().await;
    seed(&db).await;
    let cheap = || Book::objects(&db).filter(Book::price.lt(Decimal::new(2500, 2)));
    let liked = || Book::objects(&db).filter(Book::likes.ge(5_i64));
    let titles_of = |qs: siderite_orm::QuerySet<Book>| async move {
        qs.order_by([Book::title.asc()])
            .all()
            .await
            .unwrap()
            .into_iter()
            .map(|b| b.title)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        titles_of(cheap().union(liked()).unwrap()).await,
        ["Async", "Rust", "SQL", "Zig"]
    );
    assert_eq!(
        titles_of(cheap().intersection(liked()).unwrap()).await,
        ["Async", "SQL"]
    );
    assert_eq!(
        titles_of(cheap().difference(liked()).unwrap()).await,
        ["Zig"]
    );
    let all = cheap().union_all(liked()).unwrap().count().await.unwrap();
    assert_eq!(all, 6);
    let limited = cheap()
        .union(liked().limit(1))
        .unwrap()
        .order_by([Book::id.asc()])
        .limit(2)
        .all()
        .await
        .unwrap();
    assert_eq!(limited.len(), 2);

    let skinny = cheap().project([Book::title.select()]);
    assert!(matches!(
        cheap().union(skinny),
        Err(QueryError::InvalidPlan(_))
    ));
    let empty = cheap().union(Book::objects(&db).none()).unwrap();
    assert_eq!(empty.count().await.unwrap(), 3);
}

#[tokio::test]
async fn filtering_on_a_window_is_rejected() {
    let db = db().await;
    let outcome = Book::objects(&db)
        .annotate("rn", siderite_orm::RowNumber::new())
        .filter(Expr::col("rn").eq(1_i64))
        .all()
        .await;
    assert!(matches!(
        outcome,
        Err(OrmError::Query(QueryError::InvalidPlan(_)))
    ));
}
