//! Redirect, streaming, file and header-decorated responses.

use crate::body::Body;
use crate::error::ApiError;
use crate::response::{IntoResponse, Response, SUCCESS, status_only};
use bytes::Bytes;
use futures_util::Stream;
use http::header::{CONTENT_DISPOSITION, CONTENT_TYPE, LOCATION};
use http::{HeaderName, HeaderValue, StatusCode};
use siderite_openapi::{Operation, SchemaObject, SchemaRegistry};
use std::error::Error as StdError;
use std::io::ErrorKind;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};

/// HTTP redirect (`Location` header).
///
/// Constructors never panic: an invalid URI is rendered as a `500` problem
/// details response. OpenAPI documents a `303` response (range tokens such
/// as `3XX` are not valid OpenAPI 3.1 status keys).
#[derive(Debug, Clone)]
pub struct Redirect {
    status: StatusCode,
    location: Result<HeaderValue, ()>,
}

impl Redirect {
    /// `303 See Other` redirect to `uri`.
    #[must_use]
    pub fn to(uri: &str) -> Self {
        Self::with_status(StatusCode::SEE_OTHER, uri)
    }

    /// `307 Temporary Redirect` to `uri` (method and body preserved).
    #[must_use]
    pub fn temporary(uri: &str) -> Self {
        Self::with_status(StatusCode::TEMPORARY_REDIRECT, uri)
    }

    /// `308 Permanent Redirect` to `uri` (method and body preserved).
    #[must_use]
    pub fn permanent(uri: &str) -> Self {
        Self::with_status(StatusCode::PERMANENT_REDIRECT, uri)
    }

    fn with_status(status: StatusCode, uri: &str) -> Self {
        Self {
            status,
            location: HeaderValue::from_str(uri).map_err(|_| ()),
        }
    }
}

impl IntoResponse for Redirect {
    fn into_response(self) -> Response {
        match self.location {
            Ok(location) => {
                let mut response = status_only(self.status);
                response.headers_mut().insert(LOCATION, location);
                response
            }
            Err(()) => ApiError::internal("invalid redirect URI").into_response(),
        }
    }

    fn describe(op: &mut Operation, _registry: &mut SchemaRegistry) {
        op.add_response("303", "Redirect", None);
    }
}

/// Streaming response body built from a [`Stream`] of byte chunks.
pub struct StreamingResponse {
    body: Body,
    content_type: Result<HeaderValue, ()>,
    status: StatusCode,
}

impl StreamingResponse {
    /// Stream `stream` as `application/octet-stream` with status `200`.
    ///
    /// Stream errors are forwarded to the HTTP body; they do not panic.
    #[must_use]
    pub fn new<S, E>(stream: S) -> Self
    where
        S: Stream<Item = Result<Bytes, E>> + Send + 'static,
        E: Into<Box<dyn StdError + Send + Sync>> + Send + 'static,
    {
        Self {
            body: Body::from_inner(axum::body::Body::from_stream(stream)),
            content_type: Ok(HeaderValue::from_static("application/octet-stream")),
            status: StatusCode::OK,
        }
    }

    /// Set the `Content-Type`. Invalid values become a `500` problem.
    #[must_use]
    pub fn content_type(mut self, value: impl AsRef<str>) -> Self {
        self.content_type = HeaderValue::from_str(value.as_ref()).map_err(|_| ());
        self
    }

    /// Set the HTTP status code.
    #[must_use]
    pub fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }
}

impl IntoResponse for StreamingResponse {
    fn into_response(self) -> Response {
        let Ok(content_type) = self.content_type else {
            return ApiError::internal("invalid Content-Type").into_response();
        };
        let mut response = Response::new(self.body);
        *response.status_mut() = self.status;
        response.headers_mut().insert(CONTENT_TYPE, content_type);
        response
    }

    fn describe(op: &mut Operation, _registry: &mut SchemaRegistry) {
        op.add_response(
            SUCCESS,
            "Successful Response",
            Some(("application/octet-stream", SchemaObject::of_type("string"))),
        );
    }
}

/// File streamed from disk.
pub struct FileResponse {
    file: tokio::fs::File,
    content_type: &'static str,
    attachment: Option<String>,
}

