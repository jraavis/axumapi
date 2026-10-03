//! Resource ownership through startup errors and caller cancellation.

use siderite_core::{ApiError, App, ServerError};
use siderite_testkit::TestClient;
use std::future::{Ready, pending};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::oneshot;

type TestResult = Result<(), Box<dyn std::error::Error>>;

type Log = Arc<Mutex<Vec<&'static str>>>;

fn add(app: App, log: &Log, open: &'static str, close: &'static str) -> App {
    let log = Arc::clone(log);
    app.lifespan_resource(move || async move {
        log.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(open);
        Ok(((), async move {
            log.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(close);
            Ok(())
        }))
    })
}

fn entries(log: &Log) -> Vec<&'static str> {
    log.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

fn sync_panic() -> Ready<Result<(), ApiError>> {
    panic!("hook factory panic")
}

#[tokio::test]
async fn startup_error_unwinds_resources_and_preserves_original_error() {
    let log = Log::default();
    let app = add(App::new(), &log, "open-one", "close-one")
        .on_shutdown(|| async { Err(ApiError::not_found("cleanup error")) });
    let app = add(app, &log, "open-two", "close-two")
        .on_startup(|| async { Err(ApiError::bad_request("startup error")) });
    let result = TestClient::start(app).await;
    assert!(matches!(result, Err(ServerError::Lifespan(ref error))
        if error.status() == siderite_core::http::StatusCode::BAD_REQUEST));
    assert_eq!(
        entries(&log),
        ["open-one", "open-two", "close-two", "close-one"]
    );
}

#[tokio::test]
async fn startup_and_shutdown_factory_panics_do_not_skip_cleanup() {
    let log = Log::default();
    let app = add(App::new(), &log, "open-one", "close-one");
    let app = app.on_shutdown(sync_panic);
    let app = add(app, &log, "open-two", "close-two");
    let app = app.on_startup(sync_panic);
    assert!(matches!(
        TestClient::start(app).await,
        Err(ServerError::Lifespan(_))
    ));
    assert_eq!(
        entries(&log),
        ["open-one", "open-two", "close-two", "close-one"]
    );
}

#[tokio::test]
async fn bind_error_cleans_resources_before_returning() {
    let log = Log::default();
    let app = add(App::new(), &log, "opened", "closed")
        .on_shutdown(|| async { Err(ApiError::not_found("cleanup error")) });
    let result = app.run("not a socket address").await;
    assert!(matches!(result, Err(ServerError::Bind { .. })));
    assert_eq!(entries(&log), ["opened", "closed"]);
}

#[tokio::test]
async fn cancelled_startup_keeps_cleanup_owned() -> TestResult {
    let (started, waiting) = oneshot::channel();
    let (closed, cleanup) = oneshot::channel();
    let app = App::new()
        .lifespan_resource(|| async {
            Ok(((), async move {
                let _ = closed.send(());
                Ok(())
            }))
        })
        .on_startup(|| async move {
            let _ = started.send(());
            pending::<Result<(), ApiError>>().await
        });
    let mut start = Box::pin(TestClient::start(app));
    tokio::select! {
        reached = waiting => reached?,
        result = &mut start => panic!("startup completed: {result:?}"),
    }
    drop(start);
    tokio::time::timeout(Duration::from_secs(1), cleanup).await??;
    Ok(())
}

#[tokio::test]
async fn cancelled_shutdown_future_does_not_cancel_cleanup() -> TestResult {
    let (closing, started) = oneshot::channel();
    let (finish, resume) = oneshot::channel();
    let (closed, done) = oneshot::channel();
    let app = App::new().lifespan_resource(|| async {
        Ok(((), async move {
            let _ = closing.send(());
            let _ = resume.await;
            let _ = closed.send(());
            Ok(())
        }))
    });
    let client = TestClient::start(app).await?;
    let mut shutdown = Box::pin(client.shutdown());
    tokio::select! {
        reached = started => reached?,
        result = &mut shutdown => panic!("shutdown completed: {result:?}"),
    }
    drop(shutdown);
    let _ = finish.send(());
    tokio::time::timeout(Duration::from_secs(1), done).await??;
    client.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn last_client_owner_drop_requests_cleanup() -> TestResult {
    let (closed, mut done) = oneshot::channel();
    let app = App::new().lifespan_resource(|| async {
        Ok(((), async move {
            let _ = closed.send(());
            Ok(())
        }))
    });
    let client = TestClient::start(app).await?;
    let other = client.clone();
    drop(client);
    assert!(matches!(
        done.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    drop(other);
    tokio::time::timeout(Duration::from_secs(1), done).await??;
    Ok(())
}

#[tokio::test]
async fn shutdown_deadline_still_attempts_remaining_hooks() -> TestResult {
    let (closed, done) = oneshot::channel();
    let app = App::new()
        .shutdown_timeout(Duration::from_millis(10))
        .on_shutdown(|| async move {
            let _ = closed.send(());
            Ok(())
        })
        .on_shutdown(pending::<Result<(), ApiError>>);
    let client = TestClient::start(app).await?;
    let budget = Duration::from_secs(1);
    let result = tokio::time::timeout(budget, client.shutdown()).await?;
    assert!(matches!(result, Err(ServerError::ShutdownTimeout)));
    done.await?;
    Ok(())
}

#[tokio::test]
async fn explicit_shutdown_during_startup_waits_for_cleanup() -> TestResult {
    let (started, reached) = oneshot::channel();
    let (closed, done) = oneshot::channel();
    let app = App::new()
        .lifespan_resource(|| async {
            Ok(((), async move {
                let _ = closed.send(());
                Ok(())
            }))
        })
        .on_startup(|| async move {
            let _ = started.send(());
            pending::<Result<(), ApiError>>().await
        });
    app.run_until("127.0.0.1:0", async {
        let _ = reached.await;
    })
    .await?;
    done.await?;
    Ok(())
}
