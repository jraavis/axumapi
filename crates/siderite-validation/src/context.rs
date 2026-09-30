//! Validation context: current location, collected errors, model settings
//! and user data shared with validators.

use crate::error::{FieldError, LocationItem, ValidationError};
use std::any::{Any, TypeId};
use std::borrow::Cow;
use std::collections::HashMap;

/// How unknown input keys are treated (Pydantic `extra`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Extra {
    /// Drop unknown keys silently (default).
    #[default]
    Ignore,
    /// Reject unknown keys with `extra_forbidden`. Keys of fields Serde
    /// skips when deserializing count as unknown, as with
    /// `deny_unknown_fields`.
    Forbid,
    /// Accept unknown keys. Rust structs cannot store them, so they are
    /// dropped after validation; see `PYDANTIC_EQUIVALENCE.md`.
    Allow,
}

/// Per-model settings (Pydantic `model_config`) that leaf types consult.
///
/// Settings are **not** inherited by nested models: each derived model
/// installs its own config while its fields are prepared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelConfig {
    /// Reject lax coercions (e.g. `"1"` for an integer).
    pub strict: bool,
    /// Unknown-key handling.
    pub extra: Extra,
    /// Trim surrounding whitespace of string inputs.
    pub str_strip_whitespace: bool,
    /// Lower-case string inputs.
    pub str_to_lower: bool,
    /// Upper-case string inputs.
    pub str_to_upper: bool,
}

impl ModelConfig {
    /// Default configuration (lax, ignore extras, no string transforms).
    pub const DEFAULT: Self = Self {
        strict: false,
        extra: Extra::Ignore,
        str_strip_whitespace: false,
        str_to_lower: false,
        str_to_upper: false,
    };
}

/// Where the input came from; text sources (query strings, forms, path
/// segments) always allow string → scalar coercion, even in strict mode,
/// because they cannot carry typed values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputKind {
    /// JSON (typed) input.
    #[default]
    Json,
    /// Text-only input (query, form, path).
    Text,
}

/// Mutable state threaded through `prepare` and `validate`.
#[derive(Default)]
pub struct ValidationContext {
    config: ModelConfig,
    strict_override: Option<bool>,
    input: InputKind,
    location: Vec<LocationItem>,
    errors: ValidationError,
    data: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl std::fmt::Debug for ValidationContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidationContext")
            .field("config", &self.config)
            .field("strict_override", &self.strict_override)
            .field("input", &self.input)
            .field("location", &self.location)
            .field("errors", &self.errors)
            .finish_non_exhaustive()
    }
}

impl ValidationContext {
    /// Fresh context for JSON input.
    pub fn new() -> Self {
        Self::default()
    }

    /// Context for text input (query strings, forms, path segments).
    pub fn for_text() -> Self {
        Self {
            input: InputKind::Text,
            ..Self::default()
        }
    }

    /// Force strict (`true`) or lax (`false`) mode for every model, like
    /// Pydantic's `model_validate(strict=...)`.
    #[must_use]
    pub fn with_strict(mut self, strict: bool) -> Self {
        self.strict_override = Some(strict);
        self
    }

    /// Prefix every reported location with `item` (e.g. `"body"`).
    #[must_use]
    pub fn at_root(mut self, item: impl Into<LocationItem>) -> Self {
        self.location.push(item.into());
        self
    }

    /// Attach user data readable by validators (Pydantic validation context).
    #[must_use]
    pub fn with_data<T: Any + Send + Sync>(mut self, value: T) -> Self {
        self.data.insert(TypeId::of::<T>(), Box::new(value));
        self
    }

    /// User data of type `T`, if attached.
    pub fn data<T: Any + Send + Sync>(&self) -> Option<&T> {
        self.data.get(&TypeId::of::<T>())?.downcast_ref()
    }

    /// Settings of the model currently being processed.
    pub fn config(&self) -> &ModelConfig {
        &self.config
    }

    /// Whether lax string → scalar coercion is allowed right now.
    pub fn allows_text_coercion(&self) -> bool {
        self.input == InputKind::Text || !self.is_strict()
    }

    /// Effective strictness (override beats model config).
    pub fn is_strict(&self) -> bool {
        self.strict_override.unwrap_or(self.config.strict)
    }

    /// Input kind.
    pub fn input_kind(&self) -> InputKind {
        self.input
    }

    /// Run `f` with `config` installed, restoring the previous one after.
    pub fn with_config<R>(&mut self, config: ModelConfig, f: impl FnOnce(&mut Self) -> R) -> R {
        let previous = std::mem::replace(&mut self.config, config);
        let out = f(self);
        self.config = previous;
        out
    }

    /// Run `f` with `item` appended to the current location.
    pub fn at<R>(&mut self, item: impl Into<LocationItem>, f: impl FnOnce(&mut Self) -> R) -> R {
        self.location.push(item.into());
        let out = f(self);
        self.location.pop();
        out
    }

    /// Current location.
    pub fn location(&self) -> &[LocationItem] {
        &self.location
    }

    /// Record an error at the current location.
    pub fn error(&mut self, code: impl Into<Cow<'static, str>>, message: impl Into<String>) {
        self.push(FieldError::new(code, message));
    }

    /// Record `error`, prefixing its location with the current location.
    pub fn push(&mut self, mut error: FieldError) {
        let mut location = self.location.clone();
        location.append(&mut error.location);
        error.location = location;
        self.errors.push(error);
    }

    /// Record the result of a rule check at the current location.
    pub fn check(&mut self, result: Result<(), FieldError>) {
        if let Err(error) = result {
            self.push(error);
        }
    }

    /// Whether any error has been recorded.
    pub fn has_errors(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Number of errors recorded so far (useful to detect new ones).
    pub fn error_count(&self) -> usize {
        self.errors.errors.len()
    }

    /// Take the collected errors, leaving the context empty of errors.
    pub fn take_errors(&mut self) -> ValidationError {
        std::mem::take(&mut self.errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_nest_and_restore() {
        let mut ctx = ValidationContext::new().at_root("body");
        ctx.at("user", |ctx| ctx.at(0_usize, |ctx| ctx.error("x", "bad")));
        ctx.error("y", "top");
        let errors = ctx.take_errors().errors;
        assert_eq!(
            errors[0].location,
            vec!["body".into(), "user".into(), LocationItem::index(0)]
        );
        assert_eq!(errors[1].location, vec![LocationItem::key("body")]);
    }

    #[test]
    fn strictness_override_and_text_input() {
        let strict = ModelConfig {
            strict: true,
            ..ModelConfig::DEFAULT
        };
        let mut ctx = ValidationContext::new();
        ctx.with_config(strict, |ctx| {
            assert!(ctx.is_strict());
            assert!(!ctx.allows_text_coercion());
        });
        assert!(!ctx.is_strict());
        let mut text = ValidationContext::for_text().with_strict(true);
        assert!(text.allows_text_coercion());
        text.with_config(ModelConfig::DEFAULT, |ctx| assert!(ctx.is_strict()));
    }

    #[test]
    fn user_data_round_trips() {
        let ctx = ValidationContext::new().with_data(42_u32);
        assert_eq!(ctx.data::<u32>(), Some(&42));
        assert!(ctx.data::<i64>().is_none());
    }
}
