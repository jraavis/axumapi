//! `#[model_hooks]`: turns an inherent `impl` block into a
//! `ModelHooks` implementation.
//!
//! Methods carry one of these helper attributes (removed from the emitted
//! impl, which otherwise stays untouched):
//!
//! | attribute | signature |
//! |---|---|
//! | `#[field_validator("a", "b", mode = "after")]` | `fn(value: &FieldTy) -> Result<(), FieldError>` |
//! | `#[field_validator("a", mode = "before")]` | `fn(value: &mut Value) -> Result<(), FieldError>` |
//! | `#[model_validator(mode = "after")]` | `fn(&self) -> Result<(), FieldError>` |
//! | `#[model_validator(mode = "before")]` | `fn(input: &mut Value) -> Result<(), FieldError>` |
//! | `#[computed_field]` / `#[computed_field(alias = "x")]` | `fn(&self) -> T` (`T: Dump + Schema`) |
//! | `#[field_serializer("a")]` | `fn(value: &FieldTy) -> V` (`V: Serialize`, `Value` included) |
//! | `#[model_serializer]` | `fn(&self, value: Value) -> Value` |

mod expand;
mod parse;

use proc_macro2::TokenStream;

/// Expand `#[model_hooks] impl ...`.
pub fn expand(args: TokenStream, item: TokenStream) -> TokenStream {
    expand::run(args, item).unwrap_or_else(|err| err.into_compile_error())
}
