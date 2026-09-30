//! Lookups: `iexact`, text matching, `IN`, `BETWEEN`, `IS NULL`, regex.

use super::Compiler;
use siderite_orm::{BackendKind, Expr, Lookup, Value};
use std::fmt::Write;

/// Escape character used for `LIKE` patterns.
const LIKE_ESCAPE: char = '\\';

/// Which part of a text value a pattern must match.
#[derive(Clone, Copy)]
enum Anchor {
    Anywhere,
    Start,
    End,
}

impl Compiler<'_> {
    pub(super) fn lookup(&mut self, target: &Expr, lookup: &Lookup) {
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
            } => self.text_match(target, needle, *case_insensitive, Anchor::Anywhere),
            Lookup::StartsWith {
                needle,
                case_insensitive,
            } => self.text_match(target, needle, *case_insensitive, Anchor::Start),
            Lookup::EndsWith {
                needle,
                case_insensitive,
            } => self.text_match(target, needle, *case_insensitive, Anchor::End),
            Lookup::Regex(pattern) => {
                // Capability check guarantees only regex-capable dialects get here.
                if self.kind == BackendKind::MySql {
                    // `c`: case-sensitive, whatever the column collation.
                    self.push("REGEXP_LIKE(");
                    self.expr(target);
                    self.push(", ");
                    self.bind(Value::Text(pattern.clone()));
                    self.push(", 'c')");
                    return;
                }
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
                self.list(values, ", ", |c, v| c.value(v));
                self.push("))");
            }
            Lookup::InSubquery(plan) => {
                self.push("(");
                self.expr(target);
                self.push(" IN (");
                self.operand_subquery(plan, false);
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
        } else if !ci && self.kind == BackendKind::MySql {
            // Default collations are case-insensitive; comparing as binary
            // strings makes the match case-sensitive.
            self.expr(target);
            self.push(" LIKE CAST(");
            self.bind(Value::Text(pattern));
            self.push(" AS BINARY)");
        } else {
            self.expr(target);
            self.push(if ci { " ILIKE " } else { " LIKE " });
            self.bind(Value::Text(pattern));
        }
        let escape = self.dialect.like_escape_literal();
        let _ = write!(self.sql, " ESCAPE {escape})");
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
