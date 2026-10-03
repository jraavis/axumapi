//! Retained-byte admission, accounting and atomic mutation contracts.

use super::{MemoryCache, MemoryCacheLimits};
use crate::{Cache, CacheError};
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn bounded(total: usize, value: usize) -> Result<MemoryCache, CacheError> {
    MemoryCache::with_limits(
        16,
        MemoryCacheLimits {
            max_total_bytes: total,
            max_value_bytes: value,
            max_key_bytes: 4,
        },
    )
}

#[tokio::test]
async fn size_pressure_evicts_lru_and_replacement_releases_bytes() -> TestResult {
    let cache = bounded(10, 8)?;
    cache.set("a", vec![1; 6], None).await?;
    cache.set("b", vec![2; 6], None).await?;
    assert_eq!(cache.get("a").await?, None);
    assert_eq!(cache.lock().bytes, 7);
    cache.set("b", vec![3], None).await?;
    assert_eq!(cache.lock().bytes, 2);
    cache.set("c", vec![4; 8], None).await?;
    assert_eq!(cache.get("b").await?, None);
    assert_eq!(cache.lock().bytes, 9);
    assert!(cache.delete("c").await?);
    assert_eq!(cache.lock().bytes, 0);
    cache.set("x", vec![5], None).await?;
    cache.clear().await?;
    assert_eq!(cache.lock().bytes, 0);
    Ok(())
}

#[tokio::test]
async fn rejected_replacements_and_increment_leave_old_values_intact() -> TestResult {
    let cache = bounded(10, 2)?;
    cache.set("n", b"9".to_vec(), None).await?;
    assert_eq!(cache.increment("n", 1).await?, 10);
    assert!(matches!(
        cache.increment("n", 90).await,
        Err(CacheError::SizeLimit)
    ));
    assert!(matches!(
        cache.set("n", vec![1; 3], None).await,
        Err(CacheError::SizeLimit)
    ));
    assert_eq!(cache.get("n").await?, Some(b"10".to_vec()));
    assert_eq!(cache.lock().bytes, 3);
    assert!(matches!(
        cache.set("long-key", vec![1], None).await,
        Err(CacheError::SizeLimit)
    ));
    Ok(())
}

#[tokio::test]
async fn expiry_and_atomic_replacement_account_for_retained_bytes() -> TestResult {
    let cache = bounded(10, 8)?;
    cache.set("e", vec![1; 8], Some(Duration::ZERO)).await?;
    assert!(cache.set_if_absent("e", vec![2; 2]).await?);
    assert_eq!(cache.lock().bytes, 3);
    assert!(!cache.set_if_absent("e", vec![3]).await?);
    assert_eq!(cache.lock().bytes, 3);
    cache.set("e", vec![4; 2], Some(Duration::ZERO)).await?;
    assert_eq!(cache.get("e").await?, None);
    assert_eq!(cache.lock().bytes, 0);
    Ok(())
}

#[tokio::test]
async fn unrepresentable_ttl_does_not_create_an_immortal_entry() -> TestResult {
    let cache = bounded(10, 8)?;
    cache.set("x", vec![1], None).await?;
    assert!(matches!(
        cache.set("x", vec![2], Some(Duration::MAX)).await,
        Err(CacheError::InvalidTtl)
    ));
    assert_eq!(cache.get("x").await?, Some(vec![1]));
    Ok(())
}

#[tokio::test]
async fn concurrent_writes_preserve_the_shared_byte_budget() -> TestResult {
    let cache = bounded(64, 16)?;
    let mut writers = Vec::new();
    for index in 0..32 {
        let cache = cache.clone();
        writers.push(tokio::spawn(async move {
            cache.set(&index.to_string(), vec![1; 16], None).await
        }));
    }
    for writer in writers {
        writer.await??;
    }
    let stored = cache.lock();
    assert!(stored.bytes <= 64);
    assert_eq!(
        stored.bytes,
        stored
            .iter()
            .map(|(key, entry)| key.len() + entry.value.len())
            .sum::<usize>()
    );
    Ok(())
}

#[test]
fn invalid_byte_limits_are_rejected() {
    let limits = MemoryCacheLimits {
        max_total_bytes: 0,
        max_value_bytes: 1,
        max_key_bytes: 1,
    };
    assert!(matches!(
        MemoryCache::with_limits(1, limits),
        Err(CacheError::InvalidLimits)
    ));
}
