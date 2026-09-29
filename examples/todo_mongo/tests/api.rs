//! Runs against a live MongoDB named by `MONGODB_URL`; without it the tests
//! print a notice and pass. Each test uses its own throw-away database.
#![allow(clippy::unwrap_used)]

use axumapi::Body;
use axumapi::http::StatusCode;
use axumapi_testkit::{TestClient, TestResponse};
use http::{Method, Request, header};
use serde_json::{Value, json};
use todo_mongo::{app, open_db};

/// A client on a fresh database, plus the URL and name to drop afterwards.
async fn client() -> Option<(TestClient, String, String)> {
    let Ok(url) = std::env::var("MONGODB_URL") else {
        eprintln!("MONGODB_URL not set: skipping");
        return None;
    };
    let name = format!(
        "todo_test_{}",
        std::process::id() as u64 * 1000 + rand_suffix()
    );
    let db = open_db(&url, &name).await.unwrap();
    Some((TestClient::new(app(db)), url, name))
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

async fn cleanup(url: &str, name: &str) {
    let backend = axumapi_backends::mongodb::MongoBackend::connect(url, name)
        .await
        .unwrap();
    backend.database().drop().await.unwrap();
}

async fn send(
    client: &TestClient,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> TestResponse {
    let mut req = Request::builder().method(method).uri(path);
    let body = match body {
        Some(value) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    client.send(req.body(body).unwrap()).await.unwrap()
}

#[tokio::test]
async fn crud_round_trip() {
    let Some((client, url, name)) = client().await else {
        return;
    };

    let created = client
        .post_json("/todos", &json!({"title": "write docs"}))
        .await
        .unwrap();
    assert_eq!(created.status, StatusCode::CREATED);
    let todo: Value = created.json().unwrap();
    assert_eq!(todo["done"], false);
    let id = todo["id"].as_i64().unwrap();

    let fetched = client.get(&format!("/todos/{id}")).await.unwrap();
    assert_eq!(fetched.json::<Value>().unwrap()["title"], "write docs");

    let patched = send(
        &client,
        Method::PATCH,
        &format!("/todos/{id}"),
        Some(json!({"done": true})),
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK);
    assert_eq!(patched.json::<Value>().unwrap()["done"], true);

    let deleted = send(&client, Method::DELETE, &format!("/todos/{id}"), None).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert_eq!(
        client.get(&format!("/todos/{id}")).await.unwrap().status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        send(&client, Method::DELETE, &format!("/todos/{id}"), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    cleanup(&url, &name).await;
}

#[tokio::test]
async fn generated_keys_filters_paging_and_stats() {
    let Some((client, url, name)) = client().await else {
        return;
    };
    let mut ids = Vec::new();
    for n in 1..=5 {
        let res = client
            .post_json("/todos", &json!({"title": format!("item {n}")}))
            .await
            .unwrap();
        ids.push(res.json::<Value>().unwrap()["id"].as_i64().unwrap());
    }
    assert!(
        ids.windows(2).all(|w| w[0] < w[1]),
        "keys increase: {ids:?}"
    );
    for id in &ids[..2] {
        send(
            &client,
            Method::PATCH,
            &format!("/todos/{id}"),
            Some(json!({"done": true})),
        )
        .await;
    }

    let done: Vec<Value> = client
        .get("/todos?done=true")
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(done.len(), 2);
    let page: Vec<Value> = client
        .get("/todos?limit=2&offset=2")
        .await
        .unwrap()
        .json()
        .unwrap();
    let titles: Vec<&str> = page.iter().map(|t| t["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["item 3", "item 4"]);
    let stats: Value = client.get("/todos/stats").await.unwrap().json().unwrap();
    assert_eq!(stats, json!({"total": 5, "done": 2}));
    assert_eq!(
        client.get("/todos?limit=1000").await.unwrap().status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    cleanup(&url, &name).await;
}

#[tokio::test]
async fn empty_title_is_422() {
    let Some((client, url, name)) = client().await else {
        return;
    };
    let res = client
        .post_json("/todos", &json!({"title": ""}))
        .await
        .unwrap();
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    cleanup(&url, &name).await;
}
