//! Building blocks used by `#[derive(Validate)]` for structs with named
//! fields. They are public so hand-written models can use them too; the
//! reference model in this module's tests shows the exact shape the derive
//! generates.

use crate::context::{Extra, ModelConfig, ValidationContext};
use crate::error::FieldError;
use crate::rules;
use crate::validate::{Validate, json_type};
use regex::Regex;
use serde::de::DeserializeOwned;
use serde_json::{Number, Value};

/// Static description of one field of a model.
#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    /// Key Serde deserializes the field from (after `rename`/`rename_all`).
    pub key: &'static str,
    /// Other accepted input keys (validation aliases; the Rust field name
    /// when `populate_by_name`). The first alias present wins over none.
    pub aliases: &'static [&'static str],
    /// Whether a missing key is an error (no `Option`, no default).
    pub required: bool,
}

impl FieldSpec {
    /// Name used in error locations: the first alias (Pydantic reports the
    /// alias by default) or the key.
    pub fn input_name(&self) -> &'static str {
        self.aliases.first().copied().unwrap_or(self.key)
    }
}

/// Prepare a model object: installs `config`, checks the input is an object,
/// resolves aliases, reports missing and extra keys, and calls `field` for
/// each present field with the location set to the key the client sent.
pub fn prepare_object(
    input: &mut Value,
    ctx: &mut ValidationContext,
    config: ModelConfig,
    specs: &[FieldSpec],
    mut field: impl FnMut(usize, &mut Value, &mut ValidationContext),
) {
    ctx.with_config(config, |ctx| {
        let Value::Object(map) = input else {
            let got = json_type(input);
            ctx.error("model_type", format!("expected an object, got {got}"));
            return;
        };
        for (index, spec) in specs.iter().enumerate() {
            let mut sent_as = spec.key;
            if !map.contains_key(spec.key)
                && let Some(alias) = spec.aliases.iter().find(|a| map.contains_key(**a))
                && let Some(value) = map.remove(*alias)
            {
                map.insert(spec.key.to_owned(), value);
                sent_as = alias;
            }
            match map.get_mut(spec.key) {
                Some(slot) => ctx.at(sent_as, |ctx| field(index, slot, ctx)),
                None if spec.required => {
                    ctx.at(spec.input_name(), |ctx| {
                        ctx.error("missing", "field required")
                    });
                }
                None => {}
            }
        }
        let known = |k: &str| specs.iter().any(|s| s.key == k || s.aliases.contains(&k));
        let extras: Vec<String> = map.keys().filter(|k| !known(k)).cloned().collect();
        for key in extras {
            if config.extra == Extra::Forbid {
                ctx.at(key.as_str(), |ctx| {
                    ctx.error("extra_forbidden", "extra inputs are not permitted")
                });
            } else {
                // Keep Serde happy even with `deny_unknown_fields`.
                map.remove(&key);
            }
        }
    });
}

