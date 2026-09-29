//! [`CacheError`] and conversion helpers.

use thiserror::Error;

/// Failure from a [`Cache`](crate::Cache) operation.
///
/// Messages never include cached values, passwords, tokens, API keys or other
/// secrets. Keys may appear because they identify the entry; do not put
/// secrets in cache keys.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CacheError {
    /// The backend could not complete the operation.
    #[error("cache backend error: {0}")]
    Backend(String),
    /// [`Cache::increment`](crate::Cache::increment) found a non-integer value,
    /// or the result would overflow a signed 64-bit integer.
    #[error("cache key `{key}` is not an integer or the increment overflowed")]
    NotInteger {
        /// The cache key (not the stored value).
        key: String,
    },
    /// JSON serialization failed before anything was stored.
    #[error("cannot serialize cache value for `{key}`: {reason}")]
    Serialize {
        /// The cache key.
        key: String,
        /// Why serialization failed.
        reason: String,
    },
    /// A stored value could not be decoded as JSON of the requested type.
    #[error("cannot deserialize cache value for `{key}`: {reason}")]
    Deserialize {
        /// The cache key.
        key: String,
        /// Why deserialization failed.
        reason: String,
    },
}
