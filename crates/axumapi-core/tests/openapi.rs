//! OpenAPI generated from handler signatures, validated against the official
//! OpenAPI 3.1 JSON Schema.
#![allow(clippy::unwrap_used)]

use ::http::request::Parts;
use axumapi_core::http::StatusCode;
use axumapi_core::*;
use axumapi_openapi::{Operation, Schema, SchemaObject, SchemaRegistry};
use axumapi_testkit::TestClient;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize)]
struct User {
    id: i64,
    name: String,
}

impl Schema for User {
    fn schema_name() -> Option<&'static str> {
        Some("User")
    }
    fn schema(r: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("object")
            .with(
                "properties",
                json!({ "id": r.subschema::<i64>(), "name": r.subschema::<String>() }),
            )
            .with("required", json!(["id", "name"]))
    }
}

#[derive(Deserialize)]
struct Paging {
    limit: Option<u32>,
}

impl Schema for Paging {
    fn schema(r: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("object").with(
            "properties",
            json!({ "limit": r.subschema::<Option<u32>>() }),
        )
    }
}

/// Custom extractor with no documentation: must still work as an argument.
struct ClientIp(String);

impl FromRequestParts for ClientIp {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        Ok(ClientIp(
            parts
                .headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_owned(),
        ))
    }
}

/// Custom extractor that documents a header parameter.
struct ApiKey;

impl FromRequestParts for ApiKey {
    async fn from_request_parts(parts: &mut Parts) -> Result<Self, ApiError> {
        parts
            .headers
            .get("x-api-key")
            .map(|_| ApiKey)
            .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "missing key"))
    }
    fn describe(op: &mut Operation, r: &mut SchemaRegistry) {
        op.add_parameter(axumapi_openapi::Parameter::new(
            "x-api-key",
            axumapi_openapi::ParameterLocation::Header,
            true,
            r.subschema::<String>(),
        ));
    }
}

async fn get_user(Path(id): Path<i64>, ClientIp(ip): ClientIp) -> ApiResult<Json<User>> {
    tracing_stub(&ip);
    Ok(Json(User {
        id,
        name: "ann".into(),
    }))
}

async fn list_users(Query(p): Query<Paging>, _key: ApiKey) -> Json<Vec<User>> {
    let n = i64::from(p.limit.unwrap_or(1));
    Json(
        (0..n)
            .map(|id| User {
                id,
                name: "x".into(),
            })
            .collect(),
    )
}

async fn create_user(Json(u): Json<User>) -> Json<User> {
    Json(u)
}

async fn opaque() -> impl IntoResponse {
    "opaque"
}

fn app() -> App {
    let v1 = App::new()
        .route(
            "/users/{id}",
            get(get_user).summary("Get a user").tag("users"),
        )
        .route(
            "/users",
            post(create_user)
                .status(StatusCode::CREATED)
                .operation_id("create_user")
                .get(list_users)
                .tag("users"),
        );
    App::new()
        .title("Test API")
        .version("2.0.0")
        .route("/opaque", get(opaque))
        .route("/closure", get(|| async { Json(1_i64) }))
        .route("/hidden", get(|| async { "h" }).hidden())
        .mount("/api/v1", v1)
}

fn doc() -> Value {
    app().openapi().unwrap().to_value()
}

#[test]
fn document_is_valid_openapi_31() {
    let meta: Value = serde_json::from_str(include_str!(
        "../../axumapi-openapi/tests/fixtures/openapi-3.1-schema.json"
    ))
    .unwrap();
    let doc = doc();
    let validator = jsonschema::validator_for(&meta).unwrap();
    let errors: Vec<String> = validator.iter_errors(&doc).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:#?}\n{doc:#}");
}

#[test]
fn operations_are_derived_from_signatures() {
    let doc = doc();
    let paths = &doc["paths"];
    assert!(paths.get("/hidden").is_none());

    let get = &paths["/api/v1/users/{id}"]["get"];
    assert_eq!(get["summary"], "Get a user");
    assert_eq!(get["parameters"][0]["name"], "id");
    assert_eq!(get["parameters"][0]["in"], "path");
    assert_eq!(get["parameters"][0]["schema"]["type"], "integer");
    assert_eq!(
        get["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/User"
    );
    assert_eq!(
        get["responses"]["default"]["content"]["application/problem+json"]["schema"]["$ref"],
        "#/components/schemas/Problem"
    );

    let list = &paths["/api/v1/users"]["get"];
    let names: Vec<&str> = list["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["limit", "x-api-key"]);
    assert_eq!(list["parameters"][0]["required"], false);

    let create = &paths["/api/v1/users"]["post"];
    assert_eq!(create["operationId"], "create_user");
    assert!(create["responses"].get("200").is_none());
    assert_eq!(
        create["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/User"
    );
    assert_eq!(
        create["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/User"
    );

    let schemas = doc["components"]["schemas"].as_object().unwrap();
    assert_eq!(schemas.keys().collect::<Vec<_>>(), ["Problem", "User"]);
}

#[tokio::test]
async fn docs_endpoints_are_served_and_status_override_applies() {
    let client = TestClient::new(app());
    let spec = client.get("/openapi.json").await.unwrap();
    assert_eq!(spec.status, 200);
    assert_eq!(spec.json::<Value>().unwrap()["info"]["title"], "Test API");
    assert!(
        client
            .get("/docs")
            .await
            .unwrap()
            .text()
            .contains("swagger-ui")
    );
    assert!(client.get("/redoc").await.unwrap().text().contains("redoc"));

    let created = client
        .post_json("/api/v1/users", &json!({"id": 1, "name": "a"}))
        .await
        .unwrap();
    assert_eq!(created.status, 201);
    assert_eq!(client.get("/opaque").await.unwrap().text(), "opaque");
    assert_eq!(client.get("/closure").await.unwrap().text(), "1");
    assert_eq!(client.get("/api/v1/users/7").await.unwrap().status, 200);
    assert_eq!(client.get("/api/v1/users").await.unwrap().status, 401);
}

#[test]
fn misconfiguration_is_an_error_not_a_panic() {
    let dup = App::new().route("/a", get(opaque)).route("/a", get(opaque));
    assert!(matches!(
        TestClient::try_new(dup),
        Err(ServerError::Configuration(_))
    ));
    let bad = App::new().route("no-slash", get(opaque));
    assert!(matches!(
        TestClient::try_new(bad),
        Err(ServerError::Configuration(_))
    ));
}

fn tracing_stub(_: &str) {}

// Opt-in impls; `#[derive(Validate)]` generates these in application code.
impl axumapi_validation::Validate for User {}
impl axumapi_validation::Dump for User {}
impl axumapi_validation::Validate for Paging {}
