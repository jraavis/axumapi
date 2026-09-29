//! Compiler output for the MySQL dialect.

use super::*;
use crate::sql::MySql;
use axumapi_orm::expr::Field;
use axumapi_orm::functions::{concat, length, substr};
use axumapi_orm::{
    BackendCapabilityError, Count, DeletePlan, InsertPlan, LockMode, OrderDirection::*, StdDev,
    StringAgg, Sum, UpdatePlan, Variance, WritePlan,
};

struct Post;
#[allow(non_upper_case_globals)]
impl Post {
    const title: Field<Post, String> = Field::new("title");
    const likes: Field<Post, i64> = Field::new("likes");
    const at: Field<Post, chrono::DateTime<chrono::Utc>> = Field::new("at");
}

fn my(p: &QueryPlan) -> CompiledQuery {
    compile(p, &MySql).unwrap()
}
fn posts() -> QueryPlan {
    QueryPlan::from_table("posts")
}

#[test]
fn backtick_identifiers_and_question_mark_placeholders() {
    let q = my(&posts()
        .filter(Post::likes.ge(10_i64))
        .filter(Post::title.eq("x")));
    assert_eq!(
        q.sql,
        "SELECT * FROM `posts` WHERE ((`likes` >= ?) AND (`title` = ?))"
    );
    assert_eq!(q.params, vec![Value::Int(10), Value::Text("x".into())]);
    let mut quoted = String::new();
    crate::sql::Dialect::write_ident(&MySql, &mut quoted, "we`ird");
    assert_eq!(quoted, "`we``ird`");
}

#[test]
fn offset_without_limit_uses_the_unbounded_limit() {
    let q = my(&posts().order_by(Post::likes, Desc).offset(5));
    assert_eq!(
        q.sql,
        "SELECT * FROM `posts` ORDER BY `likes` DESC LIMIT 18446744073709551615 OFFSET 5"
    );
    assert_eq!(
        my(&posts().limit(2).offset(5)).sql,
        "SELECT * FROM `posts` LIMIT 2 OFFSET 5"
    );
}

#[test]
fn text_lookups_are_case_sensitive_or_lowered() {
    let p = posts().filter(Post::title.contains("50%"));
    assert_eq!(
        my(&p).sql,
        r"SELECT * FROM `posts` WHERE (`title` LIKE CAST(? AS BINARY) ESCAPE '\\')"
    );
    assert_eq!(my(&p).params, vec![Value::Text(r"%50\%%".into())]);
    let ci = posts().filter(Post::title.icontains("a"));
    assert_eq!(
        my(&ci).sql,
        r"SELECT * FROM `posts` WHERE (LOWER(`title`) LIKE LOWER(?) ESCAPE '\\')"
    );
    let re = posts().filter(Post::title.regex("^a"));
    assert_eq!(
        my(&re).sql,
        "SELECT * FROM `posts` WHERE REGEXP_LIKE(`title`, ?, 'c')"
    );
}

#[test]
fn functions_and_casts_use_mysql_names() {
    let p = posts().select(concat([Post::title.expr(), Expr::val("-")]), Some("joined"));
    assert_eq!(
        my(&p).sql,
        "SELECT CONCAT(COALESCE(`title`, ''), COALESCE(?, '')) AS `joined` FROM `posts`"
    );
    let p = posts()
        .select(length(Post::title.expr()), Some("len"))
        .select(substr(Post::title.expr(), 2, Some(3)), Some("sub"));
    assert_eq!(
        my(&p).sql,
        "SELECT CHAR_LENGTH(`title`) AS `len`, SUBSTR(`title`, CAST(? AS SIGNED), CAST(? AS SIGNED)) AS `sub` FROM `posts`"
    );
    let cast = |ty| {
        my(&posts().select(
            Expr::Cast {
                expr: Box::new(Post::likes.expr()),
                ty,
            },
            None,
        ))
        .sql
    };
    assert!(cast(axumapi_orm::SqlType::BigInt).contains("CAST(`likes` AS SIGNED)"));
    assert!(cast(axumapi_orm::SqlType::Decimal).contains("AS DECIMAL(38,10))"));
    assert!(cast(axumapi_orm::SqlType::Timestamp).contains("AS DATETIME(6))"));
    assert!(cast(axumapi_orm::SqlType::Text).contains("AS CHAR)"));
    assert!(cast(axumapi_orm::SqlType::Uuid).contains("AS CHAR(36))"));
}

