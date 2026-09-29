//! Request extraction.
//!
//! axumapi owns its extractor traits so that every extractor can also
//! *document itself* for OpenAPI. The `describe` hooks default to doing
//! nothing, so a custom extractor needs only the extraction method.
//!
//! Handler arguments are extracted in order; all but the last must implement
//! [`FromRequestParts`]. The last may consume the body ([`FromRequest`]).

use crate::body::Body;
use crate::error::ApiError;
use axumapi_openapi::{
    Operation, Parameter, ParameterLocation, Schema, SchemaObject, SchemaRegistry,
};
use http::request::Parts;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::future::Future;

pub use crate::response::Json;

/// An HTTP request with an axumapi [`Body`].
pub type Request = http::Request<Body>;

/// Extract a value from request metadata (method, URI, headers, extensions).
pub trait FromRequestParts: Sized + Send {
    /// Perform the extraction.
    fn from_request_parts(parts: &mut Parts)
    -> impl Future<Output = Result<Self, ApiError>> + Send;

    /// Document this extractor on `op` (parameters, security, ...).
    fn describe(_op: &mut Operation, _registry: &mut SchemaRegistry) {}
}

/// Extract a value from the whole request, possibly consuming the body.
pub trait FromRequest: Sized + Send {
    /// Perform the extraction.
    fn from_request(req: Request) -> impl Future<Output = Result<Self, ApiError>> + Send;

    /// Document this extractor on `op` (request body, ...).
    fn describe(_op: &mut Operation, _registry: &mut SchemaRegistry) {}
}

impl<T: FromRequestParts> FromRequest for T {
    fn from_request(req: Request) -> impl Future<Output = Result<Self, ApiError>> + Send {
        let (mut parts, _body) = req.into_parts();
        async move { T::from_request_parts(&mut parts).await }
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        <T as FromRequestParts>::describe(op, registry);
    }
}

impl<T: FromRequestParts> FromRequestParts for Option<T> {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        Ok(T::from_request_parts(parts).await.ok())
    }
}

/// Build a 422 error with one location-tagged entry.
pub(crate) fn invalid(location: &str, code: &str, message: impl Into<String>) -> ApiError {
    ApiError::unprocessable(json!([{
        "location": [location],
        "code": code,
        "message": message.into(),
    }]))
}

/// Path parameter extractor (`/users/{id}`).
///
/// `T` may be a scalar, a tuple (positional) or a struct (by name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Path<T>(pub T);

impl<T> FromRequestParts for Path<T>
where
    T: DeserializeOwned + Send + Schema + 'static,
{
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        use axum::extract::FromRequestParts as _;
        use axum::extract::rejection::PathRejection;
        match axum::extract::Path::<T>::from_request_parts(parts, &()).await {
            Ok(axum::extract::Path(value)) => Ok(Path(value)),
            Err(PathRejection::FailedToDeserializePathParams(err)) => {
                Err(invalid("path", "path_invalid", err.body_text()))
            }
            Err(other) => Err(ApiError::internal(other)),
        }
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        let schema = T::schema(registry);
        if let Some(props) = object_properties(&schema) {
            for (name, prop, _) in props {
                op.add_parameter(Parameter::new(name, ParameterLocation::Path, true, prop));
            }
        } else if let Some(Value::Array(items)) = schema.get("prefixItems") {
            for item in items {
                let item = serde_json::from_value(item.clone()).unwrap_or_default();
                op.add_parameter(Parameter::new("", ParameterLocation::Path, true, item));
            }
        } else {
            op.add_parameter(Parameter::new("", ParameterLocation::Path, true, schema));
        }
    }
}

/// Query-string extractor. Each field of `T` becomes a query parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Query<T>(pub T);

impl<T> FromRequestParts for Query<T>
where
    T: DeserializeOwned + Send + Schema + 'static,
{
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        use axum::extract::FromRequestParts as _;
        match axum::extract::Query::<T>::from_request_parts(parts, &()).await {
            Ok(axum::extract::Query(value)) => Ok(Query(value)),
            Err(rejection) => Err(invalid("query", "query_invalid", rejection.body_text())),
        }
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        let schema = T::schema(registry);
        for (name, prop, required) in object_properties(&schema).unwrap_or_default() {
            op.add_parameter(Parameter::new(
                name,
                ParameterLocation::Query,
                required,
                prop,
            ));
        }
    }
}

/// `(name, schema, required)` for each property of an object schema.
pub(crate) fn object_properties(
    schema: &SchemaObject,
) -> Option<Vec<(String, SchemaObject, bool)>> {
    let Value::Object(props) = schema.get("properties")? else {
        return None;
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    Some(
        props
            .iter()
            .map(|(name, prop)| {
                let prop = serde_json::from_value(prop.clone()).unwrap_or_default();
                (name.clone(), prop, required.contains(&name.as_str()))
            })
            .collect(),
    )
}

impl<T> FromRequest for Json<T>
where
    T: DeserializeOwned + Send + Schema + 'static,
{
    async fn from_request(req: Request) -> Result<Self, ApiError> {
        use axum::extract::FromRequest as _;
        use axum::extract::rejection::JsonRejection;
        match axum::Json::<T>::from_request(req.map(Body::into_inner), &()).await {
            Ok(axum::Json(value)) => Ok(Json(value)),
            Err(rejection) => {
                let message = rejection.body_text();
                Err(match rejection {
                    JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_) => {
                        invalid("body", "json_invalid", message)
                    }
                    JsonRejection::MissingJsonContentType(_) => {
                        ApiError::new(http::StatusCode::UNSUPPORTED_MEDIA_TYPE, message)
                    }
                    other => ApiError::new(other.status(), message),
                })
            }
        }
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        let schema = registry.subschema::<T>();
        op.set_request_body("application/json", schema, true);
    }
}

/// The raw request, for handlers that need full control (must be last).
#[derive(Debug)]
pub struct RawRequest(pub Request);

impl FromRequest for RawRequest {
    async fn from_request(req: Request) -> Result<Self, ApiError> {
        Ok(RawRequest(req))
    }
}

/// The request method.
impl FromRequestParts for http::Method {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        Ok(parts.method.clone())
    }
}

/// The request URI.
impl FromRequestParts for http::Uri {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        Ok(parts.uri.clone())
    }
}

/// All request headers.
impl FromRequestParts for http::HeaderMap {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        Ok(parts.headers.clone())
    }
}
