//! Generic parameter helpers for generated impls.

use proc_macro2::TokenStream;
use syn::{Generics, parse_quote};

/// `generics` with `param: bound` added for every type parameter.
pub fn with_bound(generics: &Generics, bound: &TokenStream) -> Generics {
    let mut out = generics.clone();
    for param in generics.type_params() {
        let ident = &param.ident;
        out.make_where_clause()
            .predicates
            .push(parse_quote!(#ident: #bound));
    }
    out
}
