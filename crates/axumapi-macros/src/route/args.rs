//! Parsing of `#[get("/path", key = value, ...)]` arguments.

use super::path;
use crate::diag::Errors;
use std::collections::HashSet;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Ident, LitInt, LitStr, Token, Type, parenthesized};

/// Every key accepted by the route attributes.
const KEYS: &str = "status, response_model, tag, tags, summary, description, operation_id, \
                    deprecated, hidden";

/// Parsed route attribute arguments.
pub struct RouteArgs {
    /// Path template literal.
    pub path: LitStr,
    /// Success status override.
    pub status: Option<u16>,
    /// Documented response type.
    pub response_model: Option<Type>,
    /// Tags, from `tag = ".."` and `tags(..)`.
    pub tags: Vec<LitStr>,
    /// Explicit summary.
    pub summary: Option<LitStr>,
    /// Explicit description.
    pub description: Option<LitStr>,
    /// Explicit operation id.
    pub operation_id: Option<LitStr>,
    /// `deprecated` flag.
    pub deprecated: bool,
    /// `hidden` flag.
    pub hidden: bool,
}

impl Parse for RouteArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let path: LitStr = input.parse().map_err(|e| {
            syn::Error::new(
                e.span(),
                "the first argument must be the route path as a string literal, \
                 e.g. `\"/users/{id}\"`",
            )
        })?;
        let mut args = Self {
            path,
            status: None,
            response_model: None,
            tags: Vec::new(),
            summary: None,
            description: None,
            operation_id: None,
            deprecated: false,
            hidden: false,
        };
        let mut errors = Errors::default();
        path::validate(&args.path, &mut errors);
        let mut seen = HashSet::new();
        // A syntax error stops parsing; semantic errors are collected.
        if let Err(err) = args.parse_entries(input, &mut seen, &mut errors) {
            errors.push(err);
        }
        errors.finish(args)
    }
}

impl RouteArgs {
    fn parse_entries(
        &mut self,
        input: ParseStream<'_>,
        seen: &mut HashSet<String>,
        errors: &mut Errors,
    ) -> syn::Result<()> {
        while !input.is_empty() {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break;
            }
            let key: Ident = input.parse()?;
            let name = key.to_string();
            if !seen.insert(name.clone()) {
                errors.error(key.span(), format!("duplicate `{name}` argument"));
            }
            self.parse_entry(&key, &name, input, errors)?;
        }
        Ok(())
    }

    fn parse_entry(
        &mut self,
        key: &Ident,
        name: &str,
        input: ParseStream<'_>,
        errors: &mut Errors,
    ) -> syn::Result<()> {
        match name {
            "status" => {
                input.parse::<Token![=]>()?;
                let lit: LitInt = input.parse()?;
                match lit.base10_parse::<u16>() {
                    Ok(code) if (100..=599).contains(&code) => self.status = Some(code),
                    _ => errors.error(
                        lit.span(),
                        format!(
                            "status must be an HTTP status code between 100 and 599, found `{lit}`"
                        ),
                    ),
                }
            }
            "response_model" => {
                input.parse::<Token![=]>()?;
                self.response_model = Some(input.parse()?);
            }
            "tag" => {
                input.parse::<Token![=]>()?;
                self.tags.push(input.parse()?);
            }
            "tags" => {
                let content;
                parenthesized!(content in input);
                let tags = Punctuated::<LitStr, Token![,]>::parse_terminated(&content)?;
                self.tags.extend(tags);
            }
            "summary" => self.summary = Some(string_value(input)?),
            "description" => self.description = Some(string_value(input)?),
            "operation_id" => self.operation_id = Some(string_value(input)?),
            "deprecated" => self.deprecated = true,
            "hidden" => self.hidden = true,
            other => {
                errors.error(
                    key.span(),
                    format!("unknown route argument `{other}`; expected one of: {KEYS}"),
                );
                skip_value(input)?;
            }
        }
        if matches!(name, "deprecated" | "hidden") && input.peek(Token![=]) {
            errors.error(
                key.span(),
                format!("`{name}` is a flag and does not take a value"),
            );
            skip_value(input)?;
        }
        Ok(())
    }
}

fn string_value(input: ParseStream<'_>) -> syn::Result<LitStr> {
    input.parse::<Token![=]>()?;
    input.parse()
}

/// Skip an unknown argument's `= expr` / `(..)` value.
fn skip_value(input: ParseStream<'_>) -> syn::Result<()> {
    if input.peek(Token![=]) {
        input.parse::<Token![=]>()?;
        input.parse::<syn::Expr>()?;
    } else if input.peek(syn::token::Paren) {
        let content;
        parenthesized!(content in input);
        content.parse::<proc_macro2::TokenStream>()?;
    }
    Ok(())
}
