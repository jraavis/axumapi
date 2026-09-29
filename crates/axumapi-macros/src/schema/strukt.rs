//! `derive(Schema)` for structs.

use super::attrs::ContainerAttrs;
use super::fields::{object_expr, tuple_items};
use crate::diag::Errors;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{DataStruct, Fields};

/// Body of `Schema::schema` (before the type's own annotations).
pub fn body(data: &DataStruct, container: &ContainerAttrs, errors: &mut Errors) -> TokenStream {
    match &data.fields {
        Fields::Named(fields) => object_expr(
            fields,
            container.rename_all,
            container.default,
            container.deny_unknown_fields,
            None,
            errors,
        ),
        Fields::Unnamed(fields) => {
            let mut items = tuple_items(fields, errors);
            if items.len() == 1 {
                // Newtype: transparent, like serde.
                items.remove(0)
            } else {
                quote!(::axumapi::__private::tuple(::std::vec![#(#items),*]))
            }
        }
        Fields::Unit => quote!(::axumapi::validation::SchemaObject::of_type("null")),
    }
}
