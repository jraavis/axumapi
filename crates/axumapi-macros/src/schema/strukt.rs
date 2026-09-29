//! `derive(Schema)` for structs.

use super::fields::{ObjectSpec, elements, object_expr};
use crate::attrs::model::Container;
use crate::diag::Errors;
use crate::probe::Hooks;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{DataStruct, Fields};

/// Body of `Schema::schema` (before the type's own annotations).
pub fn body(
    data: &DataStruct,
    container: &Container,
    hooks: Hooks,
    errors: &mut Errors,
) -> TokenStream {
    match &data.fields {
        Fields::Named(fields) => {
            let spec = ObjectSpec {
                rule: container.serde.rename_all,
                container_default: container.serde.default.is_some(),
                deny_unknown_fields: container.forbids_extra(),
                tag: None,
                computed: Some(hooks),
            };
            object_expr(fields, &spec, errors)
        }
        Fields::Unnamed(fields) => {
            let mut items = elements(fields, errors);
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
