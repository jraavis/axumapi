//! Request extraction.
//!
//! siderite owns its extractor traits so that every extractor can also
//! *document itself* for OpenAPI. The `describe` hooks default to doing
//! nothing, so a custom extractor needs only the extraction method.
//!
//! Handler arguments are extracted in order; all but the last must implement
//! [`FromRequestParts`]. The last may consume the body ([`FromRequest`]).

use crate::body::Body;
use crate::error::ApiError;
use http::request::Parts;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use siderite_openapi::{
    Operation, Parameter, ParameterLocation, Schema, SchemaObject, SchemaRegistry,
};
use siderite_validation::{
    Validate, ValidationContext, ValidationError, parse_value, text_pairs_to_value,
};
use std::future::Future;

pub use crate::response::Json;

/// An HTTP request with a siderite [`Body`].
pub type Request = http::Request<Body>;

/// Extract a value from request metadata (method, URI, headers, extensions).
pub trait FromRequestParts: Sized + Send {
    /// Whether extraction needs a request-owned background queue.
    /// Custom wrappers delegating to BackgroundTasks must propagate this.
    const BACKGROUND_TASKS: bool = false;

    /// Perform the extraction.
    fn from_request_parts(parts: &mut Parts)
    -> impl Future<Output = Result<Self, ApiError>> + Send;

    /// Document this extractor on `op` (parameters, security, ...).
    fn describe(_op: &mut Operation, _registry: &mut SchemaRegistry) {}
}

/// Extract a value from the whole request, possibly consuming the body.
pub trait FromRequest: Sized + Send {
    /// Whether extraction needs a request-owned background queue.
    const BACKGROUND_TASKS: bool = false;

    /// Perform the extraction.
    fn from_request(req: Request) -> impl Future<Output = Result<Self, ApiError>> + Send;

    /// Document this extractor on `op` (request body, ...).
    fn describe(_op: &mut Operation, _registry: &mut SchemaRegistry) {}
}

impl<T: FromRequestParts> FromRequest for T {
    const BACKGROUND_TASKS: bool = T::BACKGROUND_TASKS;

    fn from_request(req: Request) -> impl Future<Output = Result<Self, ApiError>> + Send {
        let (mut parts, _body) = req.into_parts();
        async move { T::from_request_parts(&mut parts).await }
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        <T as FromRequestParts>::describe(op, registry);
    }
}

/// Optional extraction: `None` when the input is absent, the error otherwise.
///
/// Only errors marked [`ApiError::absent`] become `None`; a header, query
/// string or credential that is present but invalid still fails the request.
impl<T: FromRequestParts> FromRequestParts for Option<T> {
    const BACKGROUND_TASKS: bool = T::BACKGROUND_TASKS;

    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        match T::from_request_parts(parts).await {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.is_absent() => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Documents `T`, with everything it adds marked optional. Security
    /// schemes it requires become an alternative next to the requirements
    /// that held before.
    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        let before = op.parameters.len();
        let had_body = op.request_body.is_some();
        let security_before = std::mem::take(&mut op.security);
        <T as FromRequestParts>::describe(op, registry);
        let added = std::mem::replace(&mut op.security, security_before);
        if !added.is_empty() {
            let base = if op.security.is_empty() {
                vec![std::collections::BTreeMap::new()]
            } else {
                op.security.clone()
            };
            let mut alternatives = base.clone();
            for alternative in &base {
                for requirement in &added {
                    let mut with = alternative.clone();
                    with.extend(requirement.clone());
                    if !alternatives.contains(&with) {
                        alternatives.push(with);
                    }
                }
            }
            op.security = alternatives;
        }
        for p in &mut op.parameters[before..] {
            // OpenAPI requires path parameters to be required.
            if p.location != ParameterLocation::Path {
                p.required = false;
            }
        }
        if !had_body && let Some(body) = &mut op.request_body {
            body.required = false;
        }
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

/// Map a [`ValidationError`] to a 422 problem document.
pub(crate) fn validation_problem(error: &ValidationError) -> ApiError {
    ApiError::unprocessable(serde_json::to_value(&error.errors).unwrap_or(Value::Null))
}

/// Run the validation pipeline (`prepare` → deserialize → `validate`).
pub(crate) fn validated<T>(input: Value, ctx: ValidationContext) -> Result<T, ApiError>
where
    T: DeserializeOwned + Validate,
{
    parse_value(input, ctx).map_err(|e| validation_problem(&e))
}

/// Parse `a=1&a=2&b=x` into a JSON object (repeated keys become arrays).
pub(crate) fn urlencoded_to_value(text: &str) -> Result<Value, serde_urlencoded::de::Error> {
    serde_urlencoded::from_str::<Vec<(String, String)>>(text).map(text_pairs_to_value)
}

/// Path parameter extractor (`/users/{id}`).
///
/// `T` may be a scalar, a tuple (positional) or a struct (by name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Path<T>(pub T);

impl<T> FromRequestParts for Path<T>
where
    T: DeserializeOwned + Send + Schema + Validate + 'static,
{
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        use axum::extract::FromRequestParts as _;
        use axum::extract::rejection::PathRejection;
        match axum::extract::Path::<T>::from_request_parts(parts, &()).await {
            Ok(axum::extract::Path(value)) => {
                let mut ctx = ValidationContext::for_text().at_root("path");
                value.validate(&mut ctx);
                if ctx.has_errors() {
                    Err(validation_problem(&ctx.take_errors()))
                } else {
                    Ok(Path(value))
                }
            }
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
    T: DeserializeOwned + Send + Schema + Validate + 'static,
{
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        let query = parts.uri.query().unwrap_or_default();
        let input = urlencoded_to_value(query)
            .map_err(|e| invalid("query", "query_invalid", e.to_string()))?;
        validated(input, ValidationContext::for_text().at_root("query"))
            .map(Query)
            .map_err(|e| if query.is_empty() { e.absent() } else { e })
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
    T: DeserializeOwned + Send + Schema + Validate + 'static,
{
    async fn from_request(req: Request) -> Result<Self, ApiError> {
        use axum::extract::FromRequest as _;
        use axum::extract::rejection::JsonRejection;
        // Read through the size-limited JSON extractor (keeps 413/415), then
        // run the validation pipeline on the raw value.
        let input = match axum::Json::<Value>::from_request(req.map(Body::into_inner), &()).await {
            Ok(axum::Json(value)) => value,
            Err(rejection) => {
                let message = rejection.body_text();
                return Err(match rejection {
                    JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_) => {
                        invalid("body", "json_invalid", message)
                    }
                    JsonRejection::MissingJsonContentType(_) => {
                        ApiError::new(http::StatusCode::UNSUPPORTED_MEDIA_TYPE, message)
                    }
                    other => ApiError::new(other.status(), message),
                });
            }
        };
        validated(input, ValidationContext::new().at_root("body")).map(Json)
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
