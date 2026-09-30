//! ORM: plan construction, SQL compilation and a 100-row SQLite fetch.
#![allow(clippy::unwrap_used, missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use siderite::orm::Db;
use siderite::prelude::*;
use siderite_backends::sql::{Postgres, Sqlite, compile};
use siderite_backends::sqlite::SqliteBackend;
use std::hint::black_box;

/// Row type for every ORM benchmark.
#[derive(Debug, Clone, PartialEq, Model)]
#[model(table = "widgets", ordering = ["id"])]
struct Widget {
    #[field(primary_key, auto)]
    id: i64,
    name: String,
    price: i64,
    active: bool,
}

const SCHEMA: &str = "CREATE TABLE widgets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    price INTEGER NOT NULL,
    active INTEGER NOT NULL
);";

fn build(db: &Db) -> QuerySet<Widget> {
    Widget::objects(db)
        .filter(Widget::active.eq(true))
        .filter(Widget::price.gt(10_i64))
        .order_by([Widget::price.desc()])
        .limit(50)
}

async fn seeded_db(rows: i64) -> Db {
    let db = Db::new(SqliteBackend::connect("sqlite::memory:").await.unwrap());
    db.execute_script(SCHEMA).await.unwrap();
    let widgets = (0..rows)
        .map(|i| Widget {
            id: 0,
            name: format!("widget-{i}"),
            price: i,
            active: true,
        })
        .collect();
    Widget::objects(&db).bulk_create(widgets).await.unwrap();
    db
}

fn bench_orm(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let db = runtime.block_on(seeded_db(100));

    let fetched = runtime.block_on(Widget::objects(&db).all()).unwrap();
    assert_eq!(fetched.len(), 100);

    let mut group = c.benchmark_group("orm");
    group.bench_function("queryset_build", |b| b.iter(|| black_box(build(&db))));

    let plan = build(&db).plan().clone();
    group.bench_function("compile/sqlite", |b| {
        b.iter(|| compile(black_box(&plan), &Sqlite).unwrap());
    });
    group.bench_function("compile/postgres", |b| {
        b.iter(|| compile(black_box(&plan), &Postgres).unwrap());
    });
    group.bench_function("sqlite_fetch_100_rows", |b| {
        b.to_async(&runtime)
            .iter(|| async { Widget::objects(&db).all().await.unwrap() });
    });
    group.finish();
}

criterion_group!(benches, bench_orm);
criterion_main!(benches);
