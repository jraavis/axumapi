//! Error accumulation so several diagnostics can be reported at once.

/// Collects [`syn::Error`]s, combining them into a single multi-span error.
#[derive(Default)]
pub struct Errors(Option<syn::Error>);

impl Errors {
    /// Record an error.
    pub fn push(&mut self, error: syn::Error) {
        match &mut self.0 {
            Some(existing) => existing.combine(error),
            None => self.0 = Some(error),
        }
    }

    /// Record an error with a message anchored at `span`.
    pub fn error(&mut self, span: proc_macro2::Span, message: impl std::fmt::Display) {
        self.push(syn::Error::new(span, message));
    }

    /// Record an error spanning all of `tokens`.
    pub fn spanned(&mut self, tokens: impl quote::ToTokens, message: impl std::fmt::Display) {
        self.push(syn::Error::new_spanned(tokens, message));
    }

    /// Record the error of `result`, if any, and return its value.
    pub fn absorb<T>(&mut self, result: syn::Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(err) => {
                self.push(err);
                None
            }
        }
    }

    /// `Ok(value)` when no error was recorded, otherwise the combined error.
    pub fn finish<T>(self, value: T) -> syn::Result<T> {
        match self.0 {
            Some(err) => Err(err),
            None => Ok(value),
        }
    }

    /// The combined error, if any.
    pub fn into_error(self) -> Option<syn::Error> {
        self.0
    }
}
