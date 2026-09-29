//! Emits the `static ModelMeta` (and the fn pointers it needs).

use super::field::{ModelField, db_type};
use super::plan::{Constraint, ManyToMany, ModelPlan, Through};
use crate::attrs::orm::DbDefaultSpec;
use proc_macro2::{Literal, Span, TokenStream};
use quote::{format_ident, quote};
use syn::Ident;

/// Name of the generated static.
pub fn static_ident() -> Ident {
    Ident::new("__AXUMAPI_MODEL_META", Span::call_site())
}

/// The static plus the target-resolver functions it references.
pub fn expand(plan: &ModelPlan<'_>) -> TokenStream {
    let orm = quote!(::axumapi::orm);
    let name = plan.ident.to_string();
    let table = &plan.table;
    let managed = plan.managed;
    let fields = plan.columns().map(field_meta);
    let relations = plan.many_to_many.iter().map(many_to_many);
    let ordering = plan.ordering.iter().map(|(column, descending)| {
        let direction = if *descending { "Desc" } else { "Asc" };
        let direction = Ident::new(direction, Span::call_site());
        quote!((#column, #orm::OrderDirection::#direction))
    });
    let indexes = plan.indexes.iter().map(|index| {
        let (name, columns, unique) = (&index.name, &index.columns, index.unique);
        quote!(#orm::IndexMeta { name: #name, columns: &[#(#columns),*], unique: #unique })
    });
    let constraints = plan.constraints.iter().map(|constraint| match constraint {
        Constraint::Unique { name, columns } => {
            quote!(#orm::ConstraintMeta::Unique { name: #name, columns: &[#(#columns),*] })
        }
        Constraint::Check { name, sql } => {
            quote!(#orm::ConstraintMeta::Check { name: #name, sql: #sql })
        }
    });
    let resolvers = plan
        .columns()
        .filter_map(|f| {
            f.relation
                .as_ref()
                .map(|r| resolver(&target_fn(&f.name), r.target))
        })
        .chain(plan.many_to_many.iter().flat_map(|m| {
            let mut fns = vec![resolver(&m2m_target_fn(m), &m.target)];
            if let Through::Model(model) = &m.through {
                fns.push(resolver(&m2m_through_fn(m), &syn::parse_quote!(#model)));
            }
            fns
        }));
    let static_name = static_ident();
    quote! {
        #(#resolvers)*
        static #static_name: #orm::ModelMeta = #orm::ModelMeta {
            name: #name,
            table: #table,
            fields: &[#(#fields),*],
            many_to_many: &[#(#relations),*],
            ordering: &[#(#ordering),*],
            indexes: &[#(#indexes),*],
            constraints: &[#(#constraints),*],
            managed: #managed,
        };
    }
}

fn target_fn(field: &str) -> Ident {
    format_ident!("__axumapi_target_{}", field)
}

fn m2m_target_fn(m: &ManyToMany) -> Ident {
    format_ident!("__axumapi_m2m_target_{}", m.name)
}

fn m2m_through_fn(m: &ManyToMany) -> Ident {
    format_ident!("__axumapi_m2m_through_{}", m.name)
}

/// `fn name() -> &'static ModelMeta { <ty as Model>::META }`.
fn resolver(name: &Ident, ty: &syn::Type) -> TokenStream {
    let orm = quote!(::axumapi::orm);
    quote! {
        fn #name() -> &'static #orm::ModelMeta {
            <#ty as #orm::Model>::META
        }
    }
}

fn field_meta(field: &ModelField<'_>) -> TokenStream {
    let orm = quote!(::axumapi::orm);
    let db = db_type(field.ty);
    let options = &field.options;
    let (name, column) = (&field.name, &field.column);
    let mut overrides = vec![quote!(nullable: #db::NULLABLE)];
    let mut flag = |key: &str, on: bool| {
        if on {
            let key = Ident::new(key, Span::call_site());
            overrides.push(quote!(#key: true));
        }
    };
    flag("primary_key", options.orm.primary_key);
    flag("auto", options.orm.auto);
    flag("unique", field.unique());
    flag("index", field.indexed());
    flag("auto_now", options.orm.auto_now);
    for (key, value) in [
        ("max_length", options.max_length),
        ("max_digits", options.max_digits),
        ("decimal_places", options.decimal_places),
    ] {
        if let Some(n) = value {
            let key = Ident::new(key, Span::call_site());
            let lit = Literal::u32_suffixed(u32::try_from(n).unwrap_or(u32::MAX));
            overrides.push(quote!(#key: ::core::option::Option::Some(#lit)));
        }
    }
    if let Some(default) = db_default(field) {
        overrides.push(quote!(default: ::core::option::Option::Some(#default)));
    }
    if let Some(relation) = &field.relation {
        let kind = Ident::new(
            if relation.one_to_one {
                "OneToOne"
            } else {
                "ForeignKey"
            },
            Span::call_site(),
        );
        let target = target_fn(&field.name);
        let on_delete = &relation.on_delete;
        let related = option_str(relation.related_name.as_ref().map(syn::LitStr::value));
        overrides.push(quote! {
            relation: ::core::option::Option::Some(#orm::RelationMeta {
                kind: #orm::RelationKind::#kind,
                target: #target,
                on_delete: #orm::OnDelete::#on_delete,
                related_name: #related,
            })
        });
    }
    quote! {
        #orm::FieldMeta {
            #(#overrides,)*
            ..#orm::FieldMeta::new(#name, #column, #db::SQL_TYPE)
        }
    }
}

fn db_default(field: &ModelField<'_>) -> Option<TokenStream> {
    let orm = quote!(::axumapi::orm);
    if field.options.orm.auto_now_add {
        return Some(quote!(#orm::DbDefault::Now));
    }
    Some(match field.options.orm.db_default.as_ref()? {
        DbDefaultSpec::Int(v) => {
            let lit = Literal::i64_suffixed(*v);
            quote!(#orm::DbDefault::Int(#lit))
        }
        DbDefaultSpec::Bool(v) => quote!(#orm::DbDefault::Bool(#v)),
        DbDefaultSpec::Text(v) => quote!(#orm::DbDefault::Text(#v)),
    })
}

fn option_str(value: Option<String>) -> TokenStream {
    match value {
        Some(s) => quote!(::core::option::Option::Some(#s)),
        None => quote!(::core::option::Option::None),
    }
}

fn many_to_many(m: &ManyToMany) -> TokenStream {
    let orm = quote!(::axumapi::orm);
    let name = m.name.to_string();
    let target = m2m_target_fn(m);
    let (source, target_column) = (&m.source_column, &m.target_column);
    let related = option_str(m.related_name.as_ref().map(syn::LitStr::value));
    let (through_table, through) = match &m.through {
        Through::Table(table) => (quote!(#table), quote!(::core::option::Option::None)),
        Through::Model(model) => {
            let through = m2m_through_fn(m);
            (
                quote!(<#model as #orm::Model>::META.table),
                quote!(::core::option::Option::Some(#through)),
            )
        }
    };
    quote! {
        #orm::ManyToManyMeta {
            name: #name,
            target: #target,
            through_table: #through_table,
            source_column: #source,
            target_column: #target_column,
            through: #through,
            related_name: #related,
        }
    }
}
