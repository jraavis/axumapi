//! Deferred side effects require request success and bounded app ownership.

use bytes::Bytes;
use futures_util::FutureExt;
use http::{StatusCode, request::Parts};
use siderite_core::BackgroundTasks as Tasks;
use siderite_core::StreamingResponse as Stream;
use siderite_core::background::{BackgroundTaskError, BackgroundTaskLimits};
use siderite_core::{ApiError, App, Body, FromRequestParts, get};
use siderite_core::{ServerError, State};
use siderite_testkit::TestClient;
use std::future::pending;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{Notify, Semaphore};
use tower::ServiceExt;

type Text = &'static str;
type ApiResult = Result<Text, ApiError>;
type Chunk = Result<Bytes, std::io::Error>;
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Clone, Default)]
struct Probe {
    queued: Arc<Notify>,
    started: Arc<Notify>,
    finished: Arc<Notify>,
    dropped: Arc<Notify>,
    calls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

struct Marker(Probe);

impl Drop for Marker {
    fn drop(&mut self) {
        self.0.drops.fetch_add(1, Ordering::SeqCst);
        self.0.dropped.notify_one();
    }
}

fn enqueue(tasks: &mut Tasks, probe: &Probe) {
    let queued = probe.queued.clone();
    let probe = probe.clone();
    let marker = Marker(probe.clone());
    tasks.add(async move {
        let _marker = marker;
        probe.calls.fetch_add(1, Ordering::SeqCst);
        probe.finished.notify_one();
    });
    queued.notify_one();
}

async fn abandoned(State(probe): State<Probe>, mut tasks: Tasks) -> Text {
    enqueue(&mut tasks, &probe);
    pending().await
}

async fn failed(State(probe): State<Probe>, mut tasks: Tasks) -> ApiResult {
    enqueue(&mut tasks, &probe);
    Err(ApiError::bad_request("failed handler"))
}

async fn panicked(State(probe): State<Probe>, mut tasks: Tasks) -> Text {
    enqueue(&mut tasks, &probe);
    panic!("handler failure")
}

struct EnqueueDuringExtraction;

impl FromRequestParts for EnqueueDuringExtraction {
    const BACKGROUND_TASKS: bool = true;

    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        let State(probe) = State::<Probe>::from_request_parts(parts).await?;
        let mut tasks = Tasks::from_request_parts(parts).await?;
        enqueue(&mut tasks, &probe);
        Ok(Self)
    }
}

struct Reject;

impl FromRequestParts for Reject {
    async fn from_request_parts(_: &mut Parts) -> Result<Self, ApiError> {
        Err(ApiError::bad_request("extraction failed"))
    }
}

async fn extraction_failure(_: EnqueueDuringExtraction, _: Reject) -> Text {
    "unreachable"
}

