use super::*;
use crate::sql::{Postgres, Sqlite};
use siderite_orm::expr::Field;
use siderite_orm::{BackendCapabilityError, Expr, LockMode, OrderDirection::*};

struct Post;
#[allow(non_upper_case_globals)]
impl Post {
    const title: Field<Post, String> = Field::new("title");
    const likes: Field<Post, i64> = Field::new("likes");
    const dislikes: Field<Post, i64> = Field::new("dislikes");
}

fn pg(p: &QueryPlan) -> CompiledQuery {
    compile(p, &Postgres).unwrap()
}
fn lite(p: &QueryPlan) -> CompiledQuery {
    compile(p, &Sqlite).unwrap()
}
fn posts() -> QueryPlan {
    QueryPlan::from_table("posts")
}

#[test]
fn comparison_and_placeholders() {
    let p = posts()
        .filter(Post::likes.ge(10_i64))
        .filter(Post::title.eq("x"));
    let q = pg(&p);
    assert_eq!(
        q.sql,
        r#"SELECT * FROM "posts" WHERE (("likes" >= $1) AND ("title" = $2))"#
    );
    assert_eq!(q.params, vec![Value::Int(10), Value::Text("x".into())]);
    assert_eq!(
        lite(&p).sql,
        r#"SELECT * FROM "posts" WHERE (("likes" >= ?) AND ("title" = ?))"#
    );
}

#[test]
fn f_expressions_and_arithmetic() {
    let p = posts().filter(Post::likes.expr().gt(Post::dislikes.expr() * 2_i64));
    assert_eq!(
        pg(&p).sql,
        r#"SELECT * FROM "posts" WHERE ("likes" > ("dislikes" * $1))"#
    );
}

#[test]
fn string_lookups_escape_wildcards() {
    let p = posts().filter(Post::title.icontains("50%_off"));
    let q = pg(&p);
    assert_eq!(
        q.sql,
        r#"SELECT * FROM "posts" WHERE ("title" ILIKE $1 ESCAPE '\')"#
    );
    assert_eq!(q.params, vec![Value::Text(r"%50\%\_off%".into())]);
    let q = lite(&p);
    assert_eq!(
        q.sql,
        r#"SELECT * FROM "posts" WHERE (LOWER("title") LIKE LOWER(?) ESCAPE '\')"#
    );
}

#[test]
fn sqlite_case_sensitive_lookups_avoid_like() {
    let q = lite(&posts().filter(Post::title.contains("Rust")));
    assert_eq!(
        q.sql,
        r#"SELECT * FROM "posts" WHERE (instr("title", ?) > 0)"#
    );
    let q = lite(&posts().filter(Post::title.starts_with("Ab")));
    assert_eq!(
        q.sql,
        r#"SELECT * FROM "posts" WHERE (substr("title", 1, 2) = ?)"#
    );
    let q = lite(&posts().filter(Post::title.ends_with("é!")));
    assert_eq!(
        q.sql,
        r#"SELECT * FROM "posts" WHERE (substr("title", -2) = ?)"#
    );
    let q = pg(&posts().filter(Post::title.starts_with("Ab")));
    assert_eq!(q.params, vec![Value::Text("Ab%".into())]);
}

