//! `QueryPlan` / `WritePlan` → SQL compiler.
//!
//! [`compile`] and [`compile_write`] are the entry points. The compiler is a
//! single-pass writer: [`Compiler`] appends SQL text and collects bind
//! parameters; sibling modules add the expression, lookup and function
//! grammar as further `impl` blocks.

mod expr;
mod function;
mod lookup;
#[cfg(test)]
mod tests;

use super::dialect::Dialect;
use axumapi_orm::expr::Ident;
use axumapi_orm::{
    BackendKind, DistinctMode, JoinKind, LockMode, OrderDirection, OrmError, QueryError, QueryPlan,
    QuerySource, SetOp, Value, WritePlan,
};
use std::fmt::Write;

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
/// [`OrmError::Capability`] if the plan needs a feature the dialect does not
/// declare; [`QueryError::InvalidPlan`] for structural problems (unresolved
/// relations, wrong function arity, `OuterRef` outside a subquery, ...).
pub fn compile(plan: &QueryPlan, dialect: &dyn Dialect) -> Result<CompiledQuery, OrmError> {
    plan.check(&dialect.capabilities())?;
    let mut c = Compiler::new(dialect);
    c.plan(plan);
    c.finish()
}

/// Compile a write `plan` for `dialect`.
///
/// # Errors
/// [`OrmError::Capability`] for unsupported features (e.g. `RETURNING`), and
/// [`QueryError::InvalidPlan`] for an insert without rows or with rows whose
/// length differs from the column list.
pub fn compile_write(plan: &WritePlan, dialect: &dyn Dialect) -> Result<CompiledQuery, OrmError> {
    plan.check(&dialect.capabilities())?;
    let mut c = Compiler::new(dialect);
    match plan {
        WritePlan::Insert(p) => {
            if p.rows.is_empty() || p.rows.iter().any(|r| r.len() != p.columns.len()) {
                return Err(QueryError::InvalidPlan(
                    "insert rows must be non-empty and match the column list".into(),
                )
                .into());
            }
            c.push("INSERT INTO ");
            c.ident(&p.table);
            if p.columns.is_empty() {
                // Only generated columns: one `DEFAULT VALUES` row per statement.
                if p.rows.len() > 1 {
                    return Err(QueryError::InvalidPlan(
                        "multi-row insert needs at least one explicit column".into(),
                    )
                    .into());
                }
                c.push(" DEFAULT VALUES");
            } else {
                c.push(" (");
                c.list(&p.columns, ", ", |c, col| c.ident(col));
                c.push(") VALUES ");
                c.list(&p.rows, ", ", |c, row| {
                    c.push("(");
                    c.list(row, ", ", |c, v| c.value(v));
                    c.push(")");
                });
            }
        }
        WritePlan::Update(p) => {
            if p.assignments.is_empty() {
                return Err(QueryError::InvalidPlan("update without assignments".into()).into());
            }
            c.write_scope(&p.table);
            c.push("UPDATE ");
            c.ident(&p.table);
            c.push(" SET ");
            c.list(&p.assignments, ", ", |c, (col, e)| {
                c.ident(col);
                c.push(" = ");
                c.expr(e);
            });
            c.where_clause(p.filter.as_ref());
        }
        WritePlan::Delete(p) => {
            c.write_scope(&p.table);
            c.push("DELETE FROM ");
            c.ident(&p.table);
            c.where_clause(p.filter.as_ref());
        }
    }
    c.returning(plan.returning());
    c.finish()
}

/// A query being compiled: the name its columns are qualified with, and
/// whether qualification is needed (the query has joins).
struct Scope {
    reference: String,
    qualify: bool,
}

struct Compiler<'d> {
    dialect: &'d dyn Dialect,
    native_ilike: bool,
    kind: BackendKind,
    sql: String,
    params: Vec<Value>,
    /// Enclosing queries, innermost last; `OuterRef` looks one level up.
    scopes: Vec<Scope>,
    /// Counter for generated derived-table aliases (`u1`, `u2`, ...).
    derived: usize,
    /// First structural problem found; reported by [`finish`](Self::finish).
    error: Option<QueryError>,
}

impl<'d> Compiler<'d> {
    fn new(dialect: &'d dyn Dialect) -> Self {
        let caps = dialect.capabilities();
        Self {
            dialect,
            native_ilike: caps.ilike,
            kind: caps.kind,
            sql: String::new(),
            params: Vec::new(),
            scopes: Vec::new(),
            derived: 0,
            error: None,
        }
    }

    fn finish(self) -> Result<CompiledQuery, OrmError> {
        match self.error {
            Some(err) => Err(err.into()),
            None => Ok(CompiledQuery {
                sql: self.sql,
                params: self.params,
            }),
        }
    }

