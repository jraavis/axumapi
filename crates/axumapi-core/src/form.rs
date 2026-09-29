//! Form and multipart body extractors.

use crate::body::Body;
use crate::error::ApiError;
use crate::extract::{FromRequest, Request, invalid, validated};
use axumapi_openapi::{Operation, Schema, SchemaObject, SchemaRegistry};
use axumapi_validation::{Validate, ValidationContext, text_pairs_to_value};
use bytes::Bytes;
use http::StatusCode;
use serde::de::DeserializeOwned;

/// Default maximum size of a `multipart/form-data` body (10 MiB).
pub const DEFAULT_MULTIPART_LIMIT: usize = 10 * 1024 * 1024;

/// URL-encoded form body (`application/x-www-form-urlencoded`).
///
/// `T` is deserialized from the request body. A wrong `Content-Type` yields
/// `415`; malformed or semantically invalid fields yield `422` with code
/// `form_invalid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Form<T>(pub T);

impl<T> FromRequest for Form<T>
where
    T: DeserializeOwned + Send + Schema + Validate + 'static,
{
    async fn from_request(req: Request) -> Result<Self, ApiError> {
        use axum::extract::FromRequest as _;
        use axum::extract::rejection::FormRejection;
        match axum::Form::<Vec<(String, String)>>::from_request(req.map(Body::into_inner), &())
            .await
        {
            Ok(axum::Form(pairs)) => validated(
                text_pairs_to_value(pairs),
                ValidationContext::for_text().at_root("body"),
            )
            .map(Form),
            Err(rejection) => {
                let message = rejection.body_text();
                Err(match rejection {
                    FormRejection::FailedToDeserializeForm(_)
                    | FormRejection::FailedToDeserializeFormBody(_) => {
                        invalid("body", "form_invalid", message)
                    }
                    FormRejection::InvalidFormContentType(_) => {
                        ApiError::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, message)
                    }
                    FormRejection::BytesRejection(rejection) => {
                        ApiError::new(rejection.status(), message)
                    }
                    other => ApiError::new(other.status(), message),
                })
            }
        }
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        let schema = registry.subschema::<T>();
        op.set_request_body("application/x-www-form-urlencoded", schema, true);
    }
}

/// Multipart form body (`multipart/form-data`).
///
/// Bodies larger than [`DEFAULT_MULTIPART_LIMIT`] are rejected with `413`
/// when the stream is read. A wrong `Content-Type` yields `415`.
#[derive(Debug)]
pub struct Multipart {
    inner: axum::extract::Multipart,
}

impl FromRequest for Multipart {
    async fn from_request(mut req: Request) -> Result<Self, ApiError> {
        let content_type = req
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !content_type
            .to_ascii_lowercase()
            .starts_with("multipart/form-data")
        {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "multipart requests must have Content-Type: multipart/form-data",
            ));
        }
        axum::extract::DefaultBodyLimit::max(DEFAULT_MULTIPART_LIMIT).apply(&mut req);
        use axum::extract::FromRequest as _;
        match axum::extract::Multipart::from_request(req.map(Body::into_inner), &()).await {
            Ok(inner) => Ok(Self { inner }),
            Err(rejection) => Err(ApiError::bad_request(rejection.body_text())),
        }
    }

    fn describe(op: &mut Operation, _registry: &mut SchemaRegistry) {
        op.set_request_body("multipart/form-data", SchemaObject::of_type("object"), true);
    }
}

impl Multipart {
    /// Yield the next field in the multipart stream.
    ///
    /// # Errors
    /// Returns [`ApiError`] if the stream is malformed or exceeds the size
    /// limit (`413`).
    pub async fn next_field(&mut self) -> Result<Option<MultipartField<'_>>, ApiError> {
        match self.inner.next_field().await {
            Ok(Some(field)) => Ok(Some(MultipartField { inner: field })),
            Ok(None) => Ok(None),
            Err(err) => Err(multipart_error(err)),
        }
    }
}

/// One field of a [`Multipart`] body.
#[derive(Debug)]
pub struct MultipartField<'a> {
    inner: axum::extract::multipart::Field<'a>,
}

impl MultipartField<'_> {
    /// The field's `name` from `Content-Disposition`, if present.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.inner.name()
    }

    /// The field's `filename` from `Content-Disposition`, if present.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.inner.file_name()
    }

    /// The field's `Content-Type`, if present.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.inner.content_type()
    }

    /// Collect the field's bytes.
    ///
    /// # Errors
    /// Returns [`ApiError`] if reading fails or the body exceeds the size
    /// limit (`413`).
    pub async fn bytes(self) -> Result<Bytes, ApiError> {
        self.inner.bytes().await.map_err(multipart_error)
    }

    /// Collect the field as UTF-8 text.
    ///
    /// # Errors
    /// Returns [`ApiError`] if reading fails, the body exceeds the size
    /// limit (`413`), or the bytes are not valid UTF-8.
    pub async fn text(self) -> Result<String, ApiError> {
        self.inner.text().await.map_err(multipart_error)
    }
}

fn multipart_error(err: axum::extract::multipart::MultipartError) -> ApiError {
    ApiError::new(err.status(), err.body_text())
}
