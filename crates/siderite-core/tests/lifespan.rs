//! Lifespan hooks and scoped resources.
#![allow(clippy::unwrap_used)]

use siderite_core::http::StatusCode;
use siderite_core::lifespan::Resource;
use siderite_core::*;
use siderite_testkit::TestClient;
use std::sync::{Arc, Mutex};

type Log = Arc<Mutex<Vec<String>>>;

fn hook(
    log: &Log,
    name: &'static str,
) -> impl FnOnce() -> std::future::Ready<Result<(), ApiError>> + Send + 'static {
    let log = Arc::clone(log);
    move || {
        log.lock().unwrap().push(name.to_owned());
        std::future::ready(Ok(()))
    }
}

fn entries(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

#[tokio::test]
async fn hooks_run_in_order_and_shutdown_reverses_including_mounts() {
    let log = Log::default();
    let child = App::new()
        .on_startup(hook(&log, "child-up"))
        .on_shutdown(hook(&log, "child-down"));
    let app = App::new()
        .on_startup(hook(&log, "up-1"))
        .on_shutdown(hook(&log, "down-1"))
        .mount("/c", child)
        .on_startup(hook(&log, "up-2"))
        .on_shutdown(hook(&log, "down-2"));
    let client = TestClient::start(app).await.unwrap();
    assert_eq!(entries(&log), ["up-1", "up-2", "child-up"]);
    client.shutdown().await.unwrap();
    assert_eq!(
        entries(&log),
        ["up-1", "up-2", "child-up", "child-down", "down-2", "down-1"]
    );
    client.shutdown().await.unwrap();
    assert_eq!(entries(&log).len(), 6, "shutdown runs once");
}

#[tokio::test]
async fn try_new_does_not_run_hooks() {
    let log = Log::default();
    let app = App::new().on_startup(hook(&log, "up"));
    let _client = TestClient::new(app);
    assert!(entries(&log).is_empty());
}

#[tokio::test]
async fn startup_failure_propagates_as_lifespan_error() {
    let log = Log::default();
    let app = App::new()
        .on_startup(hook(&log, "before"))
        .on_startup(|| async { Err(ApiError::internal("boom")) })
        .on_startup(hook(&log, "after"));
    let err = TestClient::start(app).await.unwrap_err();
    assert!(
        matches!(err, ServerError::Lifespan(ref e) if e.status() == StatusCode::INTERNAL_SERVER_ERROR)
    );
    assert_eq!(entries(&log), ["before"]);
}

#[tokio::test]
async fn shutdown_runs_every_hook_and_reports_the_first_failure() {
    let log = Log::default();
    let app = App::new()
        .on_shutdown(hook(&log, "down-1"))
        .on_shutdown(|| async { Err(ApiError::bad_request("first failure")) })
        .on_shutdown(|| async { Err(ApiError::not_found("second failure")) });
    let client = TestClient::start(app).await.unwrap();
    let err = client.shutdown().await.unwrap_err();
    assert!(matches!(err, ServerError::Lifespan(ref e) if e.status() == StatusCode::NOT_FOUND));
    assert_eq!(entries(&log), ["down-1"]);
}

struct Pool {
    name: &'static str,
}

fn pool_app(log: &Log, name: &'static str) -> App {
    let log = Arc::clone(log);
    App::new().lifespan_resource(move || async move {
        log.lock().unwrap().push(format!("open-{name}"));
        let closing = Arc::clone(&log);
        Ok((Pool { name }, async move {
            closing.lock().unwrap().push(format!("close-{name}"));
            Ok(())
        }))
    })
}

async fn pool_name(pool: Resource<Pool>) -> String {
    pool.name.to_owned()
}

#[tokio::test]
async fn resource_is_unavailable_before_start_and_available_after() {
    let log = Log::default();
    let app = || pool_app(&log, "db").route("/p", get(pool_name));

    let cold = TestClient::new(app());
    let res = cold.get("/p").await.unwrap();
    assert_eq!(res.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(res.content_type(), Some("application/problem+json"));

    let warm = TestClient::start(app()).await.unwrap();
    assert_eq!(warm.get("/p").await.unwrap().text(), "db");
    warm.shutdown().await.unwrap();
    assert_eq!(entries(&log), ["open-db", "close-db"]);
    assert_eq!(
        warm.get("/p").await.unwrap().status,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn resources_close_in_reverse_order_and_unregistered_is_a_500() {
    let log = Log::default();
    let app = pool_app(&log, "a").route("/p", get(pool_name));
    let second = {
        let log = Arc::clone(&log);
        app.lifespan_resource(move || async move {
            log.lock().unwrap().push("open-b".to_owned());
            Ok((3_u32, async move { Ok(()) }))
        })
    };
    let client = TestClient::start(second).await.unwrap();
    client.shutdown().await.unwrap();
    assert_eq!(entries(&log), ["open-a", "open-b", "close-a"]);

    let bare = TestClient::new(App::new().route("/p", get(pool_name)));
    assert_eq!(
        bare.get("/p").await.unwrap().status,
        StatusCode::INTERNAL_SERVER_ERROR
    );
}

#[tokio::test]
async fn resource_init_failure_aborts_startup() {
    let app = App::new().lifespan_resource(|| async {
        Err::<(u8, std::future::Ready<Result<(), ApiError>>), _>(ApiError::internal("db down"))
    });
    assert!(matches!(
        TestClient::start(app).await.unwrap_err(),
        ServerError::Lifespan(_)
    ));
}

#[tokio::test]
async fn resources_of_mounted_children_are_visible_in_the_child() {
    let log = Log::default();
    let child = pool_app(&log, "child").route("/p", get(pool_name));
    let client = TestClient::start(App::new().mount("/m", child))
        .await
        .unwrap();
    assert_eq!(client.get("/m/p").await.unwrap().text(), "child");
    client.shutdown().await.unwrap();
}
