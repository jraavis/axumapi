//! Admission bounds, cancellation and body permit ownership.

use bytes::Bytes;
use http::{Request, StatusCode};
use siderite_core::StreamingResponse;
use siderite_core::{App, Body, ConcurrencyLimit, State, get};
use siderite_testkit::TestClient;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Chunk = Result<Bytes, std::io::Error>;

#[derive(Clone, Default)]
struct Gate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

async fn blocked(State(gate): State<Gate>) -> &'static str {
    gate.entered.notify_one();
    gate.release.notified().await;
    "done"
}

fn setup(limit: ConcurrencyLimit, gate: Gate) -> App {
    App::new()
        .route("/", get(blocked))
        .with_state(gate)
        .layer(limit)
}

async fn wait_for(mut ready: impl FnMut() -> bool) -> TestResult {
    tokio::time::timeout(Duration::from_secs(1), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn bounded_queue_rejects_and_releases_after_abort() -> TestResult {
    let limit = ConcurrencyLimit::new(1).queue(1, Duration::from_secs(1));
    let gate = Gate::default();
    let client = TestClient::try_new(setup(limit.clone(), gate.clone()))?;
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.get("/").await });
    wait_for(|| limit.stats().active == 1).await?;
    let second_client = client.clone();
    let second = tokio::spawn(async move { second_client.get("/").await });
    wait_for(|| limit.stats().waiting == 1).await?;
    let rejected = client.get("/").await?;
    assert_eq!(rejected.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejected.headers.get("retry-after"), Some(&"1".parse()?));
    assert_eq!(limit.stats().rejected, 1);
    second.abort();
    assert!(second.await.is_err());
    first.abort();
    assert!(first.await.is_err());
    wait_for(|| {
        let stats = limit.stats();
        stats.active == 0 && stats.waiting == 0
    })
    .await?;
    let next_client = client.clone();
    let next = tokio::spawn(async move { next_client.get("/").await });
    wait_for(|| limit.stats().active == 1).await?;
    gate.release.notify_one();
    assert_eq!(next.await??.status, StatusCode::OK);
    assert_eq!(limit.stats().active, 0);
    Ok(())
}

#[tokio::test]
async fn deadline_and_close_wake_waiters() -> TestResult {
    let limit = ConcurrencyLimit::new(1).queue(1, Duration::from_millis(10));
    let client = TestClient::try_new(setup(limit.clone(), Gate::default()))?;
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.get("/").await });
    wait_for(|| limit.stats().active == 1).await?;
    assert_eq!(
        client.get("/").await?.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(limit.stats().timed_out, 1);
    assert!(limit.stats().wait_nanos > 0);
    limit.close();
    assert_eq!(
        client.get("/").await?.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    first.abort();
    assert!(first.await.is_err());
    Ok(())
}

#[tokio::test]
async fn queued_request_runs_after_execution_releases() -> TestResult {
    let limit = ConcurrencyLimit::new(1).queue(1, Duration::from_secs(1));
    let gate = Gate::default();
    let client = TestClient::try_new(setup(limit.clone(), gate.clone()))?;
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.get("/").await });
    wait_for(|| limit.stats().active == 1).await?;
    let second_client = client.clone();
    let second = tokio::spawn(async move { second_client.get("/").await });
    wait_for(|| limit.stats().waiting == 1).await?;
    gate.release.notify_one();
    assert_eq!(first.await??.status, StatusCode::OK);
    wait_for(|| limit.stats().admitted == 2).await?;
    gate.release.notify_one();
    assert_eq!(second.await??.status, StatusCode::OK);
    assert_eq!(limit.stats().active, 0);
    assert_eq!(limit.stats().waiting, 0);
    Ok(())
}

async fn pending_stream() -> StreamingResponse {
    StreamingResponse::new(futures_util::stream::pending::<Chunk>())
}

