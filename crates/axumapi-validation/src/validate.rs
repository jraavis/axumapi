//! The [`Validate`] trait and its implementations for standard types.
//!
//! Validation has two phases, mirroring Pydantic's before/after split:
//!
//! 1. [`Validate::prepare`] runs on the raw [`Value`] **before**
//!    deserialization. It is *type driven*: every type checks and, in lax
//!    mode, coerces its own slot of the input (`"42"` → `42` for integers,
//!    trimming for strings, ...). Derived models walk their fields, report
//!    missing/extra keys and recurse with the location extended. Because all
//!    errors are collected here, one request reports every problem at once.
//! 2. [`Validate::validate`] runs on the typed value **after**
//!    deserialization: field constraints, after-validators, model validators.
//!
//! Both phases push errors into a [`ValidationContext`]; neither panics.

use crate::context::ValidationContext;
use crate::error::LocationItem;
use crate::rules;
use crate::types::{BoundedI64, ConstrainedString, Email, PositiveInt, SecretString};
use serde_json::{Map, Number, Value};
use std::collections::{BTreeMap, HashMap};

/// A type that takes part in validation.
///
/// Both methods default to doing nothing, so opting a type in is a one-line
/// `impl Validate for T {}`. `#[derive(Validate)]` generates both.
pub trait Validate {
    /// Check and normalise the raw input for this type in place.
    fn prepare(_input: &mut Value, _ctx: &mut ValidationContext) {}

    /// Check invariants of the deserialized value.
    fn validate(&self, _ctx: &mut ValidationContext) {}
}

/// JSON type name used in error messages.
pub(crate) fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_error(ctx: &mut ValidationContext, code: &'static str, expected: &str, got: &Value) {
    ctx.error(code, format!("expected {expected}, got {}", json_type(got)));
}

/// Coerce/check an integer slot against `[min, max]`.
fn prepare_int(input: &mut Value, ctx: &mut ValidationContext, min: i128, max: i128) {
    let candidate: Option<i128> = match &*input {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i128::from(i))
            } else if let Some(u) = n.as_u64() {
                Some(i128::from(u))
            } else if let Some(f) = n.as_f64() {
                if ctx.is_strict() || f.fract() != 0.0 || !f.is_finite() {
                    ctx.error(
                        "int_from_float",
                        "expected an integer, got a number with a fractional part",
                    );
                    return;
                }
                // Exact: fract() == 0 and range is checked below.
                #[allow(clippy::cast_possible_truncation)]
                Some(f as i128)
            } else {
                None
            }
        }
        Value::String(s) if ctx.allows_text_coercion() => match s.trim().parse::<i128>() {
            Ok(i) => Some(i),
            Err(_) => {
                ctx.error("int_parsing", "input is not a valid integer");
                return;
            }
        },
        other => {
            let other = other.clone();
            type_error(ctx, "int_type", "an integer", &other);
            return;
        }
    };
    let Some(value) = candidate else {
        ctx.error("int_parsing", "input is not a valid integer");
        return;
    };
    if value < min || value > max {
        ctx.error(
            "int_out_of_range",
            format!("integer must be between {min} and {max}"),
        );
        return;
    }
    *input = if value < 0 {
        i64::try_from(value).map_or(Value::Null, |v| Value::Number(v.into()))
    } else {
        u64::try_from(value).map_or(Value::Null, |v| Value::Number(v.into()))
    };
}

macro_rules! int_validate {
    ($($t:ty),*) => {$(
        impl Validate for $t {
            fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
                prepare_int(input, ctx, i128::from(<$t>::MIN), i128::from(<$t>::MAX));
            }
        }
    )*};
}
int_validate!(i8, i16, i32, i64, u8, u16, u32, u64);

fn prepare_float(input: &mut Value, ctx: &mut ValidationContext) {
    match &*input {
        Value::Number(_) => {}
        Value::String(s) if ctx.allows_text_coercion() => {
            match s.trim().parse::<f64>().ok().and_then(Number::from_f64) {
                Some(n) => *input = Value::Number(n),
                None => ctx.error("float_parsing", "input is not a valid finite number"),
            }
        }
        other => {
            let other = other.clone();
            type_error(ctx, "float_type", "a number", &other);
        }
    }
}

impl Validate for f32 {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        prepare_float(input, ctx);
    }
}

impl Validate for f64 {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        prepare_float(input, ctx);
    }
}

