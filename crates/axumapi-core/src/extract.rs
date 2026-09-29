//! Request extractors whose rejections are rendered as [`ApiError`].

use crate::error::ApiError;
use crate::response::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, FromRequestParts, Request};
use http::StatusCode;
use http::request::Parts;
use serde::de::DeserializeOwned;
use serde_json::json;

/// Path parameter extractor (`/users/{id}`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Path<T>(pub T);

/// Query-string extractor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Query<T>(pub T);

/// Build a 422 error with one FastAPI-style entry.
fn invalid(location: &str, code: &str, message: String) -> ApiError {
    ApiError::unprocessable(json!([{
        "location": [location],
        "code": code,
        "message": message,
    }]))
}

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Json(value)),
            Err(rejection) => Err(json_error(&rejection)),
        }
    }
}

fn json_error(rejection: &JsonRejection) -> ApiError {
    let message = rejection.body_text();
    match rejection {
        JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_) => {
            invalid("body", "json_invalid", message)
        }
        JsonRejection::MissingJsonContentType(_) => {
            ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, message)
        }
        other => ApiError::new(other.status(), message),
    }
}

impl<T, S> FromRequestParts<S> for Path<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        use axum::extract::rejection::PathRejection;
        match axum::extract::Path::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Path(value)) => Ok(Path(value)),
            Err(PathRejection::FailedToDeserializePathParams(err)) => {
                Err(invalid("path", "path_invalid", err.body_text()))
            }
            Err(other) => Err(ApiError::internal(other)),
        }
    }
}

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(value)) => Ok(Query(value)),
            Err(rejection) => Err(invalid("query", "query_invalid", rejection.body_text())),
        }
    }
}