#[test]
fn date_parts() {
    let p = posts()
        .select(Post::at.year(), Some("y"))
        .select(Post::at.week(), Some("w"))
        .select(Post::at.quarter(), Some("q"))
        .select(Post::at.second(), Some("s"))
        .select(Post::at.date(), Some("d"));
    assert_eq!(
        my(&p).sql,
        "SELECT YEAR(`at`) AS `y`, WEEK(`at`, 3) AS `w`, QUARTER(`at`) AS `q`, SECOND(`at`) AS `s`, DATE(`at`) AS `d` FROM `posts`"
    );
}

#[test]
fn aggregates_rewrite_filter_and_string_agg() {
    let p = posts()
        .select(Count::all().filter(Post::likes.gt(1_i64)), Some("n"))
        .select(
            Sum::of(Post::likes).filter(Post::likes.gt(1_i64)),
            Some("s"),
        )
        .select(StringAgg::of(Post::title, r"'\,").distinct(), Some("t"))
        .select(StdDev::sample(Post::likes), Some("sd"))
        .select(Variance::population(Post::likes), Some("v"));
    assert_eq!(
        my(&p).sql,
        "SELECT COUNT(CASE WHEN (`likes` > ?) THEN 1 END) AS `n`, \
         SUM(CASE WHEN (`likes` > ?) THEN `likes` END) AS `s`, \
         GROUP_CONCAT(DISTINCT `title` SEPARATOR '''\\\\,') AS `t`, \
         STDDEV_SAMP(`likes`) AS `sd`, VAR_POP(`likes`) AS `v` FROM `posts`"
    );
    // The other dialects keep FILTER.
    let filtered = posts().select(Count::all().filter(Post::likes.gt(1_i64)), Some("n"));
    assert!(pg_sql(&filtered).contains("FILTER (WHERE"));
}

fn pg_sql(p: &QueryPlan) -> String {
    compile(p, &crate::sql::Postgres).unwrap().sql
}

#[test]
fn limited_subqueries_are_derived_tables() {
    let top = posts()
        .order_by(Post::likes, Desc)
        .limit(3)
        .select(Expr::col("id"), None);
    let p = QueryPlan::from_table("authors").filter(Expr::col("id").in_subquery(top.clone()));
    assert_eq!(
        my(&p).sql,
        "SELECT * FROM `authors` WHERE (`id` IN (SELECT * FROM (SELECT `id` FROM `posts` ORDER BY `likes` DESC LIMIT 3) AS `u1`))"
    );
    // Without LIMIT the subquery stays inline.
    let plain = QueryPlan::from_table("authors")
        .filter(Expr::col("id").in_subquery(posts().select(Expr::col("id"), None)));
    assert_eq!(
        my(&plain).sql,
        "SELECT * FROM `authors` WHERE (`id` IN (SELECT `id` FROM `posts`))"
    );
}

#[test]
fn writes_over_the_target_table_go_through_a_derived_table() {
    let keys = posts()
        .filter(Post::likes.lt(2_i64))
        .select(Expr::col("id"), None);
    let delete = WritePlan::Delete(DeletePlan {
        table: "posts".into(),
        filter: Some(Expr::col("id").in_subquery(keys.clone())),
        returning: vec![],
    });
    assert_eq!(
        compile_write(&delete, &MySql).unwrap().sql,
        "DELETE FROM `posts` WHERE (`id` IN (SELECT * FROM (SELECT `id` FROM `posts` WHERE (`likes` < ?)) AS `u1`))"
    );
    let update = WritePlan::Update(UpdatePlan {
        table: "posts".into(),
        assignments: vec![(
            "likes".into(),
            Expr::subquery(posts().select(axumapi_orm::Max::of(Post::likes), None)),
        )],
        filter: None,
        returning: vec![],
    });
    assert_eq!(
        compile_write(&update, &MySql).unwrap().sql,
        "UPDATE `posts` SET `likes` = (SELECT * FROM (SELECT MAX(`likes`) FROM `posts`) AS `u1`)"
    );
    // A subquery over another table is left alone.
    let other = WritePlan::Delete(DeletePlan {
        table: "authors".into(),
        filter: Some(Expr::col("id").in_subquery(keys)),
        returning: vec![],
    });
    assert!(
        compile_write(&other, &MySql)
            .unwrap()
            .sql
            .contains("IN (SELECT * FROM (SELECT `id` FROM `posts`")
    );
}

