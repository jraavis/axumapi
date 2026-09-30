//! Code generation of the `ModelHooks` implementation.

use super::parse::{HELPERS, Hook, Mode, recognise};
use crate::diag::Errors;
use proc_macro2::TokenStream;
use quote::quote;
use std::collections::HashSet;
use syn::{Ident, ImplItem, ItemImpl, LitStr, Type};

pub fn run(args: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let mut errors = Errors::default();
    if !args.is_empty() {
        errors.spanned(args, "`#[model_hooks]` takes no arguments");
    }
    let mut imp: ItemImpl = syn::parse2(item)?;
    if let Some((_, path, _)) = &imp.trait_ {
        errors.spanned(path, "`#[model_hooks]` goes on an inherent `impl` block");
    }
    let mut hooks = Vec::new();
    for item in &mut imp.items {
        if let ImplItem::Fn(method) = item {
            if let Some(hook) = recognise(method, &mut errors) {
                hooks.push(hook);
            }
            method
                .attrs
                .retain(|a| !HELPERS.iter().any(|h| a.path().is_ident(h)));
        }
    }
    let methods = Methods::collect(&hooks, &mut errors);
    if let Some(err) = errors.into_error() {
        // Keep the (stripped) impl so unrelated errors are not drowned out.
        let err = err.into_compile_error();
        return Ok(quote!(#err #imp));
    }

    let validation = quote!(::siderite::validation);
    let private = quote!(::siderite::__private);
    let value = quote!(#private::serde_json::Value);
    let (impl_generics, _, where_clause) = imp.generics.split_for_impl();
    let self_ty = &imp.self_ty;
    let before_model = methods.before_model(&value, &validation);
    let before_field = methods.before_field(&value, &validation);
    let after_fields = methods.after_fields(&validation);
    let after_model = methods.after_model(&validation);
    let computed_fields = methods.computed_fields(&value, &validation);
    let computed_schema = methods.computed_schema(&value, &validation);
    let serialize_field = methods.serialize_field(&value, &validation);
    let serialize_model = methods.serialize_model(&value, &validation);
    Ok(quote! {
        #imp

        #[allow(clippy::needless_borrow)]
        impl #impl_generics #validation::ModelHooks for #self_ty #where_clause {
            #before_model
            #before_field
            #after_fields
            #after_model
            #computed_fields
            #computed_schema
            #serialize_field
            #serialize_model
        }
    })
}

/// A field reference by name; the literal's span makes typos point at it.
fn field_ident(lit: &LitStr, errors: &mut Errors) -> Option<Ident> {
    errors.absorb(lit.parse::<Ident>().map_err(|_| {
        syn::Error::new(
            lit.span(),
            "expected a field name (a valid Rust identifier)",
        )
    }))
}

/// Hooks grouped by generated method, in declaration order.
#[derive(Default)]
struct Methods<'a> {
    model_before: Vec<&'a Ident>,
    /// `(rust field, its identifier, validator)`.
    field_before: Vec<(String, Ident, &'a Ident)>,
    field_after: Vec<(LitStr, Ident, &'a Ident)>,
    model_after: Vec<&'a Ident>,
    computed: Vec<(&'a str, &'a Type, &'a Ident, &'a Option<String>)>,
    field_serializers: Vec<(String, Ident, &'a Ident)>,
    model_serializers: Vec<&'a Ident>,
}

impl<'a> Methods<'a> {
    fn collect(hooks: &'a [Hook], errors: &mut Errors) -> Self {
        let mut out = Self::default();
        let mut serialized = HashSet::new();
        let mut computed_keys = HashSet::new();
        for hook in hooks {
            match hook {
                Hook::FieldValidator {
                    mode,
                    fields,
                    method,
                } => {
                    for lit in fields {
                        let Some(ident) = field_ident(lit, errors) else {
                            continue;
                        };
                        match mode {
                            Mode::Before => out.field_before.push((lit.value(), ident, method)),
                            Mode::After => out.field_after.push((lit.clone(), ident, method)),
                        }
                    }
                }
                Hook::ModelValidator { mode, method } => match mode {
                    Mode::Before => out.model_before.push(method),
                    Mode::After => out.model_after.push(method),
                },
                Hook::Computed {
                    key,
                    ty,
                    method,
                    doc,
                } => {
                    if !computed_keys.insert(key.clone()) {
                        errors.spanned(method, format!("duplicate computed field `{key}`"));
                    }
                    out.computed.push((key, ty, method, doc));
                }
                Hook::FieldSerializer { field, method } => {
                    if !serialized.insert(field.value()) {
                        errors.spanned(field, "a field can have only one `field_serializer`");
                    }
                    if let Some(ident) = field_ident(field, errors) {
                        out.field_serializers.push((field.value(), ident, method));
                    }
                }
                Hook::ModelSerializer { method } => out.model_serializers.push(method),
            }
        }
        out
    }

    fn before_model(&self, value: &TokenStream, v: &TokenStream) -> TokenStream {
        if self.model_before.is_empty() {
            return TokenStream::new();
        }
        let calls = self.model_before.iter();
        quote! {
            fn before_model(input: &mut #value, ctx: &mut #v::ValidationContext) {
                #(ctx.check(Self::#calls(input));)*
            }
        }
    }

    fn before_field(&self, value: &TokenStream, v: &TokenStream) -> TokenStream {
        if self.field_before.is_empty() {
            return TokenStream::new();
        }
        // Group by field, keeping declaration order.
        let mut names: Vec<&String> = Vec::new();
        for (name, _, _) in &self.field_before {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        let arms = names.iter().map(|name| {
            let calls = self
                .field_before
                .iter()
                .filter(|(n, _, _)| n == *name)
                .map(|(_, _, method)| quote!(ctx.check(Self::#method(input));));
            quote!(#name => { #(#calls)* })
        });
        // Never executed: makes an unknown field name a compile error.
        let typo_checks = self
            .field_before
            .iter()
            .map(|(_, ident, _)| quote!(let _ = |__model: &Self| { let _ = &__model.#ident; };));
        quote! {
            fn before_field(
                field: &str,
                input: &mut #value,
                ctx: &mut #v::ValidationContext,
            ) {
                #(#typo_checks)*
                match field {
                    #(#arms)*
                    _ => {}
                }
            }
        }
    }

    fn after_fields(&self, v: &TokenStream) -> TokenStream {
        if self.field_after.is_empty() {
            return TokenStream::new();
        }
        let calls = self.field_after.iter().map(|(lit, ident, method)| {
            let name = lit.value();
            quote! {
                ctx.at(Self::__siderite_input_key(#name), |ctx| {
                    ctx.check(Self::#method(&self.#ident));
                });
            }
        });
        quote! {
            fn after_fields(&self, ctx: &mut #v::ValidationContext) {
                #(#calls)*
            }
        }
    }

    fn after_model(&self, v: &TokenStream) -> TokenStream {
        if self.model_after.is_empty() {
            return TokenStream::new();
        }
        let calls = self.model_after.iter();
        quote! {
            fn after_model(&self, ctx: &mut #v::ValidationContext) {
                #(ctx.check(Self::#calls(self));)*
            }
        }
    }

    fn computed_fields(&self, value: &TokenStream, v: &TokenStream) -> TokenStream {
        if self.computed.is_empty() {
            return TokenStream::new();
        }
        let inserts = self.computed.iter().map(|(key, _, method, _)| {
            quote! {
                if let ::core::option::Option::Some(__child) = opts.for_field(#key) {
                    let __value = #v::Dump::dump(&self.#method(), &__child)?;
                    if !opts.drops_value(&__value) {
                        out.insert(::std::string::ToString::to_string(#key), __value);
                    }
                }
            }
        });
        quote! {
            fn computed_fields(
                &self,
                out: &mut ::siderite::__private::serde_json::Map<::std::string::String, #value>,
                opts: &#v::DumpOptions,
            ) -> ::core::result::Result<(), #v::DumpError> {
                #(#inserts)*
                ::core::result::Result::Ok(())
            }
        }
    }

    fn computed_schema(&self, value: &TokenStream, v: &TokenStream) -> TokenStream {
        if self.computed.is_empty() {
            return TokenStream::new();
        }
        let inserts = self.computed.iter().map(|(key, ty, _, doc)| {
            let description = doc
                .as_ref()
                .map(|d| quote!(("description", ::siderite::__private::json!(#d)),));
            quote! {
                let __schema = ::siderite::__private::annotate(
                    registry.subschema::<#ty>(),
                    ::std::vec![
                        ("readOnly", ::siderite::__private::json!(true)),
                        #description
                    ],
                );
                properties.insert(::std::string::ToString::to_string(#key), __schema.into_value());
            }
        });
        quote! {
            fn computed_schema(
                properties: &mut ::siderite::__private::serde_json::Map<::std::string::String, #value>,
                registry: &mut #v::SchemaRegistry,
            ) {
                #(#inserts)*
            }
        }
    }

    fn serialize_field(&self, value: &TokenStream, v: &TokenStream) -> TokenStream {
        if self.field_serializers.is_empty() {
            return TokenStream::new();
        }
        let arms = self.field_serializers.iter().map(|(name, ident, method)| {
            quote! {
                #name => ::core::result::Result::Ok(
                    ::siderite::__private::serde_json::to_value(Self::#method(&self.#ident))?
                ),
            }
        });
        quote! {
            fn serialize_field(
                &self,
                field: &str,
                value: #value,
            ) -> ::core::result::Result<#value, #v::DumpError> {
                match field {
                    #(#arms)*
                    _ => ::core::result::Result::Ok(value),
                }
            }
        }
    }

    fn serialize_model(&self, value: &TokenStream, v: &TokenStream) -> TokenStream {
        if self.model_serializers.is_empty() {
            return TokenStream::new();
        }
        let steps = self
            .model_serializers
            .iter()
            .map(|method| quote!(let value = Self::#method(self, value);));
        quote! {
            fn serialize_model(
                &self,
                value: #value,
            ) -> ::core::result::Result<#value, #v::DumpError> {
                #(#steps)*
                ::core::result::Result::Ok(value)
            }
        }
    }
}
