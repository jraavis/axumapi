//! Runtime checks generated from `#[field(...)]` constraints.

use crate::attrs::field::FieldOptions;
use proc_macro2::{Literal, TokenStream};
use quote::{format_ident, quote};

/// Statements and statics implementing a field's constraints.
#[derive(Default)]
pub struct Checks {
    /// Items placed next to the impl (compiled regexes).
    pub statics: Vec<TokenStream>,
    /// Statements run on `__slot` once the field type accepted the input.
    pub stmts: Vec<TokenStream>,
}

/// Whether `options` asks for any runtime check.
pub fn has_rules(options: &FieldOptions) -> bool {
    !checks(options, 0).stmts.is_empty() || !options.validators.is_empty()
}

/// Build the checks of `options`; `id` makes static names unique.
pub fn checks(options: &FieldOptions, id: usize) -> Checks {
    let model = quote!(::axumapi::validation::model);
    let mut out = Checks::default();
    let mut push = |constraint: TokenStream| {
        out.stmts
            .push(quote!(#model::check(__slot, #model::Constraint::#constraint, ctx);));
    };
    if let Some(n) = options.min_length {
        let n = Literal::usize_suffixed(usize::try_from(n).unwrap_or(usize::MAX));
        push(quote!(MinLength(#n)));
    }
    if let Some(n) = options.max_length {
        let n = Literal::usize_suffixed(usize::try_from(n).unwrap_or(usize::MAX));
        push(quote!(MaxLength(#n)));
    }
    let mut statics = Vec::new();
    if let Some(pattern) = &options.pattern {
        let name = format_ident!("__AXUMAPI_RE_{}", id);
        statics.push(quote! {
            static #name: ::std::sync::LazyLock<
                ::core::option::Option<::axumapi::__private::Regex>,
            > = ::std::sync::LazyLock::new(|| ::axumapi::__private::Regex::new(#pattern).ok());
        });
        push(quote!(Pattern((*#name).as_ref())));
    }
    if options.email {
        push(quote!(Email));
    }
    if options.url {
        push(quote!(Url));
    }
    for (variant, bound) in [
        (quote!(Gt), &options.gt),
        (quote!(Ge), &options.ge),
        (quote!(Lt), &options.lt),
        (quote!(Le), &options.le),
        (quote!(MultipleOf), &options.multiple_of),
    ] {
        if let Some(n) = bound {
            let number = n.number();
            push(quote!(#variant(&#number)));
        }
    }
    out.statics = statics;
    out
}
