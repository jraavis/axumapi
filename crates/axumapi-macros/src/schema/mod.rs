//! `#[derive(Schema)]`.

mod attrs;
mod enums;
mod fields;
mod rename;
mod strukt;

use crate::diag::Errors;
use crate::docs;
use attrs::ContainerAttrs;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, parse_quote};

/// Expand `#[derive(Schema)]`.
pub fn derive(input: &DeriveInput) -> syn::Result<TokenStream> {
    let mut errors = Errors::default();
    let container = ContainerAttrs::parse(&input.attrs, &mut errors);
    let body = match &input.data {
        Data::Struct(data) => strukt::body(data, &container, &mut errors),
        Data::Enum(data) => enums::body(data, &container, &mut errors),
        Data::Union(_) => {
            errors.spanned(input, "derive(Schema) does not support unions");
            TokenStream::new()
        }
    };
    let keywords = fields::doc_keywords(docs::description(&input.attrs));
    let body = fields::annotate(body, &keywords);
    errors.finish(())?;

    let ident = &input.ident;
    let name =
        if container.inline || (container.name.is_none() && !input.generics.params.is_empty()) {
            quote!(::core::option::Option::None)
        } else {
            let name = container
                .name
                .as_ref()
                .map_or_else(|| ident.to_string(), syn::LitStr::value);
            quote!(::core::option::Option::Some(#name))
        };

    let mut generics = input.generics.clone();
    for param in input.generics.type_params() {
        let ty = &param.ident;
        generics
            .make_where_clause()
            .predicates
            .push(parse_quote!(#ty: ::axumapi::validation::Schema + 'static));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics ::axumapi::validation::Schema for #ident #ty_generics #where_clause {
            fn schema_name() -> ::core::option::Option<&'static str> {
                #name
            }

            #[allow(unused_variables)]
            fn schema(registry: &mut ::axumapi::validation::SchemaRegistry)
                -> ::axumapi::validation::SchemaObject
            {
                #body
            }
        }
    })
}
