//! Parsing of `#[schema]`, `#[field]` and `#[serde]` helper attributes.

use super::rename::{EXPECTED, RenameRule};
use crate::diag::Errors;
use crate::meta::{flag, for_each_meta, lit_str, skip_value};
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::meta::ParseNestedMeta;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, ExprLit, ExprUnary, Lit, LitInt, LitStr, Token, UnOp, token};

/// serde keys that change the wire format in ways this derive cannot describe.
const UNSUPPORTED_CONTAINER: &[&str] = &["transparent", "from", "into", "try_from", "remote"];
const UNSUPPORTED_FIELD: &[&str] = &["flatten", "with", "serialize_with", "deserialize_with"];
const UNSUPPORTED_VARIANT: &[&str] = &[
    "other",
    "untagged",
    "with",
    "serialize_with",
    "deserialize_with",
];

fn unsupported(meta: &ParseNestedMeta<'_>, key: &str) -> syn::Error {
    let message = if key == "flatten" {
        "flatten is not supported by derive(Schema) yet".to_owned()
    } else {
        format!("`{key}` is not supported by derive(Schema) yet")
    };
    meta.error(message)
}

fn path_key(meta: &ParseNestedMeta<'_>) -> String {
    meta.path.get_ident().map_or_else(
        || meta.path.to_token_stream().to_string(),
        ToString::to_string,
    )
}

/// `rename = "x"`; the `(serialize = .., deserialize = ..)` form is rejected.
fn rename_value(meta: &ParseNestedMeta<'_>) -> syn::Result<String> {
    if meta.input.peek(token::Paren) {
        return Err(
            meta.error("separate serialize/deserialize names are not supported by derive(Schema)")
        );
    }
    Ok(lit_str(meta)?.value())
}

fn rename_rule(meta: &ParseNestedMeta<'_>) -> syn::Result<RenameRule> {
    if meta.input.peek(token::Paren) {
        return Err(
            meta.error("separate serialize/deserialize rules are not supported by derive(Schema)")
        );
    }
    let lit = lit_str(meta)?;
    RenameRule::parse(&lit.value()).ok_or_else(|| {
        syn::Error::new(
            lit.span(),
            format!(
                "unknown rename rule `{}`; expected one of: {EXPECTED}",
                lit.value()
            ),
        )
    })
}

/// Attributes on the type itself.
#[derive(Default)]
pub struct ContainerAttrs {
    pub rename_all: Option<RenameRule>,
    pub rename_all_fields: Option<RenameRule>,
    pub default: bool,
    pub deny_unknown_fields: bool,
    pub tag: Option<LitStr>,
    pub content: Option<LitStr>,
    pub untagged: bool,
    pub name: Option<LitStr>,
    pub inline: bool,
}

impl ContainerAttrs {
    pub fn parse(attrs: &[Attribute], errors: &mut Errors) -> Self {
        let mut out = Self::default();
        for_each_meta(attrs, "serde", errors, |meta, _| {
            let key = path_key(&meta);
            match key.as_str() {
                "rename_all" => out.rename_all = Some(rename_rule(&meta)?),
                "rename_all_fields" => out.rename_all_fields = Some(rename_rule(&meta)?),
                "default" => {
                    skip_value(&meta)?;
                    out.default = true;
                }
                "deny_unknown_fields" => out.deny_unknown_fields = true,
                "tag" => out.tag = Some(lit_str(&meta)?),
                "content" => out.content = Some(lit_str(&meta)?),
                "untagged" => out.untagged = true,
                k if UNSUPPORTED_CONTAINER.contains(&k) => return Err(unsupported(&meta, k)),
                _ => skip_value(&meta)?,
            }
            Ok(())
        });
        for_each_meta(attrs, "schema", errors, |meta, errors| {
            match path_key(&meta).as_str() {
                "name" => {
                    let lit = lit_str(&meta)?;
                    if lit.value().is_empty() {
                        errors.spanned(&lit, "schema name must not be empty");
                    }
                    out.name = Some(lit);
                }
                "inline" => out.inline = flag(&meta)?,
                other => {
                    return Err(meta.error(format!(
                        "unknown `schema` key `{other}`; expected `name` or `inline`"
                    )));
                }
            }
            Ok(())
        });
        out
    }
}

