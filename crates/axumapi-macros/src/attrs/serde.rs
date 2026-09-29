//! The subset of `#[serde(...)]` understood by every derive.
//!
//! Serde stays the single source of truth for wire names, defaults and
//! skipping: `derive(Schema)`, `derive(Validate)` and the generated `Dump`
//! all read these attributes through this module, so they cannot disagree.

use super::rename::{EXPECTED, RenameRule};
use crate::diag::Errors;
use crate::meta::{for_each_meta, lit_str, skip_value};
use quote::ToTokens;
use syn::meta::ParseNestedMeta;
use syn::{Attribute, LitStr, Path, Token, token};

/// serde keys that change the wire format in ways the derives cannot describe.
const UNSUPPORTED_CONTAINER: &[&str] = &["transparent", "from", "into", "try_from", "remote"];
const UNSUPPORTED_FIELD: &[&str] = &["flatten", "with", "serialize_with", "deserialize_with"];
const UNSUPPORTED_VARIANT: &[&str] = &[
    "other",
    "untagged",
    "with",
    "serialize_with",
    "deserialize_with",
];

fn unsupported(meta: &ParseNestedMeta<'_>, key: &str) -> syn::Error {
    let message = if key == "flatten" {
        "flatten is not supported by axumapi's derives yet".to_owned()
    } else {
        format!("`{key}` is not supported by axumapi's derives yet")
    };
    meta.error(message)
}

/// Name of the key of `meta` as a string.
pub fn path_key(meta: &ParseNestedMeta<'_>) -> String {
    meta.path.get_ident().map_or_else(
        || meta.path.to_token_stream().to_string(),
        ToString::to_string,
    )
}

/// `rename = "x"`; the `(serialize = .., deserialize = ..)` form is rejected.
fn rename_value(meta: &ParseNestedMeta<'_>) -> syn::Result<String> {
    if meta.input.peek(token::Paren) {
        return Err(meta.error(
            "separate serialize/deserialize names are not supported: \
             validation and serialization must agree on one name",
        ));
    }
    Ok(lit_str(meta)?.value())
}

fn rename_rule(meta: &ParseNestedMeta<'_>) -> syn::Result<RenameRule> {
    if meta.input.peek(token::Paren) {
        return Err(meta.error(
            "separate serialize/deserialize rules are not supported: \
             validation and serialization must agree on one name",
        ));
    }
    let lit = lit_str(meta)?;
    RenameRule::parse(&lit.value()).ok_or_else(|| {
        syn::Error::new(
            lit.span(),
            format!(
                "unknown rename rule `{}`; expected one of: {EXPECTED}",
                lit.value()
            ),
        )
    })
}

/// Where serde gets a missing value from.
#[derive(Clone)]
pub enum SerdeDefault {
    /// `#[serde(default)]`: `Default::default()`.
    Trait,
    /// `#[serde(default = "path")]`: a function returning the value.
    Path(Path),
}

/// `default` or `default = "path"`.
fn default_value(meta: &ParseNestedMeta<'_>) -> syn::Result<SerdeDefault> {
    if meta.input.peek(Token![=]) {
        let lit: LitStr = meta.value()?.parse()?;
        Ok(SerdeDefault::Path(lit.parse()?))
    } else {
        Ok(SerdeDefault::Trait)
    }
}

/// `#[serde(...)]` on the type itself.
#[derive(Default)]
pub struct ContainerSerde {
    pub rename_all: Option<RenameRule>,
    pub rename_all_fields: Option<RenameRule>,
    pub default: Option<SerdeDefault>,
    pub deny_unknown_fields: bool,
    pub tag: Option<LitStr>,
    pub content: Option<LitStr>,
    pub untagged: bool,
}

impl ContainerSerde {
    pub fn parse(attrs: &[Attribute], errors: &mut Errors) -> Self {
        let mut out = Self::default();
        for_each_meta(attrs, "serde", errors, |meta, _| {
            let key = path_key(&meta);
            match key.as_str() {
                "rename_all" => out.rename_all = Some(rename_rule(&meta)?),
                "rename_all_fields" => out.rename_all_fields = Some(rename_rule(&meta)?),
                "default" => out.default = Some(default_value(&meta)?),
                "deny_unknown_fields" => out.deny_unknown_fields = true,
                "tag" => out.tag = Some(lit_str(&meta)?),
                "content" => out.content = Some(lit_str(&meta)?),
                "untagged" => out.untagged = true,
                k if UNSUPPORTED_CONTAINER.contains(&k) => return Err(unsupported(&meta, k)),
                _ => skip_value(&meta)?,
            }
            Ok(())
        });
        out
    }
}

/// `#[serde(...)]` on a field.
#[derive(Default)]
pub struct FieldSerde {
    pub rename: Option<String>,
    /// Extra accepted input names (`alias = ".."`, repeatable).
    pub aliases: Vec<String>,
    pub skip_serializing: bool,
    pub skip_deserializing: bool,
    pub default: Option<SerdeDefault>,
    pub skip_serializing_if: Option<Path>,
}

impl FieldSerde {
    pub fn parse(attrs: &[Attribute], errors: &mut Errors) -> Self {
        let mut out = Self::default();
        for_each_meta(attrs, "serde", errors, |meta, _| {
            let key = path_key(&meta);
            match key.as_str() {
                "rename" => out.rename = Some(rename_value(&meta)?),
                "alias" => out.aliases.push(lit_str(&meta)?.value()),
                "skip" => {
                    out.skip_serializing = true;
                    out.skip_deserializing = true;
                }
                "skip_serializing" => out.skip_serializing = true,
                "skip_deserializing" => out.skip_deserializing = true,
                "default" => out.default = Some(default_value(&meta)?),
                "skip_serializing_if" => {
                    let lit = lit_str(&meta)?;
                    out.skip_serializing_if = Some(lit.parse()?);
                }
                k if UNSUPPORTED_FIELD.contains(&k) => return Err(unsupported(&meta, k)),
                _ => skip_value(&meta)?,
            }
            Ok(())
        });
        out
    }

    /// Whether the field is absent from the wire format entirely.
    pub fn skipped(&self) -> bool {
        self.skip_serializing && self.skip_deserializing
    }
}

/// `#[serde(...)]` on an enum variant.
#[derive(Default)]
pub struct VariantSerde {
    pub rename: Option<String>,
    pub aliases: Vec<String>,
    pub rename_all: Option<RenameRule>,
    pub skip_serializing: bool,
    pub skip_deserializing: bool,
}

impl VariantSerde {
    pub fn parse(attrs: &[Attribute], errors: &mut Errors) -> Self {
        let mut out = Self::default();
        for_each_meta(attrs, "serde", errors, |meta, _| {
            let key = path_key(&meta);
            match key.as_str() {
                "rename" => out.rename = Some(rename_value(&meta)?),
                "alias" => out.aliases.push(lit_str(&meta)?.value()),
                "rename_all" => out.rename_all = Some(rename_rule(&meta)?),
                "skip" => {
                    out.skip_serializing = true;
                    out.skip_deserializing = true;
                }
                "skip_serializing" => out.skip_serializing = true,
                "skip_deserializing" => out.skip_deserializing = true,
                k if UNSUPPORTED_VARIANT.contains(&k) => return Err(unsupported(&meta, k)),
                _ => skip_value(&meta)?,
            }
            Ok(())
        });
        out
    }

    pub fn skipped(&self) -> bool {
        self.skip_serializing && self.skip_deserializing
    }
}
