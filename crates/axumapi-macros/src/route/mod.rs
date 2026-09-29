//! `#[get]`, `#[post]`, ... attribute macros.

mod args;
mod path;

use crate::diag::Errors;
use crate::docs;
use args::RouteArgs;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::ext::IdentExt;
use syn::{Item, ItemFn};

/// HTTP method selected by the attribute macro.
#[derive(Clone, Copy)]
pub enum Method {
    /// `#[get]`
    Get,
    /// `#[post]`
    Post,
    /// `#[put]`
    Put,
    /// `#[patch]`
    Patch,
    /// `#[delete]`
    Delete,
    /// `#[head]`
    Head,
    /// `#[options]`
    Options,
    /// `#[ws]` (a `GET` upgrade request)
    Ws,
}

impl Method {
    /// Attribute name, for diagnostics.
    fn attribute(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Post => "post",
            Self::Put => "put",
            Self::Patch => "patch",
            Self::Delete => "delete",
            Self::Head => "head",
            Self::Options => "options",
            Self::Ws => "ws",
        }
    }

    /// Name of the `axumapi` constructor function this attribute expands to.
    fn constructor(self) -> &'static str {
        match self {
            Self::Ws => "get",
            other => other.attribute(),
        }
    }
}

/// Name of the hidden function generated for handler `name`.
pub fn route_fn_ident(name: &syn::Ident) -> syn::Ident {
    format_ident!("__axumapi_route_{}", name.unraw())
}

/// Expand a route attribute. On error the original item is kept so that
/// only the real diagnostics are reported.
pub fn expand(method: Method, args: TokenStream, item: TokenStream) -> TokenStream {
    match try_expand(method, args, item.clone()) {
        Ok(tokens) => tokens,
        Err(err) => {
            let err = err.into_compile_error();
            quote!(#item #err)
        }
    }
}

fn try_expand(method: Method, args: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let attr = method.attribute();
    let mut errors = Errors::default();
    let parsed_item: Item = syn::parse2(item)?;
    let Item::Fn(func) = parsed_item else {
        return Err(syn::Error::new_spanned(
            parsed_item,
            format!("`#[{attr}]` can only be applied to `async fn` items"),
        ));
    };
    let args = errors.absorb(syn::parse2::<RouteArgs>(args));
    check_signature(attr, &func, &mut errors);
    match (errors.into_error(), args) {
        (None, Some(args)) => Ok(generate(method, &args, func)),
        (Some(err), _) => Err(err),
        (None, None) => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "invalid route arguments",
        )),
    }
}

fn check_signature(attr: &str, func: &ItemFn, errors: &mut Errors) {
    if func.sig.asyncness.is_none() {
        errors.spanned(
            func.sig.fn_token,
            format!("`#[{attr}]` handlers must be `async fn`"),
        );
    }
    if !func.sig.generics.params.is_empty() {
        errors.spanned(
            &func.sig.generics,
            format!("`#[{attr}]` handlers cannot be generic"),
        );
    }
}

fn generate(method: Method, args: &RouteArgs, func: ItemFn) -> TokenStream {
    let name = &func.sig.ident;
    let vis = &func.vis;
    let route_fn = route_fn_ident(name);
    let path = &args.path;
    let constructor = format_ident!("{}", method.constructor());
    let (doc_summary, doc_description) = docs::summary_and_description(&func.attrs);

    let operation_id = args
        .operation_id
        .as_ref()
        .map_or_else(|| name.unraw().to_string(), syn::LitStr::value);
    let summary = args
        .summary
        .as_ref()
        .map(syn::LitStr::value)
        .or(doc_summary)
        .map(|s| quote!(.summary(#s)));
    let description = args
        .description
        .as_ref()
        .map(syn::LitStr::value)
        .or(doc_description)
        .map(|d| quote!(.description(#d)));
    let tags = args.tags.iter().map(|t| quote!(.tag(#t)));
    let deprecated = args.deprecated.then(|| quote!(.deprecated()));
    let hidden = args.hidden.then(|| quote!(.hidden()));
    let status = args.status.map(|code| {
        quote!(.status(
            ::axumapi::http::StatusCode::from_u16(#code)
                .unwrap_or(::axumapi::http::StatusCode::OK)
        ))
    });
    let response_model = args
        .response_model
        .as_ref()
        .map(|ty| quote!(.response_model::<#ty>()));

    quote! {
        #func

        #[doc(hidden)]
        #[allow(non_snake_case, dead_code)]
        #vis fn #route_fn() -> ::axumapi::Route {
            ::axumapi::Route::new(
                #path,
                ::axumapi::#constructor(#name)
                    .operation_id(#operation_id)
                    #summary
                    #description
                    #(#tags)*
                    #deprecated
                    #hidden
                    #status
                    #response_model
            )
        }
    }
}
