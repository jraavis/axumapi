//! Opaque HTTP body type.

use crate::error::BodyError;

/// An HTTP request or response body.
#[derive(Debug, Default)]
pub struct Body(axum::body::Body);

impl Body {
    /// An empty body.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Collect the whole body into memory.
    ///
    /// # Errors
    /// Returns [`BodyError`] if the body stream fails.
    pub async fn into_bytes(self) -> Result<Vec<u8>, BodyError> {
        axum::body::to_bytes(self.0, usize::MAX)
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|err| BodyError(err.to_string()))
    }

    /// The body length when it is known up front (fully buffered bodies).
    ///
    /// `None` for streaming bodies (server-sent events, file streams, ...),
    /// whose length is only known once they end.
    pub fn exact_len(&self) -> Option<u64> {
        axum::body::HttpBody::size_hint(&self.0).exact()
    }

    pub(crate) fn from_inner(inner: axum::body::Body) -> Self {
        Self(inner)
    }

    pub(crate) fn into_inner(self) -> axum::body::Body {
        self.0
    }
}

impl From<Vec<u8>> for Body {
    fn from(bytes: Vec<u8>) -> Self {
        Self(axum::body::Body::from(bytes))
    }
}

impl From<String> for Body {
    fn from(text: String) -> Self {
        Self(axum::body::Body::from(text))
    }
}

impl From<&'static str> for Body {
    fn from(text: &'static str) -> Self {
        Self(axum::body::Body::from(text))
    }
}