fn assert_discarded(probe: &Probe) {
    assert_eq!(probe.calls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
}

async fn wait(notify: &Notify) -> TestResult {
    tokio::time::timeout(Duration::from_secs(1), notify.notified()).await?;
    Ok(())
}

#[tokio::test]
async fn timeout_discards_queued_side_effects() -> TestResult {
    let probe = Probe::default();
    let client = TestClient::try_new(
        App::new()
            .route("/", get(abandoned))
            .with_state(probe.clone())
            .timeout(Duration::from_millis(10)),
    )?;
    assert_eq!(client.get("/").await?.status, StatusCode::GATEWAY_TIMEOUT);
    assert_discarded(&probe);
    Ok(())
}

#[tokio::test]
async fn caller_abort_discards_queued_side_effects() -> TestResult {
    let probe = Probe::default();
    let client = TestClient::try_new(
        App::new()
            .route("/", get(abandoned))
            .with_state(probe.clone()),
    )?;
    let request = tokio::spawn(async move { client.get("/").await });
    wait(&probe.queued).await?;
    request.abort();
    assert!(request.await.is_err());
    assert_discarded(&probe);
    Ok(())
}

#[tokio::test]
async fn errors_and_panic_discard_work() -> TestResult {
    for kind in ["error", "extract", "panic"] {
        let probe = Probe::default();
        let methods = match kind {
            "error" => get(failed),
            "extract" => get(extraction_failure),
            _ => get(panicked),
        };
        let app = App::new().route("/", methods).with_state(probe.clone());
        let client = TestClient::try_new(app)?;
        let result = AssertUnwindSafe(client.get("/")).catch_unwind().await;
        if kind == "panic" {
            assert!(result.is_err());
        } else {
            let result = result.map_err(|_| "unexpected handler panic")??;
            assert_eq!(result.status, StatusCode::BAD_REQUEST);
        }
        assert_discarded(&probe);
    }
    Ok(())
}

async fn succeeded(State(probe): State<Probe>, mut tasks: Tasks) -> Text {
    enqueue(&mut tasks, &probe);
    "ok"
}

#[tokio::test]
async fn status_override_is_checked_before_execution() -> TestResult {
    let probe = Probe::default();
    let client = TestClient::try_new(
        App::new()
            .route(
                "/",
                get(succeeded).status(StatusCode::INTERNAL_SERVER_ERROR),
            )
            .with_state(probe.clone()),
    )?;
    assert_eq!(
        client.get("/").await?.status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_discarded(&probe);
    Ok(())
}

async fn streaming(State(probe): State<Probe>, mut tasks: Tasks) -> Stream {
    enqueue(&mut tasks, &probe);
    let stream = futures_util::stream::pending::<Chunk>();
    Stream::new(stream)
}

#[tokio::test]
async fn stream_delivery_does_not_gate_execution() -> TestResult {
    let probe = Probe::default();
    let service = App::new()
        .route("/", get(streaming))
        .with_state(probe.clone())
        .into_router_service()?;
    let request = http::Request::builder().uri("/").body(Body::empty())?;
    let Ok(response) = service.clone().oneshot(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    // The response body remains unpolled and never completes.
    wait(&probe.finished).await?;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    drop(response);
    Ok(())
}

#[derive(Clone)]
struct Held {
    probe: Probe,
    gate: Arc<Semaphore>,
}

async fn held(State(held): State<Held>, mut tasks: Tasks) -> ApiResult {
    let probe = held.probe.clone();
    let marker = Marker(probe.clone());
    tasks
        .try_add(async move {
            let _marker = marker;
            probe.calls.fetch_add(1, Ordering::SeqCst);
            probe.started.notify_one();
            let Ok(permit) = held.gate.acquire().await else {
                return;
            };
            permit.forget();
            probe.finished.notify_one();
        })
        .map_err(unavailable)?;
    Ok("accepted")
}

fn unavailable(error: BackgroundTaskError) -> ApiError {
    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, error.to_string())
}

fn limits(total: usize) -> BackgroundTaskLimits {
    BackgroundTaskLimits {
        max_active_batches: 1,
        max_tasks: total,
        max_tasks_per_request: 1,
    }
}

#[tokio::test]
async fn overload_rejects_and_recovers() -> TestResult {
    let probe = Probe::default();
    let gate = Arc::new(Semaphore::new(0));
    let client = TestClient::start(
        App::new()
            .route("/", get(held))
            .with_state(Held {
                probe: probe.clone(),
                gate: gate.clone(),
            })
            .background_tasks(limits(1)),
    )
    .await?;
    assert_eq!(client.get("/").await?.status, StatusCode::OK);
    wait(&probe.started).await?;
    assert_eq!(
        client.get("/").await?.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    gate.add_permits(1);
    wait(&probe.finished).await?;
    assert_eq!(client.get("/").await?.status, StatusCode::OK);
    gate.add_permits(1);
    client.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn bounded_batches_drain_before_teardown() -> TestResult {
    let probe = Probe::default();
    let gate = Arc::new(Semaphore::new(0));
    let observed = Arc::new(AtomicUsize::new(0));
    let on_close = observed.clone();
    let closing_probe = probe.clone();
    let client = TestClient::start(
        App::new()
            .route("/", get(held))
            .with_state(Held {
                probe: probe.clone(),
                gate: gate.clone(),
            })
            .background_tasks(limits(2))
            .on_shutdown(move || async move {
                let count = closing_probe.drops.load(Ordering::SeqCst);
                on_close.store(count, Ordering::SeqCst);
                Ok(())
            }),
    )
    .await?;
    assert_eq!(client.get("/").await?.status, StatusCode::OK);
    wait(&probe.started).await?;
    assert_eq!(client.get("/").await?.status, StatusCode::OK);
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        client.get("/").await?.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    gate.add_permits(2);
    client.shutdown().await?;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    assert_eq!(observed.load(Ordering::SeqCst), 3);
    Ok(())
}

#[tokio::test]
async fn shutdown_deadline_aborts_owned_background_work() -> TestResult {
    let probe = Probe::default();
    let client = TestClient::start(
        App::new()
            .route("/", get(held))
            .with_state(Held {
                probe: probe.clone(),
                gate: Arc::new(Semaphore::new(0)),
            })
            .background_tasks(limits(1))
            .shutdown_timeout(Duration::from_millis(20)),
    )
    .await?;
    assert_eq!(client.get("/").await?.status, StatusCode::OK);
    wait(&probe.started).await?;
    assert!(matches!(
        client.shutdown().await,
        Err(ServerError::ShutdownTimeout)
    ));
    wait(&probe.dropped).await?;
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert_eq!(
        client.get("/").await?.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    Ok(())
}

#[test]
fn invalid_limits_and_unmanaged_work_fail_explicitly() {
    let mut tasks = Tasks::default();
    assert_eq!(
        tasks.try_add(async {}),
        Err(BackgroundTaskError::Unavailable)
    );
    let mut invalid = limits(1);
    invalid.max_active_batches = 0;
    assert!(
        App::new()
            .background_tasks(invalid)
            .into_router_service()
            .is_err()
    );
}

async fn per_request(State(probe): State<Probe>, mut tasks: Tasks) -> Text {
    enqueue(&mut tasks, &probe);
    assert_eq!(tasks.try_add(async {}), Err(BackgroundTaskError::Capacity));
    "limited"
}

async fn optional(State(probe): State<Probe>, tasks: Option<Tasks>) -> Text {
    if let Some(mut tasks) = tasks {
        enqueue(&mut tasks, &probe);
    }
    "optional"
}

#[tokio::test]
async fn request_limit_and_optional_ownership() -> TestResult {
    let probe = Probe::default();
    let client = TestClient::start(
        App::new()
            .route("/limit", get(per_request))
            .route("/optional", get(optional))
            .with_state(probe.clone())
            .background_tasks(limits(2)),
    )
    .await?;
    assert_eq!(client.get("/limit").await?.text(), "limited");
    wait(&probe.finished).await?;
    assert_eq!(client.get("/optional").await?.text(), "optional");
    wait(&probe.finished).await?;
    client.shutdown().await?;
    assert_eq!(probe.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn mounted_queue_ownership() -> TestResult {
    let root = Probe::default();
    let child = Probe::default();
    let gate = Arc::new(Semaphore::new(0));
    let nested = App::new()
        .route("/", get(held))
        .with_state(Held {
            probe: child.clone(),
            gate: gate.clone(),
        })
        .background_tasks(limits(1));
    let client = TestClient::start(
        App::new()
            .route("/", get(held))
            .with_state(Held {
                probe: root.clone(),
                gate: gate.clone(),
            })
            .background_tasks(limits(1))
            .mount("/child", nested),
    )
    .await?;
    assert_eq!(client.get("/").await?.status, StatusCode::OK);
    wait(&root.started).await?;
    assert_eq!(client.get("/child").await?.status, StatusCode::OK);
    wait(&child.started).await?;
    assert_eq!(
        client.get("/").await?.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        client.get("/child").await?.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
    gate.add_permits(2);
    client.shutdown().await?;
    assert_eq!(root.calls.load(Ordering::SeqCst), 1);
    assert_eq!(child.calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn dropping_last_router_owner_aborts_in_process_work() -> TestResult {
    let probe = Probe::default();
    let state = Held {
        probe: probe.clone(),
        gate: Arc::new(Semaphore::new(0)),
    };
    let app = App::new().route("/", get(held)).with_state(state);
    let client = TestClient::try_new(app)?;
    assert_eq!(client.get("/").await?.status, StatusCode::OK);
    wait(&probe.started).await?;
    drop(client);
    wait(&probe.dropped).await?;
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    Ok(())
}
