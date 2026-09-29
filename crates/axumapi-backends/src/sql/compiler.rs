//! `QueryPlan` → SQL compiler.

use super::dialect::Dialect;
use axumapi_orm::{
    BackendKind, BinaryOp, Column, DistinctMode, Expr, JoinKind, LockMode, Lookup, OrderDirection,
    OrmError, QueryPlan, QuerySource, UnaryOp, Value,
};
use std::fmt::Write;

/// Escape character used for `LIKE` patterns.
const LIKE_ESCAPE: char = '\\';

/// SQL text plus its bind parameters, in placeholder order.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledQuery {
    /// Parameterised SQL.
    pub sql: String,
    /// Parameters to bind, in order.
    pub params: Vec<Value>,
}

/// Compile `plan` for `dialect`.
///
/// # Errors
/// Returns [`OrmError::Capability`] if the plan needs a feature the dialect
/// does not declare.
pub fn compile(plan: &QueryPlan, dialect: &dyn Dialect) -> Result<CompiledQuery, OrmError> {
    let caps = dialect.capabilities();
    plan.check(&caps)?;
    let mut c = Compiler {
        dialect,
        native_ilike: caps.ilike,
        kind: caps.kind,
        sql: String::new(),
        params: Vec::new(),
    };
    c.plan(plan);
    Ok(CompiledQuery {
        sql: c.sql,
        params: c.params,
    })
}

struct Compiler<'d> {
    dialect: &'d dyn Dialect,
    native_ilike: bool,
    kind: BackendKind,
    sql: String,
    params: Vec<Value>,
}

/// Which part of a text value a pattern must match.
#[derive(Clone, Copy)]
enum Anchor {
    Anywhere,
    Start,
    End,
}