    /// Record the first structural problem; later ones are ignored.
    fn fail(&mut self, message: impl Into<String>) {
        self.error
            .get_or_insert_with(|| QueryError::InvalidPlan(message.into()));
    }

    fn returning(&mut self, columns: &[Ident]) {
        if !columns.is_empty() {
            self.push(" RETURNING ");
            self.list(columns, ", ", |c, col| c.ident(col));
        }
    }

    fn where_clause(&mut self, filter: Option<&axumapi_orm::Expr>) {
        if let Some(f) = filter {
            self.push(" WHERE ");
            self.expr(f);
        }
    }

    fn push(&mut self, s: &str) {
        self.sql.push_str(s);
    }

    fn ident(&mut self, ident: &str) {
        self.dialect.write_ident(&mut self.sql, ident);
    }

    /// The scope of an `UPDATE` / `DELETE` target, so correlated subqueries in
    /// its assignments and filter can refer to the row with `OuterRef`.
    fn write_scope(&mut self, table: &str) {
        self.scopes.push(Scope {
            reference: table.to_owned(),
            qualify: false,
        });
    }

    /// A value operand: `NULL` is written as the keyword, everything else is bound.
    ///
    /// Spelling `NULL` out (instead of binding an untyped parameter) keeps the
    /// statement text distinct from the same statement with a value, which
    /// matters for PostgreSQL: SQLx caches prepared statements by text, and a
    /// parameter whose type the server inferred from `NULL` would reject a
    /// later binary value of another width.
    fn value(&mut self, v: &Value) {
        if v.is_null() {
            self.push("NULL");
        } else {
            self.bind(v.clone());
        }
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

    /// A complete query: the core `SELECT`, its set-operation members, then
    /// the `ORDER BY` / `LIMIT` / `OFFSET` / lock clauses of the whole result.
    fn plan(&mut self, p: &QueryPlan) {
        self.scopes.push(Scope {
            reference: p.source.reference().to_string(),
            qualify: !p.joins.is_empty(),
        });
        self.select(p);
        for member in &p.compound {
            self.push(match member.op {
                SetOp::Union => " UNION ",
                SetOp::UnionAll => " UNION ALL ",
                SetOp::Intersect => " INTERSECT ",
                SetOp::Except => " EXCEPT ",
            });
            self.set_member(&member.plan);
        }
        self.tail(p);
        self.scopes.pop();
    }

    /// A member of a set operation. Members with their own `ORDER BY` /
    /// `LIMIT` are wrapped in a derived table, which every dialect accepts
    /// (bare parenthesised members are rejected by SQLite).
    fn set_member(&mut self, member: &QueryPlan) {
        let needs_wrap = !member.ordering.is_empty()
            || member.limit.is_some()
            || member.offset.is_some()
            || member.lock.is_some()
            || !member.compound.is_empty();
        if !needs_wrap {
            self.plan(member);
            return;
        }
        self.derived += 1;
        let alias = format!("u{}", self.derived);
        self.push("SELECT * FROM (");
        self.plan(member);
        self.push(") AS ");
        self.ident(&alias);
    }

    /// `SELECT .. FROM .. WHERE .. GROUP BY .. HAVING ..`.
    fn select(&mut self, p: &QueryPlan) {
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
        if !p.projection.is_empty() {
            self.list(&p.projection, ", ", |c, s| {
                c.expr(&s.expr);
                if let Some(alias) = &s.alias {
                    c.push(" AS ");
                    c.ident(alias);
                }
            });
        } else if p.joins.is_empty() {
            self.push("*");
        } else {
            // `*` would also select the joined tables' (clashing) columns.
            let root = p.source.reference().clone();
            self.ident(&root);
            self.push(".*");
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
        self.where_clause(p.filter.as_ref());
        if !p.grouping.is_empty() {
            self.push(" GROUP BY ");
            self.list(&p.grouping, ", ", Self::expr);
        }
        if let Some(h) = &p.having {
            self.push(" HAVING ");
            self.expr(h);
        }
    }

    /// `ORDER BY .. LIMIT .. OFFSET .. FOR UPDATE ..`.
    fn tail(&mut self, p: &QueryPlan) {
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
        if let Some(inner) = &s.subquery {
            self.push("(");
            self.plan(inner);
            self.push(")");
        } else {
            self.ident(&s.name);
            if let Some(alias) = &s.alias {
                self.push(" AS ");
                self.ident(alias);
            }
            return;
        }
        self.push(" AS ");
        self.ident(s.reference());
    }
}
