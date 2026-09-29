//! Emits `impl Model` and the typed `Field` constants.

use super::meta::static_ident;
use super::plan::ModelPlan;
use proc_macro2::TokenStream;
use quote::quote;

/// The field constants (`User::name`) as an inherent impl.
pub fn field_consts(plan: &ModelPlan<'_>) -> TokenStream {
    let ident = plan.ident;
    let consts = plan.columns().map(|field| {
        let (vis, name, ty, column) = (field.vis, field.ident, field.ty, &field.column);
        let doc = format!("Typed handle of the `{column}` column.");
        quote! {
            #[doc = #doc]
            #[allow(non_upper_case_globals)]
            #vis const #name: ::axumapi::orm::Field<#ident, #ty> =
                ::axumapi::orm::Field::new(#column);
        }
    });
    quote! {
        #[automatically_derived]
        impl #ident {
            #(#consts)*
        }
    }
}

/// `impl Model for T`.
pub fn model_impl(plan: &ModelPlan<'_>) -> TokenStream {
    let orm = quote!(::axumapi::orm);
    let ident = plan.ident;
    let meta = static_ident();
    let pk = plan.pk_field();
    let (pk_ident, pk_ty, pk_column) = (pk.ident, pk.ty, &pk.column);
    let is_unsaved = if pk.options.orm.auto {
        quote!(self.#pk_ident == <#pk_ty as ::core::default::Default>::default())
    } else {
        quote!(false)
    };
    let values = plan.columns().map(|f| {
        let (field, column) = (f.ident, &f.column);
        quote!((#column, #orm::DbType::to_value(&self.#field)))
    });
    let reads = plan.fields.iter().map(|f| {
        let field = f.ident;
        if f.is_column() {
            let column = &f.column;
            quote!(#field: #orm::read_column(row, prefix, #column)?)
        } else {
            quote!(#field: ::core::default::Default::default())
        }
    });
    quote! {
        #[automatically_derived]
        impl #orm::Model for #ident {
            type Pk = #pk_ty;
            const META: &'static #orm::ModelMeta = &#meta;

            fn pk(&self) -> #pk_ty {
                ::core::clone::Clone::clone(&self.#pk_ident)
            }

            fn set_pk(&mut self, value: #orm::Value) -> ::core::result::Result<(), #orm::QueryError> {
                self.#pk_ident = #orm::types::decode(#pk_column, value)?;
                ::core::result::Result::Ok(())
            }

            fn is_unsaved(&self) -> bool {
                #is_unsaved
            }

            fn to_values(&self) -> ::std::vec::Vec<(&'static str, #orm::Value)> {
                ::std::vec![#(#values),*]
            }

            fn from_row(
                row: &#orm::Row,
                prefix: &str,
            ) -> ::core::result::Result<Self, #orm::QueryError> {
                ::core::result::Result::Ok(Self { #(#reads),* })
            }
        }
    }
}
