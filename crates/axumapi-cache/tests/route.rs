//! [`RouteCache`] through [`TestClient`].
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axumapi_cache::{MemoryCache, RouteCache};
use axumapi_core::header::{SetCookie, WithCookies};
use axumapi_core::http::StatusCode;
use axumapi_core::responses::WithHeaders;
use axumapi_core::{App, Body, get, post};
use axumapi_testkit::{TestClient, TestResponse};
use http::Method;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

async fn call(
    client: &TestClient,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
) -> TestResponse {
    let mut req = http::Request::builder().method(method).uri(path);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    client.send(req.body(Body::empty()).unwrap()).await.unwrap()
}

fn header<'a>(res: &'a TestResponse, name: &str) -> Option<&'a str> {
    res.headers.get(name)?.to_str().ok()
}

fn counted_app(hits: Arc<AtomicUsize>, ttl: Duration) -> (TestClient, Arc<AtomicUsize>) {
    let hits_get = Arc::clone(&hits);
    let hits_head = Arc::clone(&hits);
    let app = App::new()
        .route(
            "/item",
            get(move || {
                let hits = Arc::clone(&hits_get);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    "payload"
                }
            })
            .head(move || {
                let hits = Arc::clone(&hits_head);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    "payload"
                }
            }),
        )
        .layer(RouteCache::new(MemoryCache::new(32), ttl));
    (TestClient::new(app), hits)
}

