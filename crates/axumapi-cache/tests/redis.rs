//! Live [`RedisCache`] tests.
//!
//! They run only when `REDIS_URL` starts with `redis` (for example
//! `redis://127.0.0.1:6379/15`) and print a note otherwise. Each test uses
//! its own key prefix on database 15 and deletes that prefix afterwards.
#![cfg(feature = "redis")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axumapi_backends::redis::RedisStore;
use axumapi_cache::{Cache, CacheError, CacheExt, RedisCache};
use serde::{Deserialize, Serialize};

static SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Widget {
    id: u32,
    name: String,
}

async fn with_cache<F, Fut>(test: F)
where
    F: FnOnce(RedisCache) -> Fut,
    Fut: Future<Output = ()>,
{
    let Some(url) = std::env::var("REDIS_URL")
        .ok()
        .filter(|value| value.starts_with("redis"))
    else {
        eprintln!("skipping Redis cache test: REDIS_URL does not start with `redis`");
        return;
    };
    let store = RedisStore::connect(&url).await.unwrap();
    assert_eq!(
        store.database(),
        15,
        "REDIS_URL must select database 15 (selected {})",
        store.database()
    );
    let prefix = format!(
        "axumapi-cache-test-{}-{}:",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    );
    let cache = RedisCache::new(store.with_prefix(prefix));
    test(cache.clone()).await;
    cache.clear().await.unwrap();
}

#[tokio::test]
async fn get_set_delete_clear_and_ttl() {
    with_cache(|cache| async move {
        assert_eq!(cache.get("missing").await.unwrap(), None);
        cache.set("name", b"ada".to_vec(), None).await.unwrap();
        assert_eq!(
            cache.get("name").await.unwrap().as_deref(),
            Some(&b"ada"[..])
        );
        assert!(cache.delete("name").await.unwrap());
        assert!(!cache.delete("name").await.unwrap());

        cache
            .set("ephemeral", b"x".to_vec(), Some(Duration::from_millis(50)))
            .await
            .unwrap();
        assert_eq!(
            cache.get("ephemeral").await.unwrap().as_deref(),
            Some(&b"x"[..])
        );
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(cache.get("ephemeral").await.unwrap(), None);

        cache.set("a", b"1".to_vec(), None).await.unwrap();
        cache.set("b", b"2".to_vec(), None).await.unwrap();
        cache.clear().await.unwrap();
        assert_eq!(cache.get("a").await.unwrap(), None);
        assert_eq!(cache.get("b").await.unwrap(), None);
    })
    .await;
}

#[tokio::test]
async fn increment_matches_redis_incrby() {
    with_cache(|cache| async move {
        assert_eq!(cache.increment("n", 3).await.unwrap(), 3);
        assert_eq!(cache.increment("n", 2).await.unwrap(), 5);
        assert_eq!(cache.increment("n", -8).await.unwrap(), -3);
        cache.set("s", b"hello".to_vec(), None).await.unwrap();
        assert!(matches!(
            cache.increment("s", 1).await.unwrap_err(),
            CacheError::NotInteger { ref key } if key == "s"
        ));
    })
    .await;
}

#[tokio::test]
async fn json_and_binary_roundtrip() {
    with_cache(|cache| async move {
        let widget = Widget {
            id: 3,
            name: "nes".into(),
        };
        cache.set_json("w", &widget, None).await.unwrap();
        assert_eq!(cache.get_json::<Widget>("w").await.unwrap(), Some(widget));

        let binary: Vec<u8> = (0..=255).collect();
        cache.set("bin", binary.clone(), None).await.unwrap();
        assert_eq!(cache.get("bin").await.unwrap(), Some(binary));
    })
    .await;
}

#[tokio::test]
async fn clones_share_the_store() {
    with_cache(|cache| async move {
        let clone = cache.clone();
        clone.set("k", b"v".to_vec(), None).await.unwrap();
        assert_eq!(cache.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
    })
    .await;
}

#[tokio::test]
async fn connect_rejects_a_malformed_url() {
    let err = RedisCache::connect("not a url").await.unwrap_err();
    assert!(matches!(err, CacheError::Backend(_)));
}

#[tokio::test]
async fn empty_prefix_is_replaced_so_clear_is_safe() {
    let Some(url) = std::env::var("REDIS_URL")
        .ok()
        .filter(|value| value.starts_with("redis"))
    else {
        eprintln!("skipping Redis cache test: REDIS_URL does not start with `redis`");
        return;
    };
    let store = RedisStore::connect(&url).await.unwrap();
    assert!(store.prefix().is_empty());
    let cache = RedisCache::new(store);
    assert_eq!(cache.store().prefix(), "axumapi-cache:");
}

#[test]
fn redis_cache_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RedisCache>();
}
