//! `derive(Validate)` for enums.
//!
//! * Enums whose variants are all unit variants and that use serde's default
//!   (externally tagged) representation are strings on the wire: `prepare`
//!   checks the input is one of the variant names (error code `enum`),
//!   honouring `rename`, `rename_all` and `alias`.
//! * Every other enum (data variants, `tag`, `untagged`) gets an implementation
//!   that validates nothing itself: `prepare` and `validate` are no-ops and
//!   Serde reports malformed input. Fields of data variants are *not*
//!   validated, so validation rules on them are a compile error; put the
//!   data in a struct that derives `Validate`.

use super::constraints::has_rules;
use crate::attrs::field::FieldOptions;
use crate::attrs::model::Container;
use crate::attrs::serde::VariantSerde;
use crate::diag::Errors;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{DataEnum, Fields};

pub fn derive(
    header: &TokenStream,
    data: &DataEnum,
    container: &Container,
    errors: &mut Errors,
) -> TokenStream {
    let mut names: Vec<String> = Vec::new();
    let mut all_unit = true;
    for variant in &data.variants {
        let serde = VariantSerde::parse(&variant.attrs, errors);
        if !matches!(variant.fields, Fields::Unit) {
            all_unit = false;
        }
        // Fields of data variants are never validated; reject rules on them
        // rather than accepting input that silently skips them.
        for field in &variant.fields {
            if has_rules(&FieldOptions::parse(&field.attrs, errors)) {
                errors.spanned(
                    field,
                    "validation rules on enum variant fields are not enforced; \
                     move the data into a struct that derives `Validate`",
                );
            }
        }
        if serde.skip_deserializing {
            continue;
        }
        let raw = syn::ext::IdentExt::unraw(&variant.ident).to_string();
        let name = serde.rename.clone().unwrap_or_else(|| {
            container
                .serde
                .rename_all
                .map_or(raw.clone(), |rule| rule.apply_to_variant(&raw))
        });
        names.push(name);
        names.extend(serde.aliases);
    }
    let external = container.serde.tag.is_none() && !container.serde.untagged;
    if !(all_unit && external) {
        return quote!(#header {});
    }
    let message = format!("input must be one of: {}", names.join(", "));
    quote! {
        #header {
            fn prepare(
                input: &mut ::siderite::__private::serde_json::Value,
                ctx: &mut ::siderite::validation::ValidationContext,
            ) {
                const NAMES: &[&str] = &[#(#names),*];
                if !input.as_str().is_some_and(|s| NAMES.contains(&s)) {
                    ctx.error("enum", #message);
                }
            }
        }
    }
}
