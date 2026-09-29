//! Field resolution: turns the fields of the struct into column plans.

use crate::attrs::field::FieldOptions;
use crate::diag::Errors;
use proc_macro2::TokenStream;
use quote::quote;
use syn::ext::IdentExt;
use syn::{
    FieldsNamed, GenericArgument, Ident, LitStr, PathArguments, PathSegment, Type, Visibility,
};

/// A relation field (`ForeignKey<T>`, `OneToOne<T>`, optionally in `Option`).
pub struct Relation<'a> {
    pub one_to_one: bool,
    /// The referenced model type `T`.
    pub target: &'a Type,
    /// Wrapped in `Option`.
    pub nullable: bool,
    /// The `OnDelete` variant name.
    pub on_delete: Ident,
    pub related_name: Option<LitStr>,
}

/// One struct field after attribute processing.
pub struct ModelField<'a> {
    pub ident: &'a Ident,
    pub vis: &'a Visibility,
    pub ty: &'a Type,
    pub options: FieldOptions,
    /// Rust name without `r#`.
    pub name: String,
    pub column: String,
    pub relation: Option<Relation<'a>>,
}

impl ModelField<'_> {
    /// `unique` including the implicit uniqueness of a one-to-one field.
    pub fn unique(&self) -> bool {
        self.options.orm.unique || self.relation.as_ref().is_some_and(|r| r.one_to_one)
    }

    /// Whether a single-column index is emitted (foreign keys get one unless
    /// a unique constraint already provides it).
    pub fn indexed(&self) -> bool {
        self.options.orm.index
            || (self.relation.is_some() && !self.unique() && !self.options.orm.primary_key)
    }

    /// Whether this field is stored in a column.
    pub fn is_column(&self) -> bool {
        !self.options.orm.skip
    }
}

/// Resolve the fields of the model struct.
pub fn resolve<'a>(fields: &'a FieldsNamed, errors: &mut Errors) -> Vec<ModelField<'a>> {
    let mut out: Vec<ModelField<'a>> = Vec::new();
    for field in &fields.named {
        let Some(ident) = &field.ident else { continue };
        let options = FieldOptions::parse(&field.attrs, errors);
        let name = ident.unraw().to_string();
        let relation = relation(&field.ty, &options, errors);
        let column = match (&options.orm.column, &relation) {
            (Some(column), _) => column.value(),
            (None, Some(_)) => format!("{name}_id"),
            (None, None) => name.clone(),
        };
        let resolved = ModelField {
            ident,
            vis: &field.vis,
            ty: &field.ty,
            options,
            name,
            column,
            relation,
        };
        check(&resolved, errors);
        if resolved.is_column()
            && out
                .iter()
                .any(|f| f.is_column() && f.column == resolved.column)
        {
            errors.spanned(
                ident,
                format!("duplicate column name `{}`", resolved.column),
            );
        }
        out.push(resolved);
    }
    out
}

/// Per-field rules that do not depend on other fields.
fn check(field: &ModelField<'_>, errors: &mut Errors) {
    let orm = &field.options.orm;
    if orm.skip {
        return;
    }
    if orm.auto {
        if !orm.primary_key {
            errors.spanned(field.ident, "`auto` is only valid on the primary key");
        } else if !is_integer(field.ty) {
            errors.spanned(
                field.ty,
                "`auto` requires an integer primary key (`i16`, `i32` or `i64`)",
            );
        }
    }
    for (key, value) in [
        ("max_length", field.options.max_length),
        ("max_digits", field.options.max_digits),
        ("decimal_places", field.options.decimal_places),
    ] {
        if value.is_some_and(|n| u32::try_from(n).is_err()) {
            errors.spanned(field.ident, format!("`{key}` does not fit in 32 bits"));
        }
    }
    if orm.auto_now_add && orm.db_default.is_some() {
        errors.spanned(
            field.ident,
            "`auto_now_add` sets the database default; remove `db_default`",
        );
    }
    if field.relation.is_none() {
        for (key, given) in [
            ("on_delete", orm.on_delete.is_some()),
            ("related_name", orm.related_name.is_some()),
        ] {
            if given {
                errors.spanned(
                    field.ident,
                    format!("`{key}` is only valid on `ForeignKey<..>` / `OneToOne<..>` fields"),
                );
            }
        }
    }
}

fn is_integer(ty: &Type) -> bool {
    last_segment(ty).is_some_and(|seg| {
        matches!(seg.arguments, PathArguments::None)
            && ["i16", "i32", "i64"].iter().any(|n| seg.ident == n)
    })
}

fn last_segment(ty: &Type) -> Option<&PathSegment> {
    match ty {
        Type::Path(path) if path.qself.is_none() => path.path.segments.last(),
        _ => None,
    }
}

/// The single type argument of `Name<T>`.
fn single_arg<'a>(ty: &'a Type, name: &[&str]) -> Option<&'a Type> {
    let seg = last_segment(ty)?;
    if !name.iter().any(|n| seg.ident == n) {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    match args.args.first()? {
        GenericArgument::Type(inner) if args.args.len() == 1 => Some(inner),
        _ => None,
    }
}

/// `Some(T)` for `Option<T>`.
pub fn option_inner(ty: &Type) -> Option<&Type> {
    single_arg(ty, &["Option"])
}

fn relation<'a>(ty: &'a Type, options: &FieldOptions, errors: &mut Errors) -> Option<Relation<'a>> {
    let (inner, nullable) = option_inner(ty).map_or((ty, false), |inner| (inner, true));
    let target = single_arg(inner, &["ForeignKey", "OneToOne"])?;
    let one_to_one = last_segment(inner).is_some_and(|seg| seg.ident == "OneToOne");
    let on_delete = match &options.orm.on_delete {
        None => Ident::new("Cascade", proc_macro2::Span::call_site()),
        Some(lit) => on_delete_variant(lit, nullable, errors),
    };
    Some(Relation {
        one_to_one,
        target,
        nullable,
        on_delete,
        related_name: options.orm.related_name.clone(),
    })
}

fn on_delete_variant(lit: &LitStr, nullable: bool, errors: &mut Errors) -> Ident {
    let variant = match lit.value().as_str() {
        "cascade" => "Cascade",
        "protect" => "Protect",
        "set_null" => "SetNull",
        "set_default" => "SetDefault",
        "do_nothing" => "DoNothing",
        other => {
            errors.spanned(
                lit,
                format!(
                    "unknown `on_delete` value `{other}`; expected \"cascade\", \"protect\", \
                     \"set_null\", \"set_default\" or \"do_nothing\""
                ),
            );
            "Cascade"
        }
    };
    if variant == "SetNull" && !nullable {
        errors.spanned(
            lit,
            "`on_delete = \"set_null\"` requires a nullable field: use `Option<ForeignKey<..>>`",
        );
    }
    Ident::new(variant, lit.span())
}

/// Tokens of the field's type as a `DbType` (`<T as DbType>`).
pub fn db_type(ty: &Type) -> TokenStream {
    quote!(<#ty as ::axumapi::orm::DbType>)
}