#[tokio::test]
async fn get_is_cached_and_tagged() {
    let (client, hits) = counted_app(Arc::default(), Duration::from_secs(60));
    let first = client.get("/item").await.unwrap();
    assert_eq!(first.status, StatusCode::OK);
    assert_eq!(first.text(), "payload");
    assert_eq!(header(&first, "x-cache"), Some("miss"));
    let second = client.get("/item").await.unwrap();
    assert_eq!(second.text(), "payload");
    assert_eq!(header(&second, "x-cache"), Some("hit"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn query_string_is_part_of_the_key() {
    let (client, hits) = counted_app(Arc::default(), Duration::from_secs(60));
    let _ = client.get("/item?a=1").await.unwrap();
    let _ = client.get("/item?a=2").await.unwrap();
    let again = client.get("/item?a=1").await.unwrap();
    assert_eq!(header(&again, "x-cache"), Some("hit"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn post_is_not_cached() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_h = Arc::clone(&hits);
    let app = App::new()
        .route(
            "/item",
            post(move || {
                let hits = Arc::clone(&hits_h);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    "posted"
                }
            }),
        )
        .layer(RouteCache::new(
            MemoryCache::new(8),
            Duration::from_secs(60),
        ));
    let client = TestClient::new(app);
    let first = client
        .post_raw("/item", "text/plain", Vec::new())
        .await
        .unwrap();
    let second = client
        .post_raw("/item", "text/plain", Vec::new())
        .await
        .unwrap();
    assert_eq!(first.text(), "posted");
    assert_eq!(second.text(), "posted");
    assert_eq!(header(&first, "x-cache"), None);
    assert_eq!(header(&second, "x-cache"), None);
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn non_200_is_not_cached() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_h = Arc::clone(&hits);
    let app = App::new()
        .route(
            "/missing",
            get(move || {
                let hits = Arc::clone(&hits_h);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::NOT_FOUND, "gone")
                }
            }),
        )
        .layer(RouteCache::new(
            MemoryCache::new(8),
            Duration::from_secs(60),
        ));
    let client = TestClient::new(app);
    let first = client.get("/missing").await.unwrap();
    let second = client.get("/missing").await.unwrap();
    assert_eq!(first.status, StatusCode::NOT_FOUND);
    assert_eq!(header(&first, "x-cache"), Some("miss"));
    assert_eq!(header(&second, "x-cache"), Some("miss"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn authorization_and_cookie_bypass_the_cache() {
    let (client, hits) = counted_app(Arc::default(), Duration::from_secs(60));
    let _ = call(
        &client,
        Method::GET,
        "/item",
        &[("authorization", "Bearer x")],
    )
    .await;
    let _ = call(
        &client,
        Method::GET,
        "/item",
        &[("authorization", "Bearer x")],
    )
    .await;
    let _ = call(&client, Method::GET, "/item", &[("cookie", "sid=1")]).await;
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    let cached = client.get("/item").await.unwrap();
    assert_eq!(header(&cached, "x-cache"), Some("miss"));
    let hit = client.get("/item").await.unwrap();
    assert_eq!(header(&hit, "x-cache"), Some("hit"));
    assert_eq!(hits.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn set_cookie_response_is_not_stored() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_h = Arc::clone(&hits);
    let app = App::new()
        .route(
            "/login",
            get(move || {
                let hits = Arc::clone(&hits_h);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    WithCookies::new("ok").cookie(SetCookie::new("sid", "abc"))
                }
            }),
        )
        .layer(RouteCache::new(
            MemoryCache::new(8),
            Duration::from_secs(60),
        ));
    let client = TestClient::new(app);
    let first = client.get("/login").await.unwrap();
    let second = client.get("/login").await.unwrap();
    assert!(first.headers.contains_key("set-cookie"));
    assert_eq!(header(&first, "x-cache"), Some("miss"));
    assert_eq!(header(&second, "x-cache"), Some("miss"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn cache_control_no_store_and_private_are_not_stored() {
    async fn assert_uncached(control: &'static str) {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_h = Arc::clone(&hits);
        let app = App::new()
            .route(
                "/x",
                get(move || {
                    let hits = Arc::clone(&hits_h);
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        WithHeaders::new("ok").header("cache-control", control)
                    }
                }),
            )
            .layer(RouteCache::new(
                MemoryCache::new(8),
                Duration::from_secs(60),
            ));
        let client = TestClient::new(app);
        let _ = client.get("/x").await.unwrap();
        let second = client.get("/x").await.unwrap();
        assert_eq!(header(&second, "x-cache"), Some("miss"));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }
    assert_uncached("no-store").await;
    assert_uncached("private, max-age=60").await;
}

#[tokio::test]
async fn public_cache_control_is_stored() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_h = Arc::clone(&hits);
    let app = App::new()
        .route(
            "/x",
            get(move || {
                let hits = Arc::clone(&hits_h);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    WithHeaders::new("ok").header("cache-control", "public, max-age=60")
                }
            }),
        )
        .layer(RouteCache::new(
            MemoryCache::new(8),
            Duration::from_secs(60),
        ));
    let client = TestClient::new(app);
    let _ = client.get("/x").await.unwrap();
    let second = client.get("/x").await.unwrap();
    assert_eq!(header(&second, "x-cache"), Some("hit"));
    assert_eq!(header(&second, "cache-control"), Some("public, max-age=60"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn head_is_cached_separately_from_get() {
    let (client, hits) = counted_app(Arc::default(), Duration::from_secs(60));
    let get = client.get("/item").await.unwrap();
    assert_eq!(header(&get, "x-cache"), Some("miss"));
    let head = call(&client, Method::HEAD, "/item", &[]).await;
    assert_eq!(header(&head, "x-cache"), Some("miss"));
    let head_hit = call(&client, Method::HEAD, "/item", &[]).await;
    assert_eq!(header(&head_hit, "x-cache"), Some("hit"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn ttl_expiry_causes_a_miss() {
    let (client, hits) = counted_app(Arc::default(), Duration::from_millis(30));
    let _ = client.get("/item").await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    let again = client.get("/item").await.unwrap();
    assert_eq!(header(&again, "x-cache"), Some("miss"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn created_status_is_not_cached() {
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_h = Arc::clone(&hits);
    let app = App::new()
        .route(
            "/new",
            get(move || {
                let hits = Arc::clone(&hits_h);
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    (StatusCode::CREATED, "made")
                }
            }),
        )
        .layer(RouteCache::new(
            MemoryCache::new(8),
            Duration::from_secs(60),
        ));
    let client = TestClient::new(app);
    let _ = client.get("/new").await.unwrap();
    let second = client.get("/new").await.unwrap();
    assert_eq!(second.status, StatusCode::CREATED);
    assert_eq!(header(&second, "x-cache"), Some("miss"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn bypass_still_sets_x_cache_miss() {
    let (client, _) = counted_app(Arc::default(), Duration::from_secs(60));
    let res = call(
        &client,
        Method::GET,
        "/item",
        &[("authorization", "Bearer x")],
    )
    .await;
    assert_eq!(header(&res, "x-cache"), Some("miss"));
    assert_eq!(res.text(), "payload");
}

#[tokio::test]
async fn zero_ttl_never_stores() {
    let (client, hits) = counted_app(Arc::default(), Duration::ZERO);
    let _ = client.get("/item").await.unwrap();
    let second = client.get("/item").await.unwrap();
    assert_eq!(header(&second, "x-cache"), Some("miss"));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn layer_is_usable_on_app() {
    let app = App::new()
        .route("/ok", get(|| async { "ok" }))
        .layer(RouteCache::new(MemoryCache::new(1), Duration::from_secs(1)));
    let res = TestClient::new(app).get("/ok").await.unwrap();
    assert_eq!(res.text(), "ok");
}
