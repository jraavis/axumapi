//! Process-local LRU cache with per-entry TTL.

use crate::{Cache, CacheError};
use async_trait::async_trait;
use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

struct Entry {
    value: Vec<u8>,
    expires_at: Option<Instant>,
}

impl Entry {
    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|deadline| deadline <= now)
    }
}

/// In-memory LRU cache with per-entry time-to-live.
///
/// Cloning shares the same map. A poisoned mutex is recovered (`into_inner`)
/// so a panic in one caller does not disable the cache. `capacity` of `0` is
/// treated as `1`.
///
/// [`increment`](Cache::increment) matches Redis `INCRBY`: a missing or
/// expired key starts at `0`, the stored form is a decimal integer, and a
/// non-integer value is [`CacheError::NotInteger`].
#[derive(Clone)]
pub struct MemoryCache {
    inner: Arc<Mutex<LruCache<String, Entry>>>,
}

impl std::fmt::Debug for MemoryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("MemoryCache")
            .field("len", &inner.len())
            .field("capacity", &inner.cap())
            .finish()
    }
}

impl MemoryCache {
    /// Bound the cache to at most `capacity` live entries (minimum 1).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity.max(1)).unwrap_or(NonZeroUsize::MIN);
        Self {
            inner: Arc::new(Mutex::new(LruCache::new(cap))),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LruCache<String, Entry>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn expires_at(ttl: Option<Duration>) -> Option<Instant> {
    ttl.and_then(|ttl| Instant::now().checked_add(ttl))
}

/// Redis `INCRBY` accepts an optional ASCII minus and one or more digits.
fn parse_incr_int(bytes: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(bytes).ok()?;
    if text.is_empty() {
        return None;
    }
    let digits = match text.as_bytes()[0] {
        b'-' => &text.as_bytes()[1..],
        b'0'..=b'9' => text.as_bytes(),
        _ => return None,
    };
    if digits.is_empty() || !digits.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn purge_expired(cache: &mut LruCache<String, Entry>, now: Instant) {
    let expired: Vec<String> = cache
        .iter()
        .filter(|(_, entry)| entry.is_expired(now))
        .map(|(key, _)| key.clone())
        .collect();
    for key in expired {
        cache.pop(&key);
    }
}

#[async_trait]
impl Cache for MemoryCache {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
        let mut cache = self.lock();
        let now = Instant::now();
        let expired = cache.peek(key).is_some_and(|entry| entry.is_expired(now));
        if expired {
            cache.pop(key);
            return Ok(None);
        }
        Ok(cache.get(key).map(|entry| entry.value.clone()))
    }

    async fn set(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<(), CacheError> {
        let mut cache = self.lock();
        let now = Instant::now();
        if cache.len() >= cache.cap().get() && cache.peek(key).is_none() {
            purge_expired(&mut cache, now);
        }
        cache.put(
            key.to_owned(),
            Entry {
                value,
                expires_at: expires_at(ttl),
            },
        );
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<bool, CacheError> {
        let mut cache = self.lock();
        let now = Instant::now();
        match cache.peek(key) {
            Some(entry) if entry.is_expired(now) => {
                cache.pop(key);
                Ok(false)
            }
            Some(_) => {
                cache.pop(key);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn increment(&self, key: &str, by: i64) -> Result<i64, CacheError> {
        let mut cache = self.lock();
        let now = Instant::now();
        if cache.peek(key).is_some_and(|entry| entry.is_expired(now)) {
            cache.pop(key);
        }
        if let Some(entry) = cache.get_mut(key) {
            let current = parse_incr_int(&entry.value).ok_or_else(|| CacheError::NotInteger {
                key: key.to_owned(),
            })?;
            let next = current
                .checked_add(by)
                .ok_or_else(|| CacheError::NotInteger {
                    key: key.to_owned(),
                })?;
            entry.value = next.to_string().into_bytes();
            return Ok(next);
        }
        if cache.len() >= cache.cap().get() {
            purge_expired(&mut cache, now);
        }
        cache.put(
            key.to_owned(),
            Entry {
                value: by.to_string().into_bytes(),
                expires_at: None,
            },
        );
        Ok(by)
    }

    async fn clear(&self) -> Result<(), CacheError> {
        self.lock().clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CacheExt;
    use std::thread;
    use std::time::Duration;

    #[tokio::test]
    async fn get_set_delete_clear() {
        let cache = MemoryCache::new(8);
        assert_eq!(cache.get("k").await.unwrap(), None);
        cache.set("k", b"v".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
        assert!(cache.delete("k").await.unwrap());
        assert!(!cache.delete("k").await.unwrap());
        cache.set("a", b"1".to_vec(), None).await.unwrap();
        cache.set("b", b"2".to_vec(), None).await.unwrap();
        cache.clear().await.unwrap();
        assert_eq!(cache.get("a").await.unwrap(), None);
        assert_eq!(cache.get("b").await.unwrap(), None);
    }

    #[tokio::test]
    async fn clones_share_storage() {
        let cache = MemoryCache::new(4);
        let clone = cache.clone();
        clone.set("k", b"v".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
    }

    #[tokio::test]
    async fn expired_entries_are_missing() {
        let cache = MemoryCache::new(4);
        cache
            .set("k", b"v".to_vec(), Some(Duration::from_millis(15)))
            .await
            .unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(cache.get("k").await.unwrap(), None);
        assert!(!cache.delete("k").await.unwrap());
    }

    #[tokio::test]
    async fn lru_evicts_the_least_recently_used() {
        let cache = MemoryCache::new(2);
        cache.set("a", b"1".to_vec(), None).await.unwrap();
        cache.set("b", b"2".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("a").await.unwrap().as_deref(), Some(&b"1"[..]));
        cache.set("c", b"3".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("b").await.unwrap(), None);
        assert_eq!(cache.get("a").await.unwrap().as_deref(), Some(&b"1"[..]));
        assert_eq!(cache.get("c").await.unwrap().as_deref(), Some(&b"3"[..]));
    }

    #[tokio::test]
    async fn zero_capacity_is_one() {
        let cache = MemoryCache::new(0);
        cache.set("a", b"1".to_vec(), None).await.unwrap();
        cache.set("b", b"2".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("a").await.unwrap(), None);
        assert_eq!(cache.get("b").await.unwrap().as_deref(), Some(&b"2"[..]));
    }

    #[tokio::test]
    async fn increment_matches_redis_incrby() {
        let cache = MemoryCache::new(8);
        assert_eq!(cache.increment("n", 3).await.unwrap(), 3);
        assert_eq!(cache.increment("n", 2).await.unwrap(), 5);
        assert_eq!(cache.increment("n", -8).await.unwrap(), -3);
        assert_eq!(cache.get("n").await.unwrap().as_deref(), Some(&b"-3"[..]));
        cache.set("n", b"9".to_vec(), None).await.unwrap();
        assert_eq!(cache.increment("n", 1).await.unwrap(), 10);
    }

    #[tokio::test]
    async fn increment_rejects_non_integers_and_overflow() {
        let cache = MemoryCache::new(8);
        cache.set("s", b"hello".to_vec(), None).await.unwrap();
        assert!(matches!(
            cache.increment("s", 1).await.unwrap_err(),
            CacheError::NotInteger { ref key } if key == "s"
        ));
        cache.set("plus", b"+1".to_vec(), None).await.unwrap();
        assert!(matches!(
            cache.increment("plus", 1).await.unwrap_err(),
            CacheError::NotInteger { .. }
        ));
        cache.set("ws", b" 1".to_vec(), None).await.unwrap();
        assert!(matches!(
            cache.increment("ws", 1).await.unwrap_err(),
            CacheError::NotInteger { .. }
        ));
        cache
            .set("max", i64::MAX.to_string().into_bytes(), None)
            .await
            .unwrap();
        assert!(matches!(
            cache.increment("max", 1).await.unwrap_err(),
            CacheError::NotInteger { ref key } if key == "max"
        ));
    }

    #[tokio::test]
    async fn increment_on_expired_key_starts_at_zero() {
        let cache = MemoryCache::new(4);
        cache
            .set("n", b"9".to_vec(), Some(Duration::from_millis(15)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(cache.increment("n", 4).await.unwrap(), 4);
    }

    #[tokio::test]
    async fn increment_preserves_ttl_of_a_live_key() {
        let cache = MemoryCache::new(4);
        cache
            .set("n", b"1".to_vec(), Some(Duration::from_millis(80)))
            .await
            .unwrap();
        assert_eq!(cache.increment("n", 1).await.unwrap(), 2);
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(cache.get("n").await.unwrap(), None);
    }

    #[tokio::test]
    async fn concurrent_increments_are_serialized() {
        let cache = MemoryCache::new(4);
        let mut joins = Vec::new();
        for _ in 0..32 {
            let cache = cache.clone();
            joins.push(tokio::spawn(async move { cache.increment("n", 1).await }));
        }
        let mut total = 0;
        for join in joins {
            total += join.await.unwrap().unwrap();
        }
        assert_eq!(cache.get("n").await.unwrap().as_deref(), Some(&b"32"[..]));
        assert_eq!(total, (1..=32).sum::<i64>());
    }

    #[tokio::test]
    async fn poisoned_mutex_is_recovered() {
        let cache = MemoryCache::new(4);
        cache.set("k", b"v".to_vec(), None).await.unwrap();
        let poison = cache.clone();
        let _ = thread::spawn(move || {
            let _guard = poison.lock();
            panic!("poison");
        })
        .join();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
        cache.set("k", b"w".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"w"[..]));
    }

    #[tokio::test]
    async fn binary_values_roundtrip() {
        let cache = MemoryCache::new(2);
        let value = vec![0, 127, 128, 255];
        cache.set("bin", value.clone(), None).await.unwrap();
        assert_eq!(cache.get("bin").await.unwrap(), Some(value));
    }

    #[tokio::test]
    async fn set_replaces_ttl() {
        let cache = MemoryCache::new(2);
        cache
            .set("k", b"1".to_vec(), Some(Duration::from_millis(15)))
            .await
            .unwrap();
        cache.set("k", b"2".to_vec(), None).await.unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"2"[..]));
    }

    #[test]
    fn parse_incr_int_is_strict() {
        assert_eq!(parse_incr_int(b"0"), Some(0));
        assert_eq!(parse_incr_int(b"01"), Some(1));
        assert_eq!(parse_incr_int(b"-12"), Some(-12));
        assert_eq!(parse_incr_int(b""), None);
        assert_eq!(parse_incr_int(b"-"), None);
        assert_eq!(parse_incr_int(b"+1"), None);
        assert_eq!(parse_incr_int(b"1 "), None);
        assert_eq!(parse_incr_int(b"1.0"), None);
        assert_eq!(parse_incr_int(b"abc"), None);
    }

    #[test]
    fn types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MemoryCache>();
        assert_send_sync::<Box<dyn Cache>>();
    }

    #[tokio::test]
    async fn json_helper_uses_compact_encoding() {
        let cache = MemoryCache::new(2);
        cache.set_json("n", &42u32, None).await.unwrap();
        assert_eq!(cache.get("n").await.unwrap().as_deref(), Some(&b"42"[..]));
    }
}
