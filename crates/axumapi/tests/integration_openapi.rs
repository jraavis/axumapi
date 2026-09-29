//! The generated OpenAPI document for an app that uses every extractor kind
//! is valid OpenAPI 3.1 and is served consistently by the app.
#![allow(clippy::unwrap_used, dead_code)]

use axumapi::header::UserAgent;
use axumapi::prelude::*;
use axumapi::security::{
    ApiKey, ApiKeyLocation, ApiKeySpec, HttpBasic, HttpBearer, OAuth2PasswordBearer, OAuth2Spec,
};
use axumapi::{Cookies, Header};
use axumapi_testkit::TestClient;
use serde_json::Value;

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Item {
    id: i64,
    #[field(min_length = 1)]
    name: String,
}

#[derive(Debug, Deserialize, Validate, Schema)]
struct Paging {
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, Validate, Schema)]
struct Login {
    user: String,
}

struct HeaderKey;
impl ApiKeySpec for HeaderKey {
    const NAME: &'static str = "X-API-Key";
    const LOCATION: ApiKeyLocation = ApiKeyLocation::Header;
    const SCHEME: &'static str = "HeaderKey";
}

struct Oauth;
impl OAuth2Spec for Oauth {
    const TOKEN_URL: &'static str = "/token";
    const SCOPES: &'static [(&'static str, &'static str)] = &[("read", "Read items")];
    const SCHEME: &'static str = "OAuth2";
}

struct Settings(&'static str);

impl Dependency for Settings {
    async fn resolve(_ctx: &mut ResolveContext<'_>) -> Result<Self, ApiError> {
        Ok(Settings("prod"))
    }
}

async fn all_inputs(
    Path(id): Path<i64>,
    Query(paging): Query<Paging>,
    Header(UserAgent(agent)): Header<UserAgent>,
    cookies: Cookies,
    settings: Depends<Settings>,
    Json(item): Json<Item>,
) -> Json<Item> {
    let _ = (id, paging, agent, cookies, settings.0);
    Json(item)
}

async fn form_login(Form(login): Form<Login>) -> PlainText<String> {
    PlainText(login.user)
}

async fn bearer(_: HttpBearer) -> &'static str {
    "bearer"
}

async fn basic(_: HttpBasic) -> &'static str {
    "basic"
}

async fn api_key(_: ApiKey<HeaderKey>) -> &'static str {
    "key"
}

async fn oauth(_: OAuth2PasswordBearer<Oauth>) -> &'static str {
    "oauth"
}

fn app() -> App {
    App::new()
        .title("Everything")
        .version("1.0.0")
        .route("/items/{id}", post(all_inputs).summary("Every input kind"))
        .route("/login", post(form_login))
        .route("/bearer", get(bearer))
        .route("/basic", get(basic))
        .route("/api-key", get(api_key))
        .route("/oauth", get(oauth))
}

fn document() -> Value {
    app().openapi().unwrap().to_value()
}

#[test]
fn the_document_is_valid_openapi_31() {
    let meta: Value = serde_json::from_str(include_str!(
        "../../axumapi-openapi/tests/fixtures/openapi-3.1-schema.json"
    ))
    .unwrap();
    let doc = document();
    let validator = jsonschema::validator_for(&meta).unwrap();
    let errors: Vec<String> = validator.iter_errors(&doc).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:#?}\n{doc:#}");
}

#[test]
fn every_extractor_kind_is_documented() {
    let doc = document();
    let op = &doc["paths"]["/items/{id}"]["post"];
    let params: Vec<(&str, &str)> = op["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["in"].as_str().unwrap(), p["name"].as_str().unwrap()))
        .collect();
    assert!(params.contains(&("path", "id")), "{params:?}");
    assert!(params.contains(&("query", "limit")), "{params:?}");
    assert!(op["requestBody"]["content"]["application/json"].is_object());
    assert!(
        doc["paths"]["/login"]["post"]["requestBody"]["content"]["application/x-www-form-urlencoded"]
            .is_object()
    );
    assert!(doc["components"]["schemas"]["Item"].is_object());
}

#[test]
fn every_security_scheme_is_registered_and_referenced() {
    let doc = document();
    let schemes = &doc["components"]["securitySchemes"];
    for (scheme, path) in [
        ("HTTPBearer", "/bearer"),
        ("HTTPBasic", "/basic"),
        ("HeaderKey", "/api-key"),
        ("OAuth2", "/oauth"),
    ] {
        assert!(schemes[scheme].is_object(), "missing scheme {scheme}");
        let security = doc["paths"][path]["get"]["security"].as_array().unwrap();
        assert!(
            security.iter().any(|req| req.get(scheme).is_some()),
            "{path} does not require {scheme}"
        );
    }
}

#[tokio::test]
async fn the_served_document_matches_the_builder_output() {
    let client = TestClient::new(app());
    let response = client.get("/openapi.json").await.unwrap();
    assert_eq!(response.status, axumapi::http::StatusCode::OK);
    assert_eq!(response.json::<Value>().unwrap(), document());
}
