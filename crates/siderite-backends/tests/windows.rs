//! Window functions on SQLite.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Book, db, seed};
use siderite_orm::{
    CumeDist, DenseRank, Expr, FirstValue, Lag, LastValue, Lead, Model, Ntile, PercentRank, Rank,
    RowNumber, Sum, Window, WindowFunc,
};

async fn annotated(window: Window) -> Vec<(String, Option<i64>)> {
    let db = db().await;
    seed(&db).await;
    Book::objects(&db)
        .annotate("w", window)
        .order_by([Book::id.asc()])
        .all_annotated()
        .await
        .unwrap()
        .into_iter()
        .map(|(book, row)| (book.title, row.get_as::<Option<i64>>("w").unwrap()))
        .collect()
}

fn by_likes() -> Vec<siderite_orm::OrderExpr> {
    vec![Book::likes.desc()]
}

#[tokio::test]
async fn ranking_functions_respect_partition_and_order() {
    // Rust(10) Async(5) SQL(8) Go(1) Zig(3); authors: Ann, Ann, Bob, Dee, Dee.
    let ranks = annotated(
        RowNumber::new()
            .partition_by([Book::author])
            .order_by(by_likes()),
    )
    .await;
    let ranks: Vec<_> = ranks.into_iter().map(|(_, r)| r.unwrap()).collect();
    assert_eq!(ranks, [1, 2, 1, 2, 1]);

    let overall = annotated(Rank::new().order_by(by_likes())).await;
    assert_eq!(
        overall.iter().map(|(_, r)| r.unwrap()).collect::<Vec<_>>(),
        [1, 3, 2, 5, 4]
    );
    let dense = annotated(
        DenseRank::new()
            .partition_by([Book::author])
            .order_by(by_likes()),
    )
    .await;
    assert_eq!(
        dense.iter().map(|(_, r)| r.unwrap()).collect::<Vec<_>>(),
        [1, 2, 1, 2, 1]
    );
}

#[tokio::test]
async fn ntile_buckets_rows() {
    let tiles = annotated(Ntile::new(2).order_by(by_likes())).await;
    // Order by likes desc: Rust, SQL, Async | Zig, Go.
    let by_title: std::collections::HashMap<_, _> = tiles.into_iter().collect();
    assert_eq!(by_title["Rust"], Some(1));
    assert_eq!(by_title["Async"], Some(1));
    assert_eq!(by_title["Zig"], Some(2));
    assert_eq!(by_title["Go"], Some(2));
}

#[tokio::test]
async fn lag_lead_first_and_last_values() {
    let by_id = || vec![Book::id.asc()];
    let lag = annotated(Lag::new(Book::likes, 1).order_by(by_id())).await;
    assert_eq!(
        lag.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        [None, Some(10), Some(5), Some(8), Some(1)]
    );
    let lead = annotated(Lead::or_default(Book::likes, 1, -1_i64).order_by(by_id())).await;
    assert_eq!(
        lead.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        [Some(5), Some(8), Some(1), Some(3), Some(-1)]
    );
    let first = annotated(
        FirstValue::new(Book::likes)
            .partition_by([Book::author])
            .order_by(by_id()),
    )
    .await;
    assert_eq!(
        first.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        [Some(10), Some(10), Some(8), Some(1), Some(1)]
    );
    // Default frame: the last value is the current row's last peer.
    let last = annotated(LastValue::new(Book::likes).order_by(by_id())).await;
    assert_eq!(
        last.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        [Some(10), Some(5), Some(8), Some(1), Some(3)]
    );
}

#[tokio::test]
async fn aggregates_over_windows_give_running_totals() {
    let running =
        Window::over(WindowFunc::Aggregate(Sum::of(Book::likes))).order_by([Book::id.asc()]);
    let totals = annotated(running).await;
    assert_eq!(
        totals.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        [Some(10), Some(15), Some(23), Some(24), Some(27)]
    );
    let per_author =
        Window::over(WindowFunc::Aggregate(Sum::of(Book::likes))).partition_by([Book::author]);
    let totals = annotated(per_author).await;
    assert_eq!(
        totals.iter().map(|(_, v)| *v).collect::<Vec<_>>(),
        [Some(15), Some(15), Some(8), Some(4), Some(4)]
    );
}

#[tokio::test]
async fn distribution_functions_return_fractions() {
    let db = db().await;
    seed(&db).await;
    let rows = Book::objects(&db)
        .annotate("cume", CumeDist::new().order_by([Book::likes.asc()]))
        .annotate("pct", PercentRank::new().order_by([Book::likes.asc()]))
        .order_by([Book::likes.asc()])
        .all_annotated()
        .await
        .unwrap();
    let cume: Vec<f64> = rows
        .iter()
        .map(|(_, r)| r.get_as("cume").unwrap())
        .collect();
    let pct: Vec<f64> = rows.iter().map(|(_, r)| r.get_as("pct").unwrap()).collect();
    assert_eq!(cume, [0.2, 0.4, 0.6, 0.8, 1.0]);
    assert_eq!(pct, [0.0, 0.25, 0.5, 0.75, 1.0]);
}

#[tokio::test]
async fn windows_can_order_but_not_filter() {
    let db = db().await;
    seed(&db).await;
    let ordered = Book::objects(&db)
        .annotate(
            "rn",
            RowNumber::new()
                .partition_by([Book::author])
                .order_by(by_likes()),
        )
        .order_by([Expr::col("rn").asc(), Book::id.asc()])
        .all()
        .await
        .unwrap();
    assert_eq!(
        ordered.iter().map(|b| b.title.as_str()).collect::<Vec<_>>(),
        ["Rust", "SQL", "Zig", "Async", "Go"]
    );
}