impl FileResponse {
    /// Open `path` for streaming. Missing files yield a `404` problem.
    ///
    /// # Errors
    /// Returns [`ApiError::not_found`] if the file does not exist, or
    /// [`ApiError::internal`] on other I/O failures.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, ApiError> {
        let path = path.as_ref();
        let file = tokio::fs::File::open(path).await.map_err(|err| {
            if err.kind() == ErrorKind::NotFound {
                ApiError::not_found("File not found.")
            } else {
                ApiError::internal(err)
            }
        })?;
        Ok(Self {
            file,
            content_type: mime_from_path(path),
            attachment: None,
        })
    }

    /// Send the file as an attachment with `filename` (`Content-Disposition`).
    #[must_use]
    pub fn attachment(mut self, filename: impl Into<String>) -> Self {
        self.attachment = Some(filename.into());
        self
    }
}

impl IntoResponse for FileResponse {
    fn into_response(self) -> Response {
        let disposition = match self.attachment.as_deref() {
            Some(name) => match HeaderValue::from_str(&content_disposition_attachment(name)) {
                Ok(value) => Some(value),
                Err(_) => {
                    return ApiError::internal("invalid attachment filename").into_response();
                }
            },
            None => None,
        };
        let mut response = Response::new(Body::from_inner(axum::body::Body::from_stream(
            FileStream { file: self.file },
        )));
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(self.content_type));
        if let Some(value) = disposition {
            response.headers_mut().insert(CONTENT_DISPOSITION, value);
        }
        response
    }

    fn describe(op: &mut Operation, _registry: &mut SchemaRegistry) {
        op.add_response(
            SUCCESS,
            "Successful Response",
            Some(("application/octet-stream", SchemaObject::of_type("string"))),
        );
    }
}

/// Wraps a response and adds headers. Invalid names or values become a `500`
/// problem instead of panicking.
pub struct WithHeaders<R> {
    inner: R,
    headers: Vec<(HeaderName, HeaderValue)>,
    error: Option<String>,
}

impl<R> WithHeaders<R> {
    /// Wrap `inner` with no extra headers.
    #[must_use]
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            headers: Vec::new(),
            error: None,
        }
    }

    /// Append a header. Invalid names or values are recorded and rendered as
    /// a `500` problem in [`IntoResponse`].
    #[must_use]
    pub fn header(mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        if self.error.is_some() {
            return self;
        }
        let name = match HeaderName::try_from(name.as_ref()) {
            Ok(name) => name,
            Err(_) => {
                self.error = Some(format!("invalid header name `{}`", name.as_ref()));
                return self;
            }
        };
        let value = match HeaderValue::try_from(value.as_ref()) {
            Ok(value) => value,
            Err(_) => {
                self.error = Some(format!("invalid header value for `{name}`"));
                return self;
            }
        };
        self.headers.push((name, value));
        self
    }
}

impl<R: IntoResponse> IntoResponse for WithHeaders<R> {
    fn into_response(self) -> Response {
        if let Some(error) = self.error {
            return ApiError::internal(error).into_response();
        }
        let mut response = self.inner.into_response();
        for (name, value) in self.headers {
            response.headers_mut().append(name, value);
        }
        response
    }

    fn describe(op: &mut Operation, registry: &mut SchemaRegistry) {
        R::describe(op, registry);
    }
}

struct FileStream {
    file: tokio::fs::File,
}

impl Stream for FileStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut storage = vec![0_u8; 8192];
        let mut buf = ReadBuf::new(&mut storage);
        match Pin::new(&mut self.file).poll_read(cx, &mut buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(err)) => Poll::Ready(Some(Err(err))),
            Poll::Ready(Ok(())) => {
                let filled = buf.filled();
                if filled.is_empty() {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Ok(Bytes::copy_from_slice(filled))))
                }
            }
        }
    }
}

fn mime_from_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("txt") => "text/plain; charset=utf-8",
        Some("pdf") => "application/pdf",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn content_disposition_attachment(filename: &str) -> String {
    let escaped = filename.replace('\\', "\\\\").replace('"', "\\\"");
    format!("attachment; filename=\"{escaped}\"")
}