#[test]
fn update_assignments_keep_standard_old_value_semantics() {
    let update = |assignments: Vec<(&'static str, Expr)>| {
        compile_write(
            &WritePlan::Update(UpdatePlan {
                table: "posts".into(),
                assignments: assignments
                    .into_iter()
                    .map(|(c, e)| (c.into(), e))
                    .collect(),
                filter: None,
                returning: vec![],
            }),
            &MySql,
        )
    };
    // `pages` reads `likes`, so it must be assigned before `likes` changes.
    let q = update(vec![
        ("likes", Expr::col("likes") * 10_i64),
        ("pages", Expr::col("likes")),
    ])
    .unwrap();
    assert_eq!(
        q.sql,
        "UPDATE `posts` SET `pages` = `likes`, `likes` = (`likes` * ?)"
    );
    // Independent assignments keep their order.
    let q = update(vec![("a", Expr::val(1)), ("b", Expr::val(2))]).unwrap();
    assert_eq!(q.sql, "UPDATE `posts` SET `a` = ?, `b` = ?");
    // A cycle cannot be expressed.
    let err = update(vec![("a", Expr::col("b")), ("b", Expr::col("a"))]);
    assert!(matches!(
        err,
        Err(OrmError::Query(QueryError::InvalidPlan(_)))
    ));
    // PostgreSQL and SQLite do not reorder.
    let pg = compile_write(
        &WritePlan::Update(UpdatePlan {
            table: "posts".into(),
            assignments: vec![
                ("likes".into(), Expr::col("likes") * 10_i64),
                ("pages".into(), Expr::col("likes")),
            ],
            filter: None,
            returning: vec![],
        }),
        &crate::sql::Postgres,
    )
    .unwrap();
    assert!(pg.sql.starts_with(r#"UPDATE "posts" SET "likes" = "#));
}

#[test]
fn default_values_insert_and_returning() {
    let insert = |returning: Vec<axumapi_orm::expr::Ident>| {
        WritePlan::Insert(InsertPlan {
            table: "posts".into(),
            columns: vec![],
            rows: vec![vec![]],
            returning,
        })
    };
    assert_eq!(
        compile_write(&insert(vec![]), &MySql).unwrap().sql,
        "INSERT INTO `posts` () VALUES ()"
    );
    // The dialect has no RETURNING: the adapter strips and emulates it.
    let err = compile_write(&insert(vec!["id".into()]), &MySql);
    assert!(matches!(err, Err(OrmError::Capability(_))));
}

#[test]
fn locks_sets_and_capability_failures() {
    let mut locked = posts();
    locked.lock = Some(LockMode::ForUpdateSkipLocked);
    assert_eq!(
        my(&locked).sql,
        "SELECT * FROM `posts` FOR UPDATE SKIP LOCKED"
    );
    let inter = posts().filter(Post::likes.gt(1_i64));
    let mut both = inter.clone();
    both.compound.push(axumapi_orm::Compound {
        op: axumapi_orm::SetOp::Intersect,
        plan: posts().filter(Post::likes.lt(9_i64)),
    });
    assert!(my(&both).sql.contains(" INTERSECT SELECT "));
    let distinct_on = posts().distinct(axumapi_orm::DistinctMode::On(vec![Expr::col("a")]));
    assert!(matches!(
        compile(&distinct_on, &MySql),
        Err(OrmError::Capability(
            BackendCapabilityError::Unsupported { .. }
        ))
    ));
    let array = posts().select(axumapi_orm::ArrayAgg::of(Post::title), None);
    assert!(matches!(
        compile(&array, &MySql),
        Err(OrmError::Capability(_))
    ));
}
