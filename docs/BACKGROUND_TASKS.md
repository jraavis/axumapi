# Process-local background work

`BackgroundTasks` is a handler extractor for bounded, deferred work. Queued
futures run only after the endpoint produces a successful 2xx response,
including its route status override. Work is discarded on extraction failure,
handler error, handler panic, timeout or caller cancellation. Dropping the
extractor no longer starts tasks. A default-constructed instance has no
request owner and rejects admission.

This boundary means response production, not streaming completion or
confirmed client receipt. Middleware may replace a produced response after
work has been accepted. For durable delivery or effects tied to a database
commit, use a durable queue/outbox; process-local tasks can be lost on crash.

```rust
use siderite::prelude::*;

type Reply = Result<PlainText<&'static str>, ApiError>;

async fn endpoint(mut tasks: BackgroundTasks) -> Reply {
    tasks.try_add(async {
        // Process-local work authorized by this endpoint's successful return.
    }).map_err(|error| ApiError::new(
        siderite::http::StatusCode::SERVICE_UNAVAILABLE,
        error.to_string(),
    ))?;
    Ok(PlainText("accepted"))
}

let app = App::new()
    .background_tasks(BackgroundTaskLimits {
        max_active_batches: 32,
        max_tasks: 512,
        max_tasks_per_request: 20,
    })
    .route("/", get(endpoint));
```

`try_add` reserves admission and returns `BackgroundTaskError` for capacity,
closed ownership or unavailable execution. Reservation does not authorize
execution: an abandoned request still releases its reservations and drops
its futures. `add` and `add_fn` preserve their convenience signatures, log
rejection and discard rejected futures. `add_fn` calls the factory
immediately, so defer side effects into its returned future.

Defaults per app are 64 active request batches, 1,024 admitted tasks total
and 100 tasks per request. Total admission includes queued, running and
waiting batches. Tasks in one batch run sequentially. A task panic is logged
and later tasks continue. Request tracing span and subscriber are propagated.
Mounted apps have separate queues; their queues join the parent lifespan.
Handlers without background extractors do not allocate a request task queue.

`App::run`, `run_until` and `TestClient::start` own accepted workers. Shutdown
closes admission and drains background work before resource hooks, sharing
the configured shutdown deadline. Expiry aborts remaining owned workers and
reports `ShutdownTimeout`. Cancellation requires cooperative Tokio futures;
blocking work or runtime destruction cannot guarantee timely asynchronous
cleanup. `into_router_service` and `TestClient::new` do not run lifespan
hooks; dropping the last router owner aborts remaining local work.

Custom extractors or handlers that delegate background extraction must
propagate the `BACKGROUND_TASKS` associated constant. `Option<T>` and the
blanket parts-to-request extractor propagate it automatically. Missing
request ownership fails explicitly rather than creating an untracked task.
