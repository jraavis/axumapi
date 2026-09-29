//! Code generation for fields: properties, tuple items and annotations.

use super::attrs::{FieldAttrs, Keyword};
use super::rename::RenameRule;
use crate::diag::Errors;
use crate::docs;
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashSet;
use syn::{Field, FieldsNamed, FieldsUnnamed, Type};

/// Wrap `base` (an expression producing a `SchemaObject`) with schema keywords.
pub fn annotate(base: TokenStream, keywords: &[Keyword]) -> TokenStream {
    if keywords.is_empty() {
        return base;
    }
    let entries = keywords
        .iter()
        .map(|(key, value)| quote!((#key, ::axumapi::__private::json!(#value))));
    quote!(::axumapi::__private::annotate(#base, ::std::vec![#(#entries),*]))
}

/// The `description` keyword for a type's doc comment.
pub fn doc_keywords(description: Option<String>) -> Vec<Keyword> {
    description
        .map(|d| ("description", quote!(#d)))
        .into_iter()
        .collect()
}

fn field_schema(ty: &Type, attrs: &FieldAttrs) -> TokenStream {
    annotate(quote!(registry.subschema::<#ty>()), &attrs.keywords)
}

/// Whether `ty` is syntactically `Option<..>`.
fn is_option(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    path.qself.is_none()
        && path.path.segments.last().is_some_and(|seg| {
            seg.ident == "Option" && matches!(seg.arguments, syn::PathArguments::AngleBracketed(_))
        })
}

fn field_attrs(field: &Field, errors: &mut Errors) -> FieldAttrs {
    FieldAttrs::parse(&field.attrs, docs::description(&field.attrs), errors)
}

/// Statements adding each non-skipped named field to `__object`.
///
/// `reserved` lists property names already taken (e.g. an enum tag).
pub fn named_properties(
    fields: &FieldsNamed,
    rule: Option<RenameRule>,
    container_default: bool,
    reserved: &[String],
    errors: &mut Errors,
) -> Vec<TokenStream> {
    let mut seen: HashSet<String> = reserved.iter().cloned().collect();
    let mut out = Vec::new();
    for field in &fields.named {
        let attrs = field_attrs(field, errors);
        if attrs.skipped() {
            continue;
        }
        let Some(ident) = &field.ident else { continue };
        let raw = syn::ext::IdentExt::unraw(ident).to_string();
        let name = attrs
            .rename
            .clone()
            .or_else(|| attrs.alias.clone())
            .unwrap_or_else(|| rule.map_or(raw.clone(), |r| r.apply_to_field(&raw)));
        if !seen.insert(name.clone()) {
            errors.spanned(ident, format!("duplicate property name `{name}`"));
            continue;
        }
        let required = !(is_option(&field.ty)
            || attrs.default
            || container_default
            || attrs.skip_deserializing);
        let schema = field_schema(&field.ty, &attrs);
        out.push(quote!(__object.property(#name, #required, #schema);));
    }
    out
}

/// Schema expressions for the elements of a tuple struct or variant.
pub fn tuple_items(fields: &FieldsUnnamed, errors: &mut Errors) -> Vec<TokenStream> {
    fields
        .unnamed
        .iter()
        .map(|field| {
            let attrs = field_attrs(field, errors);
            if attrs.skip_serializing || attrs.skip_deserializing {
                errors.spanned(
                    field,
                    "`skip` is not supported on tuple fields by derive(Schema)",
                );
            }
            field_schema(&field.ty, &attrs)
        })
        .collect()
}

/// Object schema expression for named fields.
///
/// `tag` optionally adds a leading required constant property
/// `(tag_name, variant_name)` for internally tagged enums.
pub fn object_expr(
    fields: &FieldsNamed,
    rule: Option<RenameRule>,
    container_default: bool,
    deny_unknown_fields: bool,
    tag: Option<(&str, &str)>,
    errors: &mut Errors,
) -> TokenStream {
    let reserved: Vec<String> = tag.iter().map(|(t, _)| (*t).to_owned()).collect();
    let props = named_properties(fields, rule, container_default, &reserved, errors);
    let tag_stmt = tag.map(|(tag, name)| {
        quote!(__object.property(#tag, true, ::axumapi::__private::const_string(#name));)
    });
    quote! {{
        let mut __object = ::axumapi::__private::ObjectBuilder::new();
        #tag_stmt
        #(#props)*
        __object.build(#deny_unknown_fields)
    }}
}
