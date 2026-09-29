//! Shared application state extractor.

use crate::error::ApiError;
use axum::extract::FromRequestParts;
use http::request::Parts;
use std::sync::Arc;

/// Extracts the `Arc<T>` registered with [`App::with_state`](crate::App::with_state).
#[derive(Debug)]
pub struct State<T>(pub Arc<T>);

impl<T, S> FromRequestParts<S> for State<T>
where
    T: Send + Sync + 'static,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, ApiError> {
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