impl Compiler<'_> {
    fn push(&mut self, s: &str) {
        self.sql.push_str(s);
    }

    fn ident(&mut self, ident: &str) {
        self.dialect.write_ident(&mut self.sql, ident);
    }

    fn bind(&mut self, v: Value) {
        self.params.push(v);
        self.dialect
            .write_placeholder(&mut self.sql, self.params.len());
    }

    fn list<T>(&mut self, items: &[T], sep: &str, mut f: impl FnMut(&mut Self, &T)) {
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.push(sep);
            }
            f(self, item);
        }
    }

    fn plan(&mut self, p: &QueryPlan) {
        self.push("SELECT ");
        match &p.distinct {
            DistinctMode::None => {}
            DistinctMode::All => self.push("DISTINCT "),
            DistinctMode::On(exprs) => {
                self.push("DISTINCT ON (");
                self.list(exprs, ", ", Self::expr);
                self.push(") ");
            }
        }
        if p.projection.is_empty() {
            self.push("*");
        } else {
            self.list(&p.projection, ", ", |c, s| {
                c.expr(&s.expr);
                if let Some(alias) = &s.alias {
                    c.push(" AS ");
                    c.ident(alias);
                }
            });
        }
        self.push(" FROM ");
        self.source(&p.source);
        for j in &p.joins {
            self.push(match j.kind {
                JoinKind::Inner => " INNER JOIN ",
                JoinKind::Left => " LEFT JOIN ",
            });
            self.source(&j.source);
            self.push(" ON ");
            self.expr(&j.on);
        }
        if let Some(f) = &p.filter {
            self.push(" WHERE ");
            self.expr(f);
        }
        if !p.grouping.is_empty() {
            self.push(" GROUP BY ");
            self.list(&p.grouping, ", ", Self::expr);
        }
        if let Some(h) = &p.having {
            self.push(" HAVING ");
            self.expr(h);
        }
        if !p.ordering.is_empty() {
            self.push(" ORDER BY ");
            self.list(&p.ordering, ", ", |c, o| {
                c.expr(&o.expr);
                c.push(match o.direction {
                    OrderDirection::Asc => " ASC",
                    OrderDirection::Desc => " DESC",
                });
            });
        }
        match (p.limit, p.offset) {
            (Some(l), _) => {
                let _ = write!(self.sql, " LIMIT {l}");
            }
            (None, Some(_)) if self.dialect.offset_requires_limit() => self.push(" LIMIT -1"),
            _ => {}
        }
        if let Some(o) = p.offset {
            let _ = write!(self.sql, " OFFSET {o}");
        }
        if let Some(lock) = p.lock {
            self.push(match lock {
                LockMode::ForUpdate => " FOR UPDATE",
                LockMode::ForUpdateNoWait => " FOR UPDATE NOWAIT",
                LockMode::ForUpdateSkipLocked => " FOR UPDATE SKIP LOCKED",
            });
        }
    }

    fn source(&mut self, s: &QuerySource) {
        self.ident(&s.name);
        if let Some(alias) = &s.alias {
            self.push(" AS ");
            self.ident(alias);
        }
    }

    fn column(&mut self, c: &Column) {
        if let Some(src) = &c.source {
            self.ident(src);
            self.push(".");
        }
        self.ident(&c.name);
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Column(c) => self.column(c),
            Expr::Value(v) => self.bind(v.clone()),
            Expr::Binary { op, lhs, rhs } => {
                self.push("(");
                self.expr(lhs);
                self.push(binary_op(*op));
                self.expr(rhs);
                self.push(")");
            }
            Expr::Unary { op, expr } => {
                self.push(match op {
                    UnaryOp::Not => "(NOT ",
                    UnaryOp::Neg => "(-",
                });
                self.expr(expr);
                self.push(")");
            }
            Expr::And(v) => self.connective(v, " AND ", "(1=1)"),
            Expr::Or(v) => self.connective(v, " OR ", "(1=0)"),
            Expr::Lookup { expr, lookup } => self.lookup(expr, lookup),
            Expr::Exists(p) => {
                self.push("EXISTS (");
                self.plan(p);
                self.push(")");
            }
            Expr::Subquery(p) => {
                self.push("(");
                self.plan(p);
                self.push(")");
            }
        }
    }

    fn connective(&mut self, items: &[Expr], sep: &str, empty: &str) {
        if items.is_empty() {
            self.push(empty);
            return;
        }
        self.push("(");
        self.list(items, sep, Self::expr);
        self.push(")");
    }

    fn lookup(&mut self, target: &Expr, lookup: &Lookup) {
        match lookup {
            Lookup::IExact(v) => {
                self.push("(LOWER(");
                self.expr(target);
                self.push(") = LOWER(");
                self.bind(v.clone());
                self.push("))");
            }
            Lookup::Contains {
                needle,
                case_insensitive,
            } => {
                self.text_match(target, needle, *case_insensitive, Anchor::Anywhere);
            }
            Lookup::StartsWith {
                needle,
                case_insensitive,
            } => {
                self.text_match(target, needle, *case_insensitive, Anchor::Start);
            }
            Lookup::EndsWith {
                needle,
                case_insensitive,
            } => {
                self.text_match(target, needle, *case_insensitive, Anchor::End);
            }
            Lookup::Regex(pattern) => {
                // Capability check guarantees only regex-capable dialects get here.
                self.push("(");
                self.expr(target);
                self.push(" ~ ");
                self.bind(Value::Text(pattern.clone()));
                self.push(")");
            }
            Lookup::In(values) if values.is_empty() => self.push("(1=0)"),
            Lookup::In(values) => {
                self.push("(");
                self.expr(target);
                self.push(" IN (");
                self.list(values, ", ", |c, v| c.bind(v.clone()));
                self.push("))");
            }
            Lookup::Range(lo, hi) => {
                self.push("(");
                self.expr(target);
                self.push(" BETWEEN ");
                self.bind(lo.clone());
                self.push(" AND ");
                self.bind(hi.clone());
                self.push(")");
            }
            Lookup::IsNull(yes) => {
                self.push("(");
                self.expr(target);
                self.push(if *yes { " IS NULL)" } else { " IS NOT NULL)" });
            }
        }
    }

    /// Substring / prefix / suffix matching with correct case semantics.
    ///
    /// SQLite's `LIKE` is ASCII case-insensitive, so case-sensitive matches
    /// there use `instr`/`substr` instead of `LIKE`.
    fn text_match(&mut self, target: &Expr, needle: &str, ci: bool, anchor: Anchor) {
        if needle.is_empty() {
            self.push("(1=1)");
            return;
        }
        if !ci && self.kind == BackendKind::Sqlite {
            self.push("(");
            match anchor {
                Anchor::Anywhere => {
                    self.push("instr(");
                    self.expr(target);
                    self.push(", ");
                    self.bind(needle.into());
                    self.push(") > 0");
                }
                Anchor::Start | Anchor::End => {
                    let len = needle.chars().count();
                    self.push("substr(");
                    self.expr(target);
                    match anchor {
                        Anchor::Start => {
                            let _ = write!(self.sql, ", 1, {len}) = ");
                        }
                        _ => {
                            let _ = write!(self.sql, ", -{len}) = ");
                        }
                    }
                    self.bind(needle.into());
                }
            }
            self.push(")");
            return;
        }
        let pattern = like_pattern(needle, anchor);
        self.push("(");
        if ci && !self.native_ilike {
            self.push("LOWER(");
            self.expr(target);
            self.push(") LIKE LOWER(");
            self.bind(Value::Text(pattern));
            self.push(")");
        } else {
            self.expr(target);
            self.push(if ci { " ILIKE " } else { " LIKE " });
            self.bind(Value::Text(pattern));
        }
        let _ = write!(self.sql, " ESCAPE '{LIKE_ESCAPE}')");
    }
}

fn binary_op(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Eq => " = ",
        BinaryOp::Ne => " <> ",
        BinaryOp::Lt => " < ",
        BinaryOp::Le => " <= ",
        BinaryOp::Gt => " > ",
        BinaryOp::Ge => " >= ",
        BinaryOp::Add => " + ",
        BinaryOp::Sub => " - ",
        BinaryOp::Mul => " * ",
        BinaryOp::Div => " / ",
        BinaryOp::Mod => " % ",
    }
}

/// Escape `%`, `_` and the escape char, then add wildcards for `anchor`.
fn like_pattern(needle: &str, anchor: Anchor) -> String {
    let mut out = String::with_capacity(needle.len() + 2);
    if matches!(anchor, Anchor::Anywhere | Anchor::End) {
        out.push('%');
    }
    for ch in needle.chars() {
        if matches!(ch, '%' | '_') || ch == LIKE_ESCAPE {
            out.push(LIKE_ESCAPE);
        }
        out.push(ch);
    }
    if matches!(anchor, Anchor::Anywhere | Anchor::Start) {
        out.push('%');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::{Postgres, Sqlite};
    use axumapi_orm::expr::Field;
    use axumapi_orm::{BackendCapabilityError, OrderDirection::*};

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
}
