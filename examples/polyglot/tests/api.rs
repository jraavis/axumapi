//! Two in-memory SQLite databases behind the router.
#![allow(clippy::unwrap_used)]

use polyglot::{ANALYTICS, Event, USERS, User, provision, registry};
use serde_json::{Value, json};
use siderite::http::StatusCode;
use siderite::orm::{Db, Model};
use siderite_testkit::{TestClient, TestDatabase};

struct Fixture {
    client: TestClient,
    users: Db,
    analytics: Db,
}

async fn fixture() -> Fixture {
    let users = TestDatabase::sqlite_memory().await.unwrap().into_db();
    let analytics = TestDatabase::sqlite_memory().await.unwrap().into_db();
    let databases = registry(users.clone(), analytics.clone());
    provision(&databases).await.unwrap();
    Fixture {
        client: TestClient::new(polyglot::app(databases)),
        users,
        analytics,
    }
}

async fn tables(db: &Db) -> Vec<String> {
    let rows = db
        .raw_sql(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            vec![],
        )
        .await
        .unwrap();
    rows.rows
        .iter()
        .map(|r| r.decode_at::<String>(0).unwrap())
        .collect()
}

#[tokio::test]
async fn provisioning_creates_each_table_only_where_the_router_allows() {
    let f = fixture().await;
    assert_eq!(tables(&f.users).await, ["users"]);
    assert_eq!(tables(&f.analytics).await, ["events"]);
}

#[tokio::test]
async fn provisioning_is_idempotent() {
    let f = fixture().await;
    let databases = registry(f.users.clone(), f.analytics.clone());
    provision(&databases).await.unwrap();
    assert_eq!(tables(&f.users).await, ["users"]);
}

#[tokio::test]
async fn rows_land_in_the_routed_database() {
    let f = fixture().await;
    let user = f
        .client
        .post_json("/users", &json!({"name": "ann"}))
        .await
        .unwrap();
    assert_eq!(user.status, StatusCode::CREATED);
    let id = user.json::<Value>().unwrap()["id"].as_i64().unwrap();
    let event = f
        .client
        .post_json("/events", &json!({"user_id": id, "kind": "login"}))
        .await
        .unwrap();
    assert_eq!(event.status, StatusCode::CREATED);

    assert_eq!(User::objects(&f.users).count().await.unwrap(), 1);
    assert_eq!(Event::objects(&f.analytics).count().await.unwrap(), 1);
}

#[tokio::test]
async fn events_for_unknown_users_are_rejected() {
    let f = fixture().await;
    let res = f
        .client
        .post_json("/events", &json!({"user_id": 99, "kind": "login"}))
        .await
        .unwrap();
    assert_eq!(res.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn user_events_are_joined_in_the_application() {
    let f = fixture().await;
    let id = f
        .client
        .post_json("/users", &json!({"name": "ann"}))
        .await
        .unwrap()
        .json::<Value>()
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    for kind in ["login", "logout"] {
        f.client
            .post_json("/events", &json!({"user_id": id, "kind": kind}))
            .await
            .unwrap();
    }
    let res = f.client.get(&format!("/users/{id}/events")).await.unwrap();
    let body: Value = res.json().unwrap();
    assert_eq!(body["user"]["name"], "ann");
    assert_eq!(body["events"].as_array().unwrap().len(), 2);
    assert_eq!(
        f.client.get("/users/99/events").await.unwrap().status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn using_bypasses_the_router_and_unknown_aliases_are_404() {
    let f = fixture().await;
    let id = f
        .client
        .post_json("/users", &json!({"name": "ann"}))
        .await
        .unwrap()
        .json::<Value>()
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    f.client
        .post_json("/events", &json!({"user_id": id, "kind": "x"}))
        .await
        .unwrap();

    let routed: Value = f.client.get("/events/count").await.unwrap().json().unwrap();
    assert_eq!(routed, json!({"database": ANALYTICS, "events": 1}));
    // The `default` database has no events table: reading it directly fails.
    let direct = f
        .client
        .get(&format!("/events/count?database={USERS}"))
        .await
        .unwrap();
    assert!(direct.status.is_server_error());
    let unknown = f
        .client
        .get("/events/count?database=nowhere")
        .await
        .unwrap();
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cross_database_combination_is_rejected() {
    let f = fixture().await;
    let res = f.client.get("/cross-database-union").await.unwrap();
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    assert!(res.text().contains("different databases"), "{}", res.text());
}
