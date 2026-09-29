//! The `#[field(...)]` attribute, shared by `derive(Schema)`,
//! `derive(Validate)` and `derive(Model)`.
//!
//! One parser accepts the union of all derives' keys, so a key is never
//! silently ignored by one derive and understood by the other (the ORM keys
//! live in [`OrmFieldOptions`]).

use super::orm::OrmFieldOptions;
use super::serde::path_key;
use crate::diag::Errors;
use crate::meta::{flag, for_each_meta, lit_str};
use proc_macro2::{Literal, TokenStream};
use quote::{ToTokens, quote};
use std::collections::HashSet;
use syn::meta::ParseNestedMeta;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, ExprLit, ExprUnary, Lit, LitInt, LitStr, Path, Token, UnOp};

/// Every key accepted by `#[field(...)]`.
pub const FIELD_KEYS: &str = "min_length, max_length, pattern (or regex), email, url, gt, ge, lt, \
     le, multiple_of, default, default_factory, alias, validation_alias, serialization_alias, \
     title, description, examples, exclude, strict, max_digits, decimal_places, validator, \
     primary_key, auto, unique, index, column, db_default, auto_now_add, auto_now, on_delete, \
     related_name, skip";

/// A schema keyword and the tokens of its JSON value (an argument to `json!`).
pub type Keyword = (&'static str, TokenStream);

/// A numeric literal bound (`ge = 18`, `lt = 2.5`).
pub struct NumLit {
    /// The literal as written (`-1` included), usable inside `json!`.
    pub expr: Expr,
    kind: NumKind,
}

enum NumKind {
    Int(i64),
    UInt(u64),
    Float(f64),
}

impl NumLit {
    fn parse(expr: Expr) -> syn::Result<Self> {
        let (negative, inner) = match &expr {
            Expr::Unary(ExprUnary {
                op: UnOp::Neg(_),
                expr,
                ..
            }) => (true, &**expr),
            other => (false, other),
        };
        let bad = || syn::Error::new_spanned(&expr, "expected a numeric literal");
        let Expr::Lit(ExprLit { lit, .. }) = inner else {
            return Err(bad());
        };
        let kind = match lit {
            Lit::Int(int) => {
                let magnitude: i128 = int.base10_parse()?;
                let value = if negative { -magnitude } else { magnitude };
                if let Ok(v) = i64::try_from(value) {
                    NumKind::Int(v)
                } else if let Ok(v) = u64::try_from(value) {
                    NumKind::UInt(v)
                } else {
                    return Err(syn::Error::new_spanned(
                        &expr,
                        "integer literal does not fit in 64 bits; write it as a float",
                    ));
                }
            }
            Lit::Float(float) => {
                let magnitude: f64 = float.base10_parse()?;
                if !magnitude.is_finite() {
                    return Err(syn::Error::new_spanned(&expr, "bound must be finite"));
                }
                NumKind::Float(if negative { -magnitude } else { magnitude })
            }
            _ => return Err(bad()),
        };
        Ok(Self { expr, kind })
    }

    fn is_zero(&self) -> bool {
        match self.kind {
            NumKind::Int(v) => v == 0,
            NumKind::UInt(v) => v == 0,
            NumKind::Float(v) => v == 0.0,
        }
    }

    /// An expression building the `serde_json::Number` (never panics).
    pub fn number(&self) -> TokenStream {
        let number = quote!(::axumapi::__private::serde_json::Number);
        match self.kind {
            NumKind::Int(v) => {
                let lit = Literal::i64_suffixed(v);
                quote!(#number::from(#lit))
            }
            NumKind::UInt(v) => {
                let lit = Literal::u64_suffixed(v);
                quote!(#number::from(#lit))
            }
            NumKind::Float(v) => {
                let lit = Literal::f64_suffixed(v);
                quote!(::axumapi::__private::number_f64(#lit))
            }
        }
    }
}

/// A declared default (`default = ..` / `default_factory = ..`).
pub enum DefaultSpec {
    /// `default = <expr>`; the expression has the field's type.
    Expr(Expr),
    /// `default_factory = path`; calling it yields the field's type.
    Factory(Path),
}

impl DefaultSpec {
    /// The literal, when the default is one (documented in the schema).
    pub fn literal(&self) -> Option<&Lit> {
        match self {
            Self::Expr(Expr::Lit(ExprLit { lit, .. })) => Some(lit),
            _ => None,
        }
    }
}

/// Parsed `#[field(...)]` attributes of one field.
#[derive(Default)]
pub struct FieldOptions {
    pub alias: Option<LitStr>,
    pub validation_aliases: Vec<LitStr>,
    pub serialization_alias: Option<LitStr>,
    pub default: Option<DefaultSpec>,
    pub min_length: Option<u64>,
    pub max_length: Option<u64>,
    pub pattern: Option<LitStr>,
    pub email: bool,
    pub url: bool,
    pub gt: Option<NumLit>,
    pub ge: Option<NumLit>,
    pub lt: Option<NumLit>,
    pub le: Option<NumLit>,
    pub multiple_of: Option<NumLit>,
    pub title: Option<LitStr>,
    pub description: Option<LitStr>,
    pub examples: Option<Vec<Expr>>,
    pub exclude: bool,
    pub strict: bool,
    pub max_digits: Option<u64>,
    pub decimal_places: Option<u64>,
    /// Reusable after-validators (`validator = path`), in source order.
    pub validators: Vec<Path>,
    /// Keys read by `derive(Model)` only.
    pub orm: OrmFieldOptions,
}

impl FieldOptions {
    pub fn parse(attrs: &[Attribute], errors: &mut Errors) -> Self {
        let mut out = Self::default();
        let mut seen = HashSet::new();
        for_each_meta(attrs, "field", errors, |meta, errors| {
            out.parse_key(&meta, &mut seen, errors)
        });
        if out.email && out.url {
            errors.error(
                proc_macro2::Span::call_site(),
                "`email` and `url` cannot be combined (both set the `format`)",
            );
        }
        out
    }

    fn parse_key(
        &mut self,
        meta: &ParseNestedMeta<'_>,
        seen: &mut HashSet<String>,
        errors: &mut Errors,
    ) -> syn::Result<()> {
        let key = path_key(meta);
        let canonical = if key == "regex" {
            "pattern"
        } else {
            key.as_str()
        };
        let repeatable = matches!(canonical, "validator" | "validation_alias");
        if !repeatable && !seen.insert(canonical.to_owned()) {
            errors.error(
                meta.path.span(),
                format!("`{key}` is specified more than once"),
            );
        }
        match canonical {
            "alias" => self.alias = Some(lit_str(meta)?),
            "validation_alias" => self.validation_aliases.push(lit_str(meta)?),
            "serialization_alias" => self.serialization_alias = Some(lit_str(meta)?),
            "default" => self.default = Some(DefaultSpec::Expr(meta.value()?.parse()?)),
            "default_factory" => {
                let expr: Expr = meta.value()?.parse()?;
                match expr {
                    Expr::Path(path) if path.qself.is_none() => {
                        self.default = Some(DefaultSpec::Factory(path.path));
                    }
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "`default_factory` expects a path to a function, \
                             e.g. `default_factory = Vec::new`",
                        ));
                    }
                }
            }
            "min_length" => self.min_length = Some(count(meta)?),
            "max_length" => self.max_length = Some(count(meta)?),
            "max_digits" => self.max_digits = Some(count(meta)?),
            "decimal_places" => self.decimal_places = Some(count(meta)?),
            "pattern" => {
                let lit = lit_str(meta)?;
                if let Err(err) = regex::Regex::new(&lit.value()) {
                    let text = err.to_string();
                    let last = text.lines().last().unwrap_or(&text);
                    let reason = last.trim_start_matches("error: ");
                    return Err(syn::Error::new(
                        lit.span(),
                        format!("invalid regular expression: {reason}"),
                    ));
                }
                self.pattern = Some(lit);
            }
            "gt" => self.gt = Some(number(meta)?),
            "ge" => self.ge = Some(number(meta)?),
            "lt" => self.lt = Some(number(meta)?),
            "le" => self.le = Some(number(meta)?),
            "multiple_of" => {
                let n = number(meta)?;
                if n.is_zero() {
                    return Err(syn::Error::new_spanned(
                        &n.expr,
                        "`multiple_of` must not be 0",
                    ));
                }
                self.multiple_of = Some(n);
            }
            "email" => self.email = flag(meta)?,
            "url" => self.url = flag(meta)?,
            "exclude" => self.exclude = flag(meta)?,
            "strict" => self.strict = flag(meta)?,
            "title" => self.title = Some(lit_str(meta)?),
            "description" => self.description = Some(lit_str(meta)?),
            "examples" => {
                let content;
                syn::parenthesized!(content in meta.input);
                let items = Punctuated::<Expr, Token![,]>::parse_terminated(&content)?;
                self.examples = Some(items.into_iter().collect());
            }
            "validator" => {
                let expr: Expr = meta.value()?.parse()?;
                match expr {
                    Expr::Path(path) if path.qself.is_none() => self.validators.push(path.path),
                    other => {
                        return Err(syn::Error::new_spanned(
                            other,
                            "`validator` expects a path to a function \
                             `fn(&FieldType) -> Result<(), FieldError>`",
                        ));
                    }
                }
            }
            other if self.orm.parse_key(other, meta)? => {}
            other => {
                return Err(meta.error(format!(
                    "unknown `field` key `{other}`; valid keys: {FIELD_KEYS}"
                )));
            }
        }
        Ok(())
    }

    /// Schema keywords for this field; `doc` is the field's doc comment,
    /// used as the description unless one is given explicitly.
    pub fn schema_keywords(&self, doc: Option<String>) -> Vec<Keyword> {
        let mut out: Vec<Keyword> = Vec::new();
        let mut push = |key: &'static str, value: TokenStream| out.push((key, value));
        if let Some(n) = self.min_length {
            push("minLength", unsuffixed(n));
        }
        if let Some(n) = self.max_length {
            push("maxLength", unsuffixed(n));
        }
        if let Some(p) = &self.pattern {
            push("pattern", quote!(#p));
        }
        for (key, bound) in [
            ("exclusiveMinimum", &self.gt),
            ("minimum", &self.ge),
            ("exclusiveMaximum", &self.lt),
            ("maximum", &self.le),
            ("multipleOf", &self.multiple_of),
        ] {
            if let Some(n) = bound {
                push(key, n.expr.to_token_stream());
            }
        }
        if self.email {
            push("format", quote!("email"));
        }
        if self.url {
            push("format", quote!("uri"));
        }
        if let Some(t) = &self.title {
            push("title", quote!(#t));
        }
        match (&self.description, doc) {
            (Some(d), _) => push("description", quote!(#d)),
            (None, Some(d)) => push("description", quote!(#d)),
            (None, None) => {}
        }
        if let Some(items) = &self.examples {
            push("examples", quote!([#(#items),*]));
        }
        if let Some(lit) = self.default.as_ref().and_then(DefaultSpec::literal) {
            push("default", quote!(#lit));
        }
        if let Some(n) = self.max_digits {
            push("x-max-digits", unsuffixed(n));
        }
        if let Some(n) = self.decimal_places {
            push("x-decimal-places", unsuffixed(n));
        }
        out
    }
}

fn unsuffixed(n: u64) -> TokenStream {
    Literal::u64_unsuffixed(n).to_token_stream()
}

/// `key = <non-negative integer literal>`.
fn count(meta: &ParseNestedMeta<'_>) -> syn::Result<u64> {
    let lit: LitInt = meta.value()?.parse()?;
    lit.base10_parse::<u64>()
}

/// `key = <numeric literal>`.
fn number(meta: &ParseNestedMeta<'_>) -> syn::Result<NumLit> {
    NumLit::parse(meta.value()?.parse()?)
}
