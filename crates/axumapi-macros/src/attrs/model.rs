//! Type-level attributes: `#[model_config(...)]`, `#[schema(...)]` and the
//! serde container attributes, gathered in [`Container`].

use super::serde::{ContainerSerde, path_key};
use crate::diag::Errors;
use crate::meta::{flag, for_each_meta, lit_str};
use proc_macro2::TokenStream;
use quote::quote;
use syn::{Attribute, LitStr};

/// Every key accepted by `#[model_config(...)]`.
const CONFIG_KEYS: &str = "strict, extra, str_strip_whitespace, str_to_lower, str_to_upper, \
                           populate_by_name, hooks";
/// Every key accepted by `#[schema(...)]`.
const SCHEMA_KEYS: &str = "name, inline, dump, no_dump";

/// Unknown-key policy (`extra = ".."`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExtraPolicy {
    Ignore,
    Forbid,
    Allow,
}

/// `#[model_config(...)]` and `#[schema(...)]`.
#[derive(Default)]
pub struct ModelOptions {
    pub strict: bool,
    pub extra: Option<ExtraPolicy>,
    pub str_strip_whitespace: bool,
    pub str_to_lower: bool,
    pub str_to_upper: bool,
    pub populate_by_name: bool,
    /// Call `<Self as ModelHooks>` directly instead of probing.
    pub hooks: bool,
    pub name: Option<LitStr>,
    pub inline: bool,
    /// `Some(false)` after `#[schema(no_dump)]`.
    pub dump: Option<bool>,
}

impl ModelOptions {
    fn parse(attrs: &[Attribute], errors: &mut Errors) -> Self {
        let mut out = Self::default();
        for_each_meta(attrs, "model_config", errors, |meta, errors| {
            match path_key(&meta).as_str() {
                "strict" => out.strict = flag(&meta)?,
                "str_strip_whitespace" => out.str_strip_whitespace = flag(&meta)?,
                "str_to_lower" => out.str_to_lower = flag(&meta)?,
                "str_to_upper" => out.str_to_upper = flag(&meta)?,
                "populate_by_name" => out.populate_by_name = flag(&meta)?,
                "hooks" => out.hooks = flag(&meta)?,
                "extra" => {
                    let lit = lit_str(&meta)?;
                    out.extra = match lit.value().as_str() {
                        "forbid" => Some(ExtraPolicy::Forbid),
                        "ignore" => Some(ExtraPolicy::Ignore),
                        "allow" => Some(ExtraPolicy::Allow),
                        other => {
                            errors.spanned(
                                &lit,
                                format!(
                                    "unknown `extra` value `{other}`; expected \"forbid\", \
                                     \"ignore\" or \"allow\""
                                ),
                            );
                            None
                        }
                    };
                }
                other => {
                    return Err(meta.error(format!(
                        "unknown `model_config` key `{other}`; valid keys: {CONFIG_KEYS}"
                    )));
                }
            }
            Ok(())
        });
        if out.str_to_lower && out.str_to_upper {
            errors.error(
                proc_macro2::Span::call_site(),
                "`str_to_lower` and `str_to_upper` are mutually exclusive",
            );
        }
        for_each_meta(attrs, "schema", errors, |meta, errors| {
            match path_key(&meta).as_str() {
                "name" => {
                    let lit = lit_str(&meta)?;
                    if lit.value().is_empty() {
                        errors.spanned(&lit, "schema name must not be empty");
                    }
                    out.name = Some(lit);
                }
                "inline" => out.inline = flag(&meta)?,
                "dump" => out.dump = Some(flag(&meta)?),
                "no_dump" => out.dump = Some(!flag(&meta)?),
                other => {
                    return Err(meta.error(format!(
                        "unknown `schema` key `{other}`; valid keys: {SCHEMA_KEYS}"
                    )));
                }
            }
            Ok(())
        });
        out
    }
}

/// Everything declared on the type itself.
#[derive(Default)]
pub struct Container {
    pub serde: ContainerSerde,
    pub options: ModelOptions,
}

impl Container {
    pub fn parse(attrs: &[Attribute], errors: &mut Errors) -> Self {
        Self {
            serde: ContainerSerde::parse(attrs, errors),
            options: ModelOptions::parse(attrs, errors),
        }
    }

    /// Effective unknown-key policy: an explicit `extra` wins, otherwise
    /// serde's `deny_unknown_fields` means "forbid".
    pub fn extra(&self) -> ExtraPolicy {
        self.options
            .extra
            .unwrap_or(if self.serde.deny_unknown_fields {
                ExtraPolicy::Forbid
            } else {
                ExtraPolicy::Ignore
            })
    }

    /// Whether unknown keys are rejected (schema: `additionalProperties: false`).
    pub fn forbids_extra(&self) -> bool {
        self.extra() == ExtraPolicy::Forbid
    }

    /// Tokens of the `const ModelConfig` initializer.
    pub fn config_tokens(&self) -> TokenStream {
        let o = &self.options;
        let validation = quote!(::axumapi::validation);
        let extra = match self.extra() {
            ExtraPolicy::Ignore => quote!(#validation::Extra::Ignore),
            ExtraPolicy::Forbid => quote!(#validation::Extra::Forbid),
            ExtraPolicy::Allow => quote!(#validation::Extra::Allow),
        };
        let (strict, strip, lower, upper) = (
            o.strict,
            o.str_strip_whitespace,
            o.str_to_lower,
            o.str_to_upper,
        );
        quote! {
            #validation::ModelConfig {
                strict: #strict,
                extra: #extra,
                str_strip_whitespace: #strip,
                str_to_lower: #lower,
                str_to_upper: #upper,
            }
        }
    }
}