#[test]
fn null_in_range_boolean() {
    let p = posts().filter(
        Post::likes
            .is_in([1_i64, 2])
            .or(Post::likes.range(5_i64, 9_i64))
            .or(!Post::title.is_null()),
    );
    assert_eq!(
        pg(&p).sql,
        r#"SELECT * FROM "posts" WHERE (("likes" IN ($1, $2)) OR ("likes" BETWEEN $3 AND $4) OR (NOT ("title" IS NULL)))"#
    );
    let empty = posts().filter(Post::likes.is_in(Vec::<i64>::new()));
    assert_eq!(pg(&empty).sql, r#"SELECT * FROM "posts" WHERE (1=0)"#);
}

#[test]
fn ordering_pagination_and_sqlite_offset() {
    let p = posts()
        .order_by(Post::likes, Desc)
        .order_by(Post::title, Asc)
        .offset(20);
    assert_eq!(
        pg(&p).sql,
        r#"SELECT * FROM "posts" ORDER BY "likes" DESC, "title" ASC OFFSET 20"#
    );
    assert_eq!(
        lite(&p).sql,
        r#"SELECT * FROM "posts" ORDER BY "likes" DESC, "title" ASC LIMIT -1 OFFSET 20"#
    );
    assert_eq!(
        pg(&posts().limit(10).offset(5)).sql,
        r#"SELECT * FROM "posts" LIMIT 10 OFFSET 5"#
    );
}

#[test]
fn subquery_parameters_are_numbered_globally() {
    let sub = QueryPlan::from_table("comments")
        .select(Expr::col("post_id"), None)
        .filter(Expr::col("spam").eq(true));
    let p = posts()
        .filter(Post::likes.gt(1_i64))
        .filter(Expr::Exists(Box::new(sub)));
    assert_eq!(
        pg(&p).sql,
        r#"SELECT * FROM "posts" WHERE (("likes" > $1) AND EXISTS (SELECT "post_id" FROM "comments" WHERE ("spam" = $2)))"#
    );
}

#[test]
fn identifiers_are_quoted_safely() {
    let q = pg(&QueryPlan::from_table(r#"we"ird"#));
    assert_eq!(q.sql, r#"SELECT * FROM "we""ird""#);
}

#[test]
fn capability_failures_are_explicit() {
    let locked = posts().lock(LockMode::ForUpdate);
    assert!(matches!(
        compile(&locked, &Sqlite),
        Err(OrmError::Capability(
            BackendCapabilityError::RowLockingUnsupported { .. }
        ))
    ));
    assert_eq!(pg(&locked).sql, r#"SELECT * FROM "posts" FOR UPDATE"#);
    assert!(compile(&posts().filter(Post::title.regex("^a")), &Sqlite).is_err());
    assert_eq!(
        pg(&posts().filter(Post::title.regex("^a"))).sql,
        r#"SELECT * FROM "posts" WHERE ("title" ~ $1)"#
    );
}

mod dsl {
    use super::*;
    use siderite_orm::expr::functions::*;
    use siderite_orm::{
        ArrayAgg, Avg, Count, DatePart, Lag, Max, Ntile, RowNumber, SetOp, SqlType, StdDev,
        StringAgg, Sum,
    };

    #[test]
    fn scalar_functions() {
        let p = posts().filter(
            lower(Post::title.expr())
                .eq("a")
                .and(length(trim(Expr::col("title"))).gt(3_i64))
                .and(substr(Expr::col("title"), 2, Some(3)).eq("bcd"))
                .and(
                    replace(Expr::col("title"), "a", "b")
                        .ne(coalesce([Expr::col("x"), Expr::val("y")])),
                ),
        );
        assert_eq!(
            pg(&p).sql,
            r#"SELECT * FROM "posts" WHERE ((LOWER("title") = $1) AND (LENGTH(TRIM("title")) > $2) AND (SUBSTR("title", CAST($3 AS INTEGER), CAST($4 AS INTEGER)) = $5) AND (REPLACE("title", $6, $7) <> COALESCE("x", $8)))"#
        );
        assert_eq!(lite(&p).params.len(), 8);
    }

    #[test]
    fn concat_differs_by_dialect_but_treats_null_as_empty() {
        let p = posts().select(concat([Expr::col("a"), Expr::col("b")]), Some("ab"));
        assert_eq!(
            pg(&p).sql,
            r#"SELECT CONCAT("a", "b") AS "ab" FROM "posts""#
        );
        assert_eq!(
            lite(&p).sql,
            r#"SELECT (COALESCE(CAST("a" AS TEXT), '') || COALESCE(CAST("b" AS TEXT), '')) AS "ab" FROM "posts""#
        );
    }

    #[test]
    fn cast_uses_dialect_type_names() {
        let p = posts().select(cast(Expr::col("x"), SqlType::Timestamp), None);
        assert_eq!(
            pg(&p).sql,
            r#"SELECT CAST("x" AS TIMESTAMPTZ) FROM "posts""#
        );
        assert_eq!(lite(&p).sql, r#"SELECT CAST("x" AS TEXT) FROM "posts""#);
        let p = posts().select(Post::likes.cast(SqlType::Decimal), None);
        assert_eq!(
            lite(&p).sql,
            r#"SELECT CAST("likes" AS NUMERIC) FROM "posts""#
        );
    }

    #[test]
    fn case_expression() {
        let heat = case()
            .when(Post::likes.gt(10_i64), "hot")
            .when(Post::likes.gt(5_i64), "warm")
            .otherwise("cold");
        let p = posts().select(heat, Some("heat"));
        assert_eq!(
            pg(&p).sql,
            r#"SELECT CASE WHEN ("likes" > $1) THEN $2 WHEN ("likes" > $3) THEN $4 ELSE $5 END AS "heat" FROM "posts""#
        );
        let open = case().when(Expr::col("a").eq(1), 2).end();
        assert_eq!(
            lite(&posts().select(open, None)).sql,
            r#"SELECT CASE WHEN ("a" = ?) THEN ? END FROM "posts""#
        );
        assert!(compile(&posts().select(case().end(), None), &Sqlite).is_err());
    }

    #[test]
    fn equality_with_null_becomes_is_null() {
        let p = posts().filter(Expr::col("a").eq(Expr::Value(Value::Null)));
        assert_eq!(pg(&p).sql, r#"SELECT * FROM "posts" WHERE ("a" IS NULL)"#);
        let p = posts().filter(Expr::col("a").ne(Expr::Value(Value::Null)));
        assert_eq!(
            pg(&p).sql,
            r#"SELECT * FROM "posts" WHERE ("a" IS NOT NULL)"#
        );
        assert!(pg(&p).params.is_empty());
    }

    #[test]
    fn aggregates_group_and_filter() {
        let p = posts()
            .select(Expr::col("author_id"), None)
            .select(Count::all(), Some("n"))
            .select(Sum::of(Post::likes).distinct(), Some("total"))
            .select(
                Max::of(Post::likes).filter(Post::title.eq("x")),
                Some("best"),
            )
            .select(Avg::of(Post::likes), Some("mean"));
        let mut p = p;
        p.grouping = vec![Expr::col("author_id")];
        p.having = Some(Expr::from(Count::all()).gt(1_i64));
        let expected = r#"SELECT "author_id", COUNT(*) AS "n", SUM(DISTINCT "likes") AS "total", MAX("likes") FILTER (WHERE ("title" = $1)) AS "best", AVG("likes") AS "mean" FROM "posts" GROUP BY "author_id" HAVING (COUNT(*) > $2)"#;
        assert_eq!(pg(&p).sql, expected);
        assert_eq!(lite(&p).sql, expected.replace("$1", "?").replace("$2", "?"));
    }

    #[test]
    fn statistical_and_array_aggregates_are_postgres_only() {
        let p = posts().select(StdDev::sample(Post::likes), None);
        assert_eq!(pg(&p).sql, r#"SELECT STDDEV_SAMP("likes") FROM "posts""#);
        assert!(compile(&p, &Sqlite).is_err());
        let p = posts().select(ArrayAgg::of(Post::title), None);
        assert_eq!(pg(&p).sql, r#"SELECT ARRAY_AGG("title") FROM "posts""#);
        assert!(compile(&p, &Sqlite).is_err());
    }

    #[test]
    fn string_agg_binds_its_separator() {
        let p = posts().select(StringAgg::of(Post::title, ", "), Some("all"));
        let q = pg(&p);
        assert_eq!(
            q.sql,
            r#"SELECT STRING_AGG("title", $1) AS "all" FROM "posts""#
        );
        assert_eq!(q.params, vec![Value::Text(", ".into())]);
        assert_eq!(
            lite(&p).sql,
            r#"SELECT GROUP_CONCAT("title", ?) AS "all" FROM "posts""#
        );
        let distinct = posts().select(StringAgg::of(Post::title, ",").distinct(), None);
        assert!(compile(&distinct, &Sqlite).is_err());
        assert!(compile(&distinct, &Postgres).is_ok());
    }

    #[test]
    fn window_functions() {
        let rank = RowNumber::new()
            .partition_by([Expr::col("author_id")])
            .order_by([Post::likes.desc()]);
        let running =
            siderite_orm::Window::over(siderite_orm::WindowFunc::Aggregate(Sum::of(Post::likes)))
                .order_by([Expr::col("id").asc()]);
        let p = posts()
            .select(rank, Some("rn"))
            .select(Lag::or_default(Post::likes, 1, 0_i64), Some("prev"))
            .select(Ntile::new(4).order_by([Post::likes.asc()]), Some("q"))
            .select(running, Some("run"));
        let expected = r#"SELECT ROW_NUMBER() OVER (PARTITION BY "author_id" ORDER BY "likes" DESC) AS "rn", LAG("likes", 1, $1) OVER () AS "prev", NTILE(4) OVER (ORDER BY "likes" ASC) AS "q", SUM("likes") OVER (ORDER BY "id" ASC) AS "run" FROM "posts""#;
        assert_eq!(pg(&p).sql, expected);
        assert_eq!(lite(&p).sql, expected.replace("$1", "?"));
    }

    #[test]
    fn date_parts_per_dialect() {
        let at = || Expr::col("at");
        let cases = [
            (
                DatePart::Year,
                r#"CAST(EXTRACT(YEAR FROM "at") AS BIGINT)"#,
                r#"CAST(strftime('%Y', "at") AS INTEGER)"#,
            ),
            (
                DatePart::Month,
                r#"CAST(EXTRACT(MONTH FROM "at") AS BIGINT)"#,
                r#"CAST(strftime('%m', "at") AS INTEGER)"#,
            ),
            (
                DatePart::Day,
                r#"CAST(EXTRACT(DAY FROM "at") AS BIGINT)"#,
                r#"CAST(strftime('%d', "at") AS INTEGER)"#,
            ),
            (
                DatePart::Hour,
                r#"CAST(EXTRACT(HOUR FROM "at") AS BIGINT)"#,
                r#"CAST(strftime('%H', "at") AS INTEGER)"#,
            ),
            (
                DatePart::Minute,
                r#"CAST(EXTRACT(MINUTE FROM "at") AS BIGINT)"#,
                r#"CAST(strftime('%M', "at") AS INTEGER)"#,
            ),
            (
                DatePart::Second,
                r#"CAST(FLOOR(EXTRACT(SECOND FROM "at")) AS BIGINT)"#,
                r#"CAST(strftime('%S', "at") AS INTEGER)"#,
            ),
            (
                DatePart::Quarter,
                r#"CAST(EXTRACT(QUARTER FROM "at") AS BIGINT)"#,
                r#"((CAST(strftime('%m', "at") AS INTEGER) + 2) / 3)"#,
            ),
            (
                DatePart::Week,
                r#"CAST(EXTRACT(WEEK FROM "at") AS BIGINT)"#,
                r#"((CAST(strftime('%j', date("at", '-3 days', 'weekday 4')) AS INTEGER) - 1) / 7 + 1)"#,
            ),
            (DatePart::Date, r#"CAST("at" AS DATE)"#, r#"date("at")"#),
        ];
        for (part, pg_sql, lite_sql) in cases {
            let p = posts().select(at().date_part(part), None);
            assert_eq!(
                pg(&p).sql,
                format!(r#"SELECT {pg_sql} FROM "posts""#),
                "{part:?}"
            );
            assert_eq!(
                lite(&p).sql,
                format!(r#"SELECT {lite_sql} FROM "posts""#),
                "{part:?}"
            );
        }
        let p = posts().filter(at().year().eq(2024_i64));
        assert_eq!(
            lite(&p).sql,
            r#"SELECT * FROM "posts" WHERE (CAST(strftime('%Y', "at") AS INTEGER) = ?)"#
        );
    }

    #[test]
    fn correlated_subqueries_and_in_subquery() {
        let inner = QueryPlan::from_table("comments")
            .select(Expr::col("post_id"), None)
            .filter(Expr::col("post_id").eq(Expr::outer("id")));
        let p = posts().filter(Expr::exists(inner.clone()));
        assert_eq!(
            pg(&p).sql,
            r#"SELECT * FROM "posts" WHERE EXISTS (SELECT "post_id" FROM "comments" WHERE ("post_id" = "posts"."id"))"#
        );
        let p = posts().filter(Post::likes.in_subquery(inner.clone()));
        assert_eq!(
            lite(&p).sql,
            r#"SELECT * FROM "posts" WHERE ("likes" IN (SELECT "post_id" FROM "comments" WHERE ("post_id" = "posts"."id")))"#
        );
        let scalar = posts().select(Expr::subquery(inner), Some("first_comment"));
        assert!(pg(&scalar).sql.starts_with(r#"SELECT (SELECT "post_id""#));
        // An aliased self-join subquery keeps inner and outer names apart.
        let mut same =
            QueryPlan::from_table("posts").filter(Expr::col("likes").gt(Expr::outer("likes")));
        same.source.alias = Some("p2".into());
        let p = posts().filter(Expr::exists(same));
        assert_eq!(
            pg(&p).sql,
            r#"SELECT * FROM "posts" WHERE EXISTS (SELECT * FROM "posts" AS "p2" WHERE ("likes" > "posts"."likes"))"#
        );
        let err = compile(&posts().filter(Expr::outer("x").eq(1)), &Postgres);
        assert!(matches!(
            err,
            Err(OrmError::Query(QueryError::InvalidPlan(_)))
        ));
    }

    #[test]
    fn arity_and_unresolved_relations_are_rejected() {
        let bad = Expr::Func {
            func: siderite_orm::Function::Lower,
            args: vec![],
        };
        assert!(compile(&posts().filter(bad.eq("x")), &Postgres).is_err());
        let related = Expr::Related(siderite_orm::RelatedColumn {
            path: vec![],
            column: "x".into(),
        });
        assert!(compile(&posts().filter(related.eq(1)), &Postgres).is_err());
    }

    #[test]
    fn joins_qualify_columns_and_project_the_root_explicitly() {
        let p = posts().filter(Post::likes.gt(1_i64)).join(
            siderite_orm::JoinKind::Left,
            siderite_orm::QuerySource {
                name: "authors".into(),
                alias: Some("author".into()),
                subquery: None,
            },
            Expr::Column(siderite_orm::Column::qualified("posts", "author_id")).eq(Expr::Column(
                siderite_orm::Column::qualified("author", "id"),
            )),
        );
        assert_eq!(
            pg(&p).sql,
            r#"SELECT "posts".* FROM "posts" LEFT JOIN "authors" AS "author" ON ("posts"."author_id" = "author"."id") WHERE ("posts"."likes" > $1)"#
        );
    }

    #[test]
    fn related_columns_resolve_into_deduplicated_left_joins() {
        use siderite_orm::{RelHop, RelatedColumn};
        let author = || RelHop {
            fk_column: "author_id".into(),
            table: "authors".into(),
            pk_column: "id".into(),
        };
        let team = RelHop {
            fk_column: "team_id".into(),
            table: "teams".into(),
            pk_column: "id".into(),
        };
        let name = Expr::Related(RelatedColumn {
            path: vec![author()],
            column: "name".into(),
        });
        let team_name = Expr::Related(RelatedColumn {
            path: vec![author(), team],
            column: "name".into(),
        });
        let p = posts()
            .filter(name.clone().eq("Ann"))
            .filter(team_name.eq("Red"))
            .order_by(name, Asc)
            .resolve_relations();
        assert_eq!(p.joins.len(), 2, "the author join is shared");
        assert_eq!(
            pg(&p).sql,
            r#"SELECT "posts".* FROM "posts" LEFT JOIN "authors" AS "author" ON ("posts"."author_id" = "author"."id") LEFT JOIN "teams" AS "author__team" ON ("author"."team_id" = "author__team"."id") WHERE (("author"."name" = $1) AND ("author__team"."name" = $2)) ORDER BY "author"."name" ASC"#
        );
        assert_eq!(p.clone().resolve_relations(), p, "idempotent");
    }

    #[test]
    fn set_operations_wrap_members_that_need_it() {
        let a = posts()
            .select(Expr::col("id"), None)
            .filter(Expr::col("x").eq(1));
        let b = posts()
            .select(Expr::col("id"), None)
            .filter(Expr::col("x").eq(2));
        let mut u = a.clone();
        u.compound.push(siderite_orm::Compound {
            op: SetOp::Union,
            plan: b.clone(),
        });
        u = u.order_by(Expr::col("id"), Desc).limit(5);
        assert_eq!(
            pg(&u).sql,
            r#"SELECT "id" FROM "posts" WHERE ("x" = $1) UNION SELECT "id" FROM "posts" WHERE ("x" = $2) ORDER BY "id" DESC LIMIT 5"#
        );
        let mut limited = b;
        limited.limit = Some(1);
        let mut e = a;
        e.compound.push(siderite_orm::Compound {
            op: SetOp::Except,
            plan: limited,
        });
        let expected = r#"SELECT "id" FROM "posts" WHERE ("x" = ?) EXCEPT SELECT * FROM (SELECT "id" FROM "posts" WHERE ("x" = ?) LIMIT 1) AS "u1""#;
        assert_eq!(lite(&e).sql, expected);
        assert_eq!(
            pg(&e).sql,
            r#"SELECT "id" FROM "posts" WHERE ("x" = $1) EXCEPT SELECT * FROM (SELECT "id" FROM "posts" WHERE ("x" = $2) LIMIT 1) AS "u1""#
        );
    }

    #[test]
    fn derived_table_source() {
        let inner = posts().filter(Post::likes.gt(1_i64)).limit(10);
        let p = QueryPlan::from_subquery(inner, "counted").select(Count::all(), Some("count"));
        assert_eq!(
            pg(&p).sql,
            r#"SELECT COUNT(*) AS "count" FROM (SELECT * FROM "posts" WHERE ("likes" > $1) LIMIT 10) AS "counted""#
        );
    }

    #[test]
    fn null_values_are_spelled_out_not_bound() {
        use siderite_orm::{InsertPlan, UpdatePlan, WritePlan};
        let update = WritePlan::Update(UpdatePlan {
            table: "books".into(),
            assignments: vec![
                ("pages".into(), Expr::Value(Value::Null)),
                ("likes".into(), Expr::val(3)),
            ],
            filter: Some(Expr::col("id").eq(1)),
            returning: vec![],
        });
        let q = compile_write(&update, &Postgres).unwrap();
        assert_eq!(
            q.sql,
            r#"UPDATE "books" SET "pages" = NULL, "likes" = $1 WHERE ("id" = $2)"#
        );
        assert_eq!(q.params, vec![Value::Int(3), Value::Int(1)]);
        let insert = WritePlan::Insert(InsertPlan {
            table: "books".into(),
            columns: vec!["a".into(), "b".into()],
            rows: vec![
                vec![Value::Null, Value::Int(1)],
                vec![Value::Int(2), Value::Null],
            ],
            returning: vec![],
        });
        assert_eq!(
            compile_write(&insert, &Sqlite).unwrap().sql,
            r#"INSERT INTO "books" ("a", "b") VALUES (NULL, ?), (?, NULL)"#
        );
    }

    #[test]
    fn writes_can_correlate_subqueries_with_the_target_row() {
        use siderite_orm::{DeletePlan, UpdatePlan, WritePlan};
        let has_books = QueryPlan::from_table("books")
            .select(Expr::val(1), None)
            .filter(Expr::col("author_id").eq(Expr::outer("id")));
        let delete = WritePlan::Delete(DeletePlan {
            table: "authors".into(),
            filter: Some(!Expr::exists(has_books.clone())),
            returning: vec![],
        });
        let expected = r#"DELETE FROM "authors" WHERE (NOT EXISTS (SELECT $1 FROM "books" WHERE ("author_id" = "authors"."id")))"#;
        assert_eq!(compile_write(&delete, &Postgres).unwrap().sql, expected);
        assert_eq!(
            compile_write(&delete, &Sqlite).unwrap().sql,
            expected.replace("$1", "?")
        );
        let update = WritePlan::Update(UpdatePlan {
            table: "authors".into(),
            assignments: vec![("n".into(), Expr::subquery(has_books))],
            filter: None,
            returning: vec![],
        });
        assert!(
            compile_write(&update, &Postgres)
                .unwrap()
                .sql
                .starts_with(r#"UPDATE "authors" SET "n" = (SELECT"#)
        );
    }

    #[test]
    fn correlating_a_table_with_itself_needs_an_alias() {
        let same =
            QueryPlan::from_table("posts").filter(Expr::col("likes").gt(Expr::outer("likes")));
        let err = compile(&posts().filter(Expr::exists(same)), &Postgres);
        assert!(
            matches!(err, Err(OrmError::Query(QueryError::InvalidPlan(m))) if m.contains("aliased"))
        );
    }
}
