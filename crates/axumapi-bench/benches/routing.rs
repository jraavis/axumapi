//! Route dispatch: axumapi `RouterService` vs a raw axum `Router`.
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use axum::Router;
use axum::extract::Path as AxumPath;
use axum::routing::get as axum_get;
use axumapi::prelude::*;
use common::{axum_empty_request, empty_request};
use criterion::{Criterion, criterion_group, criterion_main};
use http::Method;
use tower::ServiceExt;

async fn ping() -> PlainText<&'static str> {
    PlainText("pong")
}

async fn user(Path(id): Path<u32>) -> PlainText<String> {
    PlainText(id.to_string())
}

async fn axum_ping() -> &'static str {
    "pong"
}

async fn axum_user(AxumPath(id): AxumPath<u32>) -> String {
    id.to_string()
}

fn bench_routing(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let ours = App::new()
        .route("/ping", get(ping))
        .route("/users/{id}", get(user))
        .into_router_service()
        .unwrap();
    let raw = Router::new()
        .route("/ping", axum_get(axum_ping))
        .route("/users/{id}", axum_get(axum_user));

    // Fail fast if a benchmark would measure an error path.
    for uri in ["/ping", "/users/42"] {
        let status = runtime
            .block_on(ours.clone().oneshot(empty_request(Method::GET, uri)))
            .unwrap()
            .status();
        assert_eq!(status, http::StatusCode::OK);
    }

    let mut group = c.benchmark_group("routing");
    for (name, uri) in [("static", "/ping"), ("path_param", "/users/42")] {
        group.bench_function(format!("axumapi/{name}"), |b| {
            b.to_async(&runtime).iter(|| {
                let svc = ours.clone();
                async move { svc.oneshot(empty_request(Method::GET, uri)).await.unwrap() }
            });
        });
        group.bench_function(format!("axum/{name}"), |b| {
            b.to_async(&runtime).iter(|| {
                let svc = raw.clone();
                async move {
                    svc.oneshot(axum_empty_request(Method::GET, uri))
                        .await
                        .unwrap()
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_routing);
criterion_main!(benches);
