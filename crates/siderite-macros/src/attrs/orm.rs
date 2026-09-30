//! The ORM keys of `#[field(...)]`, read by `derive(Model)` and ignored by
//! `derive(Schema)` / `derive(Validate)`.

use crate::meta::{flag, lit_str};
use syn::meta::ParseNestedMeta;
use syn::{Expr, ExprLit, ExprUnary, Lit, LitStr, UnOp};

/// A database default written as `db_default = <literal>`.
pub enum DbDefaultSpec {
    /// Integer literal (negative allowed).
    Int(i64),
    /// `true` / `false`.
    Bool(bool),
    /// String literal.
    Text(LitStr),
}

impl DbDefaultSpec {
    fn parse(expr: Expr) -> syn::Result<Self> {
        let bad = || {
            syn::Error::new_spanned(
                &expr,
                "`db_default` expects an integer, boolean or string literal",
            )
        };
        let (negative, inner) = match &expr {
            Expr::Unary(ExprUnary {
                op: UnOp::Neg(_),
                expr,
                ..
            }) => (true, &**expr),
            other => (false, other),
        };
        let Expr::Lit(ExprLit { lit, .. }) = inner else {
            return Err(bad());
        };
        match lit {
            Lit::Int(int) => {
                let magnitude: i128 = int.base10_parse()?;
                let value = if negative { -magnitude } else { magnitude };
                i64::try_from(value)
                    .map(Self::Int)
                    .map_err(|_| syn::Error::new_spanned(&expr, "integer does not fit in 64 bits"))
            }
            Lit::Bool(b) if !negative => Ok(Self::Bool(b.value)),
            Lit::Str(s) if !negative => Ok(Self::Text(s.clone())),
            _ => Err(bad()),
        }
    }
}

/// Parsed ORM keys of one field.
#[derive(Default)]
pub struct OrmFieldOptions {
    pub primary_key: bool,
    pub auto: bool,
    pub unique: bool,
    pub index: bool,
    pub auto_now_add: bool,
    pub auto_now: bool,
    /// Not a column; filled with `Default::default()` when loaded.
    pub skip: bool,
    pub column: Option<LitStr>,
    pub db_default: Option<DbDefaultSpec>,
    pub on_delete: Option<LitStr>,
    pub related_name: Option<LitStr>,
}

impl OrmFieldOptions {
    /// Parse `key` if it is an ORM key; `Ok(false)` if it is not one.
    pub fn parse_key(&mut self, key: &str, meta: &ParseNestedMeta<'_>) -> syn::Result<bool> {
        match key {
            "primary_key" => self.primary_key = flag(meta)?,
            "auto" => self.auto = flag(meta)?,
            "unique" => self.unique = flag(meta)?,
            "index" => self.index = flag(meta)?,
            "auto_now_add" => self.auto_now_add = flag(meta)?,
            "auto_now" => self.auto_now = flag(meta)?,
            "skip" => self.skip = flag(meta)?,
            "column" => self.column = Some(lit_str(meta)?),
            "on_delete" => self.on_delete = Some(lit_str(meta)?),
            "related_name" => self.related_name = Some(lit_str(meta)?),
            "db_default" => self.db_default = Some(DbDefaultSpec::parse(meta.value()?.parse()?)?),
            _ => return Ok(false),
        }
        Ok(true)
    }
}
