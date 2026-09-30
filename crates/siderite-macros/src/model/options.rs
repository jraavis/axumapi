//! The type-level `#[model(...)]` attribute.

use crate::attrs::serde::path_key;
use crate::diag::Errors;
use crate::meta::{for_each_meta, lit_str};
use syn::meta::ParseNestedMeta;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Expr, ExprArray, ExprLit, Ident, Lit, LitBool, LitStr, Path, Token, Type, parenthesized,
};

/// Every key accepted by `#[model(...)]`.
const MODEL_KEYS: &str = "table, ordering, indexes, unique_together, checks, managed, many_to_many";

/// `indexes(name(columns = [..], unique))`.
pub struct IndexSpec {
    pub name: Ident,
    pub columns: Vec<LitStr>,
    pub unique: bool,
}

/// `many_to_many(tags(Tag, through_table = "..", ..))`.
pub struct ManyToManySpec {
    pub name: Ident,
    pub target: Type,
    pub through: Option<Path>,
    pub through_table: Option<LitStr>,
    pub source_column: Option<LitStr>,
    pub target_column: Option<LitStr>,
    pub related_name: Option<LitStr>,
}

/// Parsed `#[model(...)]`.
pub struct ModelOptions {
    pub table: Option<LitStr>,
    pub ordering: Vec<LitStr>,
    pub indexes: Vec<IndexSpec>,
    pub unique_together: Vec<Vec<LitStr>>,
    pub checks: Vec<(Ident, LitStr)>,
    pub managed: bool,
    pub many_to_many: Vec<ManyToManySpec>,
}

impl ModelOptions {
    /// Parse every `#[model(...)]` attribute, reporting all problems.
    pub fn parse(attrs: &[syn::Attribute], errors: &mut Errors) -> Self {
        let mut out = Self {
            table: None,
            ordering: Vec::new(),
            indexes: Vec::new(),
            unique_together: Vec::new(),
            checks: Vec::new(),
            managed: true,
            many_to_many: Vec::new(),
        };
        for_each_meta(attrs, "model", errors, |meta, _| out.parse_key(&meta));
        out
    }

    fn parse_key(&mut self, meta: &ParseNestedMeta<'_>) -> syn::Result<()> {
        match path_key(meta).as_str() {
            "table" => self.table = Some(lit_str(meta)?),
            "ordering" => self.ordering.extend(str_array(meta.value()?.parse()?)?),
            "managed" => self.managed = meta.value()?.parse::<LitBool>()?.value,
            "indexes" => meta.parse_nested_meta(|index| {
                self.indexes.push(parse_index(&index)?);
                Ok(())
            })?,
            "unique_together" => {
                let content;
                parenthesized!(content in meta.input);
                for group in Punctuated::<ExprArray, Token![,]>::parse_terminated(&content)? {
                    self.unique_together.push(str_array(group)?);
                }
            }
            "checks" => meta.parse_nested_meta(|check| {
                let name = ident_of(&check)?;
                self.checks.push((name, lit_str(&check)?));
                Ok(())
            })?,
            "many_to_many" => meta.parse_nested_meta(|relation| {
                self.many_to_many.push(parse_many_to_many(&relation)?);
                Ok(())
            })?,
            other => {
                return Err(meta.error(format!(
                    "unknown `model` key `{other}`; valid keys: {MODEL_KEYS}"
                )));
            }
        }
        Ok(())
    }
}

fn ident_of(meta: &ParseNestedMeta<'_>) -> syn::Result<Ident> {
    meta.path
        .get_ident()
        .cloned()
        .ok_or_else(|| meta.error("expected a plain name"))
}

/// `["a", "b"]` as string literals.
fn str_array(array: ExprArray) -> syn::Result<Vec<LitStr>> {
    array
        .elems
        .into_iter()
        .map(|elem| match elem {
            Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) => Ok(s),
            other => Err(syn::Error::new(other.span(), "expected a string literal")),
        })
        .collect()
}

fn parse_index(meta: &ParseNestedMeta<'_>) -> syn::Result<IndexSpec> {
    let mut spec = IndexSpec {
        name: ident_of(meta)?,
        columns: Vec::new(),
        unique: false,
    };
    meta.parse_nested_meta(|key| {
        match path_key(&key).as_str() {
            "columns" => spec.columns = str_array(key.value()?.parse()?)?,
            "unique" => spec.unique = crate::meta::flag(&key)?,
            other => {
                return Err(key.error(format!(
                    "unknown index key `{other}`; valid keys: columns, unique"
                )));
            }
        }
        Ok(())
    })?;
    if spec.columns.is_empty() {
        return Err(syn::Error::new(
            spec.name.span(),
            "an index needs `columns = [\"field\", ..]`",
        ));
    }
    Ok(spec)
}

/// `name(Target, key = value, ..)`.
fn parse_many_to_many(meta: &ParseNestedMeta<'_>) -> syn::Result<ManyToManySpec> {
    let content;
    parenthesized!(content in meta.input);
    let mut spec = ManyToManySpec {
        name: ident_of(meta)?,
        target: content.parse()?,
        through: None,
        through_table: None,
        source_column: None,
        target_column: None,
        related_name: None,
    };
    while content.parse::<Option<Token![,]>>()?.is_some() && !content.is_empty() {
        let key: Ident = content.parse()?;
        content.parse::<Token![=]>()?;
        match key.to_string().as_str() {
            "through" => spec.through = Some(content.parse()?),
            "through_table" => spec.through_table = Some(content.parse()?),
            "source_column" => spec.source_column = Some(content.parse()?),
            "target_column" => spec.target_column = Some(content.parse()?),
            "related_name" => spec.related_name = Some(content.parse()?),
            other => {
                return Err(syn::Error::new(
                    key.span(),
                    format!(
                        "unknown many_to_many key `{other}`; valid keys: through, through_table, \
                         source_column, target_column, related_name"
                    ),
                ));
            }
        }
    }
    Ok(spec)
}