/// A field constraint checked on the (already coerced) raw value.
///
/// Checks are skipped for `null` (optional fields) and for values of the
/// wrong JSON type (the type error is already reported by `prepare`).
#[derive(Debug, Clone, Copy)]
pub enum Constraint<'a> {
    /// Minimum length (characters for strings, items for arrays/objects).
    MinLength(usize),
    /// Maximum length.
    MaxLength(usize),
    /// Regex the whole string must match (`None` = pattern failed to compile).
    Pattern(Option<&'a Regex>),
    /// Email format.
    Email,
    /// `> n`.
    Gt(&'a Number),
    /// `>= n`.
    Ge(&'a Number),
    /// `< n`.
    Lt(&'a Number),
    /// `<= n`.
    Le(&'a Number),
    /// Multiple of `n`.
    MultipleOf(&'a Number),
}

/// Compare two JSON numbers exactly when both are integers.
fn cmp_numbers(a: &Number, b: &Number) -> Option<std::cmp::Ordering> {
    match (
        a.as_i64().map(i128::from).or(a.as_u64().map(i128::from)),
        b.as_i64().map(i128::from).or(b.as_u64().map(i128::from)),
    ) {
        (Some(x), Some(y)) => Some(x.cmp(&y)),
        _ => a.as_f64()?.partial_cmp(&b.as_f64()?),
    }
}

/// Apply `constraint` to `value`, reporting at the current location.
pub fn check(value: &Value, constraint: Constraint<'_>, ctx: &mut ValidationContext) {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let len = match value {
        Value::String(s) => Some(s.chars().count()),
        Value::Array(a) => Some(a.len()),
        Value::Object(o) => Some(o.len()),
        _ => None,
    };
    let result = match (constraint, value) {
        (_, Value::Null) => Ok(()),
        (Constraint::MinLength(min), _) => match len {
            Some(n) if n < min => Err(FieldError::new(
                "too_short",
                format!("should have at least {min} items/characters"),
            )),
            _ => Ok(()),
        },
        (Constraint::MaxLength(max), _) => match len {
            Some(n) if n > max => Err(FieldError::new(
                "too_long",
                format!("should have at most {max} items/characters"),
            )),
            _ => Ok(()),
        },
        (Constraint::Pattern(Some(re)), Value::String(s)) => rules::pattern(s, re),
        (Constraint::Pattern(None), _) => Err(FieldError::new(
            "pattern_invalid",
            "the field pattern is not a valid regex",
        )),
        (Constraint::Email, Value::String(s)) => rules::email(s),
        (c, Value::Number(n)) => {
            let bound =
                |limit: &Number, ok: &[std::cmp::Ordering], code: &'static str, op: &str| {
                    match cmp_numbers(n, limit) {
                        Some(o) if ok.contains(&o) => Ok(()),
                        _ => Err(FieldError::new(code, format!("should be {op} {limit}"))),
                    }
                };
            match c {
                Constraint::Gt(l) => bound(l, &[Greater], "greater_than", ">"),
                Constraint::Ge(l) => bound(l, &[Greater, Equal], "greater_than_equal", ">="),
                Constraint::Lt(l) => bound(l, &[Less], "less_than", "<"),
                Constraint::Le(l) => bound(l, &[Less, Equal], "less_than_equal", "<="),
                Constraint::MultipleOf(m) => match (n.as_i64(), m.as_i64()) {
                    (Some(x), Some(y)) => rules::multiple_of(x, y),
                    _ => match (n.as_f64(), m.as_f64()) {
                        (Some(x), Some(y))
                            if y != 0.0 && ((x / y) - (x / y).round()).abs() < 1e-9 =>
                        {
                            Ok(())
                        }
                        _ => Err(FieldError::new(
                            "multiple_of",
                            format!("should be a multiple of {m}"),
                        )),
                    },
                },
                _ => Ok(()),
            }
        }
        _ => Ok(()),
    };
    ctx.check(result);
}

/// Differential check used in tests: for every input, `prepare` reporting
/// no errors must imply Serde succeeds, and a Serde failure must have been
/// reported by `prepare`. Returns a description of the first disagreement.
///
/// # Errors
/// The first input on which `prepare` and Serde disagree.
#[doc(hidden)]
pub fn prepare_agrees_with_serde<T>(inputs: &[Value]) -> Result<(), String>
where
    T: DeserializeOwned + Validate,
{
    for input in inputs {
        let mut prepared = input.clone();
        let mut ctx = ValidationContext::new();
        T::prepare(&mut prepared, &mut ctx);
        let prepare_ok = !ctx.has_errors();
        let serde_ok = serde_json::from_value::<T>(prepared.clone()).is_ok();
        if prepare_ok && !serde_ok {
            return Err(format!(
                "prepare accepted but serde rejected: {input} (prepared {prepared})"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::needless_borrow)]
mod tests {
    //! A hand-written model identical to what `#[derive(Validate)]` generates
    //! for:
    //!
    //! ```ignore
    //! #[derive(Deserialize, Validate)]
    //! #[serde(rename_all = "camelCase")]
    //! #[model_config(extra = "forbid", str_strip_whitespace)]
    //! struct Signup {
    //!     #[field(min_length = 2, validation_alias = "login")]
    //!     user_name: String,
    //!     #[field(ge = 18)]
    //!     age: Option<u8>,
    //!     tags: Vec<String>,
    //! }
    //! #[model_hooks]
    //! impl Signup {
    //!     #[model_validator(mode = "after")]
    //!     fn no_admin(&self) -> ValidationResult<()> { ... }
    //! }
    //! ```
    use super::*;
    use crate::context::ModelConfig;
    #[allow(unused_imports)]
    use crate::hooks::{ModelHooks, Probe, ViaDefault as _, ViaHooks as _};
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Signup {
        user_name: String,
        age: Option<u8>,
        tags: Vec<String>,
    }

    const CONFIG: ModelConfig = ModelConfig {
        extra: Extra::Forbid,
        str_strip_whitespace: true,
        ..ModelConfig::DEFAULT
    };
    const FIELDS: &[FieldSpec] = &[
        FieldSpec {
            key: "userName",
            aliases: &["login"],
            required: true,
        },
        FieldSpec {
            key: "age",
            aliases: &[],
            required: false,
        },
        FieldSpec {
            key: "tags",
            aliases: &[],
            required: true,
        },
    ];

    impl Validate for Signup {
        fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
            (&&Probe::<Self>::new()).before_model(input, ctx);
            prepare_object(input, ctx, CONFIG, FIELDS, |index, slot, ctx| match index {
                0 => {
                    (&&Probe::<Self>::new()).before_field("user_name", slot, ctx);
                    let before = ctx.error_count();
                    <String as Validate>::prepare(slot, ctx);
                    if ctx.error_count() == before {
                        check(slot, Constraint::MinLength(2), ctx);
                    }
                }
                1 => {
                    (&&Probe::<Self>::new()).before_field("age", slot, ctx);
                    let before = ctx.error_count();
                    <Option<u8> as Validate>::prepare(slot, ctx);
                    if ctx.error_count() == before {
                        check(slot, Constraint::Ge(&Number::from(18)), ctx);
                    }
                }
                2 => {
                    (&&Probe::<Self>::new()).before_field("tags", slot, ctx);
                    <Vec<String> as Validate>::prepare(slot, ctx);
                }
                _ => {}
            });
        }

        fn validate(&self, ctx: &mut ValidationContext) {
            ctx.with_config(CONFIG, |ctx| {
                ctx.at("userName", |ctx| self.user_name.validate(ctx));
                ctx.at("age", |ctx| self.age.validate(ctx));
                ctx.at("tags", |ctx| self.tags.validate(ctx));
                (&&Probe::<Self>::new()).after_fields(self, ctx);
                (&&Probe::<Self>::new()).after_model(self, ctx);
            });
        }
    }

    impl ModelHooks for Signup {
        fn after_model(&self, ctx: &mut ValidationContext) {
            if self.user_name == "admin" {
                ctx.error("reserved", "admin is reserved");
            }
        }
    }

    fn codes(input: Value) -> Vec<(String, String)> {
        match crate::parse_value::<Signup>(input, ValidationContext::new().at_root("body")) {
            Ok(_) => vec![],
            Err(e) => e
                .errors
                .into_iter()
                .map(|e| {
                    (
                        e.location
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join("."),
                        e.code.into_owned(),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn valid_input_with_alias_and_coercion() {
        let s: Signup = crate::parse_value(
            json!({"login": "  bob ", "age": "30", "tags": []}),
            ValidationContext::new(),
        )
        .unwrap();
        assert_eq!(s.user_name, "bob");
        assert_eq!(s.age, Some(30));
    }

    #[test]
    fn every_error_is_reported_with_input_locations() {
        let got = codes(json!({"login": "b", "age": 10, "extra": 1}));
        assert_eq!(
            got,
            vec![
                ("body.login".into(), "too_short".into()),
                ("body.age".into(), "greater_than_equal".into()),
                ("body.tags".into(), "missing".into()),
                ("body.extra".into(), "extra_forbidden".into()),
            ]
        );
    }

    #[test]
    fn model_validator_runs_after_fields() {
        assert_eq!(
            codes(json!({"userName": "admin", "tags": []})),
            vec![("body".into(), "reserved".into())]
        );
    }

    #[test]
    fn non_object_input() {
        assert_eq!(
            codes(json!([1])),
            vec![("body".into(), "model_type".into())]
        );
    }

    #[test]
    fn prepare_and_serde_agree() {
        let inputs = [
            json!({}),
            json!(null),
            json!({"userName": "ab", "tags": ["x"]}),
            json!({"userName": 5, "tags": "x"}),
            json!({"userName": "ab", "tags": [1]}),
            json!({"login": "ab", "userName": "cd", "tags": []}),
            json!({"userName": "ab", "age": 300, "tags": []}),
            json!({"userName": "ab", "age": null, "tags": []}),
            json!({"userName": "ab", "age": "19", "tags": []}),
        ];
        prepare_agrees_with_serde::<Signup>(&inputs).unwrap();
    }
}
