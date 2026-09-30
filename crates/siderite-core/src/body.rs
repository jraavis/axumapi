//! Opaque HTTP body type.

use crate::error::BodyError;

/// Default cap for [`Body::into_bytes`], matching the `Json` and `Form` extractors.
pub const DEFAULT_BODY_LIMIT: usize = 2 * 1024 * 1024;

fn is_length_limit(err: &axum::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(current) = source {
        if current.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        source = current.source();
    }
    false
}

/// An HTTP request or response body.
#[derive(Debug, Default)]
pub struct Body(axum::body::Body);

impl Body {
    /// An empty body.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Collect the whole body into memory, up to [`DEFAULT_BODY_LIMIT`] bytes.
    ///
    /// Use [`Body::into_bytes_limited`] to pick a different cap.
    ///
    /// # Errors
    /// Returns [`BodyError`] if the body stream fails or exceeds the limit
    /// ([`BodyError::is_too_large`]).
    pub async fn into_bytes(self) -> Result<Vec<u8>, BodyError> {
        self.into_bytes_limited(DEFAULT_BODY_LIMIT).await
    }

    /// Collect the whole body into memory, failing once it exceeds `max_bytes`.
    ///
    /// # Errors
    /// Returns [`BodyError`] if the body stream fails or exceeds `max_bytes`
    /// ([`BodyError::is_too_large`]).
    pub async fn into_bytes_limited(self, max_bytes: usize) -> Result<Vec<u8>, BodyError> {
        if self.exact_len().is_some_and(|len| len > max_bytes as u64) {
            return Err(BodyError::too_large(max_bytes));
        }
        match axum::body::to_bytes(self.0, max_bytes).await {
            Ok(bytes) => Ok(bytes.to_vec()),
            Err(err) if is_length_limit(&err) => Err(BodyError::too_large(max_bytes)),
            Err(err) => Err(BodyError::failed(err.to_string())),
        }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod limit_tests {
    use super::*;
    use crate::error::ApiError;
    use http::StatusCode;

    fn streamed(chunks: usize, size: usize) -> Body {
        let stream = futures_util::stream::iter(
            (0..chunks).map(move |_| Ok::<_, std::io::Error>(vec![b'x'; size])),
        );
        Body::from_inner(axum::body::Body::from_stream(stream))
    }

    #[tokio::test]
    async fn buffered_body_over_limit_is_too_large() {
        let err = Body::from(vec![0u8; 11])
            .into_bytes_limited(10)
            .await
            .unwrap_err();
        assert!(err.is_too_large());
        assert_eq!(ApiError::from(err).status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn streamed_body_over_limit_is_too_large() {
        let err = streamed(4, 4).into_bytes_limited(10).await.unwrap_err();
        assert!(err.is_too_large());
    }

    #[tokio::test]
    async fn default_limit_applies_and_bodies_within_it_pass() {
        assert_eq!(
            streamed(2, 5).into_bytes_limited(10).await.unwrap().len(),
            10
        );
        let err = streamed(DEFAULT_BODY_LIMIT / 1024 + 1, 1024)
            .into_bytes()
            .await
            .unwrap_err();
        assert!(err.is_too_large());
    }
}
