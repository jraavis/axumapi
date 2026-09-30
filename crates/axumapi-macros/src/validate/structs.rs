//! `derive(Validate)` for structs.

use super::constraints::{checks, has_rules};
use crate::attrs::model::Container;
use crate::attrs::plan::{FieldPlan, resolve_named, resolve_unnamed};
use crate::diag::Errors;
use crate::probe::Hooks;
use proc_macro2::{Literal, TokenStream};
use quote::quote;
use syn::{DeriveInput, FieldsNamed, FieldsUnnamed, Generics};

/// Named-field struct: the shape of the reference model in
/// `axumapi_validation::model`.
pub fn named(
    input: &DeriveInput,
    fields: &FieldsNamed,
    container: &Container,
    generics: &Generics,
    errors: &mut Errors,
) -> TokenStream {
    let plans = resolve_named(
        fields,
        container.serde.rename_all,
        container.serde.default.is_some(),
        &[],
        errors,
    );
    let hooks = Hooks::for_type(&input.generics, container);
    let validation = quote!(::axumapi::validation);
    let value = quote!(::axumapi::__private::serde_json::Value);
    let populate = container.options.populate_by_name;

    let mut statics = Vec::new();
    let mut specs = Vec::new();
    let mut arms = Vec::new();
    let mut validate = Vec::new();
    let mut keys = Vec::new();
    for plan in &plans {
        let key = &plan.key;
        let rust_name = &plan.rust_name;
        keys.push(quote!(#rust_name => #key,));
        if !plan.read {
            continue;
        }
        let index = specs.len();
        let aliases = plan.input_aliases(populate);
        let required = plan.required;
        specs.push(quote! {
            #validation::model::FieldSpec {
                key: #key,
                aliases: &[#(#aliases),*],
                required: #required,
            }
        });
        let field_checks = checks(&plan.options, index);
        statics.extend(field_checks.statics);
        arms.push(prepare_arm(plan, index, &field_checks.stmts, hooks));
        validate.push(validate_stmt(plan));
    }

    let ident = &input.ident;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let (_, plain_ty_generics, plain_where) = input.generics.split_for_impl();
    let plain_impl_generics = input.generics.split_for_impl().0;
    let preamble = hooks.preamble();
    let config = container.config_tokens();
    let before_model = hooks.before_model();
    let after_fields = hooks.after_fields();
    let after_model = hooks.after_model();
    quote! {
        #preamble
        #(#statics)*
        const __CONFIG: #validation::ModelConfig = #config;
        const __FIELDS: &[#validation::model::FieldSpec] = &[#(#specs),*];

        #[allow(unused_variables, clippy::needless_borrow, clippy::match_single_binding)]
        impl #impl_generics #validation::Validate for #ident #ty_generics #where_clause {
            fn prepare(input: &mut #value, ctx: &mut #validation::ValidationContext) {
                #before_model
                #validation::model::prepare_object(
                    input,
                    ctx,
                    __CONFIG,
                    __FIELDS,
                    |__index, __slot, ctx| match __index {
                        #(#arms)*
                        _ => {}
                    },
                );
            }

            fn validate(&self, ctx: &mut #validation::ValidationContext) {
                ctx.with_config(__CONFIG, |ctx| {
                    #(#validate)*
                    #after_fields
                    #after_model
                });
            }
        }

        impl #plain_impl_generics #ident #plain_ty_generics #plain_where {
            /// Input key of a field, used by `#[model_hooks]`.
            #[doc(hidden)]
            #[allow(dead_code)]
            pub fn __axumapi_input_key(field: &str) -> &'static str {
                match field {
                    #(#keys)*
                    _ => "",
                }
            }
        }
    }
}

fn prepare_arm(
    plan: &FieldPlan<'_>,
    index: usize,
    stmts: &[TokenStream],
    hooks: Hooks,
) -> TokenStream {
    let validation = quote!(::axumapi::validation);
    let ty = plan.ty;
    let before_field = hooks.before_field(&plan.rust_name);
    let prepare = if plan.options.strict {
        quote! {
            let __config = #validation::ModelConfig { strict: true, ..*ctx.config() };
            ctx.with_config(__config, |ctx| {
                <#ty as #validation::Validate>::prepare(__slot, ctx);
            });
        }
    } else {
        quote!(<#ty as #validation::Validate>::prepare(__slot, ctx);)
    };
    let index = Literal::usize_unsuffixed(index);
    if stmts.is_empty() {
        quote!(#index => { #before_field #prepare })
    } else {
        quote! {
            #index => {
                #before_field
                let __before = ctx.error_count();
                #prepare
                if ctx.error_count() == __before {
                    #(#stmts)*
                }
            }
        }
    }
}

fn validate_stmt(plan: &FieldPlan<'_>) -> TokenStream {
    let validation = quote!(::axumapi::validation);
    let (ident, ty, key) = (plan.ident, plan.ty, &plan.key);
    let validators = plan.options.validators.iter();
    quote! {
        ctx.at(#key, |ctx| {
            <#ty as #validation::Validate>::validate(&self.#ident, ctx);
            #(ctx.check(#validators(&self.#ident));)*
        });
    }
}

/// Tuple struct: a newtype delegates to its inner type.
pub fn tuple(header: &TokenStream, fields: &FieldsUnnamed, errors: &mut Errors) -> TokenStream {
    let validation = quote!(::axumapi::validation);
    let elements = resolve_unnamed(fields, errors);
    let [element] = elements.as_slice() else {
        // Only newtypes are validated; rules on wider tuples would be ignored.
        for (field, element) in fields.unnamed.iter().zip(&elements) {
            if has_rules(&element.options) {
                errors.spanned(
                    field,
                    "validation rules are only supported on single-field tuple structs; \
                     use a struct with named fields",
                );
            }
        }
        return quote!(#header {});
    };
    let ty = element.ty;
    let field_checks = checks(&element.options, 0);
    let statics = &field_checks.statics;
    let stmts = &field_checks.stmts;
    let validators = element.options.validators.iter();
    quote! {
        #(#statics)*
        #[allow(unused_variables)]
        #header {
            fn prepare(
                input: &mut ::axumapi::__private::serde_json::Value,
                ctx: &mut #validation::ValidationContext,
            ) {
                let __slot = input;
                let __before = ctx.error_count();
                <#ty as #validation::Validate>::prepare(__slot, ctx);
                if ctx.error_count() == __before {
                    #(#stmts)*
                }
            }

            fn validate(&self, ctx: &mut #validation::ValidationContext) {
                <#ty as #validation::Validate>::validate(&self.0, ctx);
                #(ctx.check(#validators(&self.0));)*
            }
        }
    }
}
