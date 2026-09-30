//! [`Cache`] and the [`CacheExt`] JSON helpers.

use crate::CacheError;
use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// Byte-oriented cache with optional per-entry time-to-live.
///
/// Keys are UTF-8 strings. Values are opaque bytes; JSON helpers live on
/// [`CacheExt`]. [`increment`](Cache::increment) follows Redis `INCRBY`: a
/// missing key is treated as `0`, and a stored value that is not a decimal
/// integer is an error.
#[async_trait]
pub trait Cache: Send + Sync + 'static {
    /// Fetch `key`. `Ok(None)` when the key is missing or has expired.
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError>;

    /// Store `value` at `key`. `ttl` of `None` means the entry does not expire.
    async fn set(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>)
    -> Result<(), CacheError>;

    /// Remove `key`. `Ok(true)` when a live entry was removed.
    async fn delete(&self, key: &str) -> Result<bool, CacheError>;

    /// Add `by` to the integer stored at `key` (Redis `INCRBY`).
    ///
    /// A missing key starts at `0`. `by` may be negative. The stored form is
    /// the decimal representation of the new value, with no expiry when the
    /// key is created by this call; an existing key keeps its expiry.
    async fn increment(&self, key: &str, by: i64) -> Result<i64, CacheError>;

    /// Remove every entry this cache is responsible for.
    async fn clear(&self) -> Result<(), CacheError>;
}

#[async_trait]
impl<T: Cache + ?Sized> Cache for Arc<T> {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
        (**self).get(key).await
    }

    async fn set(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        (**self).set(key, value, ttl).await
    }

    async fn delete(&self, key: &str) -> Result<bool, CacheError> {
        (**self).delete(key).await
    }

    async fn increment(&self, key: &str, by: i64) -> Result<i64, CacheError> {
        (**self).increment(key, by).await
    }

    async fn clear(&self) -> Result<(), CacheError> {
        (**self).clear().await
    }
}

/// JSON helpers implemented for every [`Cache`].
pub trait CacheExt: Cache {
    /// Fetch `key` and deserialize it as JSON of type `T`.
    ///
    /// # Errors
    /// [`CacheError::Deserialize`] when the stored bytes are not JSON of type
    /// `T`. Backend failures from [`Cache::get`].
    fn get_json<T>(&self, key: &str) -> impl Future<Output = Result<Option<T>, CacheError>> + Send
    where
        T: DeserializeOwned,
    {
        async move {
            match self.get(key).await? {
                None => Ok(None),
                Some(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|err| {
                    CacheError::Deserialize {
                        key: key.to_owned(),
                        reason: err.to_string(),
                    }
                }),
            }
        }
    }

    /// Serialize `value` as compact JSON and store it at `key`.
    ///
    /// # Errors
    /// [`CacheError::Serialize`] when `value` cannot be encoded. Backend
    /// failures from [`Cache::set`].
    fn set_json<T>(
        &self,
        key: &str,
        value: &T,
        ttl: Option<Duration>,
    ) -> impl Future<Output = Result<(), CacheError>> + Send
    where
        T: Serialize,
    {
        let encoded = serde_json::to_vec(value).map_err(|err| CacheError::Serialize {
            key: key.to_owned(),
            reason: err.to_string(),
        });
        async move { self.set(key, encoded?, ttl).await }
    }

    /// Return the JSON value at `key`, computing and storing it on a miss.
    ///
    /// A stored value that cannot be deserialized is treated as a miss so a
    /// corrupt entry can be replaced. Concurrent misses may both run `f`.
    ///
    /// # Errors
    /// Errors from `f`, JSON encoding of the computed value, or the backend.
    fn get_or_set<T, F, Fut>(
        &self,
        key: &str,
        ttl: Option<Duration>,
        f: F,
    ) -> impl Future<Output = Result<T, CacheError>> + Send
    where
        T: DeserializeOwned + Serialize + Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<T, CacheError>> + Send,
    {
        async move {
            if let Some(bytes) = self.get(key).await?
                && let Ok(value) = serde_json::from_slice::<T>(&bytes)
            {
                return Ok(value);
            }
            let value = f().await?;
            self.set_json(key, &value, ttl).await?;
            Ok(value)
        }
    }
}

impl<C: Cache> CacheExt for C {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryCache;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Widget {
        id: u32,
        name: String,
    }

    fn widget() -> Widget {
        Widget {
            id: 7,
            name: "ada".into(),
        }
    }

    #[tokio::test]
    async fn json_roundtrip_and_missing() {
        let cache = MemoryCache::new(8);
        assert_eq!(cache.get_json::<Widget>("w").await.unwrap(), None);
        cache.set_json("w", &widget(), None).await.unwrap();
        assert_eq!(cache.get_json::<Widget>("w").await.unwrap(), Some(widget()));
    }

    #[tokio::test]
    async fn get_json_rejects_invalid_payload() {
        let cache = MemoryCache::new(8);
        cache.set("w", b"not-json".to_vec(), None).await.unwrap();
        let err = cache.get_json::<Widget>("w").await.unwrap_err();
        assert!(matches!(err, CacheError::Deserialize { ref key, .. } if key == "w"));
    }

    #[tokio::test]
    async fn get_or_set_computes_once() {
        let cache = MemoryCache::new(8);
        let calls = std::sync::atomic::AtomicU32::new(0);
        let first = cache
            .get_or_set("w", None, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async { Ok(widget()) }
            })
            .await
            .unwrap();
        let second = cache
            .get_or_set("w", None, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    Ok(Widget {
                        id: 0,
                        name: "x".into(),
                    })
                }
            })
            .await
            .unwrap();
        assert_eq!(first, widget());
        assert_eq!(second, widget());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn get_or_set_does_not_store_factory_errors() {
        let cache = MemoryCache::new(8);
        let err = cache
            .get_or_set::<Widget, _, _>("w", None, || async {
                Err(CacheError::Backend("boom".into()))
            })
            .await
            .unwrap_err();
        assert!(matches!(err, CacheError::Backend(ref m) if m == "boom"));
        assert_eq!(cache.get("w").await.unwrap(), None);
    }

    #[tokio::test]
    async fn get_or_set_replaces_corrupt_entry() {
        let cache = MemoryCache::new(8);
        cache.set("w", b"nope".to_vec(), None).await.unwrap();
        let value = cache
            .get_or_set("w", None, || async { Ok(widget()) })
            .await
            .unwrap();
        assert_eq!(value, widget());
        assert_eq!(cache.get_json::<Widget>("w").await.unwrap(), Some(widget()));
    }

    #[tokio::test]
    async fn arc_dyn_cache_is_usable() {
        let cache: Arc<dyn Cache> = Arc::new(MemoryCache::new(4));
        cache.set("k", b"v".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
        assert_eq!(cache.get_json::<u32>("missing").await.unwrap(), None);
    }
}
