//! Explicit live Redis contracts for atomic generations and shared writes.
#![cfg(feature = "redis")]

use futures_util::FutureExt;
use siderite_backends::redis::RedisStore;
use siderite_cache::{Cache, RedisCache, RouteCache};
use siderite_core::{App, Cached, get};
use siderite_testkit::TestClient;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn app(cache: RedisCache, value: Arc<AtomicUsize>) -> App {
    let reader = value.clone();
    App::new()
        .route(
            "/item",
            get(move || {
                let reader = reader.clone();
                async move {
                    Cached::public(
                        Duration::from_secs(60),
                        reader.load(Ordering::SeqCst).to_string(),
                    )
                }
            })
            .post(move || {
                let value = value.clone();
                async move {
                    value.fetch_add(1, Ordering::SeqCst);
                    "updated"
                }
            }),
        )
        .layer(RouteCache::new(cache).namespace("shared-contract"))
}

#[tokio::test]
#[ignore = "requires REDIS_URL selecting database 15"]
async fn atomic_insert_and_shared_http_invalidation() -> TestResult {
    let url = std::env::var("REDIS_URL")?;
    let store = RedisStore::connect(&url).await?;
    if store.database() != 15 {
        return Err("select Redis database 15".into());
    }
    let prefix = format!("cache-generation-{}:", uuid::Uuid::new_v4());
    let cache = RedisCache::new(store.with_prefix(prefix));
    let outcome = AssertUnwindSafe(async {
        let mut workers = Vec::new();
        for value in 0..32 {
            let cache = cache.clone();
            workers.push(tokio::spawn(async move {
                cache.set_if_absent("winner", vec![value]).await
            }));
        }
        let mut winners = 0;
        for worker in workers {
            winners += usize::from(worker.await??);
        }
        assert_eq!(winners, 1);
        let state = Arc::new(AtomicUsize::new(0));
        let first = TestClient::try_new(app(cache.clone(), state.clone()))?;
        let second = TestClient::try_new(app(cache.clone(), state))?;
        assert_eq!(first.get("/item").await?.text(), "0");
        let hit = second.get("/item").await?;
        assert_eq!(
            hit.headers
                .get("x-cache")
                .ok_or("missing cache marker")?
                .to_str()?,
            "hit"
        );
        second.post_raw("/item", "text/plain", Vec::new()).await?;
        let fresh = first.get("/item").await?;
        assert_eq!(fresh.text(), "1");
        assert_eq!(
            fresh
                .headers
                .get("x-cache")
                .ok_or("missing refreshed marker")?
                .to_str()?,
            "miss"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .catch_unwind()
    .await;
    cache.clear().await?;
    match outcome {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
