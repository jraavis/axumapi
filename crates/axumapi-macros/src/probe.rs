//! How generated code reaches a model's `ModelHooks` implementation.
//!
//! * **Probe** (concrete types): autoref specialisation. The hook runs if
//!   the type implements `ModelHooks`, otherwise the call is a no-op, so
//!   hooks need no opt-in flag.
//! * **Direct** (`#[model_config(hooks)]`): `<Self as ModelHooks>` is called
//!   unconditionally. Required for generic types, where probing cannot see
//!   the impl.
//! * **Off** (generic type without `hooks`): no hook calls are emitted.

use crate::attrs::model::Container;
use proc_macro2::TokenStream;
use quote::quote;
use syn::Generics;

/// Hook dispatch strategy for one type.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hooks {
    Probe,
    Direct,
    Off,
}

impl Hooks {
    pub fn for_type(generics: &Generics, container: &Container) -> Self {
        let generic = generics
            .params
            .iter()
            .any(|p| !matches!(p, syn::GenericParam::Lifetime(_)));
        if container.options.hooks {
            Self::Direct
        } else if generic {
            Self::Off
        } else {
            Self::Probe
        }
    }

    /// Items to place before the impls (trait imports for probing).
    pub fn preamble(self) -> TokenStream {
        if self == Self::Probe {
            quote! {
                #[allow(unused_imports)]
                use ::axumapi::__private::{ViaDefault as _, ViaHooks as _};
            }
        } else {
            TokenStream::new()
        }
    }

    /// Call `method` of `ModelHooks`; `args` are the arguments in the
    /// probe's order (the model comes first for `&self` methods).
    fn call(self, method: &str, args: TokenStream) -> TokenStream {
        let method = quote::format_ident!("{}", method);
        match self {
            Self::Probe => quote!((&&::axumapi::__private::Probe::<Self>::new()).#method(#args)),
            Self::Direct => quote!(<Self as ::axumapi::validation::ModelHooks>::#method(#args)),
            Self::Off => unreachable_call(),
        }
    }

    /// `before_model(input, ctx)` statement.
    pub fn before_model(self) -> TokenStream {
        self.statement("before_model", quote!(input, ctx))
    }

    /// `before_field("name", slot, ctx)` statement.
    pub fn before_field(self, name: &str) -> TokenStream {
        self.statement("before_field", quote!(#name, __slot, ctx))
    }

    /// `after_fields(self, ctx)` statement.
    pub fn after_fields(self) -> TokenStream {
        self.statement("after_fields", quote!(self, ctx))
    }

    /// `after_model(self, ctx)` statement.
    pub fn after_model(self) -> TokenStream {
        self.statement("after_model", quote!(self, ctx))
    }

    /// `computed_schema(properties, registry)` statement.
    pub fn computed_schema(self, properties: TokenStream) -> TokenStream {
        self.statement("computed_schema", quote!(#properties, registry))
    }

    /// `computed_fields(self, &mut map, opts)?` statement.
    pub fn computed_fields(self) -> TokenStream {
        match self {
            Self::Off => TokenStream::new(),
            other => {
                let call = other.call("computed_fields", quote!(self, &mut __map, opts));
                quote!(#call?;)
            }
        }
    }

    /// Expression yielding the field's value after `serialize_field`.
    pub fn serialize_field(self, name: &str, value: TokenStream) -> TokenStream {
        match self {
            Self::Off => value,
            other => {
                let call = other.call("serialize_field", quote!(self, #name, #value));
                quote!(#call?)
            }
        }
    }

    /// Expression yielding the `Result` of `serialize_model`.
    pub fn serialize_model(self, value: TokenStream) -> TokenStream {
        match self {
            Self::Off => quote!(::core::result::Result::Ok(#value)),
            other => other.call("serialize_model", quote!(self, #value)),
        }
    }

    fn statement(self, method: &str, args: TokenStream) -> TokenStream {
        match self {
            Self::Off => TokenStream::new(),
            other => {
                let call = other.call(method, args);
                quote!(#call;)
            }
        }
    }
}

fn unreachable_call() -> TokenStream {
    TokenStream::new()
}
