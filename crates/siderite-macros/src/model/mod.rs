//! `#[derive(Model)]`.

mod access;
mod field;
mod imp;
mod meta;
mod options;
mod plan;

use crate::diag::Errors;
use options::ModelOptions;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields};

/// Expand `#[derive(Model)]`.
pub fn derive(input: &DeriveInput) -> syn::Result<TokenStream> {
    let mut errors = Errors::default();
    if !input.generics.params.is_empty() {
        errors.spanned(
            &input.generics,
            "derive(Model) does not support generic types",
        );
    }
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => fields,
            _ => return unsupported(errors, input),
        },
        _ => return unsupported(errors, input),
    };
    let options = ModelOptions::parse(&input.attrs, &mut errors);
    let plan = plan::build(input, fields, options, &mut errors);
    // `build` only returns `None` after recording an error.
    let Some(plan) = errors.finish(plan)? else {
        return Ok(TokenStream::new());
    };
    let meta = meta::expand(&plan);
    let consts = imp::field_consts(&plan);
    let model = imp::model_impl(&plan);
    let accessors = access::expand(&plan);
    Ok(quote! {
        const _: () = {
            #meta
            #consts
            #model
            #accessors
        };
    })
}

fn unsupported(mut errors: Errors, input: &DeriveInput) -> syn::Result<TokenStream> {
    errors.spanned(
        &input.ident,
        "derive(Model) supports structs with named fields",
    );
    errors.finish(TokenStream::new())
}
