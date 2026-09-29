//! `#[derive(Validate)]`.

mod constraints;
mod enums;
mod structs;

use crate::attrs::model::Container;
use crate::diag::Errors;
use crate::generics::with_bound;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, parse_quote};

/// Expand `#[derive(Validate)]`.
pub fn derive(input: &DeriveInput) -> syn::Result<TokenStream> {
    let mut errors = Errors::default();
    let container = Container::parse(&input.attrs, &mut errors);
    let mut generics = with_bound(&input.generics, &quote!(::axumapi::validation::Validate));
    if container.options.hooks {
        // Defer the (user-chosen) bounds of the hooks impl to the use site.
        let ident = &input.ident;
        let (_, ty_generics, _) = input.generics.split_for_impl();
        generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(for<'__a> #ident #ty_generics: ::axumapi::validation::ModelHooks));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let ident = &input.ident;
    let header = quote!(impl #impl_generics ::axumapi::validation::Validate for #ident #ty_generics #where_clause);
    let tokens = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => {
                structs::named(input, fields, &container, &generics, &mut errors)
            }
            Fields::Unnamed(fields) => structs::tuple(&header, fields, &mut errors),
            Fields::Unit => quote!(#header {}),
        },
        Data::Enum(data) => enums::derive(&header, data, &container, &mut errors),
        Data::Union(_) => {
            errors.spanned(input, "derive(Validate) does not support unions");
            TokenStream::new()
        }
    };
    errors.finish(quote!(const _: () = { #tokens };))
}
