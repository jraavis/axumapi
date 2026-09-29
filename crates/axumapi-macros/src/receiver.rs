//! `#[receiver(signal, model = M)]` expansion.

use proc_macro2::TokenStream;

/// Expand `#[receiver]` (implemented in the signals branch).
pub(crate) fn expand(_args: TokenStream, item: TokenStream) -> TokenStream {
    item
}
