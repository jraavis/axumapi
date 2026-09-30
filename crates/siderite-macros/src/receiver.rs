//! `#[receiver(signal, model = M)]` expansion.
//!
//! ```ignore
//! #[receiver(post_save, model = User)]
//! async fn audit(user: &User, event: &SignalEvent<'_>) -> Result<(), SignalError> { .. }
//! ```
//!
//! keeps `audit` unchanged and adds
//! `fn audit_receiver() -> ::siderite::orm::signals::Receiver`.

use crate::diag::Errors;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{FnArg, Ident, ItemFn, Path, Token};

/// Signal names accepted by the attribute, with their `SignalName` variant.
const SIGNALS: &[(&str, &str)] = &[
    ("pre_save", "PreSave"),
    ("post_save", "PostSave"),
    ("pre_delete", "PreDelete"),
    ("post_delete", "PostDelete"),
    ("m2m_changed", "M2mChanged"),
];

/// Parsed attribute arguments: `post_save, model = User`.
struct ReceiverArgs {
    variant: Ident,
    model: Path,
}

impl Parse for ReceiverArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let signal: Ident = input.parse().map_err(|e| {
            syn::Error::new(
                e.span(),
                format!("expected a signal name: {}", signal_list()),
            )
        })?;
        let variant = SIGNALS
            .iter()
            .find(|(name, _)| signal == name)
            .map(|(_, variant)| Ident::new(variant, signal.span()))
            .ok_or_else(|| {
                syn::Error::new(
                    signal.span(),
                    format!("unknown signal `{signal}`; expected {}", signal_list()),
                )
            })?;
        input.parse::<Token![,]>().map_err(|e| {
            syn::Error::new(
                e.span(),
                "expected `, model = <ModelType>` after the signal name",
            )
        })?;
        let key: Ident = input.parse()?;
        if key != "model" {
            return Err(syn::Error::new(
                key.span(),
                format!("unknown argument `{key}`; expected `model = <ModelType>`"),
            ));
        }
        input.parse::<Token![=]>()?;
        let model: Path = input.parse()?;
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
        }
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after `model = <ModelType>`"));
        }
        Ok(Self { variant, model })
    }
}

fn signal_list() -> String {
    SIGNALS
        .iter()
        .map(|(name, _)| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Expand `#[receiver]`: the original function plus `<name>_receiver()`.
pub(crate) fn expand(args: TokenStream, item: TokenStream) -> TokenStream {
    match try_expand(args, &item) {
        Ok(tokens) => tokens,
        // Keep the function so unrelated errors do not cascade.
        Err(err) => {
            let error = err.into_compile_error();
            quote!(#item #error)
        }
    }
}

fn try_expand(args: TokenStream, item: &TokenStream) -> syn::Result<TokenStream> {
    let args: ReceiverArgs = syn::parse2(args)?;
    let func: ItemFn = syn::parse2(item.clone())?;
    check_signature(&func)?;

    let ReceiverArgs { variant, model } = args;
    let name = &func.sig.ident;
    let vis = &func.vis;
    let ctor = format_ident!("{}_receiver", syn::ext::IdentExt::unraw(name));
    let doc = format!("Receiver for [`{name}`], to pass to `Signals::connect`.");
    Ok(quote! {
        #func

        #[doc = #doc]
        #vis fn #ctor() -> ::siderite::orm::signals::Receiver {
            ::siderite::orm::signals::Receiver::new::<#model, _>(
                ::siderite::orm::signals::SignalName::#variant,
                |instance, event| ::std::boxed::Box::pin(#name(instance, event)),
            )
        }
    })
}

/// A receiver is `async fn(&Model, &SignalEvent<'_>) -> Result<(), SignalError>`.
fn check_signature(func: &ItemFn) -> syn::Result<()> {
    let sig = &func.sig;
    let mut errors = Errors::default();
    if sig.asyncness.is_none() {
        errors.spanned(sig.fn_token, "a signal receiver must be an `async fn`");
    }
    if !sig.generics.params.is_empty() {
        errors.spanned(&sig.generics, "a signal receiver cannot be generic");
    }
    if sig.inputs.len() != 2 || sig.inputs.iter().any(|a| matches!(a, FnArg::Receiver(_))) {
        errors.spanned(
            &sig.inputs,
            "a signal receiver takes `(instance: &Model, event: &SignalEvent<'_>)`",
        );
    }
    errors.finish(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    fn expand_str(args: TokenStream, item: TokenStream) -> Result<String, String> {
        try_expand(args, &item)
            .map(|t| t.to_string())
            .map_err(|e| e.to_string())
    }

    fn func() -> TokenStream {
        quote! {
            pub async fn audit(user: &User, event: &SignalEvent<'_>) -> Result<(), SignalError> {
                Ok(())
            }
        }
    }

    #[test]
    fn keeps_the_function_and_generates_a_receiver_constructor() {
        let out = expand_str(quote!(post_save, model = crate::User), func()).unwrap();
        assert!(out.contains("pub async fn audit"));
        assert!(
            out.contains("pub fn audit_receiver () -> :: siderite :: orm :: signals :: Receiver")
        );
        assert!(out.contains(":: siderite :: orm :: signals :: SignalName :: PostSave"));
        assert!(out.contains("new :: < crate :: User , _ >"));
    }

    #[test]
    fn maps_every_signal_name() {
        for (name, variant) in SIGNALS {
            let name = Ident::new(name, proc_macro2::Span::call_site());
            let out = expand_str(quote!(#name, model = User), func()).unwrap();
            assert!(out.contains(&format!("SignalName :: {variant}")), "{out}");
        }
    }

    #[test]
    fn rejects_unknown_signals_and_lists_the_choices() {
        let err = expand_str(quote!(on_save, model = User), func()).unwrap_err();
        assert!(err.contains("unknown signal `on_save`"), "{err}");
        assert!(err.contains("m2m_changed"), "{err}");
    }

    #[test]
    fn rejects_missing_or_wrong_model_argument() {
        let missing = expand_str(quote!(post_save), func()).unwrap_err();
        assert!(missing.contains("model = <ModelType>"), "{missing}");
        let wrong = expand_str(quote!(post_save, sender = User), func()).unwrap_err();
        assert!(wrong.contains("unknown argument `sender`"), "{wrong}");
        let trailing = expand_str(quote!(post_save, model = User, extra), func()).unwrap_err();
        assert!(trailing.contains("unexpected tokens"), "{trailing}");
        let empty = expand_str(quote!(), func()).unwrap_err();
        assert!(empty.contains("expected a signal name"), "{empty}");
    }

    #[test]
    fn rejects_bad_signatures() {
        let sync = quote!(
            fn f(a: &User, b: &SignalEvent<'_>) {}
        );
        assert!(
            expand_str(quote!(pre_save, model = User), sync)
                .unwrap_err()
                .contains("async fn")
        );
        let arity = quote!(
            async fn f(a: &User) {}
        );
        assert!(
            expand_str(quote!(pre_save, model = User), arity)
                .unwrap_err()
                .contains("instance: &Model")
        );
        let generic = quote!(
            async fn f<T>(a: &User, b: &SignalEvent<'_>) {}
        );
        assert!(
            expand_str(quote!(pre_save, model = User), generic)
                .unwrap_err()
                .contains("generic")
        );
    }

    #[test]
    fn errors_still_emit_the_original_function() {
        let out = expand(quote!(bogus, model = User), func()).to_string();
        assert!(out.contains("pub async fn audit"));
        assert!(out.contains("compile_error"));
    }
}
