//! Emits the relation accessors: forward foreign keys, reverse accessors
//! (`related_name`) and many-to-many managers.

use super::field::{ModelField, Relation};
use super::plan::{ManyToMany, ModelPlan};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::LitStr;

/// All accessor impls of the model.
pub fn expand(plan: &ModelPlan<'_>) -> TokenStream {
    let forward = forward(plan);
    let reverse = plan
        .columns()
        .filter_map(|f| f.relation.as_ref().map(|r| reverse_fk(plan, f, r)));
    let m2m = m2m_manager(plan);
    let m2m_reverse = plan
        .many_to_many
        .iter()
        .enumerate()
        .filter_map(|(index, m)| reverse_m2m(plan, index, m));
    quote! {
        #forward
        #m2m
        #(#reverse)*
        #(#m2m_reverse)*
    }
}

/// `fetch_<field>` for each foreign key (a method cannot share the name of
/// the field's associated constant).
fn forward(plan: &ModelPlan<'_>) -> TokenStream {
    let ident = plan.ident;
    let methods = plan.columns().filter_map(|field| {
        let relation = field.relation.as_ref()?;
        let (vis, name, target) = (field.vis, &field.name, relation.target);
        let method = format_ident!("fetch_{}", name);
        let doc = format!("Load the object `{name}` refers to (cached, else queried).");
        let field = field.ident;
        Some(if relation.nullable {
            quote! {
                #[doc = #doc]
                #vis async fn #method(&self, db: &::siderite::orm::Db)
                    -> ::core::result::Result<
                        ::core::option::Option<::std::sync::Arc<#target>>,
                        ::siderite::orm::OrmError,
                    >
                {
                    match &self.#field {
                        ::core::option::Option::Some(fk) => fk.get(db).await.map(::core::option::Option::Some),
                        ::core::option::Option::None => ::core::result::Result::Ok(::core::option::Option::None),
                    }
                }
            }
        } else {
            quote! {
                #[doc = #doc]
                #vis async fn #method(&self, db: &::siderite::orm::Db)
                    -> ::core::result::Result<::std::sync::Arc<#target>, ::siderite::orm::OrmError>
                {
                    self.#field.get(db).await
                }
            }
        })
    });
    let methods: Vec<_> = methods.collect();
    if methods.is_empty() {
        return TokenStream::new();
    }
    quote! {
        #[automatically_derived]
        impl #ident {
            #(#methods)*
        }
    }
}

/// `impl Target { fn <related_name>(&self, db) }` for a foreign key.
fn reverse_fk(
    plan: &ModelPlan<'_>,
    field: &ModelField<'_>,
    relation: &Relation<'_>,
) -> TokenStream {
    let Some(related) = &relation.related_name else {
        return TokenStream::new();
    };
    let orm = quote!(::siderite::orm);
    let (source, target, constant) = (plan.ident, relation.target, field.ident);
    let name = LitStr::new(&related.value(), related.span());
    let method = format_ident!("{}", name.value(), span = related.span());
    let queryset = quote! {
        <#source as #orm::Model>::objects(db).filter(
            #source::#constant.expr().eq(#orm::DbType::to_value(&<#target as #orm::Model>::pk(self)))
        )
    };
    let doc = format!(
        "Objects of `{source}` whose `{}` refers to this one (generated from `related_name`).",
        field.name
    );
    if relation.one_to_one {
        quote! {
            #[automatically_derived]
            impl #target {
                #[doc = #doc]
                pub async fn #method(&self, db: &#orm::Db)
                    -> ::core::result::Result<::core::option::Option<#source>, #orm::OrmError>
                {
                    #queryset.first().await
                }
            }
        }
    } else {
        quote! {
            #[automatically_derived]
            impl #target {
                #[doc = #doc]
                pub fn #method(&self, db: &#orm::Db) -> #orm::QuerySet<#source> {
                    #queryset
                }
            }
        }
    }
}

/// `fn <name>(&self, db) -> ManyToManyManager<Self, Target>`.
fn m2m_manager(plan: &ModelPlan<'_>) -> TokenStream {
    if plan.many_to_many.is_empty() {
        return TokenStream::new();
    }
    let orm = quote!(::siderite::orm);
    let ident = plan.ident;
    let methods = plan.many_to_many.iter().enumerate().map(|(index, m)| {
        let (name, target) = (&m.name, &m.target);
        let doc = format!("Manager of the many-to-many relation `{name}`.");
        quote! {
            #[doc = #doc]
            pub fn #name(&self, db: &#orm::Db) -> #orm::ManyToManyManager<#ident, #target> {
                #orm::ManyToManyManager::new(
                    db,
                    &<#ident as #orm::Model>::META.many_to_many[#index],
                    #orm::DbType::to_value(&<#ident as #orm::Model>::pk(self)),
                )
            }
        }
    });
    quote! {
        #[automatically_derived]
        impl #ident {
            #(#methods)*
        }
    }
}

/// `impl Target { fn <related_name>(&self, db) -> QuerySet<Source> }`.
fn reverse_m2m(plan: &ModelPlan<'_>, index: usize, m: &ManyToMany) -> Option<TokenStream> {
    let related = m.related_name.as_ref()?;
    let orm = quote!(::siderite::orm);
    let (source, target) = (plan.ident, &m.target);
    let method = format_ident!("{}", related.value(), span = related.span());
    let pk_column = &plan.pk_field().column;
    let doc = format!(
        "`{source}` objects related to this one through `{}` (generated from `related_name`).",
        m.name
    );
    Some(quote! {
        #[automatically_derived]
        impl #target {
            #[doc = #doc]
            pub fn #method(&self, db: &#orm::Db) -> #orm::QuerySet<#source> {
                ::siderite::__private::reverse_many_to_many::<#source>(
                    db,
                    &<#source as #orm::Model>::META.many_to_many[#index],
                    #pk_column,
                    #orm::DbType::to_value(&<#target as #orm::Model>::pk(self)),
                )
            }
        }
    })
}
