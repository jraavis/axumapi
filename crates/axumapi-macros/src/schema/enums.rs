//! `derive(Schema)` for enums.

use super::attrs::{ContainerAttrs, VariantAttrs};
use super::fields::{object_expr, tuple_items};
use crate::diag::Errors;
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashSet;
use syn::{DataEnum, Fields, Variant};

/// serde enum representation.
enum Repr {
    External,
    Internal(String),
    Adjacent(String, String),
    Untagged,
}

fn representation(container: &ContainerAttrs, errors: &mut Errors) -> Repr {
    match (&container.tag, &container.content, container.untagged) {
        (Some(tag), _, true) => {
            errors.spanned(tag, "`untagged` cannot be combined with `tag`");
            Repr::Untagged
        }
        (None, _, true) => Repr::Untagged,
        (Some(tag), Some(content), false) => Repr::Adjacent(tag.value(), content.value()),
        (Some(tag), None, false) => Repr::Internal(tag.value()),
        (None, Some(content), false) => {
            errors.spanned(content, "`content` requires `tag`");
            Repr::External
        }
        (None, None, false) => Repr::External,
    }
}

/// Body of `Schema::schema` (before the type's own annotations).
pub fn body(data: &DataEnum, container: &ContainerAttrs, errors: &mut Errors) -> TokenStream {
    let repr = representation(container, errors);
    let mut seen = HashSet::new();
    let mut variants = Vec::new();
    for variant in &data.variants {
        let attrs = VariantAttrs::parse(&variant.attrs, errors);
        if attrs.skipped() {
            continue;
        }
        let raw = syn::ext::IdentExt::unraw(&variant.ident).to_string();
        let name = attrs.rename.clone().unwrap_or_else(|| {
            container
                .rename_all
                .map_or(raw.clone(), |r| r.apply_to_variant(&raw))
        });
        if !seen.insert(name.clone()) {
            errors.spanned(&variant.ident, format!("duplicate variant name `{name}`"));
            continue;
        }
        variants.push((variant, attrs, name));
    }

    let all_unit = variants
        .iter()
        .all(|(v, _, _)| matches!(v.fields, Fields::Unit));
    if all_unit && matches!(repr, Repr::External) && !variants.is_empty() {
        let names = variants.iter().map(|(_, _, name)| name);
        return quote!(::axumapi::__private::string_enum(&[#(#names),*]));
    }
    let schemas = variants
        .iter()
        .map(|(v, attrs, name)| variant_schema(&repr, container, v, attrs, name, errors));
    let schemas: Vec<TokenStream> = schemas.collect();
    quote!(::axumapi::__private::one_of(::std::vec![#(#schemas),*]))
}

fn variant_schema(
    repr: &Repr,
    container: &ContainerAttrs,
    variant: &Variant,
    attrs: &VariantAttrs,
    name: &str,
    errors: &mut Errors,
) -> TokenStream {
    let private = quote!(::axumapi::__private);
    // The payload of the variant, without any tagging.
    let payload: Option<TokenStream> = match &variant.fields {
        Fields::Unit => None,
        Fields::Unnamed(fields) => {
            let mut items = tuple_items(fields, errors);
            Some(if items.len() == 1 {
                items.remove(0)
            } else {
                quote!(#private::tuple(::std::vec![#(#items),*]))
            })
        }
        Fields::Named(fields) => {
            let rule = attrs.rename_all.or(container.rename_all_fields);
            let tag = match repr {
                Repr::Internal(tag) => Some((tag.as_str(), name)),
                _ => None,
            };
            let object = object_expr(
                fields,
                rule,
                false,
                container.deny_unknown_fields,
                tag,
                errors,
            );
            if tag.is_some() {
                // Internally tagged struct variants are already complete.
                return object;
            }
            Some(object)
        }
    };
    match (repr, payload) {
        (Repr::External, None) => quote!(#private::const_string(#name)),
        (Repr::External, Some(p)) => quote!(#private::externally_tagged(#name, #p)),
        (Repr::Untagged, None) => quote!(::axumapi::validation::SchemaObject::of_type("null")),
        (Repr::Untagged, Some(p)) => p,
        (Repr::Internal(tag), None) => quote!(#private::tagged_unit(#tag, #name)),
        (Repr::Internal(tag), Some(p)) => {
            if matches!(variant.fields, Fields::Unnamed(ref f) if f.unnamed.len() == 1) {
                quote!(#private::internally_tagged_newtype(#tag, #name, #p))
            } else {
                errors.spanned(
                    variant,
                    "tuple variants cannot be internally tagged (serde rejects them too)",
                );
                quote!(#private::const_string(#name))
            }
        }
        (Repr::Adjacent(tag, content), None) => {
            quote!(#private::adjacently_tagged(#tag, #name, #content, ::core::option::Option::None))
        }
        (Repr::Adjacent(tag, content), Some(p)) => {
            quote!(#private::adjacently_tagged(#tag, #name, #content, ::core::option::Option::Some(#p)))
        }
    }
}