/// A schema keyword and the tokens of its JSON value (an argument to `json!`).
pub type Keyword = (&'static str, TokenStream);

/// Attributes on a field (or tuple element).
#[derive(Default)]
pub struct FieldAttrs {
    pub rename: Option<String>,
    pub alias: Option<String>,
    pub skip_serializing: bool,
    pub skip_deserializing: bool,
    pub default: bool,
    /// Constraint and annotation keywords, in source order.
    pub keywords: Vec<Keyword>,
}

impl FieldAttrs {
    pub fn parse(attrs: &[Attribute], doc: Option<String>, errors: &mut Errors) -> Self {
        let mut out = Self::default();
        for_each_meta(attrs, "serde", errors, |meta, _| {
            let key = path_key(&meta);
            match key.as_str() {
                "rename" => out.rename = Some(rename_value(&meta)?),
                "skip" => {
                    out.skip_serializing = true;
                    out.skip_deserializing = true;
                }
                "skip_serializing" => out.skip_serializing = true,
                "skip_deserializing" => out.skip_deserializing = true,
                "default" => {
                    skip_value(&meta)?;
                    out.default = true;
                }
                k if UNSUPPORTED_FIELD.contains(&k) => return Err(unsupported(&meta, k)),
                _ => skip_value(&meta)?,
            }
            Ok(())
        });
        for_each_meta(attrs, "field", errors, |meta, errors| {
            out.parse_field_key(&meta, errors)
        });
        if let Some(doc) = doc
            && !out.keywords.iter().any(|(k, _)| *k == "description")
        {
            out.keywords.push(("description", quote!(#doc)));
        }
        out
    }

    /// Whether the field is absent from the schema entirely.
    pub fn skipped(&self) -> bool {
        self.skip_serializing && self.skip_deserializing
    }

    fn add(
        &mut self,
        meta: &ParseNestedMeta<'_>,
        key: &'static str,
        value: TokenStream,
        errors: &mut Errors,
    ) {
        if self.keywords.iter().any(|(k, _)| *k == key) {
            errors.error(
                meta.path.span(),
                format!("schema keyword `{key}` is specified more than once"),
            );
        } else {
            self.keywords.push((key, value));
        }
    }

    fn parse_field_key(
        &mut self,
        meta: &ParseNestedMeta<'_>,
        errors: &mut Errors,
    ) -> syn::Result<()> {
        let key = path_key(meta);
        match key.as_str() {
            "alias" => self.alias = Some(lit_str(meta)?.value()),
            "default" | "default_factory" => {
                skip_value(meta)?;
                self.default = true;
            }
            "min_length" => {
                let n = length(meta)?;
                self.add(meta, "minLength", n, errors);
            }
            "max_length" => {
                let n = length(meta)?;
                self.add(meta, "maxLength", n, errors);
            }
            "pattern" | "regex" => {
                let lit = lit_str(meta)?;
                self.add(meta, "pattern", quote!(#lit), errors);
            }
            "gt" => self.number(meta, "exclusiveMinimum", errors)?,
            "ge" => self.number(meta, "minimum", errors)?,
            "lt" => self.number(meta, "exclusiveMaximum", errors)?,
            "le" => self.number(meta, "maximum", errors)?,
            "multiple_of" => self.number(meta, "multipleOf", errors)?,
            "email" => {
                if flag(meta)? {
                    self.add(meta, "format", quote!("email"), errors);
                }
            }
            "url" => {
                if flag(meta)? {
                    self.add(meta, "format", quote!("uri"), errors);
                }
            }
            "title" => {
                let lit = lit_str(meta)?;
                self.add(meta, "title", quote!(#lit), errors);
            }
            "description" => {
                let lit = lit_str(meta)?;
                self.add(meta, "description", quote!(#lit), errors);
            }
            "examples" => {
                let content;
                syn::parenthesized!(content in meta.input);
                let items = Punctuated::<Expr, Token![,]>::parse_terminated(&content)?;
                let items = items.iter();
                self.add(meta, "examples", quote!([#(#items),*]), errors);
            }
            // Owned by other derives (validation); accepted and ignored here.
            _ => skip_value(meta)?,
        }
        Ok(())
    }

    fn number(
        &mut self,
        meta: &ParseNestedMeta<'_>,
        keyword: &'static str,
        errors: &mut Errors,
    ) -> syn::Result<()> {
        let expr: Expr = meta.value()?.parse()?;
        if is_number(&expr) {
            self.add(meta, keyword, expr.into_token_stream(), errors);
            Ok(())
        } else {
            Err(syn::Error::new_spanned(expr, "expected a numeric literal"))
        }
    }
}

fn length(meta: &ParseNestedMeta<'_>) -> syn::Result<TokenStream> {
    let lit: LitInt = meta.value()?.parse()?;
    lit.base10_parse::<u64>()?;
    Ok(lit.into_token_stream())
}

fn is_number(expr: &Expr) -> bool {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(_) | Lit::Float(_),
            ..
        }) => true,
        Expr::Unary(ExprUnary {
            op: UnOp::Neg(_),
            expr,
            ..
        }) => is_number(expr),
        _ => false,
    }
}

/// Attributes on an enum variant.
#[derive(Default)]
pub struct VariantAttrs {
    pub rename: Option<String>,
    pub rename_all: Option<RenameRule>,
    pub skip_serializing: bool,
    pub skip_deserializing: bool,
}

impl VariantAttrs {
    pub fn parse(attrs: &[Attribute], errors: &mut Errors) -> Self {
        let mut out = Self::default();
        for_each_meta(attrs, "serde", errors, |meta, _| {
            let key = path_key(&meta);
            match key.as_str() {
                "rename" => out.rename = Some(rename_value(&meta)?),
                "rename_all" => out.rename_all = Some(rename_rule(&meta)?),
                "skip" => {
                    out.skip_serializing = true;
                    out.skip_deserializing = true;
                }
                "skip_serializing" => out.skip_serializing = true,
                "skip_deserializing" => out.skip_deserializing = true,
                k if UNSUPPORTED_VARIANT.contains(&k) => return Err(unsupported(&meta, k)),
                _ => skip_value(&meta)?,
            }
            Ok(())
        });
        out
    }

    pub fn skipped(&self) -> bool {
        self.skip_serializing && self.skip_deserializing
    }
}
