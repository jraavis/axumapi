use super::*;
use crate::sql::{Postgres, Sqlite};
use axumapi_orm::expr::Field;
use axumapi_orm::{BackendCapabilityError, Expr, LockMode, OrderDirection::*};

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
