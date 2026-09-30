//! Field resolution: the single place that decides a field's wire name and
//! whether it is required. Schema, prepare, validate and dump all consume
//! the resulting [`FieldPlan`]s.

use super::field::{FieldOptions, Keyword};
use super::rename::RenameRule;
use super::serde::FieldSerde;
use crate::diag::Errors;
use crate::docs;
use std::collections::HashSet;
use syn::{FieldsNamed, FieldsUnnamed, Ident, Type};

/// A named field after all attributes have been applied.
pub struct FieldPlan<'a> {
    pub ident: &'a Ident,
    pub ty: &'a Type,
    /// Rust name without `r#`.
    pub rust_name: String,
    /// Wire key (serde `rename` / `rename_all`), used for input and output.
    pub key: String,
    pub serde: FieldSerde,
    pub options: FieldOptions,
    pub doc: Option<String>,
    /// Whether a missing key is an error.
    pub required: bool,
    /// Whether the value comes from the input at all.
    pub read: bool,
    /// Whether the value is written to the output.
    pub written: bool,
}

impl FieldPlan<'_> {
    /// Extra accepted input keys, in the order used for error locations:
    /// validation aliases, then serde aliases, then (with
    /// `populate_by_name`) the Rust name. The wire key leads the list when
    /// no validation alias exists so missing-field errors name it.
    pub fn input_aliases(&self, populate_by_name: bool) -> Vec<String> {
        let mut aliases: Vec<String> = self
            .options
            .validation_aliases
            .iter()
            .map(syn::LitStr::value)
            .collect();
        let others = self.serde.aliases.iter().cloned().chain(
            (populate_by_name && self.rust_name != self.key).then(|| self.rust_name.clone()),
        );
        let mut others: Vec<String> = others.collect();
        if aliases.is_empty() && !others.is_empty() {
            aliases.push(self.key.clone());
        }
        aliases.append(&mut others);
        let mut seen = HashSet::new();
        aliases.retain(|a| seen.insert(a.clone()));
        aliases
    }

    /// Schema keywords (constraints and annotations).
    pub fn schema_keywords(&self) -> Vec<Keyword> {
        self.options.schema_keywords(self.doc.clone())
    }
}

/// Whether `ty` is syntactically `Option<..>`.
pub fn is_option(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    path.qself.is_none()
        && path.path.segments.last().is_some_and(|seg| {
            seg.ident == "Option" && matches!(seg.arguments, syn::PathArguments::AngleBracketed(_))
        })
}

/// Resolve every field of a struct or struct-like variant.
///
/// `reserved` lists wire names already taken (e.g. an enum tag) and
/// `container_default` whether the type has `#[serde(default)]`. Fields
/// skipped in both directions are dropped.
pub fn resolve_named<'a>(
    fields: &'a FieldsNamed,
    rule: Option<RenameRule>,
    container_default: bool,
    reserved: &[String],
    errors: &mut Errors,
) -> Vec<FieldPlan<'a>> {
    let mut seen: HashSet<String> = reserved.iter().cloned().collect();
    let mut out = Vec::new();
    for field in &fields.named {
        let Some(ident) = &field.ident else { continue };
        let serde = FieldSerde::parse(&field.attrs, errors);
        let options = FieldOptions::parse(&field.attrs, errors);
        if serde.skipped() {
            continue;
        }
        let rust_name = syn::ext::IdentExt::unraw(ident).to_string();
        let key = serde
            .rename
            .clone()
            .unwrap_or_else(|| rule.map_or(rust_name.clone(), |r| r.apply_to_field(&rust_name)));
        check_names(&options, &key, errors);
        let has_serde_default =
            serde.default.is_some() || container_default || serde.skip_deserializing;
        if let Some(default) = &options.default
            && !has_serde_default
        {
            let (span, what) = match default {
                super::field::DefaultSpec::Expr(e) => (syn::spanned::Spanned::span(e), "default"),
                super::field::DefaultSpec::Factory(p) => {
                    (syn::spanned::Spanned::span(p), "default_factory")
                }
            };
            errors.error(
                span,
                format!(
                    "`{what}` needs a matching serde default so deserialization agrees: \
                     add `#[serde(default)]` or `#[serde(default = \"path\")]` to this field"
                ),
            );
        }
        if !seen.insert(key.clone()) {
            errors.spanned(ident, format!("duplicate property name `{key}`"));
            continue;
        }
        let required = !(is_option(&field.ty) || has_serde_default);
        out.push(FieldPlan {
            ident,
            ty: &field.ty,
            rust_name,
            key,
            read: !serde.skip_deserializing,
            written: !serde.skip_serializing,
            serde,
            options,
            doc: docs::description(&field.attrs),
            required,
        });
    }
    out
}

/// `alias` and `serialization_alias` must equal the wire key: there is one
/// schema per type, so input and output names cannot differ.
fn check_names(options: &FieldOptions, key: &str, errors: &mut Errors) {
    if let Some(alias) = &options.alias
        && alias.value() != key
    {
        errors.spanned(
            alias,
            format!(
                "`alias` must equal the serde key `{key}`; use `#[serde(rename = \"{}\")]` \
                 to rename the field, or `validation_alias` for an extra accepted input name",
                alias.value()
            ),
        );
    }
    if let Some(alias) = &options.serialization_alias
        && alias.value() != key
    {
        errors.spanned(
            alias,
            format!(
                "`serialization_alias` must equal the serde key `{key}`: validation and \
                 serialization share one schema, so use `#[serde(rename = \"{}\")]` instead",
                alias.value()
            ),
        );
    }
}

/// An element of a tuple struct or variant.
pub struct TupleElement<'a> {
    pub ty: &'a Type,
    pub options: FieldOptions,
    pub doc: Option<String>,
}

impl TupleElement<'_> {
    /// Schema keywords (constraints and annotations).
    pub fn schema_keywords(&self) -> Vec<Keyword> {
        self.options.schema_keywords(self.doc.clone())
    }
}

/// Resolve the elements of a tuple struct or variant.
pub fn resolve_unnamed<'a>(
    fields: &'a FieldsUnnamed,
    errors: &mut Errors,
) -> Vec<TupleElement<'a>> {
    fields
        .unnamed
        .iter()
        .map(|field| {
            let serde = FieldSerde::parse(&field.attrs, errors);
            if serde.skip_serializing || serde.skip_deserializing {
                errors.spanned(
                    field,
                    "`skip` is not supported on tuple fields by the derives",
                );
            }
            TupleElement {
                ty: &field.ty,
                options: FieldOptions::parse(&field.attrs, errors),
                doc: docs::description(&field.attrs),
            }
        })
        .collect()
}
