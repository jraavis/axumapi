//! The `routes![]` macro.

use crate::diag::Errors;
use crate::route::route_fn_ident;
use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::{Path, PathArguments, Token};

/// Expand `routes![a, m::b]` into a `Vec` of the generated route functions.
pub fn expand(input: TokenStream) -> TokenStream {
    match try_expand(input) {
        Ok(tokens) => tokens,
        Err(err) => err.into_compile_error(),
    }
}

fn try_expand(input: TokenStream) -> syn::Result<TokenStream> {
    let paths = Punctuated::<Path, Token![,]>::parse_terminated.parse2(input)?;
    let mut errors = Errors::default();
    let mut calls = Vec::new();
    for mut path in paths {
        if path
            .segments
            .iter()
            .any(|s| !matches!(s.arguments, PathArguments::None))
        {
            errors.spanned(&path, "generic arguments are not supported in `routes![]`");
            continue;
        }
        if let Some(last) = path.segments.last_mut() {
            last.ident = route_fn_ident(&last.ident);
        }
        calls.push(quote!(#path()));
    }
    errors.finish(quote!(::std::vec![#(#calls),*]))
}