impl Validate for bool {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        let coerced = match &*input {
            Value::Bool(_) => return,
            Value::String(s) if ctx.allows_text_coercion() => {
                match s.trim().to_ascii_lowercase().as_str() {
                    "true" | "t" | "yes" | "y" | "on" | "1" => Some(true),
                    "false" | "f" | "no" | "n" | "off" | "0" => Some(false),
                    _ => None,
                }
            }
            Value::Number(n) if !ctx.is_strict() => match n.as_u64() {
                Some(0) => Some(false),
                Some(1) => Some(true),
                _ => None,
            },
            other => {
                let other = other.clone();
                type_error(ctx, "bool_type", "a boolean", &other);
                return;
            }
        };
        match coerced {
            Some(b) => *input = Value::Bool(b),
            None => ctx.error("bool_parsing", "input is not a valid boolean"),
        }
    }
}

/// Apply the current model's string transforms and check the type.
fn prepare_string(input: &mut Value, ctx: &mut ValidationContext) -> Option<()> {
    let Value::String(s) = input else {
        let other = input.clone();
        type_error(ctx, "string_type", "a string", &other);
        return None;
    };
    let config = *ctx.config();
    if config.str_strip_whitespace {
        let trimmed = s.trim();
        if trimmed.len() != s.len() {
            *s = trimmed.to_owned();
        }
    }
    if config.str_to_lower {
        *s = s.to_lowercase();
    } else if config.str_to_upper {
        *s = s.to_uppercase();
    }
    Some(())
}

impl Validate for String {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        prepare_string(input, ctx);
    }
}

impl Validate for Value {}

impl<T: Validate> Validate for Option<T> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        if !input.is_null() {
            T::prepare(input, ctx);
        }
    }

    fn validate(&self, ctx: &mut ValidationContext) {
        if let Some(inner) = self {
            inner.validate(ctx);
        }
    }
}

impl<T: Validate + ?Sized> Validate for Box<T> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        T::prepare(input, ctx);
    }

    fn validate(&self, ctx: &mut ValidationContext) {
        (**self).validate(ctx);
    }
}

impl<T: Validate> Validate for Vec<T> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        if !input.is_array() {
            // Text sources repeat keys for lists; a single value is a one-item list.
            if ctx.input_kind() == crate::context::InputKind::Text && !input.is_null() {
                *input = Value::Array(vec![input.take()]);
            } else {
                let other = input.clone();
                type_error(ctx, "list_type", "an array", &other);
                return;
            }
        }
        if let Value::Array(items) = input {
            for (index, item) in items.iter_mut().enumerate() {
                ctx.at(LocationItem::index(index), |ctx| T::prepare(item, ctx));
            }
        }
    }

    fn validate(&self, ctx: &mut ValidationContext) {
        for (index, item) in self.iter().enumerate() {
            ctx.at(LocationItem::index(index), |ctx| item.validate(ctx));
        }
    }
}

fn prepare_map<T: Validate>(input: &mut Value, ctx: &mut ValidationContext) {
    let Value::Object(map) = input else {
        let other = input.clone();
        type_error(ctx, "dict_type", "an object", &other);
        return;
    };
    for (key, value) in map.iter_mut() {
        ctx.at(key.as_str(), |ctx| T::prepare(value, ctx));
    }
}

impl<T: Validate> Validate for BTreeMap<String, T> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        prepare_map::<T>(input, ctx);
    }

    fn validate(&self, ctx: &mut ValidationContext) {
        for (key, value) in self {
            ctx.at(key.as_str(), |ctx| value.validate(ctx));
        }
    }
}

impl<T: Validate, S> Validate for HashMap<String, T, S> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        prepare_map::<T>(input, ctx);
    }

    fn validate(&self, ctx: &mut ValidationContext) {
        for (key, value) in self {
            ctx.at(key.as_str(), |ctx| value.validate(ctx));
        }
    }
}

impl Validate for Map<String, Value> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        prepare_map::<Value>(input, ctx);
    }
}

macro_rules! tuple_validate {
    ($($t:ident $i:tt),+) => {
        /// Tuples (e.g. positional path parameters) validate each element.
        impl<$($t: Validate),+> Validate for ($($t,)+) {
            fn validate(&self, ctx: &mut ValidationContext) {
                $(ctx.at(LocationItem::index($i), |ctx| self.$i.validate(ctx));)+
            }
        }
    };
}
tuple_validate!(A 0);
tuple_validate!(A 0, B 1);
tuple_validate!(A 0, B 1, C 2);
tuple_validate!(A 0, B 1, C 2, D 3);

impl Validate for char {}

impl<const MIN: usize, const MAX: usize> Validate for ConstrainedString<MIN, MAX> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        if prepare_string(input, ctx).is_some() {
            let s = input.as_str().unwrap_or_default();
            ctx.check(rules::min_length(s, MIN));
            ctx.check(rules::max_length(s, MAX));
        }
    }
}

impl Validate for Email {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        if prepare_string(input, ctx).is_some() {
            ctx.check(rules::email(input.as_str().unwrap_or_default()));
        }
    }
}

impl Validate for SecretString {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        prepare_string(input, ctx);
    }
}

