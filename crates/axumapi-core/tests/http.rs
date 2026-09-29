//! End-to-end HTTP behaviour through the in-process test client.

use axumapi_core::*;
use axumapi_orm::{OrmError, QueryError};
use axumapi_testkit::TestClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Params {
    shout: bool,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Item {
    id: u32,
}

async fn hello(Path(name): Path<String>, Query(p): Query<Params>) -> PlainText<String> {
    PlainText(if p.shout { name.to_uppercase() } else { name })
}

async fn create(Json(item): Json<Item>) -> WithStatus<Json<Item>> {
    WithStatus(http::StatusCode::CREATED, Json(item))
}

async fn missing() -> ApiResult<Json<Item>> {
    Err(OrmError::from(QueryError::DoesNotExist))?
}

async fn item(Path(id): Path<u32>) -> Json<Item> {
    Json(Item { id })
}

struct Counter(u32);

async fn counter(State(c): State<Counter>) -> Json<u32> {
    Json(c.0)
}

async fn unregistered(State(_c): State<String>) -> NoContent {
    NoContent
}

fn client() -> TestClient {
    let v1 = App::new().route("/items/{id}", get(item));
    TestClient::new(
        App::new()
            .route("/hello/{name}", get(hello))
            .route("/items", post(create))
            .route("/missing", get(missing))
            .route("/counter", get(counter))
            .route("/nostate", get(unregistered))
            .nest("/api/v1", v1)
            .with_state(Counter(7)),
    )
}

#[tokio::test]
async fn extracts_path_and_query() {
    let res = client().get("/hello/bob?shout=true").await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.text(), "BOB");
}

#[tokio::test]
async fn unknown_route_is_404_problem() {
    let res = client().get("/nope").await.unwrap();
    assert_eq!(res.status, 404);
    assert_eq!(res.content_type(), Some("application/problem+json"));
    assert_eq!(res.json::<Value>().unwrap()["status"], 404);
}

#[tokio::test]
async fn bad_json_is_422_problem() {
    let res = client()
        .post_raw("/items", "application/json", b"{\"id\":\"x\"}".to_vec())
        .await
        .unwrap();
    assert_eq!(res.status, 422);
    assert_eq!(res.content_type(), Some("application/problem+json"));
    let body = res.json::<Value>().unwrap();
    assert_eq!(body["errors"][0]["location"], json!(["body"]));
    assert_eq!(body["errors"][0]["code"], "json_invalid");
}

#[tokio::test]
async fn valid_json_round_trips_with_status() {
    let res = client().post_json("/items", &Item { id: 3 }).await.unwrap();
    assert_eq!(res.status, 201);
    assert_eq!(res.json::<Item>().unwrap(), Item { id: 3 });
}

#[tokio::test]
async fn bad_query_is_422() {
    let res = client().get("/hello/bob?shout=maybe").await.unwrap();
    assert_eq!(res.status, 422);
    let body = res.json::<Value>().unwrap();
    assert_eq!(body["errors"][0]["location"], json!(["query"]));
}

#[tokio::test]
async fn bad_path_is_422() {
    let res = client().get("/api/v1/items/abc").await.unwrap();
    assert_eq!(res.status, 422);
    let body = res.json::<Value>().unwrap();
    assert_eq!(body["errors"][0]["location"], json!(["path"]));
}

#[tokio::test]
async fn orm_does_not_exist_maps_to_404() {
    let res = client().get("/missing").await.unwrap();
    assert_eq!(res.status, 404);
    assert_eq!(res.content_type(), Some("application/problem+json"));
}

#[tokio::test]
async fn orm_errors_map_to_expected_statuses() {
    let multi = ApiError::from(OrmError::from(QueryError::MultipleObjectsReturned(2)));
    assert_eq!(multi.status(), 500);
    assert!(!multi.detail().unwrap_or_default().contains('2'));
}

#[tokio::test]
async fn nested_app_serves_under_prefix() {
    let res = client().get("/api/v1/items/5").await.unwrap();
    assert_eq!(res.status, 200);
    assert_eq!(res.json::<Item>().unwrap(), Item { id: 5 });
    assert_eq!(client().get("/items/5").await.unwrap().status, 404);
}

#[tokio::test]
async fn state_is_extracted() {
    let res = client().get("/counter").await.unwrap();
    assert_eq!(res.json::<u32>().unwrap(), 7);
}

#[tokio::test]
async fn missing_state_is_generic_500() {
    let res = client().get("/nostate").await.unwrap();
    assert_eq!(res.status, 500);
    assert!(!res.text().contains("String"));
}

#[tokio::test]
async fn state_applies_regardless_of_registration_order() {
    let app = App::new()
        .with_state(Counter(7))
        .route("/counter", get(counter));
    let res = TestClient::new(app).get("/counter").await.unwrap();
    assert_eq!(res.status, 200);
}
