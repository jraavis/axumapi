//! Expression grammar: columns, values, operators and boolean connectives.

use super::Compiler;
use axumapi_orm::{BinaryOp, Column, Expr, UnaryOp, Value};

impl Compiler<'_> {
    pub(super) fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Column(c) => self.column(c),
            Expr::Value(v) => self.value(v),
            Expr::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs),
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
                self.operand_subquery(p, true);
                self.push(")");
            }
            Expr::Func { func, args } => self.function(*func, args),
            Expr::Cast { expr, ty } => {
                self.push("CAST(");
                self.expr(expr);
                self.push(" AS ");
                let name = self.dialect.cast_type(*ty);
                self.push(name);
                self.push(")");
            }
            Expr::Case {
                branches,
                otherwise,
            } => self.case(branches, otherwise.as_deref()),
            Expr::Aggregate(agg) => self.aggregate(agg),
            Expr::Window(window) => self.window(window),
            Expr::DatePart { part, expr } => self.date_part(*part, expr),
            Expr::OuterRef(column) => self.outer_ref(column),
            Expr::Related(related) => {
                self.fail(format!(
                    "unresolved relation `{}`: call QueryPlan::resolve_relations first",
                    related.output_name()
                ));
                self.push("NULL");
            }
        }
    }

    /// Column reference; qualified with the query's source when it has joins.
    fn column(&mut self, c: &Column) {
        let qualifier = match (&c.source, self.scopes.last()) {
            (Some(source), _) => Some(source.to_string()),
            (None, Some(scope)) if scope.qualify => Some(scope.reference.clone()),
            _ => None,
        };
        if let Some(q) = qualifier {
            self.ident(&q);
            self.push(".");
        }
        self.ident(&c.name);
    }

    /// Column of the query enclosing the current subquery.
    fn outer_ref(&mut self, c: &Column) {
        let outer = self
            .scopes
            .len()
            .checked_sub(2)
            .and_then(|i| self.scopes.get(i))
            .map(|s| s.reference.clone());
        let current = self.scopes.last().map(|s| s.reference.as_str());
        if outer.as_deref().is_some() && outer.as_deref() == current {
            self.fail(format!(
                "OuterRef on `{}` inside a subquery over the same table: name the subquery's table with aliased(..)",
                c.name
            ));
            self.push("NULL");
            return;
        }
        match outer {
            Some(reference) => {
                self.ident(&reference);
                self.push(".");
                self.ident(&c.name);
            }
            None => {
                self.fail("OuterRef used outside of a subquery");
                self.push("NULL");
            }
        }
    }

    fn binary(&mut self, op: BinaryOp, lhs: &Expr, rhs: &Expr) {
        // `x = NULL` never matches; comparing with NULL means IS [NOT] NULL.
        let is_null = |e: &Expr| matches!(e, Expr::Value(Value::Null));
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && (is_null(lhs) || is_null(rhs)) {
            self.push("(");
            self.expr(if is_null(rhs) { lhs } else { rhs });
            self.push(if op == BinaryOp::Eq {
                " IS NULL)"
            } else {
                " IS NOT NULL)"
            });
            return;
        }
        self.push("(");
        self.expr(lhs);
        self.push(binary_op(op));
        self.expr(rhs);
        self.push(")");
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