#[tokio::test]
async fn closing_admission_wakes_pending_requests() -> TestResult {
    let limit = ConcurrencyLimit::new(1).queue(1, Duration::from_secs(60));
    let client = TestClient::try_new(setup(limit.clone(), Gate::default()))?;
    let first_client = client.clone();
    let first = tokio::spawn(async move { first_client.get("/").await });
    wait_for(|| limit.stats().active == 1).await?;
    let queued_client = client.clone();
    let queued = tokio::spawn(async move { queued_client.get("/").await });
    wait_for(|| limit.stats().waiting == 1).await?;
    limit.close();
    let result = tokio::time::timeout(Duration::from_secs(1), queued).await??;
    assert_eq!(result?.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(limit.stats().waiting, 0);
    first.abort();
    assert!(first.await.is_err());
    assert_eq!(limit.stats().active, 0);
    Ok(())
}

fn request() -> Result<Request<Body>, http::Error> {
    Request::builder().uri("/").body(Body::empty())
}

#[tokio::test]
async fn body_drop_retains_then_releases_without_buffering() -> TestResult {
    let limit = ConcurrencyLimit::new(1)
        .queue(0, Duration::ZERO)
        .hold_body(true);
    let router = App::new()
        .route("/", get(pending_stream))
        .layer(limit.clone())
        .into_router_service()?;
    let response = router.clone().oneshot(request()?).await?;
    assert_eq!(limit.stats().active, 1);
    let rejected = router.clone().oneshot(request()?).await?;
    assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(response);
    assert_eq!(limit.stats().active, 0);
    let response = router.clone().oneshot(request()?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    drop(response);
    assert_eq!(limit.stats().active, 0);
    Ok(())
}

#[tokio::test]
async fn body_completion_and_error_release_permits() -> TestResult {
    for fail in [false, true] {
        let limit = ConcurrencyLimit::new(1).hold_body(true);
        let router = App::new()
            .route(
                "/",
                get(move || async move {
                    let chunk = if fail {
                        Err(std::io::Error::other("stream fault"))
                    } else {
                        Ok(Bytes::from_static(b"done"))
                    };
                    StreamingResponse::new(futures_util::stream::iter([chunk]))
                }),
            )
            .layer(limit.clone())
            .into_router_service()?;
        let response = router.clone().oneshot(request()?).await?;
        assert_eq!(limit.stats().active, 1);
        let result = response.into_body().into_bytes_limited(64).await;
        assert_eq!(result.is_err(), fail);
        assert_eq!(limit.stats().active, 0);
    }
    Ok(())
}

#[tokio::test]
async fn default_scope_releases_at_response_production() -> TestResult {
    let limit = ConcurrencyLimit::new(1).queue(0, Duration::ZERO);
    let router = App::new()
        .route("/", get(pending_stream))
        .layer(limit.clone())
        .into_router_service()?;
    let response = router.clone().oneshot(request()?).await?;
    assert_eq!(limit.stats().active, 0);
    assert_eq!(router.clone().oneshot(request()?).await?.status(), 200);
    drop(response);
    Ok(())
}

#[tokio::test]
async fn timeout_order_and_invalid_configuration() -> TestResult {
    let deadline = Duration::from_millis(10);
    for outer in [false, true] {
        let limit = ConcurrencyLimit::new(1).queue(1, Duration::from_secs(1));
        let app = if !outer {
            setup(limit.clone(), Gate::default()).timeout(deadline)
        } else {
            App::new()
                .route("/", get(blocked))
                .with_state(Gate::default())
                .timeout(Duration::from_millis(10))
                .layer(limit.clone())
        };
        let client = TestClient::try_new(app)?;
        let first_client = client.clone();
        let first = tokio::spawn(async move { first_client.get("/").await });
        wait_for(|| limit.stats().active == 1).await?;
        assert_eq!(client.get("/").await?.status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(first.await??.status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(limit.stats().active, 0);
        assert_eq!(limit.stats().waiting, 0);
    }
    assert!(ConcurrencyLimit::try_new(0).is_err());
    assert!(ConcurrencyLimit::try_new(usize::MAX).is_err());
    let invalid = ConcurrencyLimit::new(1).queue(usize::MAX, Duration::MAX);
    let client = TestClient::try_new(setup(invalid, Gate::default()))?;
    assert_eq!(
        client.get("/").await?.status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    let app = App::new().concurrency_limit(usize::MAX);
    assert!(TestClient::try_new(app).is_err());
    Ok(())
}
