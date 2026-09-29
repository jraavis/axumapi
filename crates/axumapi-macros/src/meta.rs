//! Helpers for parsing `#[name(key = value, ...)]` attributes.

use crate::diag::Errors;
use proc_macro2::TokenStream;
use syn::meta::ParseNestedMeta;
use syn::{Attribute, Expr, LitBool, LitStr, Token, parenthesized, token};

/// Run `f` for every nested item of every `#[name(...)]` attribute in `attrs`.
///
/// Syntax errors abort the attribute they occur in; all errors are collected.
pub fn for_each_meta(
    attrs: &[Attribute],
    name: &str,
    errors: &mut Errors,
    mut f: impl FnMut(ParseNestedMeta<'_>, &mut Errors) -> syn::Result<()>,
) {
    for attr in attrs.iter().filter(|a| a.path().is_ident(name)) {
        if let Err(err) = attr.parse_nested_meta(|meta| f(meta, errors)) {
            errors.push(err);
        }
    }
}

/// Consume (and ignore) the value of an unrecognised key: `= expr` or `(...)`.
pub fn skip_value(meta: &ParseNestedMeta<'_>) -> syn::Result<()> {
    if meta.input.peek(Token![=]) {
        meta.value()?.parse::<Expr>()?;
    } else if meta.input.peek(token::Paren) {
        let content;
        parenthesized!(content in meta.input);
        content.parse::<TokenStream>()?;
    }
    Ok(())
}

/// `key = "literal"`.
pub fn lit_str(meta: &ParseNestedMeta<'_>) -> syn::Result<LitStr> {
    meta.value()?.parse()
}

/// A flag: bare `key` or `key = true|false`.
pub fn flag(meta: &ParseNestedMeta<'_>) -> syn::Result<bool> {
    if meta.input.peek(Token![=]) {
        Ok(meta.value()?.parse::<LitBool>()?.value)
    } else {
        Ok(true)
    }
}
