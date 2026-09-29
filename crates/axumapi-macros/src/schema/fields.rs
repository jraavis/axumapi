//! Code generation for fields: properties, tuple items and annotations.

use crate::attrs::field::Keyword;
use crate::attrs::plan::{TupleElement, resolve_named};
use crate::attrs::rename::RenameRule;
use crate::diag::Errors;
use crate::probe::Hooks;
use proc_macro2::TokenStream;
use quote::quote;
use syn::{FieldsNamed, FieldsUnnamed, Type};

/// Wrap `base` (an expression producing a `SchemaObject`) with schema keywords.
pub fn annotate(base: TokenStream, keywords: &[Keyword]) -> TokenStream {
    if keywords.is_empty() {
        return base;
    }
    let entries = keywords
        .iter()
        .map(|(key, value)| quote!((#key, ::axumapi::__private::json!(#value))));
    quote!(::axumapi::__private::annotate(#base, ::std::vec![#(#entries),*]))
}

/// The `description` keyword for a type's doc comment.
pub fn doc_keywords(description: Option<String>) -> Vec<Keyword> {
    description
        .map(|d| ("description", quote!(#d)))
        .into_iter()
        .collect()
}

fn field_schema(ty: &Type, keywords: &[Keyword]) -> TokenStream {
    annotate(quote!(registry.subschema::<#ty>()), keywords)
}

/// Schema expressions for the elements of a tuple struct or variant.
pub fn tuple_items(elements: &[TupleElement<'_>]) -> Vec<TokenStream> {
    elements
        .iter()
        .map(|e| field_schema(e.ty, &e.schema_keywords()))
        .collect()
}

/// How an object schema is built.
pub struct ObjectSpec<'a> {
    pub rule: Option<RenameRule>,
    pub container_default: bool,
    pub deny_unknown_fields: bool,
    /// Leading constant property `(tag_name, variant_name)` of internally
    /// tagged variants.
    pub tag: Option<(&'a str, &'a str)>,
    /// Computed-field documentation (top-level structs only).
    pub computed: Option<Hooks>,
}

/// Object schema expression for named fields.
pub fn object_expr(
    fields: &FieldsNamed,
    spec: &ObjectSpec<'_>,
    errors: &mut Errors,
) -> TokenStream {
    let reserved: Vec<String> = spec.tag.iter().map(|(t, _)| (*t).to_owned()).collect();
    let plans = resolve_named(fields, spec.rule, spec.container_default, &reserved, errors);
    let props = plans.iter().map(|plan| {
        let (key, required) = (&plan.key, plan.required);
        let schema = field_schema(plan.ty, &plan.schema_keywords());
        quote!(__object.property(#key, #required, #schema);)
    });
    let tag_stmt = spec.tag.map(|(tag, name)| {
        quote!(__object.property(#tag, true, ::axumapi::__private::const_string(#name));)
    });
    let computed = spec
        .computed
        .map(|hooks| hooks.computed_schema(quote!(__object.properties_mut())));
    let deny = spec.deny_unknown_fields;
    quote! {{
        let mut __object = ::axumapi::__private::ObjectBuilder::new();
        #tag_stmt
        #(#props)*
        #computed
        __object.build(#deny)
    }}
}

/// Tuple elements of an unnamed field list.
pub fn elements(fields: &FieldsUnnamed, errors: &mut Errors) -> Vec<TokenStream> {
    let resolved = crate::attrs::plan::resolve_unnamed(fields, errors);
    tuple_items(&resolved)
}