impl<const MIN: i64, const MAX: i64> Validate for BoundedI64<MIN, MAX> {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        prepare_int(input, ctx, i128::from(MIN), i128::from(MAX));
    }
}

impl Validate for PositiveInt {
    fn prepare(input: &mut Value, ctx: &mut ValidationContext) {
        let before = ctx.error_count();
        prepare_int(input, ctx, i128::from(i64::MIN), i128::from(i64::MAX));
        if ctx.error_count() == before {
            ctx.check(rules::gt(&input.as_i64().unwrap_or_default(), &0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ModelConfig;
    use serde_json::json;

    fn run<T: Validate>(mut input: Value, ctx: &mut ValidationContext) -> (Value, Vec<String>) {
        T::prepare(&mut input, ctx);
        let codes = ctx
            .take_errors()
            .errors
            .into_iter()
            .map(|e| e.code.into_owned())
            .collect();
        (input, codes)
    }

    fn lax<T: Validate>(input: Value) -> (Value, Vec<String>) {
        run::<T>(input, &mut ValidationContext::new())
    }

    fn strict<T: Validate>(input: Value) -> (Value, Vec<String>) {
        run::<T>(input, &mut ValidationContext::new().with_strict(true))
    }

    #[test]
    fn integers_coerce_in_lax_mode_only() {
        assert_eq!(lax::<i32>(json!("42")), (json!(42), vec![]));
        assert_eq!(lax::<i32>(json!(3.0)), (json!(3), vec![]));
        assert_eq!(lax::<i32>(json!(3.5)).1, ["int_from_float"]);
        assert_eq!(lax::<u8>(json!(300)).1, ["int_out_of_range"]);
        assert_eq!(lax::<u8>(json!(-1)).1, ["int_out_of_range"]);
        assert_eq!(lax::<i64>(json!("x")).1, ["int_parsing"]);
        assert_eq!(strict::<i32>(json!("42")).1, ["int_type"]);
        assert_eq!(strict::<i32>(json!(3.0)).1, ["int_from_float"]);
        assert_eq!(lax::<i32>(json!(true)).1, ["int_type"]);
    }

    #[test]
    fn text_input_coerces_even_when_strict() {
        let mut ctx = ValidationContext::for_text().with_strict(true);
        assert_eq!(run::<i64>(json!("7"), &mut ctx), (json!(7), vec![]));
        assert_eq!(run::<Vec<u8>>(json!("7"), &mut ctx), (json!([7]), vec![]));
    }

    #[test]
    fn floats_and_bools() {
        assert_eq!(lax::<f64>(json!("1.5")), (json!(1.5), vec![]));
        assert_eq!(lax::<f64>(json!("nan")).1, ["float_parsing"]);
        assert_eq!(lax::<bool>(json!("Yes")), (json!(true), vec![]));
        assert_eq!(lax::<bool>(json!(0)), (json!(false), vec![]));
        assert_eq!(lax::<bool>(json!("maybe")).1, ["bool_parsing"]);
        assert_eq!(strict::<bool>(json!(1)).1, ["bool_type"]);
    }

    #[test]
    fn strings_apply_model_transforms() {
        let config = ModelConfig {
            str_strip_whitespace: true,
            str_to_lower: true,
            ..ModelConfig::DEFAULT
        };
        let mut ctx = ValidationContext::new();
        let out = ctx.with_config(config, |ctx| run::<String>(json!("  HeLLo "), ctx));
        assert_eq!(out, (json!("hello"), vec![]));
        assert_eq!(lax::<String>(json!(5)).1, ["string_type"]);
    }

    #[test]
    fn containers_collect_every_error_with_locations() {
        let mut ctx = ValidationContext::new().at_root("body");
        let mut input = json!({"a": [1, "x", 2.5], "b": null});
        <BTreeMap<String, Vec<Option<i32>>>>::prepare(&mut input, &mut ctx);
        let errors = ctx.take_errors().errors;
        assert_eq!(errors.len(), 3);
        assert_eq!(
            errors[0].location,
            vec!["body".into(), "a".into(), LocationItem::index(1)]
        );
        assert_eq!(errors[2].location, vec!["body".into(), "b".into()]);
        assert_eq!(errors[2].code, "list_type");
    }

    #[test]
    fn constrained_types_check_during_prepare() {
        assert_eq!(
            lax::<ConstrainedString<2, 3>>(json!("a")).1,
            ["string_too_short"]
        );
        assert_eq!(lax::<Email>(json!("nope")).1, ["invalid_email"]);
        assert_eq!(lax::<PositiveInt>(json!("0")).1, ["greater_than"]);
        assert_eq!(lax::<BoundedI64<1, 5>>(json!(9)).1, ["int_out_of_range"]);
    }
}
