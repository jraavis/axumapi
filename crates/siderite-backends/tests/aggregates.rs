//! `aggregate`, `annotate` with aggregates, grouping and `HAVING` on SQLite.
#![cfg(feature = "sqlite")]
#![allow(clippy::unwrap_used)]

mod common;

use common::{Author, Book, db, seed};
use rust_decimal::Decimal;
use siderite_orm::{Avg, Count, Expr, Max, Min, Model, StringAgg, Sum};

#[tokio::test]
async fn aggregate_computes_one_row_over_the_queryset() {
    let db = db().await;
    seed(&db).await;
    let row = Book::objects(&db)
        .aggregate([
            ("n", Count::all()),
            ("with_pages", Count::of(Book::pages)),
            ("likes", Sum::of(Book::likes)),
            ("mean", Avg::of(Book::likes)),
            ("lo", Min::of(Book::likes)),
            ("hi", Max::of(Book::likes)),
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
}

#[tokio::test]
async fn aggregate_ignores_ordering_and_respects_filters_limits_and_none() {
    let db = db().await;
    seed(&db).await;
    let filtered = Author::objects(&db)
        .filter(Author::age.gt(26))
        .aggregate([("n", Count::all()), ("oldest", Count::all())])
        .await
        .unwrap();
    assert_eq!(
        filtered.get_as::<i64>("n").unwrap(),
        2,
        "default ordering does not break the aggregate"
    );
    let limited = Book::objects(&db)
        .order_by([Book::likes.desc()])
        .limit(2)
        .aggregate([("total", Sum::of(Book::likes))])
        .await
        .unwrap();
    assert_eq!(limited.get_as::<i64>("total").unwrap(), 18);
    let none = Book::objects(&db)
        .none()
        .aggregate([("n", Count::all())])
        .await
        .unwrap();
    assert_eq!(none.get_as::<i64>("n").unwrap(), 0);
    let empty_sum = Book::objects(&db)
        .filter(Book::likes.gt(99_i64))
        .aggregate([("total", Sum::of(Book::likes))])
        .await
        .unwrap();
    assert_eq!(empty_sum.get_as::<Option<i64>>("total").unwrap(), None);
}

#[tokio::test]
async fn aggregate_supports_distinct_filter_and_decimals() {
    let db = db().await;
    seed(&db).await;
    let row = Book::objects(&db)
        .aggregate([
            ("authors", Count::of(Book::author).distinct()),
            ("popular", Count::all().filter(Book::likes.ge(5_i64))),
            ("price", Sum::of(Book::price)),
        ])
        .await
        .unwrap();
    assert_eq!(row.get_as::<i64>("authors").unwrap(), 3);
    assert_eq!(row.get_as::<i64>("popular").unwrap(), 3);
    assert_eq!(
        row.get_as::<Decimal>("price").unwrap(),
        Decimal::new(11848, 2)
    );
}

#[tokio::test]
async fn project_then_annotate_groups_by_the_projection() {
    let db = db().await;
    let s = seed(&db).await;
    let rows = Book::objects(&db)
        .project([Book::author.select()])
        .annotate("books", Count::all())
        .annotate("total", Sum::of(Book::likes))
        .order_by([Expr::col("total").desc()])
        .rows()
        .await
        .unwrap();
    let summary: Vec<(i64, i64, i64)> = rows
        .iter()
        .map(|r| {
            (
                r.get_as("author_id").unwrap(),
                r.get_as("books").unwrap(),
                r.get_as("total").unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [(s.ann.id, 2, 15), (s.bob.id, 1, 8), (s.dee.id, 2, 4)]
    );
}

#[tokio::test]
async fn filters_on_aggregate_annotations_become_having() {
    let db = db().await;
    let s = seed(&db).await;
    let busy = Book::objects(&db)
        .project([Book::author.select()])
        .annotate("books", Count::all())
        .filter(Expr::col("books").gt(1_i64))
        .filter(Book::likes.ge(0_i64))
        .order_by([Book::author.asc()])
        .values_list::<(i64, i64), _>(["author_id", "books"])
        .await
        .unwrap();
    assert_eq!(busy, [(s.ann.id, 2), (s.dee.id, 2)]);
    let plan = Book::objects(&db)
        .project([Book::author.select()])
        .annotate("books", Count::all())
        .filter(Expr::col("books").gt(1_i64))
        .plan()
        .clone();
    assert!(plan.having.is_some());
    assert_eq!(plan.grouping.len(), 1);
}

#[tokio::test]
async fn grouping_across_a_related_field_adds_the_join() {
    let db = db().await;
    seed(&db).await;
    let rows = Book::objects(&db)
        .project([Book::author.join(Author::name).select()])
        .annotate("total", Sum::of(Book::likes))
        .order_by([Book::author.join(Author::name).asc()])
        .rows()
        .await
        .unwrap();
    let names: Vec<String> = rows
        .iter()
        .map(|r| r.get_as("author__name").unwrap())
        .collect();
    assert_eq!(names, ["Ann", "Bob", "Dee"]);
    assert_eq!(rows[0].get_as::<i64>("total").unwrap(), 15);
}

#[tokio::test]
async fn annotating_models_with_aggregates_keeps_them_decodable() {
    let db = db().await;
    seed(&db).await;
    let rows = Author::objects(&db)
        .annotate("oldest", Max::of(Author::age))
        .order_by([Author::name.asc()])
        .all_annotated()
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        4,
        "grouped by every model column: one row per author"
    );
    assert_eq!(rows[0].1.get_as::<Option<i64>>("oldest").unwrap(), Some(30));
    assert_eq!(rows[2].1.get_as::<Option<i64>>("oldest").unwrap(), None);
}

#[tokio::test]
async fn string_agg_uses_group_concat_on_sqlite() {
    let db = db().await;
    seed(&db).await;
    let row = Book::objects(&db)
        .filter(Book::author.eq(1_i64))
        .order_by([Book::id.asc()])
        .aggregate([("titles", StringAgg::of(Book::title, ", "))])
        .await
        .unwrap();
    assert_eq!(row.get_as::<String>("titles").unwrap(), "Rust, Async");
}
