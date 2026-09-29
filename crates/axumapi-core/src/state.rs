//! Shared application state extractor.

use crate::error::ApiError;
use crate::extract::FromRequestParts;
use http::request::Parts;
use std::sync::Arc;

/// Extracts the `Arc<T>` registered with [`App::with_state`](crate::App::with_state).
#[derive(Debug)]
pub struct State<T>(pub Arc<T>);

impl<T> Clone for State<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T: Send + Sync + 'static> FromRequestParts for State<T> {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<Arc<T>>()
            .cloned()
            .map(State)
            .ok_or_else(|| {
                ApiError::internal(format!(
                    "no application state of type `{}` registered",
                    std::any::type_name::<T>()
                ))
            })
    }
}
